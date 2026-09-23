//! Topology projection — the relay's materialized view of Herdr state.
//!
//! The [`TopologyActor`](crate::TopologyActor) maintains one `Topology`:
//! a [`SessionSnapshot`] replaced wholesale on every `Synced` signal plus a
//! monotonically increasing `revision` consumers use as a change counter.
//! `stale` marks periods where the event stream is down (a supervisor
//! `Reconnecting` signal arrived) — readers may keep serving the last
//! snapshot but should not trust it as current.
//!
//! Baseline semantics (doc 02, Phase-1 slice): **events are invalidation
//! signals, never payloads** — a topology event triggers a fresh
//! `session.snapshot` rather than a partial apply. The Go oracle's
//! `SessionCache.Apply` event→state table can replace this later without
//! changing the published shape; snapshot-refresh is correct (if chattier)
//! for every topology event kind.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use lerdr_core::json::MaybeNull;
use lerdr_core::protocol::{AgentState, HerdrStatus, Workspace, WorkspaceWorktree};
use lerdr_herdr::{AgentInfo, SessionSnapshot, WorkspaceInfo};

/// `SessionCache` observation times for one pane (state.go:470-520) —
/// survives `accept()` like `generations`: `last_seen_at` refreshes on
/// every apply that still contains the agent, `updated_at` bumps only when
/// the agent's observable state changed (Herdr's `state_change_seq` or
/// `revision` advanced, or first appearance). `last_active_at` stays 0 —
/// the oracle derives it from activity entries this slice does not keep;
/// clients fall back to `updated_at`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AgentTimes {
    pub updated_at: i64,
    pub last_seen_at: i64,
    /// Change key the times were taken for: `(state_change_seq, revision)`.
    change_key: (u64, u64),
    seen: bool,
}

/// One projected view of Herdr topology.
#[derive(Debug)]
pub struct Topology {
    /// The last `session.snapshot` accepted (bootstrap or post-invalidation
    /// refresh).
    pub snapshot: SessionSnapshot,
    /// Bumped on every observable change — accepted snapshot, stale
    /// transition, generation bump — consumers diff on this.
    pub revision: u64,
    /// `true` while the event stream is reconnecting — `snapshot` is the
    /// last-known state, not necessarily current.
    pub stale: bool,
    /// `State.generation` (state.go:76) — per-pane session epoch.
    /// Coordinator bookkeeping, not snapshot data: entries survive
    /// `accept()` so a pane replaced between refreshes never validates a
    /// stale exact target (`validateExactPaneTarget`, server.go:345).
    pub(crate) generations: BTreeMap<String, i64>,
    /// Per-pane observation times — see [`AgentTimes`].
    pub(crate) agent_times: BTreeMap<String, AgentTimes>,
    /// Wall-clock of the last accepted snapshot — `last_success_at` on the
    /// `inventory_status` projection (the snapshot poll *is* the inventory).
    pub(crate) accepted_at: i64,
    /// Herdr capability/probe evidence projected into `herdr_status` —
    /// the oracle's `herdrStatusPayload`, filled field-for-field from the
    /// `lerdr_herdr` capability report (`ServerStatus`). Populated by the
    /// actor on every (re)bootstrap and refresh tick; `features` stays an
    /// empty object until the first report lands. Survives `accept()`
    /// like `generations` — it is relay-side evidence, not snapshot data.
    pub herdr_status: HerdrStatus,
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Default for Topology {
    fn default() -> Self {
        Self {
            snapshot: SessionSnapshot::default(),
            revision: 0,
            stale: true,
            generations: BTreeMap::new(),
            agent_times: BTreeMap::new(),
            accepted_at: 0,
            herdr_status: HerdrStatus {
                // `features` decodes non-nullable on Kotlin — an empty
                // object, never `null` (see `snapshot::herdr_status`).
                features: MaybeNull::Value(BTreeMap::new()),
                ..HerdrStatus::default()
            },
        }
    }
}

impl Topology {
    /// Replace the snapshot and mark the view fresh.
    pub(crate) fn accept(&mut self, snapshot: SessionSnapshot) {
        let now = now_millis();
        for agent in &snapshot.agents {
            let key = (agent.state_change_seq, agent.revision);
            let times = self.agent_times.entry(agent.pane_id.clone()).or_default();
            times.last_seen_at = now;
            if !times.seen || times.change_key != key {
                times.change_key = key;
                times.updated_at = now;
                times.seen = true;
            }
        }
        self.snapshot = snapshot;
        self.revision += 1;
        self.stale = false;
        self.accepted_at = now;
    }

    /// Install the capability evidence the actor collected. Returns
    /// `true` (and bumps the revision so broadcasts republish
    /// `herdr_status`) when any field changed — an unchanged report
    /// carries unchanged per-feature generations, so whole-struct
    /// equality suppresses no-change republishes.
    pub fn set_herdr_status(&mut self, status: HerdrStatus) -> bool {
        if self.herdr_status == status {
            return false;
        }
        self.herdr_status = status;
        self.revision += 1;
        true
    }

    /// Mark the view stale (event stream down). Returns `true` when the
    /// transition published a new revision.
    pub(crate) fn mark_stale(&mut self) -> bool {
        if self.stale {
            return false;
        }
        self.stale = true;
        self.revision += 1;
        true
    }

    /// `State.Generation` (state.go:284) — the pane's current session
    /// epoch (0 before its first replacement).
    pub fn generation_of(&self, pane_id: &str) -> i64 {
        self.generations.get(pane_id).copied().unwrap_or(0)
    }

    /// `State.BumpGeneration` (state.go:304) — a lifecycle mutation
    /// replaced the pane's session; the epoch advances. The epoch rides
    /// the `agents` projection, so the view counts as revised — and the
    /// row visibly changed (session cleared/restarted), so `updated_at`
    /// moves too rather than waiting for Herdr's `state_change_seq`.
    pub(crate) fn bump_generation(&mut self, pane_id: &str) {
        *self.generations.entry(pane_id.to_owned()).or_insert(0) += 1;
        let times = self.agent_times.entry(pane_id.to_owned()).or_default();
        times.updated_at = now_millis();
        times.seen = true;
        self.revision += 1;
    }

    /// `AgentInfo` → `protocol.AgentState`. `server_session_id` is always
    /// `"primary"` (`projectAgentResource`, server.go:3407) and
    /// `generation` carries this pane's epoch — both belong to the
    /// exact-target tuple clients echo back. `agent_session_id` is the
    /// oracle's `SessionID`: `TrimSpace(agent_session.value)`
    /// (`resolveAgentSessionName`, server.go:518-524) — the Go client
    /// flattens `agent_session` into `Pane.Session` without consulting
    /// `kind` (client.go:382). `session` is that raw value verbatim (the
    /// oracle rewrites it to the resolved *title* only when its sessions
    /// store has one — this relay has no title resolver, so
    /// `session_name` stays empty; doc 10). `updated_at`/`last_seen_at`
    /// come from [`AgentTimes`] observation bookkeeping; fields the Herdr
    /// snapshot does not carry (`raw_pane_id`, `tab_*` numbering,
    /// `last_active_at`, attention fields) stay zero — see doc 10.
    pub(crate) fn agent_state(&self, info: &AgentInfo) -> AgentState {
        let (session, agent_session_id, session_name) = match &info.agent_session {
            Some(s) => (s.value.clone(), s.value.trim().to_owned(), String::new()),
            None => (String::new(), String::new(), String::new()),
        };
        let times = self
            .agent_times
            .get(&info.pane_id)
            .copied()
            .unwrap_or_default();
        AgentState {
            pane_id: info.pane_id.clone(),
            terminal_id: info.terminal_id.clone(),
            server_session_id: "primary".to_owned(),
            generation: self.generation_of(&info.pane_id),
            workspace_id: info.workspace_id.clone(),
            tab_id: info.tab_id.clone(),
            focused: info.focused,
            status: info.agent_status.to_string(),
            agent: info.agent.clone().unwrap_or_default(),
            name: info.name.clone().unwrap_or_default(),
            cwd: info
                .cwd
                .clone()
                .or_else(|| info.foreground_cwd.clone())
                .unwrap_or_default(),
            session,
            agent_session_id,
            session_name,
            updated_at: times.updated_at,
            last_seen_at: times.last_seen_at,
            ..AgentState::default()
        }
    }

    /// `WorkspaceInfo` → `protocol.Workspace`.
    fn workspace(info: &WorkspaceInfo) -> Workspace {
        Workspace {
            workspace_id: info.workspace_id.clone(),
            number: info.number as i64,
            label: info.label.clone(),
            focused: info.focused,
            pane_count: info.pane_count as i64,
            tab_count: info.tab_count as i64,
            active_tab_id: info.active_tab_id.clone(),
            agent_status: info.agent_status.to_string(),
            cwd: String::new(),
            worktree: info.worktree.as_ref().map(|w| WorkspaceWorktree {
                repo_key: w.repo_key.clone(),
                repo_name: w.repo_name.clone(),
                repo_root: w.repo_root.clone(),
                checkout_path: w.checkout_path.clone(),
                is_linked_worktree: w.is_linked_worktree,
            }),
        }
    }

    /// Projected agent list for the `agents` outbound message, sorted by
    /// `pane_id` like `snapshotLocked` (state.go:1016) — broadcast dedupe
    /// and stable client rendering both need a deterministic order.
    pub fn agents(&self) -> Vec<AgentState> {
        let mut agents: Vec<AgentState> = self
            .snapshot
            .agents
            .iter()
            .map(|info| self.agent_state(info))
            .collect();
        agents.sort_by(|a, b| a.pane_id.cmp(&b.pane_id));
        agents
    }

    /// Projected workspace list for the `workspaces` outbound message.
    pub fn workspaces(&self) -> Vec<Workspace> {
        self.snapshot
            .workspaces
            .iter()
            .map(Self::workspace)
            .collect()
    }

    /// Look up one agent's pane id (none when the pane is gone).
    pub fn pane_of(&self, pane_id: &str) -> Option<&AgentInfo> {
        self.snapshot.agents.iter().find(|a| a.pane_id == pane_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_stale() {
        let t = Topology::default();
        assert!(t.stale);
        assert_eq!(t.revision, 0);
        assert!(t.agents().is_empty());
    }

    #[test]
    fn accept_replaces_and_bumps() {
        let mut t = Topology::default();
        t.accept(SessionSnapshot::default());
        assert!(!t.stale);
        assert_eq!(t.revision, 1);
        t.mark_stale();
        assert!(t.stale);
        assert_eq!(t.revision, 2);
        // Stale→stale does not churn the revision.
        t.mark_stale();
        assert_eq!(t.revision, 2);
    }

    #[test]
    fn agent_projection_maps_wire_fields() {
        let mut t = Topology::default();
        t.accept(SessionSnapshot {
            agents: vec![AgentInfo {
                pane_id: "wE:p1".into(),
                terminal_id: "term_1".into(),
                workspace_id: "wE".into(),
                tab_id: "wE:t1".into(),
                focused: true,
                agent_status: lerdr_herdr::AgentStatus::Working,
                agent: Some("devin".into()),
                cwd: Some("/home/l".into()),
                ..AgentInfo::default()
            }],
            ..SessionSnapshot::default()
        });
        let agents = t.agents();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].pane_id, "wE:p1");
        assert_eq!(agents[0].status, "working");
        assert_eq!(agents[0].agent, "devin");
        assert_eq!(agents[0].cwd, "/home/l");
        assert!(agents[0].focused);
    }

    #[test]
    fn generation_survives_accept_and_bumps() {
        let mut t = Topology::default();
        assert_eq!(t.generation_of("wE:p1"), 0);
        t.bump_generation("wE:p1");
        assert_eq!(t.generation_of("wE:p1"), 1);
        // Generations are coordinator bookkeeping — a snapshot replace
        // must not reset them (state.go:304 + the oracle's map lifecycle).
        t.accept(SessionSnapshot::default());
        assert_eq!(t.generation_of("wE:p1"), 1);
        t.bump_generation("wE:p1");
        assert_eq!(t.generation_of("wE:p1"), 2);
    }

    #[test]
    fn agents_carry_exact_target_tuple_sorted() {
        let mut t = Topology::default();
        t.accept(SessionSnapshot {
            agents: vec![
                AgentInfo {
                    pane_id: "wE:p2".into(),
                    terminal_id: "term_2".into(),
                    agent_session: Some(lerdr_herdr::AgentSessionInfo {
                        source: "sess".into(),
                        agent: "devin".into(),
                        kind: lerdr_herdr::AgentSessionRefKind::Id,
                        // The oracle emits `TrimSpace(value)`.
                        value: " sess-2 ".into(),
                    }),
                    ..AgentInfo::default()
                },
                AgentInfo {
                    pane_id: "wE:p1".into(),
                    terminal_id: "term_1".into(),
                    ..AgentInfo::default()
                },
            ],
            ..SessionSnapshot::default()
        });
        t.bump_generation("wE:p2");
        let agents = t.agents();
        assert_eq!(agents.len(), 2);
        // `snapshotLocked` order: ascending pane_id.
        assert_eq!(agents[0].pane_id, "wE:p1");
        assert_eq!(agents[1].pane_id, "wE:p2");
        for agent in &agents {
            assert_eq!(agent.server_session_id, "primary");
        }
        assert_eq!(agents[0].generation, 0);
        assert_eq!(agents[1].generation, 1);
        assert_eq!(agents[1].agent_session_id, "sess-2");
        // Wire `session` is `agent_session.value` verbatim
        // (client.go:382); `session_name` is the oracle's resolved title —
        // with no title resolver here it stays empty.
        assert_eq!(agents[1].session, " sess-2 ");
        assert_eq!(agents[1].session_name, "");
    }
}
