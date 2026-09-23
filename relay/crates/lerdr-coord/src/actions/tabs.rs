//! Tab/agent rename + reorder + pane acknowledgment — the
//! `handleTabRename`/`handleTabReorder`/`handleAcknowledge` port.
//!
//! `agent_rename` renames the pane's **tab**, not the agent record — the
//! oracle routes it to `tab rename` (`tab.rename{tab_id,label}`) after
//! resolving `pane_id → tab_id` through its agent table. `tab_reorder`
//! resolves the same way into `tab.move{tab_id,insert_index}`.
//!
//! `acknowledge_pane` never touches Herdr: the oracle clears the pane's
//! triage attention in local state and rebroadcasts `agent_update` when the
//! displayed status changed. The Rust topology does not project attention
//! state (doc 10), so the honest port is an in-memory ledger keyed by the
//! pane's `state_change_seq` at ack time — an ack only covers the state the
//! client could have seen.

use lerdr_core::protocol::Inbound;
use serde::Serialize;

use super::{
    dispatch_failure, record_activity, ActionContext, Outcome, COMMAND_DEADLINE, MAX_INSERT_INDEX,
};

#[derive(Serialize)]
struct TabRenameParams<'a> {
    tab_id: &'a str,
    label: &'a str,
}

#[derive(Serialize)]
struct TabMoveParams<'a> {
    tab_id: &'a str,
    insert_index: i64,
}

/// `handleTabRename` — `name` is the new tab label; the pane's tab is the
/// target.
pub(crate) async fn agent_rename(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<lerdr_core::protocol::Outbound> {
    let pane_id = message.pane_id.as_str();
    let label = message.name.trim();
    let outcome = if pane_id.is_empty() {
        Outcome::failed(pane_id, "Agent is required")
    } else if label.is_empty() {
        Outcome::failed(pane_id, "Tab name is required")
    } else {
        match tab_of(&ctx, pane_id) {
            None => Outcome::failed(pane_id, "Tab is unavailable"),
            Some(tab_id) => {
                match ctx
                    .client
                    .call_with_timeout(
                        "tab.rename",
                        &TabRenameParams {
                            tab_id: &tab_id,
                            label,
                        },
                        Some(COMMAND_DEADLINE),
                    )
                    .await
                {
                    Ok(_) => {
                        // `d.wake()` — republish so the new label reaches the
                        // phone without waiting for the next event.
                        ctx.handle.refresh().await;
                        Outcome::completed(pane_id, None)
                    }
                    Err(err) => dispatch_failure(pane_id, &err),
                }
            }
        }
    };
    if outcome.ok {
        record_activity(
            &ctx,
            "agent_rename",
            "renamed",
            format!("Renamed tab to {label}"),
            pane_id,
            request_id,
        );
    }
    outcome.frames(request_id, "agent_rename", action_id)
}

/// `handleTabReorder` — `tab.move{tab_id, insert_index}` with the index
/// bound shared by workspaces.
pub(crate) async fn tab_reorder(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<lerdr_core::protocol::Outbound> {
    let pane_id = message.pane_id.as_str();
    let outcome = if pane_id.is_empty() {
        Outcome::failed(pane_id, "Agent is required")
    } else {
        match message.insert_index {
            Some(index) if (0..=MAX_INSERT_INDEX).contains(&index) => match tab_of(&ctx, pane_id) {
                None => Outcome::failed(pane_id, "Tab is unavailable"),
                Some(tab_id) => {
                    match ctx
                        .client
                        .call_with_timeout(
                            "tab.move",
                            &TabMoveParams {
                                tab_id: &tab_id,
                                insert_index: index,
                            },
                            Some(COMMAND_DEADLINE),
                        )
                        .await
                    {
                        Ok(_) => {
                            ctx.handle.refresh().await;
                            Outcome::completed(
                                pane_id,
                                Some(serde_json::json!({ "insert_index": index })),
                            )
                        }
                        Err(err) => dispatch_failure(pane_id, &err),
                    }
                }
            },
            _ => Outcome::failed(pane_id, "Tab position is invalid"),
        }
    };
    if outcome.ok {
        record_activity(
            &ctx,
            "tab_reorder",
            "reordered",
            "Reordered tab",
            pane_id,
            request_id,
        );
    }
    outcome.frames(request_id, "tab_reorder", action_id)
}

/// `handleAcknowledge` (dispatch.go:619-636) — the shared
/// [`acknowledge_pane_state`] half records the ack, journals the failure
/// row for a gone pane, wakes the poller, and broadcasts `agent_update`
/// on a displayed-status change; the routed command just maps the result.
pub(crate) fn acknowledge(ctx: &ActionContext, request_id: &str, pane_id: &str) -> Outcome {
    if super::acknowledge_pane_state(
        &ctx.handle,
        &ctx.notices,
        &ctx.activities,
        pane_id,
        request_id,
    ) {
        Outcome::completed(pane_id, None)
    } else {
        Outcome::failed(pane_id, "Agent is unavailable")
    }
}

/// `handleAcknowledge` as a full frame pair for the routed action.
pub(crate) async fn acknowledge_pane(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<lerdr_core::protocol::Outbound> {
    let pane_id = message.pane_id.as_str();
    if pane_id.is_empty() {
        return Outcome::failed(pane_id, "Agent is unavailable").frames(
            request_id,
            "acknowledge_pane",
            action_id,
        );
    }
    acknowledge(&ctx, request_id, pane_id).frames(request_id, "acknowledge_pane", action_id)
}

/// `d.state.Agent(paneID).TabID`.
fn tab_of(ctx: &ActionContext, pane_id: &str) -> Option<String> {
    let tab_id = ctx.topology.pane_of(pane_id)?.tab_id.as_str();
    (!tab_id.is_empty()).then(|| tab_id.to_owned())
}
