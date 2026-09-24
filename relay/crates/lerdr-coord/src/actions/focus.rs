//! Focus family — the Phase-5 `focus_*` actions (docs/13 §1.1), the
//! notification-tap → desktop-jump leg. Each maps to one Herdr method:
//! `focus_pane` → `pane.focus`, `focus_tab` → `tab.focus`,
//! `focus_workspace` → `workspace.focus`, `focus_agent` → `agent.focus`
//! (the session id resolves to its hosting pane first — `agent.focus`'s
//! `target` takes agent names and pane ids, not session references).
//!
//! The session gate already guarantees the `focus` capability is live for
//! this client; the per-method check here covers a *partial* family — a
//! Herdr build that ships `pane.focus` but not `workspace.focus` keeps
//! `focus` advertised while `workspace_focus` refuses with the same
//! `capability_unsupported` code the session gate emits.

use lerdr_core::protocol::{Inbound, Outbound};

use super::{
    capability_gap, dispatch_failure, method_refuted, pane_of, ActionContext, Outcome,
    COMMAND_DEADLINE,
};

/// `focus_pane` — raise the pane's tab and window. The exact-target check
/// already ran at admission, so `pane_id` is a live, current pane.
pub(crate) async fn focus_pane(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    let pane_id = pane_of(message);
    let outcome = if pane_id.is_empty() {
        Outcome::failed(pane_id, "Pane is required")
    } else if method_refuted(&ctx, "pane.focus") {
        capability_gap("pane.focus", pane_id)
    } else {
        match ctx.client.pane_focus(pane_id, Some(COMMAND_DEADLINE)).await {
            Ok(_) => {
                // `d.wake()` — republish so the focused row reaches the
                // phone without waiting for the next event.
                ctx.handle.refresh().await;
                Outcome::completed(pane_id, None)
            }
            Err(err) => dispatch_failure(pane_id, &err),
        }
    };
    outcome.frames(request_id, "focus_pane", action_id)
}

/// `focus_tab` — activate the tab and its workspace. `target.tab_id` is
/// the address; `pane_id` rides along only as client context (a stale
/// pane must not veto focusing a still-live tab — `target.rs` exempts
/// this action from the exact-tuple check).
pub(crate) async fn focus_tab(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    let pane_id = pane_of(message);
    let tab_id = message
        .target
        .as_ref()
        .map(|t| t.tab_id.as_str())
        .unwrap_or_default();
    let outcome = if tab_id.is_empty() {
        Outcome::failed(pane_id, "Tab is required")
    } else if method_refuted(&ctx, "tab.focus") {
        capability_gap("tab.focus", pane_id)
    } else {
        match ctx.client.tab_focus(tab_id, Some(COMMAND_DEADLINE)).await {
            Ok(_) => {
                ctx.handle.refresh().await;
                Outcome::completed(pane_id, None)
            }
            Err(err) => dispatch_failure(pane_id, &err),
        }
    };
    outcome.frames(request_id, "focus_tab", action_id)
}

/// `focus_workspace` — activate the workspace. `target.workspace_id` is
/// the address; no pane identity is required or checked.
pub(crate) async fn focus_workspace(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    let pane_id = pane_of(message);
    let workspace_id = message
        .target
        .as_ref()
        .map(|t| t.workspace_id.as_str())
        .unwrap_or_default();
    let outcome = if workspace_id.is_empty() {
        Outcome::failed(pane_id, "Workspace is required")
    } else if method_refuted(&ctx, "workspace.focus") {
        capability_gap("workspace.focus", pane_id)
    } else {
        match ctx
            .client
            .workspace_focus(workspace_id, Some(COMMAND_DEADLINE))
            .await
        {
            Ok(_) => {
                ctx.handle.refresh().await;
                Outcome::completed(pane_id, None)
            }
            Err(err) => dispatch_failure(pane_id, &err),
        }
    };
    outcome.frames(request_id, "focus_workspace", action_id)
}

/// `focus_agent` — focus the agent's pane. `target.agent_session_id`
/// resolves through topology to the hosting pane; an unknown session
/// fails pre-dispatch (the client could not have addressed it).
pub(crate) async fn focus_agent(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    let session_id = message
        .target
        .as_ref()
        .map(|t| t.agent_session_id.as_str())
        .unwrap_or_default();
    let pane_id = ctx
        .topology
        .pane_for_session(session_id)
        .map(|agent| agent.pane_id.clone())
        .unwrap_or_default();
    let outcome = if session_id.is_empty() {
        Outcome::failed("", "Agent session is required")
    } else if pane_id.is_empty() {
        Outcome::failed("", "Agent session is unavailable")
    } else if method_refuted(&ctx, "agent.focus") {
        capability_gap("agent.focus", &pane_id)
    } else {
        match ctx
            .client
            .agent_focus(&pane_id, Some(COMMAND_DEADLINE))
            .await
        {
            Ok(_) => {
                ctx.handle.refresh().await;
                Outcome::completed(&pane_id, None)
            }
            Err(err) => dispatch_failure(&pane_id, &err),
        }
    };
    outcome.frames(request_id, "focus_agent", action_id)
}
