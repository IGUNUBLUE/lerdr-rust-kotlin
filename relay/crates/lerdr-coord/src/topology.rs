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

use lerdr_core::protocol::{AgentState, Workspace, WorkspaceWorktree};
use lerdr_herdr::{AgentInfo, AgentSessionRefKind, SessionSnapshot, WorkspaceInfo};

/// One projected view of Herdr topology.
#[derive(Debug)]
pub struct Topology {
    /// The last `session.snapshot` accepted (bootstrap or post-invalidation
    /// refresh).
    pub snapshot: SessionSnapshot,
    /// Bumped on every accepted snapshot — consumers diff on this.
    pub revision: u64,
    /// `true` while the event stream is reconnecting — `snapshot` is the
    /// last-known state, not necessarily current.
    pub stale: bool,
}

impl Default for Topology {
    fn default() -> Self {
        Self {
            snapshot: SessionSnapshot::default(),
            revision: 0,
            stale: true,
        }
    }
}

impl Topology {
    /// Replace the snapshot and mark the view fresh.
    pub(crate) fn accept(&mut self, snapshot: SessionSnapshot) {
        self.snapshot = snapshot;
        self.revision += 1;
        self.stale = false;
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

    /// `AgentInfo` → `protocol.AgentState`. Fields the Herdr snapshot does
    /// not carry (`raw_pane_id`, `server_session_id`, `generation`,
    /// `tab_*` numbering, timestamps, attention fields) stay zero — the
    /// oracle fills those from `SessionCache.Apply` bookkeeping; see doc 10.
    fn agent_state(info: &AgentInfo) -> AgentState {
        let (session, agent_session_id, session_name) = match &info.agent_session {
            Some(s) => {
                let name = match s.kind {
                    AgentSessionRefKind::Id => s.value.clone(),
                    _ => String::new(),
                };
                (s.source.clone(), s.value.clone(), name)
            }
            None => (String::new(), String::new(), String::new()),
        };
        AgentState {
            pane_id: info.pane_id.clone(),
            terminal_id: info.terminal_id.clone(),
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

    /// Projected agent list for the `agents` outbound message.
    pub fn agents(&self) -> Vec<AgentState> {
        self.snapshot.agents.iter().map(Self::agent_state).collect()
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
}
