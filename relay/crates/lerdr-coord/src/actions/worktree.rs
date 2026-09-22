//! Worktree actions — `workspace.go`'s `HandleWorktree*` port.
//!
//! Method mapping (oracle CLI → socket): `worktree list` →
//! `worktree.list`, `worktree create` → `worktree.create`,
//! `worktree open` → `worktree.open`, `worktree remove` →
//! `worktree.remove`. All go through `topology_failure` so refusal codes
//! (`workspace_not_found`, `dirty_worktree_requires_force`, …) keep their
//! structured shape and post-success decode drift reads as
//! `dispatched_unknown`.
//!
//! `command_result.data` carries the socket result verbatim minus its
//! `type` tag — the oracle's `WorktreeListResult`/`WorktreeMutationResult`
//! have no `type` member, so stripping it reproduces the oracle's shape.

use lerdr_core::protocol::Inbound;
use serde::Serialize;

use super::{topology_failure, ActionContext, Outcome, WORKSPACE_DEADLINE, WORKTREE_DEADLINE};
use crate::actions::workspace::workspace_target;

/// `worktreeValueMaxRunes`.
const VALUE_MAX_RUNES: usize = 512;

#[derive(Serialize)]
struct WorktreeListParams<'a> {
    workspace_id: &'a str,
}

#[derive(Serialize)]
struct WorktreeCreateParams<'a> {
    workspace_id: &'a str,
    branch: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    base: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    label: &'a str,
    focus: bool,
}

#[derive(Serialize)]
struct WorktreeOpenParams<'a> {
    workspace_id: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    path: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    branch: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    label: &'a str,
    focus: bool,
}

#[derive(Serialize)]
struct WorktreeRemoveParams<'a> {
    workspace_id: &'a str,
    force: bool,
}

/// `HandleWorktreeList` — `worktree.list{workspace_id}`; data is the
/// listing (`{source, worktrees}`).
pub(crate) async fn worktree_list(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<lerdr_core::protocol::Outbound> {
    let outcome = match workspace_target(&ctx.topology, &message.workspace_id) {
        Err(outcome) => *outcome,
        Ok(workspace_id) => {
            match ctx
                .client
                .call_with_timeout(
                    "worktree.list",
                    &WorktreeListParams {
                        workspace_id: &workspace_id,
                    },
                    Some(WORKSPACE_DEADLINE),
                )
                .await
            {
                Err(err) => topology_failure("worktree_list", &err),
                Ok(value) => Outcome::completed("", Some(strip_type(value))),
            }
        }
    };
    outcome.frames(request_id, "worktree_list", action_id)
}

/// `HandleWorktreeCreate` — branch required, custom paths refused, then
/// `worktree.create{workspace_id, branch, base?, label?, focus:false}`.
/// `dispatchedAfterSuccess`: a decode failure after dispatch is
/// `dispatched_unknown`, never `failed`.
pub(crate) async fn worktree_create(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<lerdr_core::protocol::Outbound> {
    let outcome = match workspace_target(&ctx.topology, &message.workspace_id) {
        Err(outcome) => *outcome,
        Ok(workspace_id) => {
            let branch = message.branch.trim();
            let base = message.base.trim();
            let path = message.path.trim();
            let label = message.label.trim();
            if branch.is_empty() {
                Outcome::failed("", "Branch is required")
            } else if !path.is_empty() {
                Outcome::failed("", "Custom worktree paths are not available from the phone")
            } else if let Err(outcome) = validate_worktree_values([branch, base, path, label]) {
                *outcome
            } else {
                match ctx
                    .client
                    .call_with_timeout(
                        "worktree.create",
                        &WorktreeCreateParams {
                            workspace_id: &workspace_id,
                            branch,
                            base,
                            label,
                            focus: false,
                        },
                        Some(WORKTREE_DEADLINE),
                    )
                    .await
                {
                    Err(err) => topology_failure("worktree_create", &err),
                    Ok(value) => {
                        ctx.handle.refresh().await;
                        Outcome::completed("", Some(strip_type(value)))
                    }
                }
            }
        }
    };
    outcome.frames(request_id, "worktree_create", action_id)
}

/// `HandleWorktreeOpen` — exactly one of `path`/`branch`, then
/// `worktree.open{workspace_id, path?, branch?, label?, focus:false}`.
pub(crate) async fn worktree_open(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<lerdr_core::protocol::Outbound> {
    let outcome = match workspace_target(&ctx.topology, &message.workspace_id) {
        Err(outcome) => *outcome,
        Ok(workspace_id) => {
            let path = message.path.trim();
            let branch = message.branch.trim();
            let label = message.label.trim();
            if path.is_empty() == branch.is_empty() {
                Outcome::failed("", "Choose exactly one worktree path or branch")
            } else if let Err(outcome) = validate_worktree_values([branch, "", path, label]) {
                *outcome
            } else {
                match ctx
                    .client
                    .call_with_timeout(
                        "worktree.open",
                        &WorktreeOpenParams {
                            workspace_id: &workspace_id,
                            path,
                            branch,
                            label,
                            focus: false,
                        },
                        Some(WORKTREE_DEADLINE),
                    )
                    .await
                {
                    Err(err) => topology_failure("worktree_open", &err),
                    Ok(value) => {
                        ctx.handle.refresh().await;
                        Outcome::completed("", Some(strip_type(value)))
                    }
                }
            }
        }
    };
    outcome.frames(request_id, "worktree_open", action_id)
}

/// `HandleWorktreeRemove` — only a linked-worktree workspace is removable;
/// `worktree.remove{workspace_id, force}`.
pub(crate) async fn worktree_remove(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<lerdr_core::protocol::Outbound> {
    let outcome = match workspace_target(&ctx.topology, &message.workspace_id) {
        Err(outcome) => *outcome,
        Ok(workspace_id) => {
            let linked = ctx
                .topology
                .snapshot
                .workspaces
                .iter()
                .find(|w| w.workspace_id == workspace_id)
                .and_then(|w| w.worktree.as_ref())
                .is_some_and(|w| w.is_linked_worktree);
            if !linked {
                Outcome::failed("", "Workspace is not a removable linked worktree")
            } else {
                match ctx
                    .client
                    .call_with_timeout(
                        "worktree.remove",
                        &WorktreeRemoveParams {
                            workspace_id: &workspace_id,
                            force: message.force,
                        },
                        Some(WORKTREE_DEADLINE),
                    )
                    .await
                {
                    Err(err) => topology_failure("worktree_remove", &err),
                    Ok(value) => {
                        ctx.handle.refresh().await;
                        Outcome::completed("", Some(strip_type(value)))
                    }
                }
            }
        }
    };
    outcome.frames(request_id, "worktree_remove", action_id)
}

/// `validWorktreeValues` — every supplied value stays within the rune cap.
fn validate_worktree_values(values: [&str; 4]) -> Result<(), Box<Outcome>> {
    for value in values {
        if value.chars().count() > VALUE_MAX_RUNES {
            return Err(Box::new(Outcome::failed("", "Worktree value is too long")));
        }
    }
    Ok(())
}

/// Drop the result envelope's `type` tag — the oracle's result structs
/// never carried one.
fn strip_type(mut value: serde_json::Value) -> serde_json::Value {
    if let Some(object) = value.as_object_mut() {
        object.remove("type");
    }
    value
}
