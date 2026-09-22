//! Push-notification actions — the `internal/push` subsystem port.
//!
//! The oracle keeps per-device push subscriptions, a notification policy
//! (which events reach which device), snooze state, and a viewed-pane
//! dedup ledger (`push_viewed_pane` suppresses notifications for panes the
//! operator is already looking at). Replies are NOT `command_result` for
//! the whole family — `push_policy_get` answers `push_policy`,
//! `push_policy_set` emits `push_policy_result` + `command_result`,
//! subscribe/unsubscribe emit `push_subscribed`/`push_unsubscribed`,
//! `push_test_device` emits a result frame; check server.go for each
//! action's exact frame shape.
//!
//! Stubs answer `dispatched_unknown`; [`Push`] carries the policy/store
//! state shared across sessions.

use std::sync::{Arc, Mutex};

use lerdr_core::protocol::{Inbound, Outbound};

use super::{unknown, ActionContext};

/// Shared push state — one per relay (the oracle's push registry: policy,
/// subscriptions, snooze, viewed-pane ledger).
#[derive(Clone, Default)]
pub(crate) struct Push {
    #[allow(dead_code)]
    inner: Arc<Mutex<()>>,
}

/// `push_policy_get` — answers `{"type":"push_policy","policy":…}`.
pub(crate) async fn policy_get(
    _ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    unknown(request_id, action_id)
}

/// `push_policy_set` — `push_policy_result` + `command_result`; rejects
/// with `push_invalid_policy` / `push_invalid_duration` /
/// `push_invalid_snooze` on bad input.
pub(crate) async fn policy_set(
    _ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    unknown(request_id, action_id)
}

/// `push_subscribe` — registers the device's subscription; emits
/// `push_subscribed`.
pub(crate) async fn subscribe(
    _ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    unknown(request_id, action_id)
}

/// `push_unsubscribe` — emits `push_unsubscribed`.
pub(crate) async fn unsubscribe(
    _ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    unknown(request_id, action_id)
}

/// `push_test_device` — sends a test notification to the device.
pub(crate) async fn test_device(
    _ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    unknown(request_id, action_id)
}

/// `push_snooze` — suppresses notifications for a duration.
pub(crate) async fn snooze(
    _ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    unknown(request_id, action_id)
}

/// `push_viewed_pane` — marks a pane viewed; suppresses its notifications.
pub(crate) async fn viewed_pane(
    _ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    unknown(request_id, action_id)
}

/// `push_open_ref` — resolves a notification's deep-link reference.
pub(crate) async fn open_ref(
    _ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    unknown(request_id, action_id)
}
