//! Workspace actions — `workspace.go`'s `HandleWorkspace*` port.
//!
//! Method mapping (oracle CLI → socket): `workspace create` →
//! `workspace.create`, `workspace rename` → `workspace.rename`,
//! `WorkspaceMove` → `workspace.move`, `WorkspaceMoveBlock` →
//! `workspace.move_block`, `WorkspaceClose` → `workspace.close`,
//! `WorkspaceList` → `workspace.list`. The socket result JSON is passed
//! through into `command_result.data` where the oracle surfaces created
//! ids; pre-dispatch group-consent refusals keep their structured codes.

use std::collections::BTreeSet;
use std::path::{Component, PathBuf};

use lerdr_core::protocol::Inbound;
use lerdr_herdr::WorkspaceInfo;
use serde::Serialize;

use super::{
    record_activity, topology_failure, ActionContext, Outcome, MAX_INSERT_INDEX, WORKSPACE_DEADLINE,
};
use crate::topology::Topology;

/// `workspaceLabelMaxRunes`.
const LABEL_MAX_RUNES: usize = 128;
/// `maxWorkspaceGroupIDs`.
const MAX_GROUP_IDS: usize = 256;
/// `maxWorkspaceIDRunes`.
const MAX_WORKSPACE_ID_RUNES: usize = 256;

#[derive(Serialize)]
struct WorkspaceCreateParams {
    cwd: String,
    label: String,
    focus: bool,
}

#[derive(Serialize)]
struct WorkspaceRenameParams<'a> {
    workspace_id: &'a str,
    label: &'a str,
}

#[derive(Serialize)]
struct WorkspaceMoveParams<'a> {
    workspace_id: &'a str,
    insert_index: i64,
}

#[derive(Serialize)]
struct WorkspaceMoveBlockParams<'a> {
    workspace_ids: &'a [String],
    before_workspace_id: Option<&'a str>,
}

#[derive(Serialize)]
struct WorkspaceCloseParams<'a> {
    workspace_id: &'a str,
    close_group: bool,
}

#[derive(Serialize)]
struct EmptyParams {}

/// `HandleWorkspaceCreate` — label first, then the home-jailed cwd
/// (`Lifecycle.ResolveCwd`), then `workspace.create{cwd,label,focus:false}`.
/// The oracle passes `--no-focus` — a phone-initiated create must not steal
/// the desktop's focus.
pub(crate) async fn workspace_create(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<lerdr_core::protocol::Outbound> {
    let label = message.label.trim();
    let outcome = match validate_label(label) {
        Err(outcome) => *outcome,
        Ok(()) => match resolve_cwd(&message.cwd) {
            Err(outcome) => *outcome,
            Ok(resolved) => {
                let result = ctx
                    .client
                    .call_with_timeout(
                        "workspace.create",
                        &WorkspaceCreateParams {
                            cwd: resolved.clone(),
                            label: label.to_owned(),
                            focus: false,
                        },
                        Some(WORKSPACE_DEADLINE),
                    )
                    .await;
                match result {
                    Err(err) => topology_failure("workspace_create", &err),
                    Ok(value) => {
                        // `ErrCreatedTargetUnknown` — the create reported no
                        // root pane; the workspace may exist anyway →
                        // dispatched_unknown.
                        let created = super::created_target(&value);
                        if created.pane_id.is_empty() {
                            created_target_unknown()
                        } else {
                            ctx.handle.refresh().await;
                            Outcome::completed(
                                "",
                                Some(serde_json::json!({
                                    "workspace_id": created.workspace_id,
                                    "pane_id": created.pane_id,
                                    "tab_id": created.tab_id,
                                    "cwd": resolved,
                                    "label": label,
                                })),
                            )
                        }
                    }
                }
            }
        },
    };
    if outcome.ok {
        record_activity(
            &ctx,
            "workspace_create",
            "created",
            format!("Created workspace {label}"),
            "",
            request_id,
        );
    }
    outcome.frames(request_id, "workspace_create", action_id)
}

/// `HandleWorkspaceRename` — `workspace.rename{workspace_id, label}`.
pub(crate) async fn workspace_rename(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<lerdr_core::protocol::Outbound> {
    let outcome = match workspace_target(&ctx.topology, &message.workspace_id) {
        Err(outcome) => *outcome,
        Ok(workspace_id) => {
            let label = message.label.trim();
            match validate_label(label) {
                Err(outcome) => *outcome,
                Ok(()) => {
                    match ctx
                        .client
                        .call_with_timeout(
                            "workspace.rename",
                            &WorkspaceRenameParams {
                                workspace_id: &workspace_id,
                                label,
                            },
                            Some(WORKSPACE_DEADLINE),
                        )
                        .await
                    {
                        Err(err) => topology_failure("workspace_rename", &err),
                        Ok(_) => {
                            ctx.handle.refresh().await;
                            Outcome::completed(
                                "",
                                Some(serde_json::json!({
                                    "workspace_id": workspace_id,
                                    "label": label,
                                })),
                            )
                        }
                    }
                }
            }
        }
    };
    if outcome.ok {
        let label = message.label.trim();
        record_activity(
            &ctx,
            "workspace_rename",
            "renamed",
            format!("Renamed workspace to {label}"),
            "",
            request_id,
        );
    }
    outcome.frames(request_id, "workspace_rename", action_id)
}

/// `HandleWorkspaceReorder` / `HandleWorkspaceReorderBlock` — the block form
/// wins whenever `workspace_ids` is present, matching server.go's dispatch.
pub(crate) async fn workspace_reorder(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<lerdr_core::protocol::Outbound> {
    let block = !message.workspace_ids.is_empty();
    let outcome = if block {
        reorder_block(&ctx, message).await
    } else {
        reorder_single(&ctx, message).await
    };
    if outcome.ok {
        let summary = if block {
            "Reordered workspace group"
        } else {
            "Reordered workspace"
        };
        record_activity(
            &ctx,
            "workspace_reorder",
            "reordered",
            summary,
            "",
            request_id,
        );
    }
    outcome.frames(request_id, "workspace_reorder", action_id)
}

async fn reorder_single(ctx: &ActionContext, message: &Inbound) -> Outcome {
    let workspace_id = match workspace_target(&ctx.topology, &message.workspace_id) {
        Err(outcome) => return *outcome,
        Ok(id) => id,
    };
    let insert_index = match message.insert_index {
        Some(index) if (0..=MAX_INSERT_INDEX).contains(&index) => index,
        _ => return Outcome::failed("", "Workspace position is invalid"),
    };
    match ctx
        .client
        .call_with_timeout(
            "workspace.move",
            &WorkspaceMoveParams {
                workspace_id: &workspace_id,
                insert_index,
            },
            Some(WORKSPACE_DEADLINE),
        )
        .await
    {
        Err(err) => topology_failure("workspace_reorder", &err),
        Ok(_) => {
            ctx.handle.refresh().await;
            Outcome::completed(
                "",
                Some(serde_json::json!({
                    "workspace_id": workspace_id,
                    "insert_index": insert_index,
                })),
            )
        }
    }
}

async fn reorder_block(ctx: &ActionContext, message: &Inbound) -> Outcome {
    let ids = &message.workspace_ids;
    if ids.is_empty() || ids.len() as i64 > MAX_INSERT_INDEX {
        return Outcome::failed("", "Workspace selection is invalid");
    }
    let mut seen = BTreeSet::new();
    for id in ids {
        if id.is_empty() || !seen.insert(id) {
            return Outcome::failed("", "Workspace selection is invalid");
        }
        if !workspace_exists(&ctx.topology, id) {
            return Outcome::failed("", "Workspace is unavailable");
        }
    }
    let before = message.before_workspace_id.as_str();
    if !before.is_empty() {
        if seen.iter().any(|id| id.as_str() == before) {
            return Outcome::failed("", "Workspace destination is invalid");
        }
        if !workspace_exists(&ctx.topology, before) {
            return Outcome::failed("", "Workspace destination is unavailable");
        }
    }
    match ctx
        .client
        .call_with_timeout(
            "workspace.move_block",
            &WorkspaceMoveBlockParams {
                workspace_ids: ids,
                before_workspace_id: (!before.is_empty()).then_some(before),
            },
            Some(WORKSPACE_DEADLINE),
        )
        .await
    {
        Err(err) => topology_failure("workspace_reorder", &err),
        Ok(_) => {
            ctx.handle.refresh().await;
            Outcome::completed(
                "",
                Some(serde_json::json!({
                    "workspace_ids": ids,
                    "before_workspace_id": before,
                })),
            )
        }
    }
}

/// `HandleWorkspaceClose` — the workspace group's consent checks run against
/// an authoritative `workspace.list`, not the projected topology: membership
/// that changed since the snapshot must refuse rather than close the wrong
/// set.
pub(crate) async fn workspace_close(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<lerdr_core::protocol::Outbound> {
    let workspace_id = message.workspace_id.trim();
    if workspace_id.is_empty() {
        return Outcome::failed("", "Workspace is required").frames(
            request_id,
            "workspace_close",
            action_id,
        );
    }
    let close_group = message.close_group;
    let outcome =
        match ctx
            .client
            .call_with_timeout("workspace.list", &EmptyParams {}, Some(WORKSPACE_DEADLINE))
            .await
        {
            Err(_) => close_refusal("workspace_group_validation_unavailable", None),
            Ok(value) => {
                let workspaces: Vec<WorkspaceInfo> =
                    serde_json::from_value(value.get("workspaces").cloned().unwrap_or_default())
                        .unwrap_or_default();
                let Some(current) = workspaces.iter().find(|w| w.workspace_id == workspace_id)
                else {
                    return if close_group {
                        close_refusal("workspace_group_changed", None)
                    } else {
                        Outcome::failed("", "Workspace is unavailable")
                    }
                    .frames(request_id, "workspace_close", action_id);
                };
                let (group_ids, primary_id) = workspace_group_ids(&workspaces, current);
                if !valid_workspace_group_ids(&group_ids, &primary_id) {
                    return close_refusal("workspace_group_validation_unavailable", None).frames(
                        request_id,
                        "workspace_close",
                        action_id,
                    );
                }
                if !close_group
                    && current
                        .worktree
                        .as_ref()
                        .is_some_and(|w| !w.is_linked_worktree)
                    && group_ids.len() > 1
                {
                    return close_refusal("workspace_group_close_required", Some(&group_ids))
                        .frames(request_id, "workspace_close", action_id);
                }
                if close_group {
                    if current
                        .worktree
                        .as_ref()
                        .is_some_and(|w| w.is_linked_worktree)
                    {
                        return close_refusal("workspace_group_primary_required", Some(&group_ids))
                            .frames(request_id, "workspace_close", action_id);
                    }
                    if !valid_workspace_group_ids(&message.expected_workspace_ids, &primary_id) {
                        return close_refusal("workspace_group_consent_invalid", Some(&group_ids))
                            .frames(request_id, "workspace_close", action_id);
                    }
                    if !same_workspace_id_set(&message.expected_workspace_ids, &group_ids) {
                        return close_refusal("workspace_group_changed", Some(&group_ids)).frames(
                            request_id,
                            "workspace_close",
                            action_id,
                        );
                    }
                }
                let affected: Vec<String> = if close_group {
                    group_ids
                } else {
                    vec![current.workspace_id.clone()]
                };
                match ctx
                    .client
                    .call_with_timeout(
                        "workspace.close",
                        &WorkspaceCloseParams {
                            workspace_id: &current.workspace_id,
                            close_group,
                        },
                        Some(WORKSPACE_DEADLINE),
                    )
                    .await
                {
                    Err(err) => topology_failure("workspace_close", &err),
                    Ok(_) => {
                        ctx.handle.refresh().await;
                        record_activity(
                            &ctx,
                            "workspace_close",
                            "closed",
                            format!("Closed workspace {}", current.label),
                            "",
                            request_id,
                        );
                        Outcome::completed(
                            "",
                            Some(serde_json::json!({
                                "workspace_id": current.workspace_id,
                                "close_group": close_group,
                                "workspace_ids": affected,
                            })),
                        )
                    }
                }
            }
        };
    outcome.frames(request_id, "workspace_close", action_id)
}

/// `workspaceCloseRefusal` — `phase:"not_started"`, the refusal's public
/// message, and `{"code": …, "workspace_ids"?: …}` data.
fn close_refusal(code: &str, workspace_ids: Option<&[String]>) -> Outcome {
    let public = if code == "workspace_group_consent_invalid" {
        "Workspace group confirmation is invalid; confirm again"
    } else {
        super::refusal_message(code)
    };
    let mut data = serde_json::json!({ "code": code });
    if let Some(ids) = workspace_ids.filter(|ids| !ids.is_empty()) {
        data["workspace_ids"] = serde_json::json!(ids);
    }
    Outcome::not_started_refusal(code, public, Some(data))
}

/// `ErrCreatedTargetUnknown` — the create reported no root pane.
fn created_target_unknown() -> Outcome {
    Outcome {
        ok: false,
        phase: "dispatched_unknown",
        error: "Herdr may have created an empty target; review Herdr before retrying".to_owned(),
        pane_id: String::new(),
        data: Some(serde_json::json!({ "dispatched_unknown": true })),
        receipt_phase: lerdr_core::protocol::ActionReceiptPhase::DISPATCHED_UNKNOWN,
        receipt_error: Some(super::api_error_plain(
            "dispatch_outcome_unknown",
            "created target response did not identify the root pane",
        )),
    }
}

/// `d.workspaceTarget` — trim, required, and present in the projected
/// topology. Returns the canonical workspace id.
pub(crate) fn workspace_target(topology: &Topology, raw: &str) -> Result<String, Box<Outcome>> {
    let id = raw.trim();
    if id.is_empty() {
        return Err(Box::new(Outcome::failed("", "Workspace is required")));
    }
    if !workspace_exists(topology, id) {
        return Err(Box::new(Outcome::failed("", "Workspace is unavailable")));
    }
    Ok(id.to_owned())
}

fn workspace_exists(topology: &Topology, id: &str) -> bool {
    topology
        .snapshot
        .workspaces
        .iter()
        .any(|w| w.workspace_id == id)
}

fn validate_label(label: &str) -> Result<(), Box<Outcome>> {
    if label.is_empty() {
        return Err(Box::new(Outcome::failed("", "Workspace label is required")));
    }
    if label.chars().count() > LABEL_MAX_RUNES {
        return Err(Box::new(Outcome::failed("", "Workspace label is too long")));
    }
    Ok(())
}

/// `workspaceGroupIDs` — the workspaces sharing `selected`'s repo_key form
/// the group; the primary is the first non-linked member.
fn workspace_group_ids(
    workspaces: &[WorkspaceInfo],
    selected: &WorkspaceInfo,
) -> (Vec<String>, String) {
    let Some(worktree) = selected.worktree.as_ref() else {
        return (
            vec![selected.workspace_id.clone()],
            selected.workspace_id.clone(),
        );
    };
    if worktree.repo_key.is_empty() {
        return (
            vec![selected.workspace_id.clone()],
            selected.workspace_id.clone(),
        );
    }
    let repo_key = worktree.repo_key.as_str();
    let mut primary_id = selected.workspace_id.clone();
    for workspace in workspaces {
        let Some(other) = workspace.worktree.as_ref() else {
            continue;
        };
        if other.repo_key != repo_key {
            continue;
        }
        if !other.is_linked_worktree {
            primary_id = workspace.workspace_id.clone();
            break;
        }
    }
    let group: Vec<String> = workspaces
        .iter()
        .filter(|w| {
            w.worktree
                .as_ref()
                .is_some_and(|other| other.repo_key == repo_key)
        })
        .map(|w| w.workspace_id.clone())
        .collect();
    if group.is_empty() {
        return (vec![selected.workspace_id.clone()], primary_id);
    }
    (group, primary_id)
}

/// `validWorkspaceGroupIDs` — bounded, non-empty, trimmed, unique ids, and
/// the primary must be a member.
fn valid_workspace_group_ids(ids: &[String], primary_id: &str) -> bool {
    if ids.is_empty() || ids.len() > MAX_GROUP_IDS || primary_id.is_empty() {
        return false;
    }
    let mut seen = BTreeSet::new();
    for id in ids {
        if id.chars().count() > MAX_WORKSPACE_ID_RUNES
            || id.is_empty()
            || id.trim() != id.as_str()
            || !seen.insert(id)
        {
            return false;
        }
    }
    seen.iter().any(|id| id.as_str() == primary_id)
}

/// `sameWorkspaceIDSet`.
fn same_workspace_id_set(left: &[String], right: &[String]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let seen: BTreeSet<&String> = left.iter().collect();
    right.iter().all(|id| seen.contains(id))
}

/// `Lifecycle.ResolveCwd` — the home-directory jail for agent/workspace
/// working directories: must be an existing directory strictly below home
/// after symlink resolution.
pub(crate) fn resolve_cwd(raw: &str) -> Result<String, Box<Outcome>> {
    if raw.is_empty() {
        return Err(Box::new(Outcome::failed("", "cwd is required")));
    }
    let Some(home) = home_dir() else {
        return Err(Box::new(Outcome::failed(
            "",
            "home directory is unavailable",
        )));
    };
    let absolute = absolute_path(raw);
    let resolved_home = match std::fs::canonicalize(&home) {
        Ok(path) => path,
        Err(_) => {
            return Err(Box::new(Outcome::failed(
                "",
                "home directory is unavailable",
            )))
        }
    };
    let resolved = match std::fs::canonicalize(&absolute) {
        Ok(path) => path,
        Err(_) => {
            return Err(Box::new(Outcome::failed(
                "",
                "cwd is not an accessible directory inside the home directory",
            )));
        }
    };
    let Ok(relative) = resolved.strip_prefix(&resolved_home) else {
        return Err(Box::new(Outcome::failed(
            "",
            "cwd must be inside the home directory",
        )));
    };
    if relative.as_os_str().is_empty() {
        return Err(Box::new(Outcome::failed(
            "",
            "cwd must be a project directory below the home directory",
        )));
    }
    match std::fs::metadata(&resolved) {
        Ok(info) if info.is_dir() => Ok(resolved.to_string_lossy().into_owned()),
        _ => Err(Box::new(Outcome::failed(
            "",
            "cwd is not an accessible directory",
        ))),
    }
}

pub(crate) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// `filepath.Abs` — relative paths resolve against the process cwd, with
/// `.`/`..` folded lexically (the filesystem canonicalization that follows
/// resolves any remaining symlinks).
fn absolute_path(raw: &str) -> PathBuf {
    let path = PathBuf::from(raw);
    let joined = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };
    let mut cleaned = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                cleaned.pop();
            }
            other => cleaned.push(other.as_os_str()),
        }
    }
    cleaned
}

#[cfg(test)]
mod tests {
    use super::*;
    use lerdr_herdr::WorkspaceWorktreeInfo;

    fn ws(id: &str, repo: Option<(&str, bool)>) -> WorkspaceInfo {
        WorkspaceInfo {
            workspace_id: id.to_owned(),
            worktree: repo.map(|(key, linked)| WorkspaceWorktreeInfo {
                repo_key: key.to_owned(),
                repo_name: "repo".into(),
                repo_root: "/repo".into(),
                checkout_path: "/repo/wt".into(),
                is_linked_worktree: linked,
            }),
            ..WorkspaceInfo::default()
        }
    }

    #[test]
    fn group_ids_no_worktree_is_singleton() {
        let selected = ws("w1", None);
        let (group, primary) = workspace_group_ids(std::slice::from_ref(&selected), &selected);
        assert_eq!(group, vec!["w1"]);
        assert_eq!(primary, "w1");
    }

    #[test]
    fn group_ids_collects_repo_members_with_primary() {
        let list = vec![
            ws("main", Some(("k", false))),
            ws("linked-a", Some(("k", true))),
            ws("linked-b", Some(("k", true))),
            ws("other", Some(("z", false))),
        ];
        let selected = list[1].clone();
        let (group, primary) = workspace_group_ids(&list, &selected);
        assert_eq!(group, vec!["main", "linked-a", "linked-b"]);
        assert_eq!(primary, "main");
    }

    #[test]
    fn group_ids_linked_only_keeps_selected_primary() {
        let list = vec![
            ws("linked-a", Some(("k", true))),
            ws("linked-b", Some(("k", true))),
        ];
        let (group, primary) = workspace_group_ids(&list, &list[0]);
        assert_eq!(group, vec!["linked-a", "linked-b"]);
        assert_eq!(primary, "linked-a");
    }

    #[test]
    fn valid_group_ids_requires_primary_membership() {
        let ids = vec!["a".to_owned(), "b".to_owned()];
        assert!(valid_workspace_group_ids(&ids, "a"));
        assert!(!valid_workspace_group_ids(&ids, "z"));
        assert!(!valid_workspace_group_ids(&[], "a"));
        let dup = vec!["a".to_owned(), "a".to_owned()];
        assert!(!valid_workspace_group_ids(&dup, "a"));
        let untrimmed = vec![" a".to_owned()];
        assert!(!valid_workspace_group_ids(&untrimmed, " a"));
    }

    #[test]
    fn same_set_is_order_independent() {
        let a = vec!["x".to_owned(), "y".to_owned()];
        let b = vec!["y".to_owned(), "x".to_owned()];
        assert!(same_workspace_id_set(&a, &b));
        assert!(!same_workspace_id_set(&a, &["x".to_owned()]));
    }
}
