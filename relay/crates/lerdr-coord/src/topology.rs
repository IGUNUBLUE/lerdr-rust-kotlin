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
//!
//! The semantic half — committed classifications, blocked event ids, the
//! unseen/ack/done bookkeeping, and the revision counters the fences read —
//! lives in the shared [`AttentionLedger`](crate::classify::AttentionLedger)
//! (`coordinator.State`'s per-pane maps). `accept` is the oracle's
//! `commitInventoryLocked`: session replacement, blocked-cycle mint/clear,
//! transition records, and completion bookkeeping all run inside the same
//! accept, and every published clone shares the ledger so projector commits
//! land without a new snapshot.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use lerdr_core::json::MaybeNull;
use lerdr_core::protocol::{AgentState, HerdrStatus, Workspace, WorkspaceWorktree};
use lerdr_herdr::{AgentInfo, SessionSnapshot, WorkspaceInfo};

use crate::classify::{
    is_attention_status, is_done_status, kind_str, mint_blocked_event_id,
    preserves_chat_completion, wire_interaction, AttentionCell, AttentionKind, PaneTransition,
    SharedLedger,
};

/// `commitInventoryLocked`'s per-pane observation times plus the
/// `LastActiveAt` half `AcknowledgePane` needs (state.go:504-523). The
/// change key is the oracle's same-fields tuple: `Status`, `Name`, `Cwd`,
/// `Agent`, `ActivitySeq`, `PaneRevision`, `ScrollMaxOffset`,
/// `ForegroundCwd` — `updated_at` holds while every one is unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AgentTimes {
    pub updated_at: i64,
    /// `LastActiveAt` — advances with `updated_at` only when the status or
    /// the activity sequence moved (`activityAdvanced`).
    pub last_active_at: i64,
    /// The same-fields tuple `updated_at` diffs on.
    change_key: ChangeKey,
    seen: bool,
}

/// The oracle's `UpdatedAt` same-fields tuple (state.go:512-514).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ChangeKey {
    status: String,
    name: String,
    cwd: String,
    agent: String,
    activity_seq: u64,
    pane_revision: u64,
    scroll_max_offset: u64,
    foreground_cwd: String,
}

impl ChangeKey {
    /// `scroll_max_offset` comes from the matching `PaneInfo.scroll`
    /// (`AgentInfo` does not carry scroll metrics — the oracle's
    /// `ScrollMaxOffset` reads `pane.Scroll.MaxOffsetFromBottom`).
    fn of(agent: &AgentInfo, scroll_max_offset: u64) -> Self {
        Self {
            status: agent.agent_status.to_string(),
            name: agent.name.clone().unwrap_or_default(),
            cwd: agent.cwd.clone().unwrap_or_default(),
            agent: agent.agent.clone().unwrap_or_default(),
            activity_seq: agent.state_change_seq,
            pane_revision: agent.revision,
            scroll_max_offset,
            foreground_cwd: agent.foreground_cwd.clone().unwrap_or_default(),
        }
    }
}

/// One projected view of Herdr topology.
#[derive(Debug)]
pub struct Topology {
    /// The last `session.snapshot` accepted (bootstrap or post-invalidation
    /// refresh).
    pub snapshot: SessionSnapshot,
    /// Bumped on every observable change — accepted snapshot, stale
    /// transition, generation bump, ack republish — consumers diff on
    /// this. It is `revCounter`'s epoch source: every `accept` assigns
    /// `revision` to each pane's `state_rev` (state.go:445-542).
    pub revision: u64,
    /// `true` while the event stream is reconnecting — `snapshot` is the
    /// last-known state, not necessarily current.
    pub stale: bool,
    /// `State.generation` (state.go:76) — per-pane session epoch.
    /// Coordinator bookkeeping, not snapshot data: entries survive
    /// `accept()` so a pane replaced between refreshes never validates a
    /// stale exact target (`validateExactPaneTarget`, server.go:345).
    /// A disappearance or a detected replacement both advance it.
    pub(crate) generations: BTreeMap<String, i64>,
    /// Per-pane observation times — see [`AgentTimes`].
    pub(crate) agent_times: BTreeMap<String, AgentTimes>,
    /// The shared per-pane attention ledger — `commitAttentionClassification`
    /// writes and `AcknowledgePane` acks land here; `agent_state` overlays
    /// it onto the snapshot row. Shared across published clones.
    pub(crate) attention: SharedLedger,
    /// `s.sessions` (server.go) — the session-title resolver
    /// `resolveAgentSessionName` consults in the pre-commit enrich pass.
    /// An `Option` slot installed by the projector so commits before the
    /// install (and `Topology::default()` tests) project `session_name`
    /// empty.
    pub(crate) resolver: crate::actor::ResolverSlot,
    /// Wall-clock of the last accepted snapshot — `last_success_at` on the
    /// `inventory_status` projection (the snapshot poll *is* the inventory).
    pub(crate) accepted_at: i64,
    /// `s.lastAttemptAt` (state.go:82) — the last inventory attempt's
    /// wall-clock, stamped by every commit and every poll failure.
    pub(crate) attempted_at: i64,
    /// `s.inventoryReady` (state.go:79) — `true` after any commit
    /// (`commitInventoryLocked` covers poll AND event paths), `false`
    /// after [`mark_inventory_failure`](Self::mark_inventory_failure).
    /// The event stream dropping does NOT touch it — the oracle's
    /// reconnect path only shortens the poll cadence.
    pub(crate) inventory_ready: bool,
    /// `s.inventoryErrorCode`/`s.inventoryMessage` — `command_failed` +
    /// the fixed operator message while a poll failure is unresolved.
    pub(crate) inventory_error_code: String,
    pub(crate) inventory_message: String,
    /// Herdr capability/probe evidence projected into `herdr_status` —
    /// the oracle's `herdrStatusPayload`, filled field-for-field from the
    /// `lerdr_herdr` capability report (`ServerStatus`). Populated by the
    /// actor on every (re)bootstrap and refresh tick; `features` stays an
    /// empty object until the first report lands. Survives `accept()`
    /// like `generations` — it is relay-side evidence, not snapshot data.
    pub herdr_status: HerdrStatus,
    /// The `publishCurrentInventory` batch for this revision
    /// (server.go:3435-3514): the actor computes the changed-vs-published
    /// diff once per commit and stamps it here, so every per-client
    /// forwarder emits the *same* dedup'd frame set — `agents`,
    /// `workspaces`, `inventory_status`, `herdr_status` only where the
    /// published view actually moved. `compose_snapshot` never consults
    /// it: the handshake burst always sends the full inventory.
    pub(crate) broadcast_frames: Vec<lerdr_core::protocol::Outbound>,
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
            attention: crate::classify::AttentionLedger::shared(),
            resolver: Default::default(),
            accepted_at: 0,
            attempted_at: 0,
            inventory_ready: false,
            inventory_error_code: String::new(),
            inventory_message: String::new(),
            herdr_status: HerdrStatus {
                // `features` decodes non-nullable on Kotlin — an empty
                // object, never `null` (see `snapshot::herdr_status`).
                features: MaybeNull::Value(BTreeMap::new()),
                ..HerdrStatus::default()
            },
            broadcast_frames: Vec::new(),
        }
    }
}

/// What `accept` reports beside the published revision: the transition
/// records `registerTransition` emitted (fed to the projector) and the
/// removed pane ids (their `customAnswers` must drop — state.go:578).
#[derive(Debug, Default)]
pub(crate) struct AcceptOutcome {
    pub transitions: Vec<PaneTransition>,
    /// Pane ids that disappeared this commit — `customAnswers` cleanup.
    pub removed: Vec<String>,
}

/// Which oracle commit path an accept follows (state.go:371-428).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommitKind {
    /// `commitTopologyLocked` — an event-path snapshot (the `Synced`
    /// bootstrap and `pane.*`/`tab.*`/`workspace.*` invalidations). The
    /// sampled `agent_status` must NOT overwrite the committed status
    /// stream for an existing, non-replaced pane: the oracle copies
    /// `existing.Status` over the incoming row and re-copies its blocked
    /// details (`copyBlockedDetails`) when that status is `blocked`, and
    /// the shared commit's `applyBlockedCycleLocked` clears whatever the
    /// enrich wrote otherwise — so a status flip never lands on the event
    /// path and an enrich classification is only ever adopted for fresh
    /// or replaced panes. Status is poll-authoritative.
    Event,
    /// `commitInventoryLocked` via `CommitPoll` — the 15s reconcile poll
    /// and every `d.wake()` (ack/read/command-driven refresh): the fresh
    /// sample is authoritative and adopts live status wholesale.
    Poll,
}

/// `paneSessionReplaced` (state.go:592-607) — a changed stable identity
/// (raw pane, terminal, tab, or workspace) means the *pane session* was
/// replaced even though the pane id survived. `RawPaneID` is `pane_id` in
/// this projection, so it can never differ across the keyed lookup; the
/// terminal/tab/workspace legs carry the check.
fn pane_session_replaced(existing: &AgentInfo, incoming: &AgentInfo) -> bool {
    pane_identity_moved(
        [
            &existing.terminal_id,
            &existing.tab_id,
            &existing.workspace_id,
        ],
        [
            &incoming.terminal_id,
            &incoming.tab_id,
            &incoming.workspace_id,
        ],
    )
}

/// The field rule `paneSessionReplaced` compares — a changed non-empty
/// stable identity (terminal, tab, or workspace) means the pane session
/// was replaced even though the pane id survived. Shared with the
/// `PaneInfo` pass so plain panes get the same reset semantics.
fn pane_identity_moved(existing: [&String; 3], incoming: [&String; 3]) -> bool {
    existing
        .into_iter()
        .zip(incoming)
        .any(|(left, right)| !left.is_empty() && !right.is_empty() && left != right)
}

impl Topology {
    /// `commitInventoryLocked` (state.go:430-590) — reconcile one full
    /// snapshot. Events are invalidations in this relay, so this is the
    /// only commit path (the oracle's event-committed rows and
    /// `pendingEvents` have no counterpart; `baseRev` ordering is moot —
    /// every commit is a whole snapshot).
    ///
    /// Per pane, in the oracle's order: session-replacement detection and
    /// generation bump, `updated_at`/`last_active_at` bookkeeping, the
    /// blocked-cycle sync (`applyBlockedCycleLocked`), content/attention
    /// revision bumps, `registerTransition`, fresh-pane done/ack seeding,
    /// `syncAttentionCompletionLocked`, and the blocked→blocked approval
    /// refire. Then the `!seen` removal pass drops every per-pane ledger
    /// (except the generation).
    ///
    /// The production commit path is [`accept_enriched`](Self::accept_enriched)
    /// — the actor always runs the poller's `SetEnrich` classification
    /// pass ahead of it; this bare form exists for tests.
    #[cfg(test)]
    pub(crate) fn accept(&mut self, snapshot: SessionSnapshot) -> AcceptOutcome {
        self.accept_enriched(snapshot, &BTreeMap::new(), CommitKind::Poll)
    }

    /// `accept` with the poller's `SetEnrich` pass (server.go:1235-1255)
    /// folded in: `enrichments` carries one fresh classification per
    /// blocked pane — read+classify ahead of the commit — landed on the
    /// committed halves in the same epoch. On entry the enriched kind
    /// survives the cycle mint (`applyBlockedCycleLocked` only defaults
    /// an empty kind); on a sustained blocked cycle it drives
    /// `attentionChanged`/`attentionRev` and the kind-drift refire.
    ///
    /// `kind` picks the oracle's commit surface: [`CommitKind::Poll`]
    /// adopts the sampled statuses wholesale; [`CommitKind::Event`]
    /// preserves the committed `agent_status` (and therefore the
    /// committed blocked details) for every existing, non-replaced pane
    /// — `commitTopologyLocked` rewrites the incoming rows before the
    /// shared commit sees them, so the same machinery below diffs,
    /// transitions, and projects the preserved row exactly like the
    /// oracle's `cp`.
    pub(crate) fn accept_enriched(
        &mut self,
        mut snapshot: SessionSnapshot,
        enrichments: &BTreeMap<String, crate::classify::Classification>,
        kind: CommitKind,
    ) -> AcceptOutcome {
        let now = now_millis();
        self.revision += 1;
        let epoch = self.revision as i64;
        // `initialSnapshot` — `!s.inventoryReady` before the first commit.
        let initial = self.accepted_at == 0;
        let mut outcome = AcceptOutcome::default();

        let old: BTreeMap<&str, &AgentInfo> = self
            .snapshot
            .agents
            .iter()
            .map(|agent| (agent.pane_id.as_str(), agent))
            .collect();
        if kind == CommitKind::Event {
            // `commitTopologyLocked` (state.go:390-412): an existing,
            // non-replaced pane keeps its committed status — the sampled
            // `agent_status` is not the authoritative stream on the event
            // path. The committed row that lands below is then identical
            // in every status/attention field the oracle preserves:
            // `sync_cycle` against the preserved status keeps the
            // committed blocked details (`cp.Status == "blocked"` keeps
            // `copyBlockedDetails`'s copy) or clears them on a
            // non-blocked committed row (`clearBlockedDetails` — the
            // enrich classification written onto the incoming row is
            // discarded either way, so the enrich gate below skips it).
            for agent in &mut snapshot.agents {
                let Some(existing) = old.get(agent.pane_id.as_str()) else {
                    continue;
                };
                if pane_session_replaced(existing, agent) {
                    continue;
                }
                agent.agent_status = existing.agent_status;
            }
        }
        // `pane.Scroll.MaxOffsetFromBottom` rides `PaneInfo`, not
        // `AgentInfo` — index the pane records once for the change keys.
        let panes: BTreeMap<&str, &lerdr_herdr::PaneInfo> = snapshot
            .panes
            .iter()
            .map(|pane| (pane.pane_id.as_str(), pane))
            .collect();
        // `resolveAgentSessionName` (server.go:505-534) — the enrich pass's
        // title half runs on every incoming row before the commit, so the
        // resolver's file I/O never executes under the ledger lock. The
        // committed row's `SessionName` lives on the cell; `agent_state`
        // projects it and rewrites `session` like the oracle's
        // `agent.Session = title`.
        let titles = self.session_titles(&snapshot.agents);
        let mut ledger = self.attention.lock().expect("attention ledger poisoned");

        for incoming in &snapshot.agents {
            let pane_id = incoming.pane_id.as_str();
            let mut existing = old.get(pane_id).copied();

            // `paneSessionReplaced` — the replaced pane's ledgers wipe and
            // its row counts as fresh (`existing = nil` downstream).
            let replaced = existing.is_some_and(|e| pane_session_replaced(e, incoming));
            if replaced {
                *self.generations.entry(pane_id.to_owned()).or_insert(0) += 1;
                // The upstream output counter restarted with the pane
                // session — drop the old epoch's watermark before the
                // seed pass folds the fresh revision.
                ledger.reset_upstream_rev(pane_id);
                ledger.cell_mut(pane_id).reset_on_replacement();
                existing = None;
            }
            let cell = ledger.cell_mut(pane_id);
            if existing.is_none()
                && !replaced
                && !initial
                && self.generations.get(pane_id).copied().unwrap_or(0) > 0
            {
                // Disappearance already ended the previous epoch;
                // reappearance opens another so work admitted during the
                // absence cannot target the replacement session.
                *self.generations.entry(pane_id.to_owned()).or_insert(0) += 1;
            }

            // `UpdatedAt` (state.go:504-518): fresh panes stamp `0` on the
            // initial snapshot and `now` afterwards; existing panes hold
            // their timestamp while the same-fields tuple is unchanged.
            let scroll_max_offset = panes
                .get(pane_id)
                .and_then(|pane| pane.scroll)
                .map(|scroll| scroll.max_offset_from_bottom)
                .unwrap_or_default();
            let key = ChangeKey::of(incoming, scroll_max_offset);
            let times = self.agent_times.entry(pane_id.to_owned()).or_default();
            if existing.is_none() {
                times.updated_at = if initial { 0 } else { now };
                times.change_key = key;
                times.seen = true;
            } else if !times.seen || times.change_key != key {
                times.change_key = key;
                times.updated_at = now;
                times.seen = true;
            }
            // `activityAdvanced` (state.go:519-523) — status or
            // activitySeq moved; `pendingTimestamp` has no counterpart.
            let activity_advanced = existing.is_some_and(|e| {
                e.agent_status != incoming.agent_status
                    || e.state_change_seq != incoming.state_change_seq
            });
            if activity_advanced && times.updated_at > times.last_active_at {
                times.last_active_at = times.updated_at;
            }

            let status = incoming.agent_status.to_string();
            let prev_status = cell.prev_status.clone();
            // `previousAttention` — the pre-commit committed kind, before
            // this accept's cycle sync + enrich land.
            let previous_attention = cell.blocked.kind;

            // `applyBlockedCycleLocked` + `attentionChanged` —
            // mint/clear the blocked cycle against the committed halves,
            // then apply the enrich classification like the oracle's
            // enriched `cp` (kind/options drift while blocked is the
            // `attentionChanged`/`attentionRev` signal — it also feeds
            // `contentRev` on the oracle's line 528-531).
            let mut attention_changed = cell.sync_cycle(&status, &mut mint_blocked_event_id);
            // `commitTopologyLocked` never lets the enrich reach a
            // preserved pane: `copyBlockedDetails` overwrites it on a
            // blocked committed row and `clearBlockedDetails` wipes it on
            // any other — applying it here would refire an
            // `attentionChanged` the oracle never publishes. Only fresh
            // or replaced rows adopt the classification on the event
            // path (their committed row *is* the incoming one).
            let adopts_enrich = kind == CommitKind::Poll || existing.is_none();
            if status == "blocked" && adopts_enrich {
                if let Some(classification) = enrichments.get(pane_id) {
                    attention_changed |= cell.apply_classification(classification);
                }
            }

            // `contentRev`/`attentionRev` (state.go:528-535) — the
            // same-fields tuple minus pane revision/scroll/foreground.
            if existing.is_none()
                || existing.is_some_and(|e| {
                    e.agent_status != incoming.agent_status
                        || e.name != incoming.name
                        || e.cwd != incoming.cwd
                        || e.agent != incoming.agent
                        || e.state_change_seq != incoming.state_change_seq
                })
                || attention_changed
            {
                cell.content_rev += 1;
            }
            if attention_changed {
                cell.attention_rev += 1;
            }

            // `agent.SessionName = title` — the enrich pass resolved it
            // unconditionally, so an unresolved row lands `""` and a stale
            // title never survives a session change.
            cell.session_name = titles.get(pane_id).cloned().unwrap_or_default();

            cell.state_rev = epoch;
            cell.prev_status.clone_from(&status);

            // `registerTransition` — before the fresh-pane done/ack
            // seeding, matching the oracle's order.
            if let Some(transition) = cell.register_transition(
                pane_id,
                incoming.agent.as_deref().unwrap_or_default(),
                &project_of(incoming.cwd.as_deref().unwrap_or_default()),
                &prev_status,
                &status,
                previous_attention,
                epoch,
            ) {
                outcome.transitions.push(transition);
            }
            // `!exists && (done || idle)` seeding (state.go:546-553).
            if existing.is_none() && (is_done_status(&status) || status == "idle") {
                if times.last_active_at > cell.last_seen_at {
                    cell.unseen_done = true;
                    cell.ack_done = false;
                } else if is_done_status(&status)
                    && (times.last_active_at > 0 || cell.last_seen_at > 0)
                {
                    cell.ack_done = true;
                }
            }
            if !preserves_chat_completion(&prev_status, &status, previous_attention) {
                cell.sync_attention_completion(previous_attention, cell.blocked.kind, epoch);
            }
            // The blocked→blocked refire (state.go:558-563): a kind
            // change or a fresh approval inside the same cycle re-runs the
            // transition so the new details broadcast + republish push.
            if prev_status == "blocked"
                && status == "blocked"
                && (previous_attention != cell.blocked.kind
                    || (attention_changed && cell.blocked.kind == Some(AttentionKind::Approval)))
            {
                outcome.transitions.push(PaneTransition {
                    pane_id: pane_id.to_owned(),
                    agent: incoming.agent.clone().unwrap_or_default(),
                    project: project_of(incoming.cwd.as_deref().unwrap_or_default()),
                    status: status.clone(),
                    revision: epoch,
                    observed_at: now,
                });
            }
            // `AgentInfo.revision` — the pane's upstream output counter —
            // seeds the shared watermark here so agent rows without a
            // `panes` row still seed (the pane pass below folds the rest).
            ledger.note_upstream_rev(pane_id, incoming.revision);
        }

        // The `!seen` removal pass (state.go:566-579): every per-pane
        // ledger drops; the generation advances so a stale exact target
        // can never validate against a future reappearance.
        let live: BTreeSet<&str> = snapshot
            .agents
            .iter()
            .map(|agent| agent.pane_id.as_str())
            .collect();
        for pane_id in old.keys() {
            if live.contains(pane_id) {
                continue;
            }
            *self.generations.entry((*pane_id).to_owned()).or_insert(0) += 1;
            ledger.remove(pane_id);
            self.agent_times.remove(*pane_id);
            outcome.removed.push((*pane_id).to_owned());
        }

        // `PaneInfo.revision` — Herdr's upstream output counter — seeds
        // the shared watermark for every pane, agent-rowed or not. An
        // identity move means the pane respawned: its counter restarted,
        // so the watermark resets before folding rather than suppressing
        // the new epoch's events. Runs on every commit — event-path
        // accepts carry the same revision table, and the fold is a
        // max-merge so replayed commits cannot move it backwards.
        let old_panes: BTreeMap<&str, &lerdr_herdr::PaneInfo> = self
            .snapshot
            .panes
            .iter()
            .map(|pane| (pane.pane_id.as_str(), pane))
            .collect();
        for pane in &snapshot.panes {
            let respawned = old_panes
                .get(pane.pane_id.as_str())
                .is_some_and(|existing| {
                    pane_identity_moved(
                        [
                            &existing.terminal_id,
                            &existing.tab_id,
                            &existing.workspace_id,
                        ],
                        [&pane.terminal_id, &pane.tab_id, &pane.workspace_id],
                    )
                });
            if respawned {
                ledger.reset_upstream_rev(&pane.pane_id);
            }
            ledger.note_upstream_rev(&pane.pane_id, pane.revision);
        }
        // Watermarks die with the pane — `live` (agent rows) plus every
        // `panes` row is the membership set; anything else is a leftover
        // from an event/read observation on a now-gone pane.
        let live_panes: BTreeSet<&str> = snapshot
            .panes
            .iter()
            .map(|pane| pane.pane_id.as_str())
            .chain(live.iter().copied())
            .collect();
        ledger.retain_upstream(|pane_id| live_panes.contains(pane_id));
        drop(ledger);
        drop(old);

        self.snapshot = snapshot;
        self.stale = false;
        // `commitInventoryLocked` (state.go:445-450) — every commit, event
        // or poll, marks the inventory ready and clears the failure pair.
        self.accepted_at = now;
        self.attempted_at = now;
        self.inventory_ready = true;
        self.inventory_error_code.clear();
        self.inventory_message.clear();
        outcome
    }

    /// `resolveAgentSessionName`'s resolver call (server.go:529) run for
    /// every incoming agent: `SessionNameWithProject(agent.Agent,
    /// {cwd, foreground_cwd}, TrimSpace(agent.Session))`. Returns the
    /// pane→title map; `""` titles are kept out — the commit loop's
    /// `unwrap_or_default` writes them anyway. `None` resolver (never
    /// installed) yields an empty map — the pre-resolver shape.
    fn session_titles(&self, agents: &[AgentInfo]) -> BTreeMap<String, String> {
        let resolver = self
            .resolver
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let Some(resolver) = resolver else {
            return BTreeMap::new();
        };
        agents
            .iter()
            .map(|agent| {
                let project = crate::conversation::ProjectContext {
                    cwd: agent.cwd.clone().unwrap_or_default(),
                    foreground_cwd: agent.foreground_cwd.clone().unwrap_or_default(),
                };
                let title = resolver.session_name_with_project(
                    agent.agent.as_deref().unwrap_or_default(),
                    &project,
                    agent
                        .agent_session
                        .as_ref()
                        .map(|s| s.value.as_str())
                        .unwrap_or_default(),
                );
                (agent.pane_id.clone(), title)
            })
            .filter(|(_, title)| !title.is_empty())
            .collect()
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

    /// `MarkInventoryFailure` (state.go:211-230) — a failed reconcile poll
    /// marks the inventory not-ready with `command_failed` and the fixed
    /// operator-facing message; committed rows stay published until the
    /// next success. `lastAttemptAt` stamps on every failure; the return
    /// is whether the projected `inventory_status` keys moved (repeat
    /// failures while already failed publish nothing — the timestamps are
    /// metadata, not a wire trigger).
    pub(crate) fn mark_inventory_failure(&mut self) -> bool {
        self.attempted_at = now_millis();
        if !self.inventory_ready && self.inventory_error_code == "command_failed" {
            return false;
        }
        self.inventory_ready = false;
        self.inventory_error_code = "command_failed".to_owned();
        self.inventory_message = "Unable to read the current Herdr agent inventory.".to_owned();
        self.revision += 1;
        true
    }

    /// `State.Generation` (state.go:284) — the pane's current session
    /// epoch (0 before its first replacement).
    pub fn generation_of(&self, pane_id: &str) -> i64 {
        self.generations.get(pane_id).copied().unwrap_or(0)
    }

    /// `State.PaneSession` (state.go:290-295) — the epoch plus whether
    /// the pane is live in the snapshot.
    pub(crate) fn pane_session(&self, pane_id: &str) -> (i64, bool) {
        (self.generation_of(pane_id), self.pane_of(pane_id).is_some())
    }

    /// `State.BumpGeneration` (state.go:304) — a lifecycle mutation
    /// replaced the pane's session; the epoch advances. The epoch rides
    /// the `agents` projection, so the view counts as revised — and the
    /// row visibly changed (session cleared/restarted), so `updated_at`
    /// moves too rather than waiting for Herdr's `state_change_seq`.
    pub(crate) fn bump_generation(&mut self, pane_id: &str) {
        *self.generations.entry(pane_id.to_owned()).or_insert(0) += 1;
        // The upstream output counter restarted with the pane session —
        // the old epoch's watermark must not suppress the replacement's
        // `pane.output_changed` events or reads as stale.
        self.attention
            .lock()
            .expect("attention ledger poisoned")
            .reset_upstream_rev(pane_id);
        let times = self.agent_times.entry(pane_id.to_owned()).or_default();
        times.updated_at = now_millis();
        times.seen = true;
        self.revision += 1;
    }

    /// The pane's committed [`AttentionCell`] — a clone so callers never
    /// hold the ledger lock across an await.
    pub(crate) fn attention_cell(&self, pane_id: &str) -> AttentionCell {
        self.attention
            .lock()
            .expect("attention ledger poisoned")
            .cell(pane_id)
            .cloned()
            .unwrap_or_default()
    }

    /// `s.revision[paneID]` — the commit epoch `pane_revision` projects.
    pub(crate) fn state_rev_of(&self, pane_id: &str) -> i64 {
        self.attention_cell(pane_id).state_rev
    }

    /// `State.ContentRevision` (state.go:1144) — the mid-read fence leg.
    pub(crate) fn content_rev_of(&self, pane_id: &str) -> i64 {
        self.attention_cell(pane_id).content_rev
    }

    /// The newest upstream output revision observed for the pane —
    /// Herdr's `content_revision` folded in from snapshot seeds,
    /// `pane_output_changed` events, and `pane.read` results. This is the
    /// *shared observed* watermark (not the coordinator's `content_rev`
    /// and not a watch's *served* mark): `0` means unobserved or
    /// unsupported — Herdr 0.9.1 stubs `pane.read`'s revision at 0 — so
    /// callers must treat `0` as "no upstream fence", never "revision
    /// zero".
    pub(crate) fn upstream_rev_of(&self, pane_id: &str) -> u64 {
        self.attention
            .lock()
            .expect("attention ledger poisoned")
            .upstream_rev(pane_id)
    }

    /// Fold one observed upstream revision into the shared watermark —
    /// the `pane_output_changed` event path and verified `pane.read`
    /// results both land here so a read in flight sees a mid-flight
    /// event's bump on its post-read check.
    pub(crate) fn note_upstream_rev(&self, pane_id: &str, revision: u64) {
        self.attention
            .lock()
            .expect("attention ledger poisoned")
            .note_upstream_rev(pane_id, revision);
    }

    /// The pane's upstream scroll offset (`PaneInfo.scroll.
    /// offset_from_bottom`) — the viewport row base the link actions'
    /// `offset_from_bottom` parameter rides on. `None` while the pane
    /// reports no scroll metrics.
    pub(crate) fn pane_scroll_offset(&self, pane_id: &str) -> Option<u64> {
        self.snapshot
            .panes
            .iter()
            .find(|pane| pane.pane_id == pane_id)
            .and_then(|pane| pane.scroll)
            .map(|scroll| scroll.offset_from_bottom)
    }

    /// `State.AttentionRevision` (state.go:1150) — the push key's
    /// `interaction_revision`.
    pub(crate) fn attention_rev_of(&self, pane_id: &str) -> i64 {
        self.attention_cell(pane_id).attention_rev
    }

    /// `State.AcknowledgePane` + `DisplayedStatus` (state.go:957-991):
    /// writes `last_seen_at = max(now, last_active_at)` and consumes the
    /// unseen-done/ack-done ledgers. Returns `(before, after, state_rev)`
    /// when the pane is live — the `agent_update` broadcast fires only
    /// when `before != after`.
    pub(crate) fn acknowledge(&self, pane_id: &str) -> Option<(String, String, i64)> {
        let info = self.pane_of(pane_id)?;
        let status = info.agent_status.to_string();
        let last_active_at = self
            .agent_times
            .get(pane_id)
            .map(|times| times.last_active_at)
            .unwrap_or_default();
        let mut ledger = self.attention.lock().expect("attention ledger poisoned");
        let cell = ledger.cell_mut(pane_id);
        let before = cell.displayed_status(&status);
        cell.acknowledge(&status, last_active_at, now_millis());
        let after = cell.displayed_status(&status);
        Some((before, after, cell.state_rev))
    }

    /// `State.TransitionCurrent` (state.go:310-315) — pane live, status
    /// and commit epoch still as the transition recorded them.
    pub(crate) fn transition_current(&self, pane_id: &str, status: &str, revision: i64) -> bool {
        self.pane_of(pane_id).is_some_and(|info| {
            info.agent_status.to_string() == status && self.state_rev_of(pane_id) == revision
        })
    }

    /// `State.BlockedTransitionCurrent` (state.go:317-325) — the pane is
    /// still blocked on the same event and generation.
    pub(crate) fn blocked_transition_current(
        &self,
        pane_id: &str,
        event_id: &str,
        generation: i64,
    ) -> bool {
        self.pane_of(pane_id).is_some_and(|info| {
            info.agent_status.to_string() == "blocked"
                && self.generation_of(pane_id) == generation
                && self.attention_cell(pane_id).blocked.event_id == event_id
        })
    }

    /// `State.AttentionTransitionCurrent` (state.go:327-342) — the
    /// blocked cycle plus the committed kind/revision the classification
    /// wrote.
    pub(crate) fn attention_transition_current(
        &self,
        pane_id: &str,
        event_id: &str,
        generation: i64,
        kind: &str,
        attention_rev: i64,
    ) -> bool {
        if !self.blocked_transition_current(pane_id, event_id, generation) {
            return false;
        }
        let cell = self.attention_cell(pane_id);
        cell.blocked.kind.map(kind_str) == Some(kind) && cell.attention_rev == attention_rev
    }

    /// `State.CompletionCurrent` (state.go:344-352) — the pane is past
    /// attention (or blocked-chat) on the recorded completion revision.
    pub(crate) fn completion_current(&self, pane_id: &str, revision: i64) -> bool {
        let Some(info) = self.pane_of(pane_id) else {
            return false;
        };
        let status = info.agent_status.to_string();
        let cell = self.attention_cell(pane_id);
        let past_attention = !is_attention_status(&status)
            || (status == "blocked" && cell.blocked.kind == Some(AttentionKind::Chat));
        past_attention && cell.completion_rev == revision
    }

    /// `State.RegisterFinishedNotificationForTransition` (state.go:
    /// 1179-1195) — atomically claim the finished notification for this
    /// completion cycle; refuses when the pane is back under attention or
    /// the cycle already fired.
    pub(crate) fn register_finished_notification(&self, pane_id: &str, revision: i64) -> bool {
        let Some(info) = self.pane_of(pane_id) else {
            return false;
        };
        let status = info.agent_status.to_string();
        let mut ledger = self.attention.lock().expect("attention ledger poisoned");
        let cell = ledger.cell_mut(pane_id);
        let attention = is_attention_status(&status)
            && !(status == "blocked" && cell.blocked.kind == Some(AttentionKind::Chat));
        if attention || cell.completion_rev != revision || cell.finished_notif {
            return false;
        }
        cell.finished_notif = true;
        true
    }

    /// `State.CommitAttentionClassification` (state.go:852-882) — write
    /// the classification only while the blocked cycle, generation, and
    /// content revision the enrich read saw are all still current.
    /// Returns the projected committed row (`AgentState` + the internal
    /// interaction id + the post-commit `attention_rev`) when the commit
    /// landed.
    pub(crate) fn commit_attention(
        &self,
        pane_id: &str,
        event_id: &str,
        generation: i64,
        content_rev: i64,
        classification: &crate::classify::Classification,
    ) -> Option<(AgentState, String, i64)> {
        let info = self.pane_of(pane_id)?.clone();
        if info.agent_status.to_string() != "blocked" || self.generation_of(pane_id) != generation {
            return None;
        }
        let mut ledger = self.attention.lock().expect("attention ledger poisoned");
        let cell = ledger.cell_mut(pane_id);
        if cell.blocked.event_id != event_id || cell.content_rev != content_rev {
            return None;
        }
        let previous = cell.blocked.kind;
        if cell.apply_classification(classification) {
            // `s.revCounter++` — the commit epoch advances for this pane
            // only; the next topology `accept` re-bases everything anyway.
            cell.state_rev += 1;
            cell.content_rev += 1;
            cell.attention_rev += 1;
            cell.sync_attention_completion(previous, cell.blocked.kind, cell.state_rev);
        }
        let attention_rev = cell.attention_rev;
        let interaction_id = cell.blocked.interaction_id.clone();
        drop(ledger);
        let agent = self.agent_state(&info);
        Some((agent, interaction_id, attention_rev))
    }

    /// `AgentInfo` → `protocol.AgentState` — the committed snapshot row
    /// plus the attention ledger's semantic and ack halves.
    ///
    /// - `server_session_id` is always `"primary"`
    ///   (`projectAgentResource`, server.go:3407) and `generation` carries
    ///   this pane's epoch — both belong to the exact-target tuple clients
    ///   echo back.
    /// - `agent_session_id` is the oracle's `SessionID`:
    ///   `TrimSpace(agent_session.value)` (`resolveAgentSessionName`,
    ///   server.go:518-524) — the Go client flattens `agent_session` into
    ///   `Pane.Session` without consulting `kind` (client.go:382).
    ///   `session_name` is the committed row's resolved title
    ///   (`resolveAgentSessionName`'s `agent.SessionName`); a resolved
    ///   title also replaces `session` verbatim (server.go:534), matching
    ///   the oracle's wire rewrite.
    ///   `conversation_history_available` =
    ///   `SessionID != "" && conversation.Supported(agent)`.
    /// - `status` is `DisplayedStatus` — an acked done reads `idle`, an
    ///   unacknowledged idle completion reads `done`.
    /// - `tab_label`/`tab_number`/`tab_order` resolve through
    ///   `snapshot.tabs` (the oracle's `projectAgentResources`); `project`
    ///   is `filepath.Base(cwd)`; `host` is the relay hostname short form;
    ///   `raw_pane_id` echoes the pane id.
    /// - `updated_at`/`last_active_at`/`last_seen_at` come from
    ///   [`AgentTimes`] + the cell's ack override; `pane_revision` is the
    ///   commit epoch; `activity_seq` is Herdr's `state_change_seq`.
    /// - The attention fields (`event_id`…`question_layout`) overlay the
    ///   committed cell — empty unless the pane is in a blocked cycle.
    pub(crate) fn agent_state(&self, info: &AgentInfo) -> AgentState {
        let (raw_session, agent_session_id) = match &info.agent_session {
            Some(s) => (s.value.clone(), s.value.trim().to_owned()),
            None => (String::new(), String::new()),
        };
        let times = self
            .agent_times
            .get(&info.pane_id)
            .cloned()
            .unwrap_or_default();
        let cell = self.attention_cell(&info.pane_id);
        let session_name = cell.session_name.clone();
        // `agent.Session = title` (server.go:534) — a resolved title
        // replaces the wire `session` display string while
        // `agent_session_id` keeps the trimmed raw id.
        let session = if session_name.is_empty() {
            raw_session
        } else {
            session_name.clone()
        };
        let (tab_label, tab_number, tab_order) = self.tab_context(info);
        let status = info.agent_status.to_string();
        let cwd = info.cwd.clone().unwrap_or_default();
        let agent = info.agent.clone().unwrap_or_default();
        let history_available =
            !agent_session_id.is_empty() && crate::conversation::supported(&agent);
        AgentState {
            pane_id: info.pane_id.clone(),
            raw_pane_id: info.pane_id.clone(),
            terminal_id: info.terminal_id.clone(),
            server_session_id: "primary".to_owned(),
            generation: self.generation_of(&info.pane_id),
            agent_session_id,
            tab_id: info.tab_id.clone(),
            tab_label,
            tab_number,
            tab_order,
            workspace_id: info.workspace_id.clone(),
            agent: agent.clone(),
            name: info.name.clone().unwrap_or_default(),
            // `DisplayedStatus` — the ack/done overlay.
            status: cell.displayed_status(&status),
            focused: info.focused,
            cwd: cwd.clone(),
            project: project_of(&cwd),
            host: hostname_short(),
            session,
            session_name,
            updated_at: times.updated_at,
            last_active_at: times.last_active_at,
            last_seen_at: cell.last_seen_at,
            activity_seq: info.state_change_seq as i64,
            event_id: cell.blocked.event_id.clone(),
            attention_kind: cell
                .blocked
                .kind
                .map(kind_str)
                .unwrap_or_default()
                .to_owned(),
            prompt: cell.blocked.prompt.clone(),
            command: cell.blocked.command.clone(),
            options: cell.blocked.options.clone(),
            approval_fingerprint: cell.blocked.approval_fingerprint.clone(),
            interaction: cell.blocked.interaction.as_ref().map(wire_interaction),
            question_layout: cell.blocked.question_layout,
            conversation_history_available: history_available,
            pane_revision: cell.state_rev,
            // `pane.report_metadata` overlays — herdr hooks report these;
            // empty maps serialize absent (no wire drift until reported).
            state_labels: info.state_labels.clone(),
            tokens: info.tokens.clone(),
        }
    }

    /// The `tab_*` projection halves — `tab_label`/`tab_number` come from
    /// `snapshot.tabs` (`tab.Number`, falling back to the slice position
    /// plus one like the oracle's `projectAgentResources`); `tab_order` is
    /// the per-workspace ordinal over the snapshot's tab order.
    fn tab_context(&self, info: &AgentInfo) -> (String, i64, i64) {
        let mut tab_order = 0i64;
        let mut ordinal = 0i64;
        let mut label = String::new();
        let mut number = 0i64;
        for (index, tab) in self.snapshot.tabs.iter().enumerate() {
            if tab.workspace_id == info.workspace_id {
                ordinal += 1;
            }
            if tab.tab_id == info.tab_id {
                label = tab.label.clone();
                number = if tab.number != 0 {
                    i64::from(tab.number)
                } else {
                    index as i64 + 1
                };
                tab_order = ordinal;
            }
        }
        (label, number, tab_order)
    }

    /// `WorkspaceInfo` → `protocol.Workspace`. `cwd` stays empty — Herdr's
    /// `WorkspaceInfo` surface does not carry it (doc 10; the oracle's
    /// `commitWorkspacesLocked` reads `Workspace.Cwd`, which the Rust
    /// socket types don't decode).
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
            tokens: info.tokens.clone(),
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

    /// The pane hosting an agent session — `focus_agent`'s
    /// `agent_session_id` resolution. Matches the trimmed raw id the wire
    /// `agent_session_id` carries (`agent_state`'s projection); empty ids
    /// resolve nothing.
    pub(crate) fn pane_for_session(&self, agent_session_id: &str) -> Option<&AgentInfo> {
        if agent_session_id.is_empty() {
            return None;
        }
        self.snapshot.agents.iter().find(|a| {
            a.agent_session
                .as_ref()
                .is_some_and(|s| s.value.trim() == agent_session_id)
        })
    }

    /// `s.state.Agent(paneID)` — the committed row projection (`None`
    /// when the pane is gone). The projector reads this fresh per fence.
    pub(crate) fn agent_state_of(&self, pane_id: &str) -> Option<AgentState> {
        self.pane_of(pane_id).map(|info| self.agent_state(info))
    }

    /// The classification agent name for a pane — `s.agentInfo(paneID)`
    /// (server.go:2556-2563) returns `a.Agent` verbatim: the detected
    /// agent name, `""` when absent (the session-ref agent is *not* a
    /// fallback there). Used by `Classify` and `display_source`.
    pub(crate) fn classification_agent(&self, pane_id: &str) -> String {
        self.pane_of(pane_id)
            .and_then(|info| info.agent.clone())
            .unwrap_or_default()
    }
}

/// `filepath.Base(cwd)` — the `Project` derivation the oracle applies in
/// `agentsFromTopology` (`project := filepath.Base(cwd)` when non-empty).
pub(crate) fn project_of(cwd: &str) -> String {
    if cwd.is_empty() {
        return String::new();
    }
    std::path::Path::new(cwd)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `s.hostname` — `os.Hostname()` truncated at the first dot
/// (`strings.Split(host, ".")[0]`), matching the oracle's `host` field on
/// agents/blocked frames and the activity `host` attribution.
pub(crate) fn hostname_short() -> String {
    let host = gethostname::gethostname().to_string_lossy().into_owned();
    host.split('.').next().unwrap_or_default().to_owned()
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
        assert_eq!(agents[0].raw_pane_id, "wE:p1");
        assert_eq!(agents[0].status, "working");
        assert_eq!(agents[0].agent, "devin");
        assert_eq!(agents[0].cwd, "/home/l");
        assert_eq!(agents[0].project, "l");
        assert!(agents[0].focused);
        // The initial snapshot stamps updated_at = 0 (state.go:507).
        assert_eq!(agents[0].updated_at, 0);
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

    /// `pane.report_metadata`/`workspace.report_metadata` overlays project
    /// through `agents[]`/`workspaces[]` when reported and stay absent
    /// (not empty) on the wire when they are not.
    #[test]
    fn reported_metadata_projects_and_omits_when_empty() {
        let mut t = Topology::default();
        let mut reported = agent("wE:p1", lerdr_herdr::AgentStatus::Working);
        reported
            .state_labels
            .insert("build".into(), "failing".into());
        reported.tokens.insert("ci".into(), "red".into());
        let plain = agent("wE:p2", lerdr_herdr::AgentStatus::Working);
        let ws = lerdr_herdr::WorkspaceInfo {
            workspace_id: "wE".into(),
            tokens: [("branch".into(), "main".into())].into_iter().collect(),
            ..lerdr_herdr::WorkspaceInfo::default()
        };
        t.accept(SessionSnapshot {
            agents: vec![reported, plain],
            workspaces: vec![ws],
            ..SessionSnapshot::default()
        });

        let agents = t.agents();
        assert_eq!(agents[0].state_labels["build"], "failing");
        assert_eq!(agents[0].tokens["ci"], "red");
        assert!(agents[1].state_labels.is_empty() && agents[1].tokens.is_empty());
        assert_eq!(t.workspaces()[0].tokens["branch"], "main");

        // Absent-on-wire: empty maps must not serialize.
        let json = serde_json::to_value(&agents[1]).unwrap();
        assert!(json.get("state_labels").is_none());
        assert!(json.get("tokens").is_none());
        let ws_json = serde_json::to_value(&Workspace {
            tokens: Default::default(),
            ..Workspace::default()
        })
        .unwrap();
        assert!(ws_json.get("tokens").is_none());
    }

    #[test]
    fn agents_carry_exact_target_tuple_sorted() {
        let mut t = Topology::default();
        t.accept(SessionSnapshot {
            agents: vec![
                AgentInfo {
                    pane_id: "wE:p2".into(),
                    terminal_id: "term_2".into(),
                    // `Supported(agent.Agent)` needs a detected provider —
                    // the session ref's agent name never substitutes.
                    agent: Some("claude".into()),
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
        // `conversation_history_available` = trimmed session + supported agent.
        assert!(agents[1].conversation_history_available);
        assert!(!agents[0].conversation_history_available);
    }

    /// `resolveAgentSessionName` end to end (server.go:505-534): the
    /// resolver's title lands on `session_name` and rewrites `session`
    /// verbatim while `agent_session_id` keeps the trimmed raw id; an
    /// unresolved or whitespace-only session projects the pre-resolver
    /// shape.
    #[test]
    fn session_name_projects_resolved_title() {
        let home = tempfile::TempDir::new().expect("temp HOME");
        // Claude transcripts live under `~/.claude/projects/<dir>/<id>.jsonl`
        // where `<dir>` is the cwd with every non-alphanumeric mapped to `-`.
        let transcript = home.path().join(".claude/projects/-work-repo/sess-1.jsonl");
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(
            &transcript,
            "{\"type\":\"custom-title\",\"customTitle\":\"Claude Title\"}\n",
        )
        .unwrap();

        let reader = std::sync::Arc::new(crate::conversation::Reader::new_with_env(
            home.path().to_path_buf(),
            Box::new(|_: &str| None),
        ));
        let mut t = Topology::default();
        *t.resolver.lock().expect("resolver slot poisoned") = Some(std::sync::Arc::new(
            crate::conversation::Resolver::with_reader(reader),
        ));

        let mut pane = agent("wE:p1", lerdr_herdr::AgentStatus::Working);
        pane.cwd = Some("/work/repo".into());
        pane.agent_session = Some(lerdr_herdr::AgentSessionInfo {
            source: "sess".into(),
            agent: "claude".into(),
            kind: lerdr_herdr::AgentSessionRefKind::Id,
            value: " sess-1 ".into(),
        });
        let mut blank = agent("wE:p2", lerdr_herdr::AgentStatus::Working);
        blank.cwd = Some("/work/repo".into());
        blank.agent_session = Some(lerdr_herdr::AgentSessionInfo {
            source: "sess".into(),
            agent: "claude".into(),
            kind: lerdr_herdr::AgentSessionRefKind::Id,
            value: "   ".into(),
        });
        let mut missing = agent("wE:p3", lerdr_herdr::AgentStatus::Working);
        missing.cwd = Some("/work/repo".into());
        missing.agent_session = Some(lerdr_herdr::AgentSessionInfo {
            source: "sess".into(),
            agent: "claude".into(),
            kind: lerdr_herdr::AgentSessionRefKind::Id,
            value: "no-transcript".into(),
        });
        t.accept(SessionSnapshot {
            agents: vec![pane, blank, missing],
            ..SessionSnapshot::default()
        });

        let agents = t.agents();
        // `agent.Session = title` + `SessionName`/`SessionID` (server.go:518-534).
        assert_eq!(agents[0].session_name, "Claude Title");
        assert_eq!(agents[0].session, "Claude Title");
        assert_eq!(agents[0].agent_session_id, "sess-1");
        assert!(agents[0].conversation_history_available);
        // Whitespace-only session: `TrimSpace` empties the id, no title.
        assert_eq!(agents[1].session_name, "");
        assert_eq!(agents[1].session, "   ");
        assert_eq!(agents[1].agent_session_id, "");
        assert!(!agents[1].conversation_history_available);
        // No transcript: the raw session id stays on `session`.
        assert_eq!(agents[2].session_name, "");
        assert_eq!(agents[2].session, "no-transcript");
        assert_eq!(agents[2].agent_session_id, "no-transcript");

        // The committed row drops → the title ledger entry drops with it.
        t.accept(SessionSnapshot::default());
        assert!(t.agents().is_empty());
    }

    fn agent(pane_id: &str, status: lerdr_herdr::AgentStatus) -> AgentInfo {
        AgentInfo {
            pane_id: pane_id.into(),
            terminal_id: format!("term_{pane_id}"),
            workspace_id: "wE".into(),
            tab_id: format!("wE:t_{pane_id}"),
            agent_status: status,
            agent: Some("claude".into()),
            cwd: Some("/home/user/project".into()),
            ..AgentInfo::default()
        }
    }

    #[test]
    fn blocked_cycle_mints_clears_and_fences() {
        let mut t = Topology::default();
        let outcome = t.accept(SessionSnapshot {
            agents: vec![agent("wE:p1", lerdr_herdr::AgentStatus::Blocked)],
            ..SessionSnapshot::default()
        });
        // Entering blocked fires one transition and mints an event id.
        assert_eq!(outcome.transitions.len(), 1);
        assert_eq!(outcome.transitions[0].status, "blocked");
        let cell = t.attention_cell("wE:p1");
        assert!(!cell.blocked.event_id.is_empty());
        assert_eq!(cell.blocked.kind, Some(AttentionKind::Unknown));
        let agents = t.agents();
        assert_eq!(agents[0].status, "blocked");
        assert_eq!(agents[0].attention_kind, "unknown");
        assert_eq!(agents[0].event_id, cell.blocked.event_id);

        // Staying blocked keeps the same event and does not refire.
        let outcome = t.accept(SessionSnapshot {
            agents: vec![agent("wE:p1", lerdr_herdr::AgentStatus::Blocked)],
            ..SessionSnapshot::default()
        });
        assert!(outcome.transitions.is_empty());
        let after = t.attention_cell("wE:p1");
        assert_eq!(after.blocked.event_id, cell.blocked.event_id);

        // blocked → working clears the blocked halves.
        t.accept(SessionSnapshot {
            agents: vec![agent("wE:p1", lerdr_herdr::AgentStatus::Working)],
            ..SessionSnapshot::default()
        });
        let cleared = t.attention_cell("wE:p1");
        assert!(cleared.blocked.event_id.is_empty());
        assert_eq!(cleared.blocked.kind, None);
        let agents = t.agents();
        assert_eq!(agents[0].status, "working");
        assert_eq!(agents[0].attention_kind, "");
    }

    #[test]
    fn completion_marks_unseen_done_then_idle_on_ack() {
        let mut t = Topology::default();
        t.accept(SessionSnapshot {
            agents: vec![agent("wE:p1", lerdr_herdr::AgentStatus::Working)],
            ..SessionSnapshot::default()
        });
        let outcome = t.accept(SessionSnapshot {
            agents: vec![agent("wE:p1", lerdr_herdr::AgentStatus::Done)],
            ..SessionSnapshot::default()
        });
        // working → done fires the completion transition.
        assert_eq!(outcome.transitions.len(), 1);
        assert_eq!(outcome.transitions[0].status, "done");
        // Unseen done displays as the raw done status.
        let agents = t.agents();
        assert_eq!(agents[0].status, "done");

        // Acknowledge: `ackDone` flips the displayed status to idle.
        let (before, after, _) = t.acknowledge("wE:p1").expect("pane is live");
        assert_eq!(before, "done");
        assert_eq!(after, "idle");
        assert_eq!(t.agents()[0].status, "idle");
    }

    #[test]
    fn working_to_idle_is_done_unseen() {
        let mut t = Topology::default();
        t.accept(SessionSnapshot {
            agents: vec![agent("wE:p1", lerdr_herdr::AgentStatus::Working)],
            ..SessionSnapshot::default()
        });
        // `last_active_at` must move for `unseen_done` to seed on idle —
        // the status change bumps `updated_at`, which `activityAdvanced`
        // copies to `last_active_at`.
        let outcome = t.accept(SessionSnapshot {
            agents: vec![agent("wE:p1", lerdr_herdr::AgentStatus::Idle)],
            ..SessionSnapshot::default()
        });
        assert_eq!(outcome.transitions.len(), 1);
        assert_eq!(outcome.transitions[0].status, "idle");
        // `unseen_done` displays idle as `done`.
        assert_eq!(t.agents()[0].status, "done");
        let (_, after, _) = t.acknowledge("wE:p1").expect("live pane");
        assert_eq!(after, "idle");
        assert_eq!(t.agents()[0].status, "idle");
    }

    #[test]
    fn replaced_pane_bumps_generation_and_resets_ledgers() {
        let mut t = Topology::default();
        t.accept(SessionSnapshot {
            agents: vec![agent("wE:p1", lerdr_herdr::AgentStatus::Working)],
            ..SessionSnapshot::default()
        });
        let mut replacement = agent("wE:p1", lerdr_herdr::AgentStatus::Blocked);
        replacement.terminal_id = "term_replaced".into();
        let outcome = t.accept(SessionSnapshot {
            agents: vec![replacement],
            ..SessionSnapshot::default()
        });
        assert_eq!(t.generation_of("wE:p1"), 1);
        assert_eq!(outcome.transitions.len(), 1);
        assert_eq!(outcome.transitions[0].status, "blocked");
        // A fresh blocked cycle mints under the new session.
        assert!(!t.attention_cell("wE:p1").blocked.event_id.is_empty());
    }

    #[test]
    fn removal_bumps_generation_and_drops_ledgers() {
        let mut t = Topology::default();
        t.accept(SessionSnapshot {
            agents: vec![agent("wE:p1", lerdr_herdr::AgentStatus::Blocked)],
            ..SessionSnapshot::default()
        });
        let outcome = t.accept(SessionSnapshot::default());
        assert_eq!(t.generation_of("wE:p1"), 1);
        assert_eq!(outcome.removed, vec!["wE:p1".to_owned()]);
        assert!(t.attention_cell("wE:p1").blocked.event_id.is_empty());
        // Reappearance after absence opens a new epoch again.
        t.accept(SessionSnapshot {
            agents: vec![agent("wE:p1", lerdr_herdr::AgentStatus::Idle)],
            ..SessionSnapshot::default()
        });
        assert_eq!(t.generation_of("wE:p1"), 2);
    }

    /// A classification the enrich pass would hand a commit — approval
    /// shape so the fingerprint/options paths engage.
    fn approval_enrichment() -> crate::classify::Classification {
        crate::classify::Classification {
            kind: AttentionKind::Approval,
            prompt: "Allow npm test?".into(),
            options: vec!["Allow once".into(), "Reject".into()],
            approval_source: "claude".into(),
            ..crate::classify::Classification::default()
        }
    }

    /// `commitTopologyLocked` on an existing non-blocked pane: the
    /// incoming `blocked` status is discarded with its enrich details —
    /// the committed row stays clean `idle` and nothing republishes. This
    /// is the `emit-blocked` trace: `pane.agent_detected` reporting
    /// `blocked` on an idle pane produces no `agents`/`blocked` frame
    /// until the next poll or wake adopts the live status.
    #[test]
    fn event_commit_preserves_committed_status_and_drops_enrich() {
        let mut t = Topology::default();
        t.accept_enriched(
            SessionSnapshot {
                agents: vec![agent("wE:p1", lerdr_herdr::AgentStatus::Idle)],
                ..SessionSnapshot::default()
            },
            &BTreeMap::new(),
            CommitKind::Poll,
        );

        let enrichments = BTreeMap::from([("wE:p1".to_owned(), approval_enrichment())]);
        let outcome = t.accept_enriched(
            SessionSnapshot {
                agents: vec![agent("wE:p1", lerdr_herdr::AgentStatus::Blocked)],
                ..SessionSnapshot::default()
            },
            &enrichments,
            CommitKind::Event,
        );
        // No transition fired; the committed row is still clean idle.
        assert!(outcome.transitions.is_empty());
        let projected = &t.agents()[0];
        assert_eq!(projected.status, "idle");
        assert_eq!(projected.event_id, "");
        assert_eq!(projected.attention_kind, "");
        assert_eq!(projected.prompt, "");
        assert!(projected.options.is_empty());
        let cell = t.attention_cell("wE:p1");
        assert!(cell.blocked.event_id.is_empty());
        assert_eq!(cell.blocked.kind, None);

        // The poll path then adopts the live status — the classification
        // lands in the same commit like the oracle's enriched `cp`.
        let outcome = t.accept_enriched(
            SessionSnapshot {
                agents: vec![agent("wE:p1", lerdr_herdr::AgentStatus::Blocked)],
                ..SessionSnapshot::default()
            },
            &enrichments,
            CommitKind::Poll,
        );
        assert_eq!(outcome.transitions.len(), 1);
        assert_eq!(outcome.transitions[0].status, "blocked");
        let projected = &t.agents()[0];
        assert_eq!(projected.status, "blocked");
        assert_eq!(projected.attention_kind, "approval");
        assert_eq!(projected.options, vec!["Allow once", "Reject"]);
        assert!(!projected.event_id.is_empty());
        assert!(!projected.approval_fingerprint.is_empty());
    }

    /// `commitTopologyLocked`'s other half: a committed `blocked` row
    /// keeps its committed details against an event-path sample —
    /// `copyBlockedDetails` — while non-status topology fields still
    /// adopt. The enrich classification on the incoming row is discarded
    /// (the oracle's `cp` gets the committed details copied over it), so
    /// a content drift only reclassifies on the next poll.
    #[test]
    fn event_commit_preserves_blocked_details() {
        let mut t = Topology::default();
        let enrichments = BTreeMap::from([("wE:p1".to_owned(), approval_enrichment())]);
        t.accept_enriched(
            SessionSnapshot {
                agents: vec![agent("wE:p1", lerdr_herdr::AgentStatus::Blocked)],
                ..SessionSnapshot::default()
            },
            &enrichments,
            CommitKind::Poll,
        );
        let committed = t.agents()[0].clone();
        let event_id = committed.event_id.clone();
        let fingerprint = committed.approval_fingerprint.clone();

        // An event-path sample carrying a *different* classification —
        // focused flipped too — keeps the committed attention fields.
        let mut drift = crate::classify::Classification {
            kind: AttentionKind::Question,
            prompt: "Pick one".into(),
            ..crate::classify::Classification::default()
        };
        drift.question_layout = true;
        let mut incoming = agent("wE:p1", lerdr_herdr::AgentStatus::Blocked);
        incoming.focused = false;
        let outcome = t.accept_enriched(
            SessionSnapshot {
                agents: vec![incoming],
                ..SessionSnapshot::default()
            },
            &BTreeMap::from([("wE:p1".to_owned(), drift)]),
            CommitKind::Event,
        );
        // No blocked→blocked refire: the committed row did not change.
        assert!(outcome.transitions.is_empty());
        let projected = &t.agents()[0];
        assert_eq!(projected.status, "blocked");
        assert_eq!(projected.event_id, event_id);
        assert_eq!(projected.attention_kind, "approval");
        assert_eq!(projected.approval_fingerprint, fingerprint);
        assert!(!projected.question_layout);
        // Topology fields adopt — `focused` is not a preserved field.
        assert!(!projected.focused);
    }

    /// Fresh panes on the event path adopt the incoming row wholesale —
    /// no committed status exists to preserve, so `final_status` and the
    /// enrich classification land in the same commit.
    #[test]
    fn event_commit_adopts_fresh_pane() {
        let mut t = Topology::default();
        t.accept_enriched(
            SessionSnapshot {
                agents: vec![agent("wE:p1", lerdr_herdr::AgentStatus::Working)],
                ..SessionSnapshot::default()
            },
            &BTreeMap::new(),
            CommitKind::Poll,
        );
        let enrichments = BTreeMap::from([("wE:p2".to_owned(), approval_enrichment())]);
        let outcome = t.accept_enriched(
            SessionSnapshot {
                agents: vec![
                    agent("wE:p1", lerdr_herdr::AgentStatus::Working),
                    agent("wE:p2", lerdr_herdr::AgentStatus::Blocked),
                ],
                ..SessionSnapshot::default()
            },
            &enrichments,
            CommitKind::Event,
        );
        assert_eq!(outcome.transitions.len(), 1);
        assert_eq!(outcome.transitions[0].pane_id, "wE:p2");
        assert_eq!(outcome.transitions[0].status, "blocked");
        let projected = t
            .agents()
            .into_iter()
            .find(|a| a.pane_id == "wE:p2")
            .expect("fresh pane projected");
        assert_eq!(projected.status, "blocked");
        assert_eq!(projected.attention_kind, "approval");
    }

    /// The shared upstream-revision watermark: seeded from `PaneInfo` /
    /// `AgentInfo` `revision`, max-folded by `pane_output_changed`
    /// events and verified `pane.read` results, reset when the pane
    /// session is replaced, and swept when the pane leaves the topology.
    #[test]
    fn upstream_revisions_seed_reset_and_sweep() {
        let pane = |pane_id: &str, terminal_id: &str, revision: u64| lerdr_herdr::PaneInfo {
            pane_id: pane_id.into(),
            terminal_id: terminal_id.into(),
            workspace_id: "wE".into(),
            tab_id: "wE:t1".into(),
            revision,
            ..lerdr_herdr::PaneInfo::default()
        };

        let mut t = Topology::default();
        t.accept(SessionSnapshot {
            panes: vec![pane("wE:p1", "term_1", 12)],
            ..SessionSnapshot::default()
        });
        assert_eq!(t.upstream_rev_of("wE:p1"), 12);

        // Max-merge — out-of-order and stale observations never move the
        // watermark backwards; `0` (the unreported stub) never stores.
        t.note_upstream_rev("wE:p1", 9);
        assert_eq!(t.upstream_rev_of("wE:p1"), 12);
        t.note_upstream_rev("wE:p1", 0);
        assert_eq!(t.upstream_rev_of("wE:p1"), 12);
        t.note_upstream_rev("wE:p1", 15);
        assert_eq!(t.upstream_rev_of("wE:p1"), 15);

        // A replayed/older commit can't lower it either.
        t.accept(SessionSnapshot {
            panes: vec![pane("wE:p1", "term_1", 13)],
            ..SessionSnapshot::default()
        });
        assert_eq!(t.upstream_rev_of("wE:p1"), 15);

        // Session-identity move = respawn: the upstream counter restarted,
        // so the new epoch's smaller revision must seed cleanly.
        t.accept(SessionSnapshot {
            panes: vec![pane("wE:p1", "term_2", 3)],
            ..SessionSnapshot::default()
        });
        assert_eq!(t.upstream_rev_of("wE:p1"), 3);

        // An explicit generation bump (lifecycle mutation) resets it too.
        t.bump_generation("wE:p1");
        assert_eq!(t.upstream_rev_of("wE:p1"), 0);

        // Observations keep folding while the pane is in the topology;
        // the per-commit membership sweep drops the watermark once it is
        // gone entirely.
        t.note_upstream_rev("wE:p1", 20);
        assert_eq!(t.upstream_rev_of("wE:p1"), 20);
        t.accept(SessionSnapshot::default());
        assert_eq!(t.upstream_rev_of("wE:p1"), 0);
    }

    /// `AgentInfo.revision` seeds the same watermark for agent rows —
    /// the same upstream counter the pane row would carry.
    #[test]
    fn upstream_revisions_seed_from_agent_rows() {
        let mut t = Topology::default();
        t.accept(SessionSnapshot {
            agents: vec![AgentInfo {
                pane_id: "wE:p1".into(),
                terminal_id: "term_1".into(),
                workspace_id: "wE".into(),
                tab_id: "wE:t1".into(),
                agent_status: lerdr_herdr::AgentStatus::Working,
                agent: Some("devin".into()),
                revision: 21,
                ..AgentInfo::default()
            }],
            ..SessionSnapshot::default()
        });
        assert_eq!(t.upstream_rev_of("wE:p1"), 21);
    }
}
