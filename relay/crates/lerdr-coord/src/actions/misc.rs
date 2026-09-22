//! Miscellaneous local actions — updates, conversation history, slash
//! commands, inventory, copy, app-origin registration.
//!
//! - `check_update`/`install_update`: the oracle's update state machine —
//!   check feeds `{"update": state}` in the result; install schedules.
//! - `get_conversation_history`: pages a conversation browser scoped to
//!   the pane's agent (`Cursor`, `Limit`, `Retry` are typed Inbound
//!   fields); fails "Conversation history could not be read" without a
//!   browser.
//! - `list_slash_commands`: resolves the pane agent's slash commands
//!   (project context + home dir).
//! - `inventory_status`: Herdr inventory snapshot.
//! - `copy_agent_response`: returns the pane's last agent response text.
//! - `register_app_origin`: stores the phone app origin (local store, no
//!   result frame — server.go records and warns only).
//!
//! Stubs answer `dispatched_unknown` (or no-op where the oracle emits
//! nothing).

use lerdr_core::protocol::{Inbound, Outbound};

use super::{unknown, ActionContext};

/// `check_update` — `command_result` with `{"update": <state>}`.
pub(crate) async fn check_update(
    _ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    unknown(request_id, action_id)
}

/// `install_update` — schedules the pending update; `command_result`
/// with `{"update": <state>}`.
pub(crate) async fn install_update(
    _ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    unknown(request_id, action_id)
}

/// `get_conversation_history` — paged conversation browse for the pane.
pub(crate) async fn conversation_history(
    _ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    unknown(request_id, action_id)
}

/// `list_slash_commands` — the pane agent's slash-command list.
pub(crate) async fn slash_commands(
    _ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    unknown(request_id, action_id)
}

/// `inventory_status` — Herdr inventory snapshot frame.
pub(crate) async fn inventory_status(
    _ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    unknown(request_id, action_id)
}

/// `copy_agent_response` — the pane's last agent response payload.
pub(crate) async fn copy_agent_response(
    _ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    unknown(request_id, action_id)
}

/// `register_app_origin` — persists the phone app origin; the oracle
/// emits no result frame on success.
pub(crate) async fn register_app_origin(
    _ctx: ActionContext,
    _request_id: &str,
    _action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    Vec::new()
}
