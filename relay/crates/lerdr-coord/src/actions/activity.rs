//! Relay-side activity journal — the `internal/activity` port.
//!
//! The oracle records every routed mutation (`recordActivity`: action,
//! status, summary, pane, request) into a bounded ring journal;
//! `get_activity` answers `activity_history` with the newest entries and
//! `clear_activities` drains it. The app merges this feed with its
//! device-local session journal.
//!
//! Stubs answer `dispatched_unknown`/`failed` until the journal lands;
//! [`Journal`] is the shared ring buffer the router hands every session.

use std::sync::{Arc, Mutex};

use lerdr_core::protocol::{Inbound, Outbound};

use super::{unknown, ActionContext};

/// Shared activity journal — one per relay (the oracle's
/// `activity.Journal` ring). Handlers call `record` as they complete so
/// the feed reflects what actually happened.
#[derive(Clone, Default)]
pub(crate) struct Journal {
    #[allow(dead_code)]
    inner: Arc<Mutex<()>>,
}

/// `get_activity` — `{"type":"activity_history","activities":[…]}`.
pub(crate) async fn get_activity(
    _ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    unknown(request_id, action_id)
}

/// `HandleClearActivities` — `command_result` once drained.
pub(crate) async fn clear_activities(
    _ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    unknown(request_id, action_id)
}
