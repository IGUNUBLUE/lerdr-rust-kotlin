//! `HerdRouter` — the [`ActionRouter`] that dispatches client actions to
//! Herdr and owns this session's pane watches.
//!
//! Contract notes:
//!
//! - `route()` is **sync** (actor-core rule): Herdr calls are spawned onto
//!   the runtime and their results arrive as [`ClientSink`] pushes —
//!   result message first, `action_receipt` last, so the client sees
//!   content before the lifecycle terminal.
//! - Receipt phases follow the dispatch-boundary taxonomy (doc 08 rule 4 /
//!   `lerdr_herdr::error::HerdrError::phase`): `NotStarted` →
//!   `failed_before_dispatch`, `DispatchedUnknown` → `dispatched_unknown`,
//!   `Refused` → `confirmed` carrying the refusal in `error`.
//! - Actions without a backend yet answer `dispatched_unknown` — the same
//!   honest terminal state `StubRouter` gives.
//! - Topology broadcasts are per-connection: each router forwards
//!   `workspaces`/`agents`/`herdr_status` to its own client on every
//!   revision (all replaceable — bursts coalesce in the send buffer).

use std::collections::BTreeMap;
use std::sync::Arc;

use lerdr_core::protocol::{
    action_receipt_response, error_codes, error_response, ActionReceipt, ActionReceiptPhase,
    ApiError, Inbound, Outbound, PaneContent, RequestScope,
};
use lerdr_herdr::{DispatchPhase, HerdrError, ReadFormat, ReadSource};
use lerdr_relay::router::{ActionRouter, ClientContext, RouterReply};
use lerdr_relay::session::ClientSink;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::actor::TopologyHandle;
use crate::snapshot::topology_broadcast;
use crate::watches::{WatchSet, DEFAULT_LINES};

/// How a router resolves its own client's push endpoint. Wired to
/// `Relay::client_sink` at construction — the lookup is lazy because the
/// sink registers only after the handshake commits.
pub type ClientSinkLookup = Arc<dyn Fn(&str) -> Option<ClientSink> + Send + Sync>;

/// Builds one [`HerdRouter`] per accepted session.
#[derive(Clone)]
pub struct HerdRouterFactory {
    handle: TopologyHandle,
    sink_of: ClientSinkLookup,
    cancel: CancellationToken,
}

impl HerdRouterFactory {
    /// `handle` comes from [`crate::TopologyActor::spawn`]; `sink_of` is
    /// typically `move |id| relay.client_sink(id)`; `cancel` should be the
    /// relay shutdown token (watches die with the relay).
    pub fn new(
        handle: TopologyHandle,
        sink_of: ClientSinkLookup,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            handle,
            sink_of,
            cancel,
        }
    }

    /// The closure `Relay::with_router_factory` expects.
    pub fn into_factory(self) -> impl Fn() -> Box<dyn ActionRouter> + Send + Sync {
        move || {
            Box::new(HerdRouter::new(
                self.handle.clone(),
                self.sink_of.clone(),
                self.cancel.child_token(),
            )) as Box<dyn ActionRouter>
        }
    }
}

/// Per-session router. One instance per client connection.
pub struct HerdRouter {
    handle: TopologyHandle,
    sink_of: ClientSinkLookup,
    cancel: CancellationToken,
    /// Lazily captured from the first `ClientContext`.
    client_id: Option<String>,
    watches: WatchSet,
    /// Forwards topology revisions to this client.
    forwarder: Option<tokio::task::JoinHandle<()>>,
}

impl HerdRouter {
    fn new(handle: TopologyHandle, sink_of: ClientSinkLookup, cancel: CancellationToken) -> Self {
        Self {
            handle,
            sink_of,
            cancel,
            client_id: None,
            watches: WatchSet::default(),
            forwarder: None,
        }
    }

    /// First-route initialization: client id + topology forwarder.
    fn ensure_started(&mut self, ctx: &ClientContext<'_>) {
        if self.client_id.is_some() {
            return;
        }
        self.client_id = Some(ctx.client_id.to_owned());
        let sink_of = self.sink_of.clone();
        let mut topo_rx = self.handle.topology.clone();
        let client_id = ctx.client_id.to_owned();
        let cancel = self.cancel.clone();
        let span_id = client_id.clone();
        self.forwarder = Some(tokio::spawn(
            async move {
                // Skip the initial value — the session snapshot already
                // carries it.
                if topo_rx.changed().await.is_err() {
                    return;
                }
                loop {
                    let topology = topo_rx.borrow().clone();
                    if let Some(sink) = sink_of(&client_id) {
                        let mut gone = false;
                        for message in topology_broadcast(&topology) {
                            gone = sink.try_send(&message).is_err();
                            if gone {
                                break;
                            }
                        }
                        if gone {
                            return; // client gone — session drop stops us
                        }
                    }
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        changed = topo_rx.changed() => {
                            if changed.is_err() { break }
                        }
                    }
                }
            }
            .instrument(tracing::info_span!("topology_forwarder", client_id = %span_id)),
        ));
    }

    /// Resolve this client's push endpoint.
    fn sink(&self) -> Option<ClientSink> {
        self.client_id.as_deref().and_then(|id| (self.sink_of)(id))
    }

    /// Push `frames` to this client from an async task — result message
    /// first, receipt last; a closed/full queue stops the sequence.
    fn push_later(&self, fut: impl std::future::Future<Output = Vec<Outbound>> + Send + 'static) {
        let sink_of = self.sink_of.clone();
        let client_id = self.client_id.clone().unwrap_or_default();
        let span_id = client_id.clone();
        tokio::spawn(
            async move {
                let frames = fut.await;
                let Some(sink) = sink_of(&client_id) else {
                    return;
                };
                for message in frames {
                    if sink.try_send(&message).is_err() {
                        break;
                    }
                }
            }
            .instrument(tracing::info_span!("action", client_id = %span_id)),
        );
    }
}

impl Drop for HerdRouter {
    fn drop(&mut self) {
        self.watches.stop_all();
        if let Some(f) = self.forwarder.take() {
            f.abort();
        }
    }
}

/// The baseline action set — everything else answers `dispatched_unknown`.
///
/// The oracle's full table (~50 cases in `dispatch.go`) lands in later
/// slices: workspace/worktree mutation, conversation history, uploads,
/// push/policy, speech, qr_code, device admin, update flow, size leases.
fn is_pane_input_action(kind: &str) -> bool {
    matches!(
        kind,
        "send_text" | "send_keys" | "respond" | "answer_question" | "submit_prompt"
    )
}

impl ActionRouter for HerdRouter {
    fn route(
        &mut self,
        ctx: &ClientContext<'_>,
        scope: &RequestScope,
        message: &Inbound,
    ) -> RouterReply {
        self.ensure_started(ctx);
        let request_id = message.request_id.clone();
        let action_id = scope.action_id.clone();

        match message.r#type.as_str() {
            // --- pane read/watch -----------------------------------------
            "read_pane" => self.route_read_pane(request_id, action_id, message),
            "watch_pane" => self.route_watch_pane(request_id, action_id, message),
            "unwatch_pane" => self.route_unwatch_pane(request_id, action_id, message),
            "pane_applied" => {
                self.watches.ack(&message.pane_id);
                RouterReply::empty()
            }
            "pane_resync" => {
                self.watches.resync(&message.pane_id);
                RouterReply::empty()
            }

            // --- input ---------------------------------------------------
            kind if is_pane_input_action(kind) => {
                self.route_pane_input(request_id, action_id, message)
            }

            // --- everything else: honest terminal state ------------------
            _ => RouterReply::send(vec![receipt(
                &request_id,
                &action_id,
                ActionReceiptPhase::DISPATCHED_UNKNOWN,
                None,
            )]),
        }
    }
}

impl HerdRouter {
    /// `read_pane` → async `pane.read` → `pane_content` push → receipt.
    /// The wire `content_fingerprint` field is raw-only (not on typed
    /// `Inbound`), so the fingerprint-hit `pane_unchanged` path needs the
    /// raw-field seam — baseline always answers full content.
    fn route_read_pane(
        &mut self,
        request_id: String,
        action_id: String,
        message: &Inbound,
    ) -> RouterReply {
        let Some(pane_id) = non_empty(&message.pane_id) else {
            return invalid_request(&request_id, &action_id, "pane_id is required");
        };
        if self.sink().is_none() {
            return refused(&request_id, &action_id, "session_not_ready");
        }
        let client = self.handle.client.clone();
        let lines = u32::try_from(message.lines)
            .ok()
            .filter(|l| *l > 0)
            .unwrap_or(DEFAULT_LINES);
        let rid = request_id.clone();
        let aid = action_id.clone();
        self.push_later(async move {
            match client
                .pane_read(
                    &pane_id,
                    ReadSource::RecentUnwrapped,
                    lines,
                    ReadFormat::Text,
                )
                .await
            {
                Ok(read) => vec![
                    Outbound::PaneContent(Box::new(PaneContent {
                        r#type: "pane_content".to_owned(),
                        pane_id: Some(pane_id.clone()),
                        content: Some(read.text.clone()),
                        content_fingerprint: Some(crate::content_fingerprint(&read.text)),
                        format: Some("text".to_owned()),
                        truncated: Some(read.truncated),
                        ..PaneContent::default()
                    })),
                    receipt(&rid, &aid, ActionReceiptPhase::CONFIRMED, None),
                ],
                Err(err) => vec![receipt_for_herdr_error(&rid, &aid, &err)],
            }
        });
        RouterReply::empty()
    }

    /// `watch_pane` — start the event-driven watch task.
    fn route_watch_pane(
        &mut self,
        request_id: String,
        action_id: String,
        message: &Inbound,
    ) -> RouterReply {
        let Some(pane_id) = non_empty(&message.pane_id) else {
            return invalid_request(&request_id, &action_id, "pane_id is required");
        };
        let Some(sink) = self.sink() else {
            return refused(&request_id, &action_id, "session_not_ready");
        };
        if !self.watches.watching(&pane_id) {
            let lines = u32::try_from(message.lines)
                .ok()
                .filter(|l| *l > 0)
                .unwrap_or(DEFAULT_LINES);
            self.watches.start(
                pane_id,
                lines,
                self.handle.client.clone(),
                sink,
                self.handle.invalidations.clone(),
                self.cancel.clone(),
            );
        }
        RouterReply::send(vec![receipt(
            &request_id,
            &action_id,
            ActionReceiptPhase::CONFIRMED,
            None,
        )])
    }

    /// `unwatch_pane` — stop the task; unknown pane is a no-op receipt.
    fn route_unwatch_pane(
        &mut self,
        request_id: String,
        action_id: String,
        message: &Inbound,
    ) -> RouterReply {
        self.watches.stop(&message.pane_id);
        RouterReply::send(vec![receipt(
            &request_id,
            &action_id,
            ActionReceiptPhase::CONFIRMED,
            None,
        )])
    }

    /// Text/keys/respond → `pane.send_input`. `respond`/`answer_question`
    /// compose the answer text (`choice`, else `prompt`/`text`) — the
    /// oracle's structured-answer composition (option labels, multi-select
    /// joins) is a follow-up; this baseline sends the primary text.
    fn route_pane_input(
        &mut self,
        request_id: String,
        action_id: String,
        message: &Inbound,
    ) -> RouterReply {
        let Some(pane_id) = non_empty(&message.pane_id) else {
            return invalid_request(&request_id, &action_id, "pane_id is required");
        };
        if self.sink().is_none() {
            return refused(&request_id, &action_id, "session_not_ready");
        }
        let text = match message.r#type.as_str() {
            "send_text" | "submit_prompt" => Some(if message.text.is_empty() {
                message.prompt.clone()
            } else {
                message.text.clone()
            }),
            "respond" | "answer_question" => Some(if message.choice.is_empty() {
                message.text.clone()
            } else {
                message.choice.clone()
            }),
            _ => None,
        }
        .filter(|t| !t.is_empty());
        let keys = if message.keys.is_empty() {
            None
        } else {
            Some(message.keys.clone())
        };
        if text.is_none() && keys.is_none() {
            return invalid_request(&request_id, &action_id, "nothing to send");
        }

        let client = self.handle.client.clone();
        let rid = request_id.clone();
        let aid = action_id.clone();
        self.push_later(async move {
            let outcome = client
                .pane_send_input(&pane_id, text.as_deref(), keys.unwrap_or_default())
                .await;
            vec![match &outcome {
                Ok(()) => receipt(&rid, &aid, ActionReceiptPhase::CONFIRMED, None),
                Err(err) => receipt_for_herdr_error(&rid, &aid, err),
            }]
        });
        RouterReply::empty()
    }
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_owned())
}

fn receipt(
    request_id: &str,
    action_id: &str,
    phase: &'static str,
    error: Option<ApiError>,
) -> Outbound {
    Outbound::ActionReceipt(action_receipt_response(
        request_id,
        ActionReceipt {
            action_id: action_id.to_owned(),
            phase: ActionReceiptPhase::from(phase),
            error,
        },
    ))
}

/// Dispatch-boundary taxonomy → receipt (doc 08 rule 4).
fn receipt_for_herdr_error(request_id: &str, action_id: &str, err: &HerdrError) -> Outbound {
    let (phase, error) = match err.phase() {
        DispatchPhase::NotStarted => (
            ActionReceiptPhase::FAILED_BEFORE_DISPATCH,
            Some(api_error("herdr_unreachable", err)),
        ),
        DispatchPhase::DispatchedUnknown => (
            ActionReceiptPhase::DISPATCHED_UNKNOWN,
            Some(api_error("dispatch_outcome_unknown", err)),
        ),
        DispatchPhase::Refused => (
            ActionReceiptPhase::CONFIRMED,
            Some(ApiError::new(
                err.refusal_code().unwrap_or("refused"),
                refusal_args(err),
            )),
        ),
    };
    receipt(request_id, action_id, phase, error)
}

fn api_error(code: &str, err: &HerdrError) -> ApiError {
    let mut args = BTreeMap::new();
    args.insert(
        "detail".to_owned(),
        serde_json::Value::String(err.to_string()),
    );
    ApiError::new(code, args)
}

fn refusal_args(err: &HerdrError) -> BTreeMap<String, serde_json::Value> {
    let mut args = BTreeMap::new();
    if let HerdrError::Refused { message, .. } = err {
        args.insert(
            "message".to_owned(),
            serde_json::Value::String(message.clone()),
        );
    }
    args
}

fn invalid_request(request_id: &str, action_id: &str, detail: &str) -> RouterReply {
    let mut args = BTreeMap::new();
    args.insert(
        "detail".to_owned(),
        serde_json::Value::String(detail.to_owned()),
    );
    RouterReply::send(vec![
        Outbound::Error(error_response(
            request_id,
            ApiError::new(error_codes::INVALID_REQUEST, args),
        )),
        receipt(
            request_id,
            action_id,
            ActionReceiptPhase::FAILED_BEFORE_DISPATCH,
            Some(ApiError::new(error_codes::INVALID_REQUEST, BTreeMap::new())),
        ),
    ])
}

fn refused(request_id: &str, action_id: &str, code: &str) -> RouterReply {
    RouterReply::send(vec![receipt(
        request_id,
        action_id,
        ActionReceiptPhase::CONFIRMED,
        Some(ApiError::new(code, BTreeMap::new())),
    )])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_phases() {
        let r = |phase: &'static str| match receipt("r1", "a1", phase, None) {
            Outbound::ActionReceipt(m) => m.receipt.unwrap().phase.as_str().to_owned(),
            _ => panic!("expected receipt"),
        };
        assert_eq!(r(ActionReceiptPhase::CONFIRMED), "confirmed");
        assert_eq!(
            r(ActionReceiptPhase::DISPATCHED_UNKNOWN),
            "dispatched_unknown"
        );
    }

    #[test]
    fn herdr_error_phase_mapping() {
        let io = || std::io::Error::new(std::io::ErrorKind::BrokenPipe, "gone");
        match receipt_for_herdr_error("r", "a", &HerdrError::NotStarted(Arc::new(io()))) {
            Outbound::ActionReceipt(m) => {
                assert_eq!(m.receipt.unwrap().phase.as_str(), "failed_before_dispatch");
            }
            _ => panic!(),
        }
        let refused_err = HerdrError::Refused {
            code: "agent_pane_busy".into(),
            message: "busy".into(),
        };
        match receipt_for_herdr_error("r", "a", &refused_err) {
            Outbound::ActionReceipt(m) => {
                let r = m.receipt.unwrap();
                assert_eq!(r.phase.as_str(), "confirmed");
                assert_eq!(r.error.unwrap().code, "agent_pane_busy");
            }
            _ => panic!(),
        }
    }
}
