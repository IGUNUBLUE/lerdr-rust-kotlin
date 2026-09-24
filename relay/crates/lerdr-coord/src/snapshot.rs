//! Post-handshake snapshot composition (`sendConnectionSnapshot` parity).
//!
//! Order matters — the oracle sends `push_config` first, then the
//! inventory/state frames, so clients see capabilities before content.

use std::collections::BTreeMap;
use std::sync::Arc;

use lerdr_core::json::MaybeNull;
use lerdr_core::protocol::{
    AgentsMessage, CapsUpdateMessage, HerdrStatus, HerdrStatusMessage, InventoryStatusMessage,
    Outbound, PushConfig, WorkspacesMessage, CAPABILITIES, VERSION,
};

use crate::topology::Topology;

/// The frames pushed to a client right after the E2EE handshake commits.
///
/// Baseline (Phase-1): `push_config` + `workspaces` + `agents` +
/// `herdr_status` + `inventory_status`. The oracle additionally sends
/// `activity_history`, `push_policy`, `speech_voices`, `update_status` —
/// those subsystems land in later slices; their absence is honest state,
/// not an error (each frame is independently optional on the client).
/// `inventory_status` is NOT optional in practice: Kotlin gates
/// `agents`/`workspaces` on `acceptsInventorySnapshots`, which needs an
/// explicit `ready` (the `push_config.inventory` fallback also covers it,
/// but the standalone frame is the oracle's contract).
pub fn compose_snapshot(topology: &Topology) -> Vec<Outbound> {
    let status = herdr_status(topology);
    vec![
        Outbound::PushConfig(Box::new(PushConfig {
            r#type: "push_config".to_owned(),
            protocol: VERSION,
            version: crate::release_version().to_owned(),
            release_version: crate::release_version().to_owned(),
            capabilities: MaybeNull::Value(effective_capabilities(topology)),
            herdr_status: status.clone(),
            ..PushConfig::default()
        })),
        Outbound::HerdrStatus(HerdrStatusMessage {
            status: Some(MaybeNull::Value(status)),
            r#type: "herdr_status".to_owned(),
            ..HerdrStatusMessage::default()
        }),
        Outbound::Workspaces(WorkspacesMessage {
            workspaces: Some(MaybeNull::Value(topology.workspaces())),
            r#type: "workspaces".to_owned(),
        }),
        Outbound::Agents(AgentsMessage {
            agents: Some(MaybeNull::Value(topology.agents())),
            r#type: "agents".to_owned(),
        }),
        Outbound::InventoryStatus(inventory_status(topology)),
    ]
}

/// The advertised capability set for one committed topology —
/// `CAPABILITIES` minus any family whose Herdr methods are ALL refuted by
/// live evidence (docs/13 §0). `focus` drops only once every `*.focus`
/// method reads `unsupported`; the pane-content families apply the same
/// rule (`pane_search` ← the copy family, `pane_links` ← the link pair,
/// `layout` ← export+apply). A partial family still serves the methods
/// the installed Herdr ships, so `unknown`/`supported` both keep it
/// advertised. Relay-local capabilities with no Herdr method behind them
/// (`convo_sub`, `frame_zstd`) are never refuted — they stay advertised
/// unconditionally. The same list rides `push_config` at connect and
/// `caps_update` mid-session (the session gate sniffs both).
pub fn effective_capabilities(topology: &Topology) -> Vec<String> {
    let features = topology.herdr_status.features.value();
    let family_refuted = |methods: &[&str]| {
        methods.iter().all(|method| {
            features
                .and_then(|map| map.get(*method))
                .is_some_and(|status| status.state == "unsupported")
        })
    };
    use lerdr_herdr::capabilities::features as f;
    let refuted = [
        ("focus", family_refuted(f::FOCUS_METHODS)),
        ("pane_search", family_refuted(f::PANE_SEARCH_METHODS)),
        ("pane_links", family_refuted(f::PANE_LINK_METHODS)),
        ("layout", family_refuted(f::LAYOUT_METHODS)),
    ];
    CAPABILITIES
        .iter()
        .filter(|cap| !refuted.iter().any(|(name, all)| *cap == name && *all))
        .map(|cap| (*cap).to_owned())
        .collect()
}

/// `inventoryStatusLocked` + `inventoryStatusMessage` (state.go:247-269,
/// server.go:2657-2667) — all six keys emit unconditionally; `stale` is
/// derived (`state != "ready" && lastSuccessAt != 0`), never stored. The
/// `stale` *transport* flag is a different axis — a reconnecting event
/// stream does not make the committed inventory unready in the oracle
/// (polls keep running), so it feeds `herdr_status.health_check`, not
/// this frame. Attempt/success timestamps marshal as Unix *seconds*
/// (`lastAttemptAt.Unix()`), not the millis the ledger keeps.
pub fn inventory_status(topology: &Topology) -> InventoryStatusMessage {
    let state = if topology.inventory_ready {
        "ready"
    } else if !topology.inventory_error_code.is_empty() {
        "error"
    } else {
        "starting"
    };
    InventoryStatusMessage {
        state: Some(state.to_owned()),
        error_code: Some(topology.inventory_error_code.clone()),
        message: Some(topology.inventory_message.clone()),
        last_attempt_at: Some(topology.attempted_at / 1000),
        last_success_at: Some(topology.accepted_at / 1000),
        stale: Some(state != "ready" && topology.accepted_at > 0),
        r#type: "inventory_status".to_owned(),
    }
}

/// `herdr_status` payload from the projection — the actor-maintained
/// capability evidence (`Topology::herdr_status`, the oracle's
/// `ServerStatus` → `herdrStatusPayload`), with `server_version`/
/// `protocol` overridden by the snapshot envelope: `accept()` refreshes
/// those on reconnect before the next capability report lands, and they
/// are the same server-reported values. `features` must be an object,
/// never `null`: the Kotlin model types it non-nullable — `null` fails
/// decode, drops `push_config`, and the inventory gate then swallows
/// every `agents`/`workspaces` frame.
pub(crate) fn herdr_status(topology: &Topology) -> HerdrStatus {
    let mut status = topology.herdr_status.clone();
    status.server_version = topology.snapshot.version.clone();
    status.server_protocol = topology.snapshot.protocol as i64;
    status.server_protocol_known = topology.snapshot.protocol > 0 || status.server_protocol_known;
    if status.features.is_null() {
        status.features = MaybeNull::Value(BTreeMap::new());
    }
    status
}

/// The `stateViewMu` triple (server.go:3420-3424) plus the `herdr_status`
/// payload — what the last broadcast carried, diffed against on the next
/// publish. Relay-global like the oracle's: one publish decision per
/// commit fans the same frame set out to every client.
///
/// Two oracle behaviors fold into the comparison:
///
/// - `agentSnapshotsEqual` zeroes `StateRevision` before marshaling, so
///   `pane_revision` churn alone never republishes — mirrored by zeroing
///   the projected rows' `pane_revision` before serialization.
/// - `mergeAgentSnapshot` keeps the published row when an incoming row's
///   `StateRevision` regresses — a no-op here because commit epochs only
///   increase, and `broadcastCommitted`'s `agent_update`/`blocked`
///   delta-merge into `agentView` has no counterpart (Rust's delta frames
///   never touch this view).
#[derive(Debug, Default)]
pub(crate) struct PublishedView {
    agents: Vec<u8>,
    workspaces: Vec<u8>,
    /// `inventoryStatusChanged`'s key set — `state`, `error_code`,
    /// `message`, `stale` — timestamps are metadata, not a wire trigger.
    inventory: (Option<String>, Option<String>, Option<String>, Option<bool>),
    /// `herdr_status` payload bytes — the oracle emits that frame only
    /// through the capability-change callback; payload-diff dedup is the
    /// equivalent gate here.
    herdr_status: Vec<u8>,
    /// Serialized advertised capability list — a flip emits `caps_update`
    /// (docs/13 §0: each side announces when its supported set changes).
    capabilities: Vec<u8>,
}

/// `publishCurrentInventory` (server.go:3435-3514): diff the committed
/// projection against the published view, emit only what changed, then
/// update the view — `[inventory_status?] + [agents?] + [workspaces?]`
/// in the oracle's batch order, with `herdr_status` appended when its
/// payload moved (the capability callback is a separate broadcast there).
/// `readyRecovery` — a `ready` state following a non-`ready` publish —
/// forces the `agents`+`workspaces` legs like the oracle's.
pub(crate) fn broadcast_diff(topology: &Topology, view: &mut PublishedView) -> Vec<Outbound> {
    let status = inventory_status(topology);
    let inventory_key = (
        status.state.clone(),
        status.error_code.clone(),
        status.message.clone(),
        status.stale,
    );
    // `agentSnapshotsEqual` — `StateRevision` is the commit epoch, not a
    // wire-change trigger.
    let mut agents = topology.agents();
    for agent in &mut agents {
        agent.pane_revision = 0;
    }
    let agents_json = serde_json::to_vec(&agents).unwrap_or_default();
    let workspaces = topology.workspaces();
    let workspaces_json = serde_json::to_vec(&workspaces).unwrap_or_default();
    let herdr = herdr_status(topology);
    let herdr_json = serde_json::to_vec(&herdr).unwrap_or_default();
    let capabilities = effective_capabilities(topology);
    let capabilities_json = serde_json::to_vec(&capabilities).unwrap_or_default();

    let status_changed = view.inventory != inventory_key;
    let ready_recovery =
        status.state.as_deref() == Some("ready") && view.inventory.0.as_deref() != Some("ready");
    let send_agents = view.agents != agents_json || ready_recovery;
    let send_workspaces = view.workspaces != workspaces_json || ready_recovery;
    let send_herdr = view.herdr_status != herdr_json;
    // First publish seeds the view silently — the connecting client's
    // snapshot already carries `push_config.capabilities`; `caps_update`
    // is for the mid-session flips after that.
    let send_caps = !view.capabilities.is_empty() && view.capabilities != capabilities_json;

    let mut frames = Vec::with_capacity(5);
    if status_changed {
        frames.push(Outbound::InventoryStatus(status));
    }
    if send_agents {
        frames.push(Outbound::Agents(AgentsMessage {
            agents: Some(MaybeNull::Value(agents)),
            r#type: "agents".to_owned(),
        }));
    }
    if send_workspaces {
        frames.push(Outbound::Workspaces(WorkspacesMessage {
            workspaces: Some(MaybeNull::Value(workspaces)),
            r#type: "workspaces".to_owned(),
        }));
    }
    if send_herdr {
        frames.push(Outbound::HerdrStatus(HerdrStatusMessage {
            status: Some(MaybeNull::Value(herdr)),
            r#type: "herdr_status".to_owned(),
            ..HerdrStatusMessage::default()
        }));
    }
    if send_caps {
        frames.push(Outbound::CapsUpdate(CapsUpdateMessage {
            capabilities: Some(MaybeNull::Value(capabilities)),
            r#type: "caps_update".to_owned(),
        }));
    }

    // The oracle's `commit` closure: the inventory view always refreshes;
    // the row views advance with their (possibly forced) publishes.
    view.inventory = inventory_key;
    if send_agents {
        view.agents = agents_json;
    }
    if send_workspaces {
        view.workspaces = workspaces_json;
    }
    if send_herdr {
        view.herdr_status = herdr_json;
    }
    view.capabilities = capabilities_json;
    frames
}

/// Broadcast frames for one topology revision — the dedup'd batch the
/// actor stamped on this view (`publishCurrentInventory` parity). All
/// entries are replaceable, so a burst of revisions coalesces in the
/// client's send buffer.
pub fn topology_broadcast(topology: &Arc<Topology>) -> Vec<Outbound> {
    topology.broadcast_frames.clone()
}

/// `sendRequestedAgentRefreshes`'s per-client push (server.go:2618-2628)
/// — `inventory_status` + `agents` + `workspaces` of the committed view,
/// sent unconditionally to the requester (explicit refresh requests are
/// answered with the full rows, not the dedup'd broadcast batch).
pub(crate) fn committed_inventory(topology: &Topology) -> Vec<Outbound> {
    vec![
        Outbound::InventoryStatus(inventory_status(topology)),
        Outbound::Agents(AgentsMessage {
            agents: Some(MaybeNull::Value(topology.agents())),
            r#type: "agents".to_owned(),
        }),
        Outbound::Workspaces(WorkspacesMessage {
            workspaces: Some(MaybeNull::Value(topology.workspaces())),
            r#type: "workspaces".to_owned(),
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use lerdr_herdr::{AgentInfo, AgentStatus, SessionSnapshot, WorkspaceInfo};

    fn snapshot_with(status: AgentStatus, focused: bool) -> SessionSnapshot {
        SessionSnapshot {
            agents: vec![AgentInfo {
                pane_id: "wE:p1".into(),
                terminal_id: "term-1".into(),
                workspace_id: "wE".into(),
                tab_id: "wE:t1".into(),
                focused,
                agent_status: status,
                agent: Some("claude".into()),
                cwd: Some("/home/relay/project".into()),
                ..AgentInfo::default()
            }],
            workspaces: vec![WorkspaceInfo {
                workspace_id: "wE".into(),
                ..WorkspaceInfo::default()
            }],
            ..SessionSnapshot::default()
        }
    }

    /// Frame `type` tags in emit order — `broadcast_diff` only ever
    /// produces the four inventory legs.
    fn types(frames: &[Outbound]) -> String {
        frames
            .iter()
            .filter_map(|frame| {
                serde_json::from_slice::<serde_json::Value>(&frame.encode())
                    .ok()
                    .and_then(|value| value.get("type")?.as_str().map(str::to_owned))
            })
            .collect::<Vec<_>>()
            .join(",")
    }

    #[test]
    fn first_publish_sends_everything_then_dedups() {
        let mut topology = Topology::default();
        let mut view = PublishedView::default();
        topology.accept(snapshot_with(AgentStatus::Idle, true));
        let batch = broadcast_diff(&topology, &mut view);
        assert_eq!(
            types(&batch),
            "inventory_status,agents,workspaces,herdr_status"
        );
        // An identical commit publishes nothing — `publishCurrentInventory`
        // is silent when the committed view did not move.
        topology.accept(snapshot_with(AgentStatus::Idle, true));
        assert!(broadcast_diff(&topology, &mut view).is_empty());
    }

    #[test]
    fn committed_change_republishes_only_the_moved_leg() {
        let mut topology = Topology::default();
        let mut view = PublishedView::default();
        topology.accept(snapshot_with(AgentStatus::Idle, true));
        let _ = broadcast_diff(&topology, &mut view);
        // `focused` is a wire field — flipping it republishes `agents`.
        topology.accept(snapshot_with(AgentStatus::Idle, false));
        assert_eq!(types(&broadcast_diff(&topology, &mut view)), "agents");
    }

    #[test]
    fn stale_marks_health_check_only() {
        let mut topology = Topology::default();
        let mut view = PublishedView::default();
        topology.accept(snapshot_with(AgentStatus::Idle, true));
        let _ = broadcast_diff(&topology, &mut view);
        // Event-stream reconnect: inventory stays `ready` (the oracle's
        // transport drop does not touch `inventoryReady`). `health_check`
        // is the server-advertised capability field — transport staleness
        // is not evidence, so the payload does not move and nothing
        // republishes (the revision still bumps; watchers see `stale`).
        assert!(topology.mark_stale());
        assert!(broadcast_diff(&topology, &mut view).is_empty());
    }

    #[test]
    fn inventory_failure_errors_then_recovers() {
        let mut topology = Topology::default();
        let mut view = PublishedView::default();
        topology.accept(snapshot_with(AgentStatus::Idle, true));
        let _ = broadcast_diff(&topology, &mut view);

        assert!(topology.mark_inventory_failure());
        let batch = broadcast_diff(&topology, &mut view);
        assert_eq!(types(&batch), "inventory_status");
        let Outbound::InventoryStatus(status) = &batch[0] else {
            panic!("expected inventory_status");
        };
        assert_eq!(status.state.as_deref(), Some("error"));
        assert_eq!(status.error_code.as_deref(), Some("command_failed"));
        assert_eq!(status.stale, Some(true));

        // Repeat failure while already failed publishes nothing.
        assert!(!topology.mark_inventory_failure());
        assert!(broadcast_diff(&topology, &mut view).is_empty());

        // The next successful commit is a `ready` recovery — the oracle
        // forces the agents+workspaces legs alongside the status flip.
        topology.accept(snapshot_with(AgentStatus::Idle, true));
        assert_eq!(
            types(&broadcast_diff(&topology, &mut view)),
            "inventory_status,agents,workspaces"
        );
    }

    #[test]
    fn pre_commit_view_is_starting() {
        let topology = Topology::default();
        let status = inventory_status(&topology);
        assert_eq!(status.state.as_deref(), Some("starting"));
        assert_eq!(status.stale, Some(false));
        assert_eq!(status.error_code.as_deref(), Some(""));
        assert_eq!(status.last_success_at, Some(0));
    }

    /// Feature evidence for `method` at `state` — one ledger row.
    fn feature(state: &str) -> lerdr_core::protocol::HerdrFeatureStatus {
        lerdr_core::protocol::HerdrFeatureStatus {
            state: state.to_owned(),
            reason: "schema_absent".to_owned(),
            generation: 1,
        }
    }

    fn set_features(topology: &mut Topology, rows: &[(&str, &str)]) {
        topology.herdr_status.features = MaybeNull::Value(
            rows.iter()
                .map(|(name, state)| ((*name).to_owned(), feature(state)))
                .collect(),
        );
    }

    #[test]
    fn effective_capabilities_keep_focus_until_every_method_is_refuted() {
        let mut topology = Topology::default();
        // No evidence yet — `focus` stays advertised (advertise while not
        // refuted, docs/13 §0).
        assert!(effective_capabilities(&topology).contains(&"focus".to_owned()));
        // Partial family: three methods refuted, `tab.focus` unknown —
        // still advertised.
        set_features(
            &mut topology,
            &[
                ("pane.focus", "unsupported"),
                ("workspace.focus", "unsupported"),
                ("agent.focus", "unsupported"),
            ],
        );
        assert!(effective_capabilities(&topology).contains(&"focus".to_owned()));
        // All four refuted — the family drops.
        set_features(
            &mut topology,
            &[
                ("pane.focus", "unsupported"),
                ("tab.focus", "unsupported"),
                ("workspace.focus", "unsupported"),
                ("agent.focus", "unsupported"),
            ],
        );
        let capabilities = effective_capabilities(&topology);
        assert!(!capabilities.contains(&"focus".to_owned()));
        // Everything else stays — the drop is surgical.
        assert!(capabilities.contains(&"workspace_management".to_owned()));
        assert_eq!(capabilities.len(), CAPABILITIES.len() - 1);
    }

    /// docs/13 §1 — `pane_search`/`pane_links`/`layout` follow the same
    /// all-refuted rule: a partial family stays advertised, a fully
    /// refuted family drops, and other families are untouched.
    #[test]
    fn effective_capabilities_refute_each_family_independently() {
        let mut topology = Topology::default();
        // No evidence — all three advertised.
        let caps = effective_capabilities(&topology);
        for cap in ["pane_search", "pane_links", "layout"] {
            assert!(caps.contains(&cap.to_owned()), "{cap} while unknown");
        }
        // Partial families stay advertised (one method still unknown).
        set_features(
            &mut topology,
            &[
                ("pane.copy_search", "unsupported"),
                ("pane.selection.read", "unsupported"),
                // pane.copy_motion unobserved — family alive.
                ("pane.link.resolve", "unsupported"),
                // pane.link.activate unobserved — family alive.
                ("layout.export", "unsupported"),
                // layout.apply unobserved — family alive.
            ],
        );
        let caps = effective_capabilities(&topology);
        for cap in ["pane_search", "pane_links", "layout"] {
            assert!(caps.contains(&cap.to_owned()), "{cap} while partial");
        }
        // Refute the rest — each family drops independently.
        set_features(
            &mut topology,
            &[
                ("pane.copy_search", "unsupported"),
                ("pane.selection.read", "unsupported"),
                ("pane.copy_motion", "unsupported"),
                ("pane.link.resolve", "unsupported"),
                ("pane.link.activate", "unsupported"),
                ("layout.export", "unsupported"),
                ("layout.apply", "unsupported"),
            ],
        );
        let caps = effective_capabilities(&topology);
        for cap in ["pane_search", "pane_links", "layout"] {
            assert!(!caps.contains(&cap.to_owned()), "{cap} once refuted");
        }
        assert!(caps.contains(&"focus".to_owned()));
        assert_eq!(caps.len(), CAPABILITIES.len() - 3);
    }

    #[test]
    fn capability_flip_emits_caps_update_once() {
        let mut topology = Topology::default();
        let mut view = PublishedView::default();
        topology.accept(snapshot_with(AgentStatus::Idle, true));
        // First publish seeds the view — the connecting client got
        // `push_config.capabilities` in its snapshot, so no `caps_update`.
        let _ = broadcast_diff(&topology, &mut view);
        // Same-set republish stays silent.
        assert!(broadcast_diff(&topology, &mut view).is_empty());

        // The family refutes — the next publish announces the narrowed set.
        set_features(
            &mut topology,
            &[
                ("pane.focus", "unsupported"),
                ("tab.focus", "unsupported"),
                ("workspace.focus", "unsupported"),
                ("agent.focus", "unsupported"),
            ],
        );
        let batch = broadcast_diff(&topology, &mut view);
        let updates: Vec<_> = batch
            .iter()
            .filter(|frame| matches!(frame, Outbound::CapsUpdate(_)))
            .collect();
        assert_eq!(updates.len(), 1, "batch: {}", types(&batch));
        let Outbound::CapsUpdate(message) = updates[0] else {
            unreachable!()
        };
        let list = message
            .capabilities
            .as_ref()
            .and_then(MaybeNull::value)
            .expect("list present");
        assert!(!list.contains(&"focus".to_owned()));
        // The flip is announced exactly once — the view now holds it.
        let batch = broadcast_diff(&topology, &mut view);
        assert!(
            !batch.iter().any(|f| matches!(f, Outbound::CapsUpdate(_))),
            "batch: {}",
            types(&batch)
        );
    }
}
