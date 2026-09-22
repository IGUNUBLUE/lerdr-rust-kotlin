//! Question/approval actions — the `approval.go` state machine port.
//!
//! The oracle tracks per-pane pending interactions (approval fingerprints,
//! question sessions, option selections, deadlines) so `respond` and the
//! `*_question` actions answer exactly the interaction the client saw —
//! stale answers and duplicate submissions are refused before they reach
//! the pane. Stubs answer `dispatched_unknown` until the state machine
//! lands; [`Questions`] is the shared handle the router hands every
//! session.
//!
//! Until then `respond`/`answer_question` fall back to the baseline
//! `pane.send_input` text path in the router (closer to the oracle than
//! refusing outright).

use std::sync::{Arc, Mutex};

use lerdr_core::protocol::{Inbound, Outbound};

use super::{dispatch_failure, unknown, ActionContext, Outcome};

/// Shared question/approval state — one per relay (the oracle's
/// coordinator question store). Fill in as the state machine lands.
#[derive(Clone, Default)]
pub(crate) struct Questions {
    #[allow(dead_code)]
    inner: Arc<Mutex<()>>,
}

/// `handleRespond` — stub keeps the baseline text send: `choice`, else
/// `text`, into `pane.send_input`.
pub(crate) async fn respond(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    baseline_answer(ctx, request_id, action_id, message, "respond").await
}

/// `handleAnswerQuestion` — same baseline shape as `respond`.
pub(crate) async fn answer_question(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    baseline_answer(ctx, request_id, action_id, message, "answer_question").await
}

/// The router's old baseline: send the composed answer as plain text.
/// Replaced once the state machine knows which interaction is pending.
async fn baseline_answer(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
    action: &str,
) -> Vec<Outbound> {
    let pane_id = message.pane_id.as_str();
    if pane_id.is_empty() {
        return Outcome::failed("", "pane_id is required").frames(request_id, action, action_id);
    }
    let text = if message.choice.is_empty() {
        message.text.clone()
    } else {
        message.choice.clone()
    };
    if text.is_empty() && message.keys.is_empty() {
        return Outcome::failed("", "nothing to send").frames(request_id, action, action_id);
    }
    match ctx
        .client
        .pane_send_input(
            pane_id,
            (!text.is_empty()).then_some(text.as_str()),
            message.keys.clone(),
        )
        .await
    {
        Ok(()) => Outcome::completed(pane_id, None).frames(request_id, action, action_id),
        Err(err) => dispatch_failure(pane_id, &err).frames(request_id, action, action_id),
    }
}

/// `handleClarifyQuestion`.
pub(crate) async fn clarify_question(
    _ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    unknown(request_id, action_id)
}

/// `handleNavigateQuestion`.
pub(crate) async fn navigate_question(
    _ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    unknown(request_id, action_id)
}
