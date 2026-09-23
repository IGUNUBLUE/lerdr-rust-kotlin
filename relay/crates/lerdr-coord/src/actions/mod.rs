//! Action handlers — the `coordinator`/`server.go` dispatch-table port.
//!
//! Every routed action produces an [`Outcome`]: the oracle's
//! `command_result` body (`ok`/`phase`/`error`/`pane_id`/`data` — the phase
//! vocabulary `completed`/`failed`/`not_started`/`dispatched_unknown` is the
//! oracle's, not the receipt taxonomy's) plus the terminal `action_receipt`
//! the relay protocol layer adds on top (doc 08 rule 4). `Outcome::frames`
//! emits the result message first and the receipt last.
//!
//! Actions without a routed handler keep the router's honest
//! `dispatched_unknown` fallthrough.

pub(crate) mod activity;
pub(crate) mod agents;
pub(crate) mod conversation;
pub(crate) mod input;
pub(crate) mod inspect;
pub(crate) mod leases;
pub(crate) mod local;
pub(crate) mod misc;
pub(crate) mod profiles;
pub(crate) mod push;
pub(crate) mod push_delivery;
pub(crate) mod questions;
pub(crate) mod speech;
pub(crate) mod tabs;
pub(crate) mod target;
pub(crate) mod uploads;
pub(crate) mod workspace;
pub(crate) mod worktree;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use lerdr_core::audit;
use lerdr_core::json::{MaybeNull, RawJson};
use lerdr_core::protocol::{
    action_receipt_response, error_codes, ActionReceipt, ActionReceiptPhase, ApiError,
    CommandResultMessage, Outbound,
};
use lerdr_herdr::{DispatchPhase, HerdrError};
use tokio::sync::broadcast;

use crate::actor::TopologyHandle;
use crate::topology::Topology;

/// `commandDeadline` in dispatch.go.
pub(crate) const COMMAND_DEADLINE: Duration = Duration::from_secs(12);
/// `workspaceCommandDeadline` in workspace.go.
pub(crate) const WORKSPACE_DEADLINE: Duration = Duration::from_secs(20);
/// `worktreeCommandDeadline` in workspace.go.
pub(crate) const WORKTREE_DEADLINE: Duration = Duration::from_secs(60);
/// `agentStartDeadline` in dispatch.go.
pub(crate) const AGENT_START_DEADLINE: Duration = Duration::from_secs(40);
/// `maxTabInsertIndex` — shared bound for tab and workspace insert indices.
pub(crate) const MAX_INSERT_INDEX: i64 = 10_000;
/// `promptMaxChars` — 100,000 runes.
pub(crate) const PROMPT_MAX_CHARS: usize = 100_000;

/// Everything a spawned action needs: the Herdr client, the topology
/// snapshot taken at dispatch time, the shared lease manager, and handles
/// for follow-up pushes.
#[derive(Clone)]
pub(crate) struct ActionContext {
    pub client: lerdr_herdr::Client,
    /// `handle.topology.borrow()` captured in `route()` — the admission-time
    /// view; handlers that need authoritative state (`workspace_close`) call
    /// Herdr directly instead.
    pub topology: Arc<Topology>,
    pub handle: TopologyHandle,
    pub leases: leases::Leases,
    /// Agent launch profile resolver — the `profiles.Resolver` port; lives
    /// behind a handle because `agent_start`/`agent_clear` share it.
    pub profiles: profiles::Resolver,
    /// Question/approval state store — `approval.go`'s per-pane pending
    /// interactions.
    pub questions: questions::Questions,
    /// Attachment upload manager — `upload.Manager`'s staging surface.
    pub uploads: uploads::Uploads,
    /// Relay-side activity journal — `activity.Journal`'s ring buffer.
    pub activities: activity::Journal,
    /// Push subsystem — policy, subscriptions, snooze, viewed-pane ledger.
    pub push: push::Push,
    /// Speech subsystem — engine handle + in-flight speech requests.
    pub speech: speech::Speech,
    /// `hub.Broadcast` — frames every session must see (voice-catalog
    /// changes, `update_status` notices). The router's forwarder drains
    /// it through `Relay::broadcast_except`.
    pub notices: Notices,
    /// `s.auditLog` — the shared write-audit sink; spawned handlers append
    /// their `result` rows here (the session writes `attempt` rows at
    /// admission and audits hub-owned admin replies itself).
    pub audit: Option<Arc<audit::AuditLog>>,
    /// The transport connection label (`client-N`) — per-connection
    /// bookkeeping only (leases, speech cancellation, broadcast
    /// exclusion); never a device identity.
    pub client_id: String,
    /// `client.Identity().DeviceID` — the authenticated device the oracle
    /// keys push policy/subscriptions/viewed-pane by. Untrusted wire
    /// `client_id` claims never substitute for it.
    pub device_id: String,
}

/// A relay-wide frame — the `hub.Broadcast`/`broadcastToAll` payloads
/// action handlers emit beside their response frames.
#[derive(Debug, Clone)]
pub(crate) struct RelayNotice {
    pub frame: Outbound,
    /// Skip this client — it already carries the frame in its own
    /// response (broadcast-before-result ordering stays deterministic on
    /// that connection). Empty means every session.
    pub exclude_client: String,
}

/// Cloneable handle over the relay-wide notice channel — every session's
/// `ActionContext` shares the one the router drains.
#[derive(Clone)]
pub(crate) struct Notices(broadcast::Sender<RelayNotice>);

impl Notices {
    /// `broadcastToAll` — no receivers is a silent no-op (the binary wires
    /// the forwarder; tests observe through [`Notices::subscribe`]).
    pub(crate) fn send(&self, frame: Outbound, exclude_client: String) {
        let _ = self.0.send(RelayNotice {
            frame,
            exclude_client,
        });
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<RelayNotice> {
        self.0.subscribe()
    }
}

impl Default for Notices {
    fn default() -> Self {
        Self(broadcast::channel(64).0)
    }
}

/// `d.state.Agent(paneID)` — the pane attribution `recordWriteAudit` reads
/// into every audit record. Same projection as [`record_activity`]: agent
/// name + session reference; project/host have no topology source and
/// stay empty.
pub(crate) fn audit_attribution(topology: &Topology, pane_id: &str) -> audit::Attribution {
    let Some(agent) = topology.pane_of(pane_id) else {
        return audit::Attribution::default();
    };
    audit::Attribution {
        agent: agent
            .agent
            .clone()
            .or_else(|| agent.agent_session.as_ref().map(|s| s.agent.clone()))
            .unwrap_or_default(),
        project: String::new(),
        session: agent
            .agent_session
            .as_ref()
            .map(|s| s.value.clone())
            .unwrap_or_default(),
        host: String::new(),
    }
}

/// `recordActivity` — commit one journal row with the pane attribution the
/// oracle reads out of `d.state.Agent(paneID)`: the detected agent name,
/// `Project` (`filepath.Base(cwd)` — the same derivation the topology
/// projection uses), the relay's short hostname, and the agent session id.
pub(crate) fn record_activity(
    ctx: &ActionContext,
    kind: &str,
    status: &str,
    summary: impl Into<String>,
    pane_id: &str,
    request_id: &str,
) {
    let mut entry = activity::NewEntry::action(kind, status, summary, pane_id, request_id);
    if let Some(agent) = ctx.topology.pane_of(pane_id) {
        let name = agent
            .agent
            .clone()
            .or_else(|| agent.agent_session.as_ref().map(|s| s.agent.clone()))
            .unwrap_or_default();
        let session = agent
            .agent_session
            .as_ref()
            .map(|s| s.value.clone())
            .unwrap_or_default();
        entry = entry.with_attribution(
            &name,
            &crate::topology::project_of(agent.cwd.as_deref().unwrap_or_default()),
            &crate::topology::hostname_short(),
            &session,
        );
    }
    ctx.activities.record(entry);
}

/// `recordActivityWithExtract` — same, carrying the `Extract` payload.
pub(crate) fn record_activity_extract(
    ctx: &ActionContext,
    kind: &str,
    status: &str,
    summary: impl Into<String>,
    extract: impl Into<String>,
    pane_id: &str,
    request_id: &str,
) {
    let mut entry = activity::NewEntry::action(kind, status, summary, pane_id, request_id)
        .with_extract(extract);
    if let Some(agent) = ctx.topology.pane_of(pane_id) {
        let name = agent
            .agent
            .clone()
            .or_else(|| agent.agent_session.as_ref().map(|s| s.agent.clone()))
            .unwrap_or_default();
        let session = agent
            .agent_session
            .as_ref()
            .map(|s| s.value.clone())
            .unwrap_or_default();
        entry = entry.with_attribution(
            &name,
            &crate::topology::project_of(agent.cwd.as_deref().unwrap_or_default()),
            &crate::topology::hostname_short(),
            &session,
        );
    }
    ctx.activities.record(entry);
}

/// `d.fail`/`d.failErr`'s journal row — every routed failure records
/// `<action> failed: <public error>` (action underscores become spaces).
pub(crate) fn record_failure(
    ctx: &ActionContext,
    action: &str,
    pane_id: &str,
    request_id: &str,
    error: &str,
) {
    if action.is_empty() {
        return;
    }
    let summary = format!("{} failed: {}", action.replace('_', " "), error);
    record_activity(ctx, action, "failed", summary, pane_id, request_id);
}

/// `handleAcknowledge` (dispatch.go:619-636) — `DisplayedStatus` →
/// `AcknowledgePane` → `wake` → `agent_update` on a displayed-status
/// change. The attention ledger is shared across topology snapshots, so
/// the ack on the latest borrow is the committed write.
///
/// A gone pane is `d.fail("acknowledge_pane", "Agent is unavailable")`:
/// the journal row lands even when the caller discards the result
/// (`HandleReadPane`, `readPaneWatchFrame`). Returns whether the pane was
/// live — the routed `acknowledge_pane` maps `false` to its `Outcome`.
pub(crate) fn acknowledge_pane_state(
    handle: &TopologyHandle,
    notices: &Notices,
    activities: &activity::Journal,
    pane_id: &str,
    request_id: &str,
) -> bool {
    let topology = handle.topology.borrow();
    let Some((before, after, state_rev)) = topology.acknowledge(pane_id) else {
        // `d.fail` — `recordActivity(action, "failed", …)`; a gone pane has
        // no attribution row to attach (the oracle's `d.state.Agent` is nil).
        activities.record(activity::NewEntry::action(
            "acknowledge_pane",
            "failed",
            "acknowledge pane failed: Agent is unavailable",
            pane_id,
            request_id,
        ));
        return false;
    };
    // `d.wake()` — the poller poke; here a topology re-read request.
    handle.try_refresh();
    if before == after {
        return true;
    }
    // The broadcast body (dispatch.go:627-633) plus `broadcastCommitted`'s
    // envelope keys (server.go:3597-3608): `server_session_id`,
    // `generation`, `terminal_id`, `agent_session_id`.
    let info = topology.pane_of(pane_id);
    let frame = Outbound::AgentUpdate(lerdr_core::protocol::AgentUpdateMessage {
        r#type: "agent_update".to_owned(),
        pane_id: Some(pane_id.to_owned()),
        // `raw_pane_id: paneID` — the display pane id verbatim, not the
        // raw Herdr id the inventory broadcast uses.
        raw_pane_id: Some(pane_id.to_owned()),
        status: Some(after),
        pane_revision: Some(state_rev),
        server_session_id: Some("primary".to_owned()),
        generation: Some(topology.generation_of(pane_id)),
        terminal_id: info.map(|i| i.terminal_id.clone()),
        agent_session_id: info
            .and_then(|i| i.agent_session.as_ref())
            .map(|s| s.value.trim().to_owned()),
        ..Default::default()
    });
    notices.send(frame, String::new());
    true
}

/// The oracle's `CommandResult` plus the terminal receipt it implies.
///
/// `phase` is the `command_result` phase string the Go server emits
/// (`completed`, `failed`, `not_started`, `dispatched_unknown`,
/// `accepted`, `completed_with_warning`); `receipt_phase`/`receipt_error`
/// carry the dispatch-boundary classification the relay adds.
pub(crate) struct Outcome {
    pub ok: bool,
    pub phase: &'static str,
    pub error: String,
    pub pane_id: String,
    pub data: Option<serde_json::Value>,
    pub receipt_phase: &'static str,
    pub receipt_error: Option<ApiError>,
}

impl Outcome {
    /// `completed(requestID, action, paneID, data)`.
    pub(crate) fn completed(pane_id: &str, data: Option<serde_json::Value>) -> Self {
        Self {
            ok: true,
            phase: "completed",
            error: String::new(),
            pane_id: pane_id.to_owned(),
            data,
            receipt_phase: ActionReceiptPhase::CONFIRMED,
            receipt_error: None,
        }
    }

    /// `completed_with_warning` — the operation applied; a follow-up step
    /// did not (the oracle uses it for `agent_clear`'s stranded pane and
    /// `agent_start`'s unconfirmed initial prompt).
    pub(crate) fn completed_with_warning(pane_id: &str, data: serde_json::Value) -> Self {
        Self {
            ok: true,
            phase: "completed_with_warning",
            error: String::new(),
            pane_id: pane_id.to_owned(),
            data: Some(data),
            receipt_phase: ActionReceiptPhase::CONFIRMED,
            receipt_error: None,
        }
    }

    /// `partiallyApplied` — an earlier step of a multi-step mutation
    /// already reached the agent (e.g. `submit_prompt`'s qoder text+Enter
    /// pair): `dispatched_unknown` on both surfaces.
    pub(crate) fn partially_applied(pane_id: &str, reason: &str) -> Self {
        Self {
            ok: false,
            phase: "dispatched_unknown",
            error: "Part of the command already reached the agent; review it before retrying"
                .to_owned(),
            pane_id: pane_id.to_owned(),
            data: Some(serde_json::json!({ "dispatched_unknown": true })),
            receipt_phase: ActionReceiptPhase::DISPATCHED_UNKNOWN,
            receipt_error: Some(api_error_plain("dispatch_outcome_unknown", reason)),
        }
    }

    /// `d.fail(...)` — validation/domain failure before anything was
    /// dispatched: `phase:"failed"` + `failed_before_dispatch` receipt.
    pub(crate) fn failed(pane_id: &str, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            ok: false,
            phase: "failed",
            error: message.clone(),
            pane_id: pane_id.to_owned(),
            data: None,
            receipt_phase: ActionReceiptPhase::FAILED_BEFORE_DISPATCH,
            receipt_error: Some(api_error_plain(error_codes::INVALID_REQUEST, &message)),
        }
    }

    /// A locally generated pre-dispatch refusal — `workspace_close`'s
    /// group-consent checks produce `phase:"not_started"` with the
    /// structured `code` in `data`, and nothing reached Herdr, so the
    /// receipt is `failed_before_dispatch` carrying the same code.
    pub(crate) fn not_started_refusal(
        code: &str,
        message: impl Into<String>,
        data: Option<serde_json::Value>,
    ) -> Self {
        Self {
            ok: false,
            phase: "not_started",
            error: message.into(),
            pane_id: String::new(),
            data,
            receipt_phase: ActionReceiptPhase::FAILED_BEFORE_DISPATCH,
            receipt_error: Some(ApiError::new(code, BTreeMap::new())),
        }
    }

    /// Emit the `command_result` frame then the terminal `action_receipt`.
    pub(crate) fn frames(self, request_id: &str, action: &str, action_id: &str) -> Vec<Outbound> {
        vec![
            command_result_frame(request_id, action, &self),
            Outbound::ActionReceipt(action_receipt_response(
                request_id,
                ActionReceipt {
                    action_id: action_id.to_owned(),
                    phase: ActionReceiptPhase::from(self.receipt_phase),
                    error: self.receipt_error,
                },
            )),
        ]
    }
}

/// `commandResultMessage` — `error` and `pane_id` are always present (the
/// oracle serializes both unconditionally, `""` when empty); `data` only
/// when the command produced a payload.
fn command_result_frame(request_id: &str, action: &str, outcome: &Outcome) -> Outbound {
    Outbound::CommandResult(CommandResultMessage {
        r#type: "command_result".to_owned(),
        request_id: (!request_id.is_empty()).then(|| request_id.to_owned()),
        action: Some(action.to_owned()),
        ok: Some(outcome.ok),
        phase: Some(outcome.phase.to_owned()),
        error: Some(outcome.error.clone()),
        pane_id: Some(outcome.pane_id.clone()),
        data: outcome.data.as_ref().and_then(|value| {
            serde_json::value::to_raw_value(value)
                .ok()
                .map(|raw| MaybeNull::Value(RawJson(raw)))
        }),
    })
}

/// `d.failErr` — the dispatch-boundary → `command_result` mapping for
/// pane/agent commands: transient refusals read "not sent; retry is safe",
/// known refusals carry `{"code": …}` data, `DispatchedUnknown` advertises
/// possible application. The receipt keeps the taxonomy classification.
pub(crate) fn dispatch_failure(pane_id: &str, err: &HerdrError) -> Outcome {
    match err.phase() {
        DispatchPhase::NotStarted => Outcome {
            ok: false,
            phase: "not_started",
            error: "Command was not sent; retry is safe".to_owned(),
            pane_id: pane_id.to_owned(),
            data: None,
            receipt_phase: ActionReceiptPhase::FAILED_BEFORE_DISPATCH,
            receipt_error: Some(api_error("herdr_unreachable", err)),
        },
        DispatchPhase::DispatchedUnknown => Outcome {
            ok: false,
            phase: "dispatched_unknown",
            error: "Command may have executed; review the agent before retrying".to_owned(),
            pane_id: pane_id.to_owned(),
            data: Some(serde_json::json!({ "dispatched_unknown": true })),
            receipt_phase: ActionReceiptPhase::DISPATCHED_UNKNOWN,
            receipt_error: Some(api_error("dispatch_outcome_unknown", err)),
        },
        DispatchPhase::Refused => {
            let code = err.refusal_code().unwrap_or("refused");
            let transient = err.is_transient_refusal();
            let (error, data) = if transient {
                ("Command was not sent; retry is safe".to_owned(), None)
            } else {
                (
                    refusal_message(code).to_owned(),
                    Some(serde_json::json!({ "code": code })),
                )
            };
            Outcome {
                ok: false,
                phase: "not_started",
                error,
                pane_id: pane_id.to_owned(),
                data,
                receipt_phase: ActionReceiptPhase::CONFIRMED,
                receipt_error: Some(ApiError::new(code, refusal_args(err))),
            }
        }
    }
}

/// `d.failTopologyErr` — the workspace/worktree variant: the refusal set is
/// the topology code list, `dirty_worktree_requires_force` advertises the
/// force escape, `worktree_list` degrades to a plain failure, and the
/// unknown-outcome wording differs ("refresh before retrying").
pub(crate) fn topology_failure(action: &str, err: &HerdrError) -> Outcome {
    if let HerdrError::Refused { code, message } = err {
        if code == "dirty_worktree_requires_force" {
            return Outcome {
                ok: false,
                phase: "not_started",
                error: "Worktree has uncommitted changes; force removal is required".to_owned(),
                pane_id: String::new(),
                data: Some(serde_json::json!({
                    "code": code,
                    "force_available": true,
                })),
                receipt_phase: ActionReceiptPhase::CONFIRMED,
                receipt_error: Some(ApiError::new(code, refusal_args(err))),
            };
        }
        if topology_refusal_code(code) {
            let public = match code.as_str() {
                "workspace_group_consent_invalid" => {
                    "Workspace group confirmation is invalid; confirm again".to_owned()
                }
                _ if group_refusal_code(code) => refusal_message(code).to_owned(),
                _ => {
                    let trimmed = message.trim();
                    if trimmed.is_empty() {
                        "Herdr refused the command".to_owned()
                    } else {
                        trimmed.to_owned()
                    }
                }
            };
            return Outcome {
                ok: false,
                phase: "not_started",
                error: public,
                pane_id: String::new(),
                data: Some(serde_json::json!({ "code": code })),
                receipt_phase: ActionReceiptPhase::CONFIRMED,
                receipt_error: Some(ApiError::new(code, refusal_args(err))),
            };
        }
    }
    if action == "worktree_list" {
        let (error, data) = match err {
            HerdrError::Refused { code, message } if !message.trim().is_empty() => (
                message.trim().to_owned(),
                Some(serde_json::json!({ "code": code })),
            ),
            _ => ("Worktrees could not be listed".to_owned(), None),
        };
        return Outcome {
            ok: false,
            phase: "failed",
            error,
            pane_id: String::new(),
            data,
            receipt_phase: ActionReceiptPhase::FAILED_BEFORE_DISPATCH,
            receipt_error: Some(api_error("herdr_unreachable", err)),
        };
    }
    match err.phase() {
        DispatchPhase::DispatchedUnknown => Outcome {
            ok: false,
            phase: "dispatched_unknown",
            error: "Herdr may have completed this command; refresh before retrying".to_owned(),
            pane_id: String::new(),
            data: Some(serde_json::json!({ "dispatched_unknown": true })),
            receipt_phase: ActionReceiptPhase::DISPATCHED_UNKNOWN,
            receipt_error: Some(api_error("dispatch_outcome_unknown", err)),
        },
        DispatchPhase::NotStarted => Outcome {
            ok: false,
            phase: "not_started",
            error: "Command was not sent; retry is safe".to_owned(),
            pane_id: String::new(),
            data: None,
            receipt_phase: ActionReceiptPhase::FAILED_BEFORE_DISPATCH,
            receipt_error: Some(api_error("herdr_unreachable", err)),
        },
        DispatchPhase::Refused => Outcome {
            ok: false,
            phase: "failed",
            error: "Command failed".to_owned(),
            pane_id: String::new(),
            data: None,
            receipt_phase: ActionReceiptPhase::CONFIRMED,
            receipt_error: Some(ApiError::new(
                err.refusal_code().unwrap_or("refused"),
                refusal_args(err),
            )),
        },
    }
}

/// `herdr.RefusalMessage` — the public strings for known refusal codes.
pub(crate) fn refusal_message(code: &str) -> &'static str {
    match code {
        "server_not_running" => "Herdr server is not running",
        "agent_pane_busy" => "Agent pane is still starting",
        "protocol_mismatch" => "Herdr server protocol is incompatible with this relay",
        "workspace_group_close_required" => "Close the workspace group explicitly",
        "workspace_group_changed" => "Workspace group changed; review it before closing",
        "workspace_group_primary_required" => "Select the primary workspace to close the group",
        "workspace_group_consent_invalid" => {
            "Workspace group confirmation is invalid; confirm again"
        }
        "workspace_group_validation_unavailable" => {
            "Current workspace membership could not be verified; try again"
        }
        _ => "Herdr rejected the command before it was sent",
    }
}

/// `topologyRefusalCode` — refusal codes the workspace handlers treat as
/// structured pre-dispatch rejections.
fn topology_refusal_code(code: &str) -> bool {
    matches!(
        code,
        "invalid_request"
            | "workspace_not_found"
            | "worktree_not_found"
            | "not_git_worktree"
            | "linked_worktree_source"
            | "worktree_operation_in_progress"
            | "protocol_mismatch"
            | "workspace_group_close_required"
            | "workspace_group_changed"
            | "workspace_group_primary_required"
            | "workspace_group_consent_invalid"
            | "workspace_group_validation_unavailable"
    )
}

fn group_refusal_code(code: &str) -> bool {
    matches!(
        code,
        "protocol_mismatch"
            | "workspace_group_close_required"
            | "workspace_group_changed"
            | "workspace_group_primary_required"
            | "workspace_group_consent_invalid"
            | "workspace_group_validation_unavailable"
    )
}

pub(crate) fn api_error(code: &str, err: &HerdrError) -> ApiError {
    api_error_plain(code, &err.to_string())
}

pub(crate) fn api_error_plain(code: &str, detail: &str) -> ApiError {
    let mut args = BTreeMap::new();
    args.insert(
        "detail".to_owned(),
        serde_json::Value::String(detail.to_owned()),
    );
    ApiError::new(code, args)
}

pub(crate) fn refusal_args(err: &HerdrError) -> BTreeMap<String, serde_json::Value> {
    let mut args = BTreeMap::new();
    if let HerdrError::Refused { message, .. } = err {
        args.insert(
            "message".to_owned(),
            serde_json::Value::String(message.clone()),
        );
    }
    args
}

/// `herdr.CreateResult` — flat-or-nested create response extraction with the
/// oracle's fallback order (`CreateResult.UnmarshalJSON`):
///
/// - `pane_id`: `pane_id` → `root_pane.pane_id` → `agent.pane_id`
/// - `tab_id`: `tab_id` → `root_pane.tab_id` → `tab.tab_id`
/// - `workspace_id`: `workspace_id` → `root_pane.workspace_id` →
///   `tab.workspace_id` → `workspace.workspace_id`
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct CreatedTarget {
    pub pane_id: String,
    pub tab_id: String,
    pub workspace_id: String,
}

pub(crate) fn created_target(value: &serde_json::Value) -> CreatedTarget {
    let str_at = |pointer: &str| -> Option<String> {
        value
            .pointer(pointer)
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let first = |candidates: &[&str]| {
        candidates
            .iter()
            .find_map(|p| str_at(p))
            .unwrap_or_default()
    };
    CreatedTarget {
        pane_id: first(&["/pane_id", "/root_pane/pane_id", "/agent/pane_id"]),
        tab_id: first(&["/tab_id", "/root_pane/tab_id", "/tab/tab_id"]),
        workspace_id: first(&[
            "/workspace_id",
            "/root_pane/workspace_id",
            "/tab/workspace_id",
            "/workspace/workspace_id",
        ]),
    }
}

/// The honest terminal receipt for an action whose backend subsystem does
/// not exist yet — bytes never left the relay, so `dispatched_unknown`
/// overstates nothing; the receipt alone (no `command_result`) is what the
/// baseline router emitted for unhandled kinds.
pub(crate) fn unknown(request_id: &str, action_id: &str) -> Vec<Outbound> {
    vec![Outbound::ActionReceipt(action_receipt_response(
        request_id,
        ActionReceipt {
            action_id: action_id.to_owned(),
            phase: ActionReceiptPhase::from(ActionReceiptPhase::DISPATCHED_UNKNOWN),
            error: None,
        },
    ))]
}

#[cfg(test)]
mod transport_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    fn outcome_of(frames: Vec<Outbound>) -> (CommandResultMessage, ActionReceipt) {
        let mut iter = frames.into_iter();
        let result = match iter.next() {
            Some(Outbound::CommandResult(m)) => m,
            other => panic!("expected command_result, got {other:?}"),
        };
        let receipt = match iter.next() {
            Some(Outbound::ActionReceipt(m)) => m.receipt.unwrap(),
            other => panic!("expected action_receipt, got {other:?}"),
        };
        assert!(iter.next().is_none());
        (result, receipt)
    }

    #[test]
    fn completed_emits_result_then_confirmed_receipt() {
        let (result, receipt) = outcome_of(
            Outcome::completed("p1", Some(serde_json::json!({"x": 1}))).frames(
                "r1",
                "send_text",
                "a1",
            ),
        );
        assert_eq!(result.phase.as_deref(), Some("completed"));
        assert_eq!(result.ok, Some(true));
        assert_eq!(result.error.as_deref(), Some(""));
        assert_eq!(result.pane_id.as_deref(), Some("p1"));
        assert!(result.data.is_some());
        assert_eq!(receipt.phase.as_str(), "confirmed");
        assert!(receipt.error.is_none());
    }

    #[test]
    fn failed_maps_to_failed_before_dispatch() {
        let (result, receipt) =
            outcome_of(Outcome::failed("p1", "Agent is required").frames("r1", "send_text", "a1"));
        assert_eq!(result.phase.as_deref(), Some("failed"));
        assert_eq!(result.error.as_deref(), Some("Agent is required"));
        assert_eq!(receipt.phase.as_str(), "failed_before_dispatch");
        assert_eq!(receipt.error.unwrap().code, "invalid_request");
    }

    #[test]
    fn herdr_refusal_is_confirmed_receipt_with_code() {
        let err = HerdrError::Refused {
            code: "workspace_not_found".into(),
            message: "gone".into(),
        };
        let (result, receipt) =
            outcome_of(dispatch_failure("p1", &err).frames("r1", "send_text", "a1"));
        assert_eq!(result.phase.as_deref(), Some("not_started"));
        assert_eq!(receipt.phase.as_str(), "confirmed");
        assert_eq!(receipt.error.unwrap().code, "workspace_not_found");
    }

    #[test]
    fn transient_refusal_keeps_retry_safe_message() {
        let err = HerdrError::Refused {
            code: "agent_pane_busy".into(),
            message: "busy".into(),
        };
        let (result, receipt) =
            outcome_of(dispatch_failure("p1", &err).frames("r1", "send_text", "a1"));
        assert_eq!(
            result.error.as_deref(),
            Some("Command was not sent; retry is safe")
        );
        assert_eq!(receipt.error.unwrap().code, "agent_pane_busy");
    }

    #[test]
    fn dispatched_unknown_keeps_data_marker() {
        let inner = HerdrError::NotStarted(Arc::new(io::Error::other("x")));
        let err = HerdrError::DispatchedUnknown(Box::new(inner));
        let (result, receipt) =
            outcome_of(dispatch_failure("p1", &err).frames("r1", "send_text", "a1"));
        assert_eq!(result.phase.as_deref(), Some("dispatched_unknown"));
        assert_eq!(receipt.phase.as_str(), "dispatched_unknown");
        assert_eq!(receipt.error.unwrap().code, "dispatch_outcome_unknown");
    }

    #[test]
    fn not_started_is_failed_before_dispatch_receipt() {
        let err =
            HerdrError::NotStarted(Arc::new(io::Error::new(io::ErrorKind::BrokenPipe, "gone")));
        let (result, receipt) =
            outcome_of(dispatch_failure("p1", &err).frames("r1", "send_text", "a1"));
        assert_eq!(result.phase.as_deref(), Some("not_started"));
        assert_eq!(
            result.error.as_deref(),
            Some("Command was not sent; retry is safe")
        );
        assert_eq!(receipt.phase.as_str(), "failed_before_dispatch");
        assert_eq!(receipt.error.unwrap().code, "herdr_unreachable");
    }

    #[test]
    fn topology_dirty_force_gets_escape_data() {
        let err = HerdrError::Refused {
            code: "dirty_worktree_requires_force".into(),
            message: "dirty".into(),
        };
        let (result, receipt) = outcome_of(topology_failure("worktree_remove", &err).frames(
            "r1",
            "worktree_remove",
            "a1",
        ));
        assert_eq!(result.phase.as_deref(), Some("not_started"));
        let data = result.data.unwrap();
        assert!(data.value().unwrap().get().contains("force_available"));
        assert_eq!(receipt.error.unwrap().code, "dirty_worktree_requires_force");
    }

    #[test]
    fn worktree_list_degrades_to_plain_failure() {
        let err = HerdrError::NotStarted(Arc::new(io::Error::other("x")));
        let (result, _) =
            outcome_of(topology_failure("worktree_list", &err).frames("r1", "worktree_list", "a1"));
        assert_eq!(result.phase.as_deref(), Some("failed"));
        assert_eq!(
            result.error.as_deref(),
            Some("Worktrees could not be listed")
        );
    }

    #[test]
    fn topology_unknown_uses_refresh_wording() {
        let inner = HerdrError::NotStarted(Arc::new(io::Error::other("x")));
        let err = HerdrError::DispatchedUnknown(Box::new(inner));
        let (result, receipt) = outcome_of(topology_failure("workspace_close", &err).frames(
            "r1",
            "workspace_close",
            "a1",
        ));
        assert_eq!(result.phase.as_deref(), Some("dispatched_unknown"));
        assert_eq!(
            result.error.as_deref(),
            Some("Herdr may have completed this command; refresh before retrying")
        );
        assert_eq!(receipt.phase.as_str(), "dispatched_unknown");
    }
}
