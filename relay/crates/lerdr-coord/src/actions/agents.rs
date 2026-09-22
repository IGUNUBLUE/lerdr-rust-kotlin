//! Agent lifecycle — `handleAgentStart`/`handleClear` plus
//! `lifecycle.go`'s `ValidateStart`/`Start` workflow.
//!
//! The oracle pipeline, preserved:
//!
//! 1. `ValidateStart` — profile exists, `name` matches
//!    `^[a-z][a-z0-9_-]{0,31}$`, prompt within the rune cap, `cwd` resolves
//!    inside the home jail ([`workspace::resolve_cwd`]).
//! 2. `reconcileExisting` — an agent.list pane already matching
//!    name+resolved cwd+workspace+profile is reused instead of spawning.
//! 3. Target selection — the requested workspace (must exist) or
//!    `SelectWorkspaceForCwd`'s labelled/exclusive/majority heuristic.
//! 4. `createTarget` — `tab.create` into an existing workspace, or
//!    `workspace.create` + `tab.rename` for a new one.
//! 5. `startInTarget` — `agent.start` with the transient-refusal retry
//!    (`agent_pane_busy` means the shell has not reached a prompt yet), or
//!    the argv path (`pane.send_input` of the shell-joined command + Enter,
//!    then `agent.get` polls until detection lands).
//! 6. Optional initial prompt through [`input::prompt_inner`] — a failed
//!    prompt after a confirmed start is `completed_with_warning`, never a
//!    failure that hides the new pane.
//!
//! `agent_clear` and `agent_restart` share the oracle's single handler: a
//! fresh same-profile pane replaces the old one, which is then closed — a
//! close failure downgrades to `completed_with_warning` because the
//! replacement already exists.

use std::path::Path;
use std::time::Duration;

use lerdr_core::protocol::Inbound;
use lerdr_herdr::{AgentInfo, HerdrError, WorkspaceInfo};
use serde::Serialize;
use tokio::time::Instant;

use super::{
    created_target, dispatch_failure, input, profiles::Profile, record_activity, workspace,
    ActionContext, Outcome, AGENT_START_DEADLINE, PROMPT_MAX_CHARS,
};

/// `agentStartProcessTimeoutMS` — per-attempt `--timeout` ceiling.
const START_PROCESS_TIMEOUT: Duration = Duration::from_secs(30);
/// `agentStartResponseReserve` — the tail kept between the last Herdr call
/// and the request deadline.
const RESPONSE_RESERVE: Duration = Duration::from_secs(5);
/// `customAgentPollInterval`.
const CUSTOM_AGENT_POLL: Duration = Duration::from_millis(250);
/// `agentStartRetryInitial` / `agentStartRetryMax`.
const RETRY_INITIAL: Duration = Duration::from_millis(50);
const RETRY_MAX: Duration = Duration::from_millis(1500);

#[derive(Serialize)]
struct TabCreateParams<'a> {
    workspace_id: &'a str,
    cwd: &'a str,
    label: &'a str,
    focus: bool,
}

#[derive(Serialize)]
struct WorkspaceCreateParams<'a> {
    cwd: &'a str,
    label: &'a str,
    focus: bool,
}

#[derive(Serialize)]
struct TabRenameParams<'a> {
    tab_id: &'a str,
    label: &'a str,
}

#[derive(Serialize)]
struct AgentStartParams<'a> {
    name: &'a str,
    kind: &'a str,
    pane_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout_ms: Option<u64>,
}

#[derive(Serialize)]
struct AgentRenameParams<'a> {
    target: &'a str,
    name: &'a str,
}

#[derive(Serialize)]
struct PaneRunInput<'a> {
    pane_id: &'a str,
    text: &'a str,
    keys: [&'a str; 1],
}

#[derive(Serialize)]
struct PaneCloseParams<'a> {
    pane_id: &'a str,
}

/// `handleAgentStart`.
pub(crate) async fn agent_start(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<lerdr_core::protocol::Outbound> {
    let outcome = start_inner(&ctx, request_id, message).await;
    outcome.frames(request_id, "agent_start", action_id)
}

async fn start_inner(ctx: &ActionContext, request_id: &str, message: &Inbound) -> Outcome {
    // The oracle checks the raw values — a whitespace name fails the name
    // pattern, not the required check.
    let profile_id = message.profile_id.as_str();
    let name = message.name.as_str();
    if profile_id.is_empty() || name.is_empty() || message.cwd.is_empty() {
        return Outcome::failed("", "Profile, name, and working directory are required");
    }
    let (profile, cwd) =
        match validate_start(ctx, profile_id, name, &message.cwd, &message.prompt).await {
            Err(outcome) => return *outcome,
            Ok(pair) => pair,
        };
    match lifecycle_start(
        ctx,
        &profile,
        name,
        &cwd,
        &message.workspace_id,
        Instant::now() + AGENT_START_DEADLINE,
    )
    .await
    {
        Err(err) => {
            // A target that survived the failure stays open: publish the
            // topology so the empty pane appears on the phone and a retry
            // can start into it (the oracle's `MarkTopologyChanged`+`wake`).
            if !err.pane_id.is_empty() {
                ctx.handle.refresh().await;
            }
            dispatch_failure(err.pane_id.as_str(), &err.err.into())
        }
        Ok(started) => {
            // `handleAgentStart`'s post-start prompt: a confirmed start with
            // an unconfirmed prompt degrades to completed_with_warning.
            let mut outcome = Outcome::completed(&started.pane_id, Some(started.data()));
            if !message.prompt.is_empty() && !started.pane_id.is_empty() {
                let prompt = input::prompt_inner(
                    ctx,
                    &started.pane_id,
                    &message.prompt,
                    &format!("{request_id}-initial"),
                )
                .await;
                if !prompt.ok {
                    outcome = Outcome::completed_with_warning(
                        &started.pane_id,
                        serde_json::json!({
                            "pane_id": started.pane_id,
                            "name": name,
                            "cwd": cwd,
                            "warning": "Agent started, but the initial prompt was not confirmed",
                        }),
                    );
                }
            }
            ctx.handle.refresh().await;
            // The oracle records the start even when the initial prompt
            // degraded the result to completed_with_warning.
            record_activity(
                ctx,
                "agent_start",
                "started",
                format!("Started {name}"),
                &started.pane_id,
                request_id,
            );
            outcome
        }
    }
}

/// `handleClear` — shared by `agent_clear` and `agent_restart`: resolve the
/// pane's profile, start a same-cwd replacement, then close the old pane.
pub(crate) async fn agent_clear(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
    action: &'static str,
) -> Vec<lerdr_core::protocol::Outbound> {
    let outcome = clear_inner(&ctx, request_id, message).await;
    outcome.frames(request_id, action, action_id)
}

async fn clear_inner(ctx: &ActionContext, request_id: &str, message: &Inbound) -> Outcome {
    let pane_id = message.pane_id.as_str();
    if pane_id.is_empty() {
        return Outcome::failed(pane_id, "Agent is required");
    }
    let Some(agent) = ctx.topology.pane_of(pane_id).cloned() else {
        return Outcome::failed(pane_id, "Agent is no longer available");
    };
    let profile_id = ctx
        .profiles
        .resolve_pane(
            &ctx.client,
            pane_id,
            agent.agent.as_deref().unwrap_or_default(),
        )
        .await;
    let Some(profile) = ctx.profiles.profile(&ctx.client, &profile_id).await else {
        return Outcome::failed(
            pane_id,
            "This agent does not match an available launch profile",
        );
    };
    // `"clear-" + hex(unix_nanos)[..8]` — the oracle's replacement name.
    let nanos_hex = format!(
        "{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    );
    let name = format!("clear-{}", &nanos_hex[..nanos_hex.len().min(8)]);
    let cwd = agent.cwd.as_deref().unwrap_or_default().to_owned();
    // `ValidateStart` — the replacement's cwd goes through the same jail.
    let cwd = match workspace::resolve_cwd(&cwd) {
        Err(outcome) => return *outcome,
        Ok(resolved) => resolved,
    };
    let deadline = Instant::now() + AGENT_START_DEADLINE;
    let replacement = match lifecycle_start(ctx, &profile, &name, &cwd, "", deadline).await {
        Err(err) => return dispatch_failure(pane_id, &err.err.into()),
        Ok(started) => started,
    };
    ctx.profiles.forget(pane_id);
    let mut data = serde_json::json!({
        "pane_id": replacement.pane_id,
        "name": replacement.name,
        "cwd": replacement.cwd,
    });
    let outcome = match ctx
        .client
        .call_with_timeout(
            "pane.close",
            &PaneCloseParams { pane_id },
            Some(remaining(deadline).max(Duration::from_secs(1))),
        )
        .await
    {
        Ok(_) => Outcome::completed(pane_id, Some(data)),
        Err(_) => {
            // The replacement exists and stays — the oracle surfaces the
            // stranded old pane as a warning, not a failure.
            data["warning"] =
                serde_json::json!("Replacement started, but the old pane could not be closed");
            Outcome::completed_with_warning(pane_id, data)
        }
    };
    // `MarkTopologyChanged` + `wake` on every OK result — the warning path
    // publishes too so the phone sees both panes.
    ctx.handle.refresh().await;
    if outcome.ok {
        // `agent_restart` flows through `handleClear` in the oracle and
        // records the `agent_clear` kind — the same quirk applies here.
        record_activity(
            ctx,
            "agent_clear",
            "cleared",
            "Cleared agent",
            pane_id,
            request_id,
        );
    }
    outcome
}

/// `requestAgentRefresh` — no command result: the keepalive semantics are
/// "push the committed topology now and re-read". The frames come from the
/// admission-time snapshot; `handle.refresh()` queues the authoritative
/// re-read that the forwarder publishes on arrival.
pub(crate) async fn refresh_agents(ctx: ActionContext) -> Vec<lerdr_core::protocol::Outbound> {
    ctx.handle.refresh().await;
    crate::snapshot::topology_broadcast(&ctx.topology)
}

// ── lifecycle.go ─────────────────────────────────────────────────────────

/// `ValidateStart` — profile lookup, name pattern, prompt cap, cwd jail.
async fn validate_start(
    ctx: &ActionContext,
    profile_id: &str,
    name: &str,
    cwd: &str,
    prompt: &str,
) -> Result<(Profile, String), Box<Outcome>> {
    let Some(profile) = ctx.profiles.profile(&ctx.client, profile_id).await else {
        return Err(Box::new(Outcome::failed("", "profile_id is not available")));
    };
    if !valid_agent_name(name) {
        return Err(Box::new(Outcome::failed(
            "",
            "name must match [a-z][a-z0-9_-]{0,31}",
        )));
    }
    if prompt.chars().count() > PROMPT_MAX_CHARS {
        return Err(Box::new(Outcome::failed(
            "",
            "prompt exceeds maximum length",
        )));
    }
    let cwd = workspace::resolve_cwd(cwd)?;
    Ok((profile, cwd))
}

/// `agentNamePattern`.
fn valid_agent_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// The pane a finished (or failed-after-target) start reports.
struct Started {
    pane_id: String,
    name: String,
    cwd: String,
    workspace_id: String,
}

impl Started {
    /// `StartResult`'s wire shape — `workspace_id` omitted when empty.
    fn data(&self) -> serde_json::Value {
        let mut data = serde_json::json!({
            "pane_id": self.pane_id,
            "name": self.name,
            "cwd": self.cwd,
        });
        if !self.workspace_id.is_empty() {
            data["workspace_id"] = serde_json::json!(self.workspace_id);
        }
        data
    }
}

/// `lifecycle.Start`'s failure carries the surviving target pane — the
/// phone needs it to render the empty pane instead of losing it.
struct StartError {
    pane_id: String,
    err: StartErrorKind,
}

enum StartErrorKind {
    /// Nothing reached Herdr / a confirmed refusal — safe classification.
    Herdr(HerdrError),
    /// The create reported no root pane — `ErrCreatedTargetUnknown`, which
    /// the oracle always couples with `ErrDispatchedUnknown`.
    CreatedTargetUnknown,
}

impl From<StartErrorKind> for HerdrError {
    fn from(kind: StartErrorKind) -> HerdrError {
        match kind {
            StartErrorKind::Herdr(err) => err,
            StartErrorKind::CreatedTargetUnknown => {
                HerdrError::dispatched_msg("created target response did not identify the root pane")
            }
        }
    }
}

/// `Lifecycle.Start` — reconcile → select workspace → create target →
/// start → remember. `deadline` is the request's absolute end; the last
/// [`RESPONSE_RESERVE`] seconds belong to the response path.
async fn lifecycle_start(
    ctx: &ActionContext,
    profile: &Profile,
    name: &str,
    cwd: &str,
    workspace_id: &str,
    deadline: Instant,
) -> Result<Started, StartError> {
    if let Some(existing) = reconcile_existing(ctx, &profile.id, name, cwd, workspace_id).await {
        ctx.profiles.remember(&existing, &profile.id);
        return Ok(Started {
            pane_id: existing,
            name: name.to_owned(),
            cwd: cwd.to_owned(),
            workspace_id: workspace_id.to_owned(),
        });
    }

    let startup_deadline = deadline.checked_sub(RESPONSE_RESERVE).unwrap_or(deadline);
    if Instant::now() >= startup_deadline {
        return Err(StartError {
            pane_id: String::new(),
            err: StartErrorKind::Herdr(HerdrError::not_started_msg(
                "agent start deadline already passed",
            )),
        });
    }

    let inventory = ctx
        .client
        .agent_list()
        .await
        .map_err(|err| start_err("", err))?;
    let workspaces = ctx
        .client
        .workspace_list()
        .await
        .map_err(|err| start_err("", err))?;
    let mut workspace_id = workspace_id.to_owned();
    if !workspace_id.is_empty() && !workspaces.iter().any(|w| w.workspace_id == workspace_id) {
        return Err(StartError {
            pane_id: String::new(),
            err: StartErrorKind::Herdr(HerdrError::refused(
                "invalid_request",
                "workspace is unavailable",
            )),
        });
    }
    if workspace_id.is_empty() {
        workspace_id = select_workspace_for_cwd(cwd, &inventory, &workspaces);
    }

    let target = create_target(ctx, &workspace_id, name, cwd, startup_deadline).await?;
    let start_err_result =
        start_in_target(ctx, profile, name, &target.pane_id, startup_deadline).await;
    if let Err(err) = start_err_result {
        // The target stays open — Herdr created it; closing it would
        // destroy the workspace the user asked for, and a retry can start
        // into the same pane.
        return Err(StartError {
            pane_id: target.pane_id.clone(),
            err,
        });
    }
    ctx.profiles.remember(&target.pane_id, &profile.id);
    Ok(Started {
        pane_id: target.pane_id,
        name: name.to_owned(),
        cwd: cwd.to_owned(),
        workspace_id: target.workspace_id,
    })
}

/// `reconcileExisting` — an agent pane with this name at the resolved cwd
/// (and the requested workspace when given) whose resolved profile matches.
async fn reconcile_existing(
    ctx: &ActionContext,
    profile_id: &str,
    name: &str,
    cwd: &str,
    workspace_id: &str,
) -> Option<String> {
    let inventory = ctx.client.agent_list().await.ok()?;
    for pane in &inventory {
        if pane.name.as_deref() != Some(name) {
            continue;
        }
        let pane_cwd = pane.cwd.as_deref().unwrap_or_default();
        let Ok(resolved) = std::fs::canonicalize(pane_cwd) else {
            continue;
        };
        if resolved != Path::new(cwd) {
            continue;
        }
        if !workspace_id.is_empty() && pane.workspace_id != workspace_id {
            continue;
        }
        if ctx
            .profiles
            .resolve_pane(
                &ctx.client,
                &pane.pane_id,
                pane.agent.as_deref().unwrap_or_default(),
            )
            .await
            == profile_id
        {
            return Some(pane.pane_id.clone());
        }
    }
    None
}

/// `createTarget` — a `tab.create` inside the selected workspace, or a fresh
/// `workspace.create` (label = `basename(cwd)`, then `tab.rename` to the
/// agent name; a failed rename closes the created pane).
async fn create_target(
    ctx: &ActionContext,
    workspace_id: &str,
    label: &str,
    cwd: &str,
    deadline: Instant,
) -> Result<super::CreatedTarget, StartError> {
    if !workspace_id.is_empty() {
        let value = ctx
            .client
            .call_with_timeout(
                "tab.create",
                &TabCreateParams {
                    workspace_id,
                    cwd,
                    label,
                    focus: false,
                },
                Some(remaining(deadline)),
            )
            .await
            .map_err(|err| start_err("", err))?;
        let created = created_target(&value);
        if created.pane_id.is_empty() {
            return Err(StartError {
                pane_id: String::new(),
                err: StartErrorKind::CreatedTargetUnknown,
            });
        }
        return Ok(created);
    }

    let mut workspace_label = Path::new(cwd)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if workspace_label.is_empty() || workspace_label == "." || workspace_label == "/" {
        workspace_label = "workspace".to_owned();
    }
    let value = ctx
        .client
        .call_with_timeout(
            "workspace.create",
            &WorkspaceCreateParams {
                cwd,
                label: &workspace_label,
                focus: false,
            },
            Some(remaining(deadline)),
        )
        .await
        .map_err(|err| start_err("", err))?;
    let created = created_target(&value);
    if created.pane_id.is_empty() {
        return Err(StartError {
            pane_id: String::new(),
            err: StartErrorKind::CreatedTargetUnknown,
        });
    }
    if created.tab_id.is_empty() {
        return Ok(created);
    }
    if let Err(err) = ctx
        .client
        .call_with_timeout(
            "tab.rename",
            &TabRenameParams {
                tab_id: &created.tab_id,
                label,
            },
            Some(remaining(deadline)),
        )
        .await
    {
        let _ = ctx
            .client
            .call_with_timeout(
                "pane.close",
                &PaneCloseParams {
                    pane_id: &created.pane_id,
                },
                Some(remaining(deadline).max(Duration::from_secs(1))),
            )
            .await;
        return Err(start_err("", err));
    }
    Ok(created)
}

/// `startInTarget` — `agent.start` for kind profiles; the argv path runs the
/// profile's command through pane input (`pane run` on the socket is
/// `pane.send_input` of the shell-joined command + Enter) and polls
/// `agent.get` until Herdr detects it.
async fn start_in_target(
    ctx: &ActionContext,
    profile: &Profile,
    name: &str,
    pane_id: &str,
    deadline: Instant,
) -> Result<(), StartErrorKind> {
    if !profile.kind.is_empty() {
        return start_kind_agent(ctx, &profile.kind, name, pane_id, deadline).await;
    }
    if profile.argv.is_empty() {
        return Err(StartErrorKind::Herdr(HerdrError::refused(
            "invalid_request",
            "profile has no executable argv",
        )));
    }
    let command = shell_join(&profile.argv);
    ctx.client
        .call_with_timeout(
            "pane.send_input",
            &PaneRunInput {
                pane_id,
                text: &command,
                keys: ["Enter"],
            },
            Some(remaining(deadline)),
        )
        .await
        .map_err(StartErrorKind::Herdr)?;
    loop {
        match ctx
            .client
            .call_with_timeout(
                "agent.get",
                &serde_json::json!({ "target": pane_id }),
                Some(remaining(deadline)),
            )
            .await
        {
            Ok(value) => {
                let detected = value
                    .pointer("/agent/agent")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|a| !a.is_empty())
                    || value
                        .pointer("/agent/agent_status")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|s| !s.is_empty() && s != "unknown")
                    || value
                        .pointer("/agent/interactive_ready")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                if detected {
                    return ctx
                        .client
                        .call_with_timeout(
                            "agent.rename",
                            &AgentRenameParams {
                                target: pane_id,
                                name,
                            },
                            Some(remaining(deadline)),
                        )
                        .await
                        .map(|_| ())
                        .map_err(StartErrorKind::Herdr);
                }
            }
            Err(err) if err.phase() == lerdr_herdr::DispatchPhase::Refused => {
                return Err(StartErrorKind::Herdr(err));
            }
            Err(_) => {}
        }
        if Instant::now() >= deadline {
            // `pane.send_input` succeeded — the command was dispatched even
            // though its eventual agent state is unknown.
            return Err(StartErrorKind::Herdr(HerdrError::dispatched_msg(
                "wait for custom agent timed out",
            )));
        }
        tokio::time::sleep(CUSTOM_AGENT_POLL).await;
    }
}

/// `startKindAgent` — retry while Herdr's `agent_pane_busy` (the fresh pane
/// has not reached a prompt) refuses the start; the refusal proves nothing
/// ran, so retrying is safe.
async fn start_kind_agent(
    ctx: &ActionContext,
    kind: &str,
    name: &str,
    pane_id: &str,
    deadline: Instant,
) -> Result<(), StartErrorKind> {
    let mut delay = RETRY_INITIAL;
    loop {
        let timeout_ms = remaining(deadline)
            .min(START_PROCESS_TIMEOUT)
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);
        let result = ctx
            .client
            .call_with_timeout(
                "agent.start",
                &AgentStartParams {
                    name,
                    kind,
                    pane_id,
                    timeout_ms: Some(timeout_ms.max(1)),
                },
                Some(remaining(deadline)),
            )
            .await;
        let Err(err) = result else { return Ok(()) };
        if !err.is_transient_refusal() {
            return Err(StartErrorKind::Herdr(err));
        }
        if Instant::now() >= deadline {
            // The refusal, not the elapsed deadline — it keeps the
            // safe-to-retry classification.
            return Err(StartErrorKind::Herdr(err));
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(RETRY_MAX);
    }
}

/// `remainingTimeoutMS` — the caller's budget, capped per call.
fn remaining(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

fn start_err(pane_id: &str, err: HerdrError) -> StartError {
    StartError {
        pane_id: pane_id.to_owned(),
        err: StartErrorKind::Herdr(err),
    }
}

/// `ShellJoin` — single-quote escaping identical to the oracle's.
fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|value| shell_quote(value))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_owned();
    }
    const SAFE: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_@%+=:,./-";
    if value.chars().all(|c| SAFE.contains(c)) {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

/// `SelectWorkspaceForCwd` — label/exclusive/majority heuristic; ambiguous
/// candidates deliberately return no match.
fn select_workspace_for_cwd(
    cwd: &str,
    panes: &[AgentInfo],
    workspaces: &[WorkspaceInfo],
) -> String {
    let Ok(target) = std::fs::canonicalize(cwd) else {
        return String::new();
    };
    // matching/total per workspace.
    let mut counts: std::collections::HashMap<&str, (usize, usize)> =
        std::collections::HashMap::new();
    for pane in panes {
        if pane.workspace_id.is_empty() {
            continue;
        }
        let entry = counts.entry(pane.workspace_id.as_str()).or_default();
        entry.1 += 1;
        if let Ok(pane_cwd) = std::fs::canonicalize(pane.cwd.as_deref().unwrap_or_default()) {
            if pane_cwd == target {
                entry.0 += 1;
            }
        }
    }
    let candidates: Vec<&str> = counts
        .iter()
        .filter(|(_, (matching, _))| *matching > 0)
        .map(|(id, _)| *id)
        .collect();
    if candidates.is_empty() {
        return String::new();
    }

    let mut labels = std::collections::HashSet::from([target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()]);
    if let Some(home) = workspace::home_dir().and_then(|h| std::fs::canonicalize(h).ok()) {
        if target == home {
            labels.insert("~".to_owned());
        }
    }
    let labelled: Vec<&str> = workspaces
        .iter()
        .filter(|w| candidates.contains(&w.workspace_id.as_str()) && labels.contains(&w.label))
        .map(|w| w.workspace_id.as_str())
        .collect();
    if labelled.len() == 1 {
        return labelled[0].to_owned();
    }
    let exclusive: Vec<&str> = candidates
        .iter()
        .copied()
        .filter(|id| {
            let (matching, total) = counts[id];
            matching == total
        })
        .collect();
    if exclusive.len() == 1 {
        return exclusive[0].to_owned();
    }
    let majority: Vec<&str> = candidates
        .iter()
        .copied()
        .filter(|id| {
            let (matching, total) = counts[id];
            matching * 2 > total
        })
        .collect();
    if majority.len() == 1 {
        return majority[0].to_owned();
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_name_pattern() {
        assert!(valid_agent_name("claude-1"));
        assert!(valid_agent_name("a"));
        assert!(valid_agent_name("clear-1a2b3c4d"));
        assert!(!valid_agent_name("1abc"));
        assert!(!valid_agent_name("Upper"));
        assert!(!valid_agent_name(""));
        assert!(!valid_agent_name(&"a".repeat(33)));
        assert!(valid_agent_name(&"a".repeat(32)));
    }

    #[test]
    fn shell_join_quotes_like_the_oracle() {
        assert_eq!(shell_join(&["a".into(), "b".into()]), "a b");
        assert_eq!(shell_join(&[]), "");
        assert_eq!(shell_join(&["".into()]), "''");
        assert_eq!(shell_join(&["it's".into()]), "'it'\"'\"'s'");
        assert_eq!(shell_join(&["a b".into()]), "'a b'");
        // `~` is outside the oracle's safe set → single-quoted like the
        // shell requires.
        assert_eq!(shell_join(&["~/x".into()]), "'~/x'");
    }
}
