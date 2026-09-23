//! Attention transition projector — `handleTransition` +
//! `enrichBlockedTransition` + `broadcastBlockedAttention` +
//! `publishAgentPush` + `RecordTransitionActivity` +
//! `handleChatCompletion` + `captureFinishedPane` +
//! `reconcileRecoveredPush` + `syncHistoryPanes`/`scheduleHistoryCapture`/
//! `captureHistoryLoop` (server.go:1467-2185).
//!
//! `Topology::accept` emits [`PaneTransition`]s inside the commit; the
//! actor forwards the whole [`AcceptOutcome`] once the snapshot is
//! published. This task is the drain: each transition becomes an async
//! task (the oracle's `transitionTasks.Start`) that re-reads the *live*
//! topology through the `watch` receiver — every fence
//! (`BlockedTransitionCurrent`, `AttentionTransitionCurrent`,
//! `CompletionCurrent`, `TransitionCurrent`) sees the latest committed
//! state, so a stale transition can never commit, broadcast, or push.
//!
//! `working` retracts every push key on the pane and journals a
//! `working` row unconditionally. `blocked` enriches (up to four
//! classify attempts over `recent_unwrapped` reads), commits the
//! classification through the ledger fence, journals the attention row,
//! publishes the push key, and broadcasts the `blocked` frame — `chat`
//! kinds instead broadcast immediately and run the chat-completion
//! branch. Done/idle completions claim the finished-notification slot,
//! capture the response (conversation transcript first, pane history
//! merge as fallback), journal `finished`, and publish the `finished`
//! push.
//!
//! The per-commit side effects — `customAnswers` cleanup for removed
//! panes, the once-per-process push reconciliation, the history pane-set
//! sync, and capture scheduling — run on every drained outcome, and the
//! 4s capture loop (`captureHistoryLoop`) rides the same drain's select.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lerdr_core::json::MaybeNull;
use lerdr_core::protocol::{AgentState, BlockedMessage, Outbound, TargetRef};
use lerdr_herdr::{ReadFormat, ReadSource};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{warn, Instrument};

use super::attention::{
    classify, latest_completed_response, pane_summary, AttentionKind, Classification,
};
use super::parse::layout_hint;
use super::store::{
    classify_semantics, is_claude_like, kind_str, record_fill_answers, wire_interaction,
    PaneTransition,
};
use crate::actions::activity::{Journal, NewEntry};
use crate::actions::push::{
    PublishRequest, Push, PushEventKey, CATEGORY_ATTENTION, CATEGORY_FINISHED, CATEGORY_QUESTION,
    PREVIEW_BRIEF, PREVIEW_HIDDEN, PREVIEW_QUESTION,
};
use crate::actions::questions::Questions;
use crate::actions::Notices;
use crate::actor::TopologyHandle;
use crate::conversation::reader::normalize_project_context;
use crate::conversation::{ProjectContext, Reader};
use crate::history;
use crate::topology::{AcceptOutcome, Topology};

/// `blockedClassificationAttempts`/`blockedClassificationRetryDelay`
/// (server.go:1786-1789).
const CLASSIFY_ATTEMPTS: usize = 4;
const CLASSIFY_RETRY: Duration = Duration::from_millis(100);
/// The per-attempt read timeout inside `enrichBlockedTransition`
/// (`WithTimeout(readCtx, 3s)`); the same bound `scheduleHistoryCapture`
/// and `captureFinishedPane` put on their `ReadPane` calls.
const READ_TIMEOUT: Duration = Duration::from_secs(3);
/// `history.CaptureInterval` — the per-pane capture throttle and the
/// loop's tick.
const CAPTURE_INTERVAL: Duration = history::CAPTURE_INTERVAL;
/// `context.WithTimeout(parent, 10*time.Second)` — the whole transition
/// task (enrich, capture, activity, push) is deadline-bound.
const TRANSITION_TIMEOUT: Duration = Duration::from_secs(10);
/// The enrich read depth (`s.herdrC.ReadPane(ctx, paneID, 80, "ansi")`).
const ENRICH_LINES: u32 = 80;
/// The finished-event id prefix — `finished-{nanos}-{paneID}`.
fn finished_event_id(pane_id: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("finished-{nanos}-{pane_id}")
}

/// Everything the projector needs — cloned cheap (`Arc` inners or
/// `watch` receivers) into every spawned transition task.
#[derive(Clone)]
pub(crate) struct ProjectorDeps {
    /// Topology + Herdr access — `handle.topology.borrow()` is the live
    /// committed view every fence reads.
    pub handle: TopologyHandle,
    /// `customAnswers` — the enrich path's record/fill and the removed-
    /// pane cleanup.
    pub questions: Questions,
    /// `history.Manager` — merge ledger + capture writes.
    pub history: history::Manager,
    /// `activity.Journal` — fenced `commit`/`publish`/`discard`.
    pub activities: Journal,
    /// `push.Manager` — publish/resolve/reconcile.
    pub push: Push,
    /// `broadcastCommitted` — `blocked`/`agent_update` to every session.
    pub notices: Notices,
    /// `conversationM` — transcript fallback for the finished extract
    /// (`Reader` isn't `Clone`; the projector shares the one instance).
    pub conversations: Arc<Reader>,
    /// Session kill switch — transition tasks die with the relay.
    pub cancel: CancellationToken,
}

/// `spawn` — the `SetOnTransition` + `SetEnrich` registrations: install
/// the enrich hook and the transition sink on the handle, then run the
/// drain (`outcomes`) and the capture ticker (`captureHistoryLoop`)
/// until `cancel`.
pub(crate) fn spawn(deps: ProjectorDeps) {
    let (tx, rx) = mpsc::channel(64);
    // `poller.SetEnrich` (server.go:1235-1255) — the actor classifies
    // each blocked incoming pane ahead of `accept` so the commit itself
    // sees kind/options drift.
    deps.handle.set_enrich(enrich_hook(&deps));
    deps.handle.set_transition_sink(tx);
    let book = Arc::new(Mutex::new(CaptureBook::default()));
    tokio::spawn(
        drain(deps.clone(), rx, book.clone()).instrument(tracing::info_span!("projector")),
    );
    tokio::spawn(capture_loop(deps, book).instrument(tracing::info_span!("history_capture")));
}

/// The outcome drain — per-commit side effects plus one spawned task per
/// transition. Ordering inside one commit matches `handleTransition`'s
/// registration order; transitions from later commits fence themselves.
async fn drain(
    deps: ProjectorDeps,
    mut rx: mpsc::Receiver<AcceptOutcome>,
    book: Arc<Mutex<CaptureBook>>,
) {
    loop {
        tokio::select! {
            biased;
            () = deps.cancel.cancelled() => break,
            outcome = rx.recv() => {
                let Some(outcome) = outcome else { break };
                // `commitInventoryLocked`'s `customAnswers` cleanup —
                // panes that left the snapshot drop their stored answers.
                for pane_id in &outcome.removed {
                    deps.questions.forget_pane(pane_id);
                }
                post_commit(&deps, &book);
                for transition in outcome.transitions {
                    let deps = deps.clone();
                    tokio::spawn(
                        async move {
                            // `context.WithTimeout(parent, 10s)` — a stale
                            // transition's reads abandon mid-flight.
                            let _ = tokio::time::timeout(
                                TRANSITION_TIMEOUT,
                                handle_transition(&deps, transition),
                            )
                            .await;
                        }
                        .instrument(tracing::info_span!("transition")),
                    );
                }
            }
        }
    }
    // `drainLifecycleWork` — flush history on shutdown.
    deps.history.save_all();
}

/// `runAgentSideEffects` (server.go:3499-3512) — the post-publish sweep:
/// recovered-push reconciliation (once), history pane-set sync, and a
/// capture schedule for every claude-like pane still working/blocked.
fn post_commit(deps: &ProjectorDeps, book: &Arc<Mutex<CaptureBook>>) {
    let topology = deps.handle.topology.borrow().clone();
    reconcile_recovered_push(deps, &topology, book);
    sync_history_panes(deps, &topology, book);
    for agent in topology.agents() {
        if is_claude_like(&agent.agent) && (agent.status == "working" || agent.status == "blocked")
        {
            schedule_history_capture(deps, book, &agent.pane_id);
        }
    }
}

/// `captureHistoryLoop` (server.go:1993-2008) — every `CaptureInterval`,
/// schedule a capture for each claude-like pane still under attention.
async fn capture_loop(deps: ProjectorDeps, book: Arc<Mutex<CaptureBook>>) {
    let mut ticker = tokio::time::interval(CAPTURE_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            () = deps.cancel.cancelled() => break,
            _ = ticker.tick() => {
                let agents = deps.handle.topology.borrow().agents();
                for agent in agents {
                    if !is_claude_like(&agent.agent)
                        || (agent.status != "working" && agent.status != "blocked")
                    {
                        continue;
                    }
                    schedule_history_capture(&deps, &book, &agent.pane_id);
                }
            }
        }
    }
}

/// The `historyCaptureMu` ledger — `historyActive` (panes captures may
/// write for), `historyLast`/`historyInflight` (the 4s throttle +
/// single-flight), plus the once-gates `historyReconciled` and
/// `pushReconciled`.
#[derive(Default)]
struct CaptureBook {
    active: HashSet<String>,
    last: HashMap<String, Instant>,
    inflight: HashSet<String>,
    history_reconciled: bool,
    push_reconciled: bool,
}

/// `syncHistoryPanes` (server.go:2056-2081) — reconcile persisted
/// history against the active pane set once, then keep `active` current:
/// panes that left drop their history (memory + file); new claude-like
/// panes join the capture set.
fn sync_history_panes(deps: &ProjectorDeps, topology: &Topology, book: &Arc<Mutex<CaptureBook>>) {
    let active: HashSet<String> = topology
        .agents()
        .iter()
        .filter(|agent| is_claude_like(&agent.agent))
        .map(|agent| agent.pane_id.clone())
        .collect();
    let mut book = book.lock().expect("capture book poisoned");
    if !book.history_reconciled {
        deps.history.reconcile(&active);
        book.history_reconciled = true;
    }
    let stale: Vec<String> = book
        .active
        .iter()
        .filter(|pane_id| !active.contains(*pane_id))
        .cloned()
        .collect();
    for pane_id in stale {
        book.active.remove(&pane_id);
        book.last.remove(&pane_id);
        deps.history.discard(&pane_id);
    }
    book.active.extend(active);
}

/// `scheduleHistoryCapture` (server.go:2011-2054) — 4s-per-pane
/// throttled, single-flight: read the pane's full `recent_unwrapped`
/// transcript and fold it into history when the pane is still a live
/// claude-like agent showing no structured layout.
fn schedule_history_capture(deps: &ProjectorDeps, book: &Arc<Mutex<CaptureBook>>, pane_id: &str) {
    {
        let mut book = book.lock().expect("capture book poisoned");
        if book.inflight.contains(pane_id)
            || book
                .last
                .get(pane_id)
                .is_some_and(|last| last.elapsed() < CAPTURE_INTERVAL)
        {
            return;
        }
        book.inflight.insert(pane_id.to_owned());
        book.last.insert(pane_id.to_owned(), Instant::now());
    }
    let deps = deps.clone();
    let book = book.clone();
    let pane_id = pane_id.to_owned();
    let span = tracing::info_span!("history_capture", pane = %pane_id);
    tokio::spawn(
        async move {
            let outcome = tokio::time::timeout(
                READ_TIMEOUT,
                deps.handle.client.pane_read(
                    &pane_id,
                    ReadSource::RecentUnwrapped,
                    history::MAX_LINES as u32,
                    ReadFormat::Ansi,
                ),
            )
            .await;
            let content = outcome.ok().and_then(Result::ok).map(|r| r.text);
            let Some(content) = content else {
                book.lock()
                    .expect("capture book poisoned")
                    .inflight
                    .remove(&pane_id);
                return;
            };
            if content.is_empty() || layout_hint(&content) {
                book.lock()
                    .expect("capture book poisoned")
                    .inflight
                    .remove(&pane_id);
                return;
            }
            let claude_like = {
                let topology = deps.handle.topology.borrow();
                topology
                    .pane_of(&pane_id)
                    .map(|info| is_claude_like(info.agent.as_deref().unwrap_or_default()))
                    .unwrap_or(false)
            };
            {
                let mut book = book.lock().expect("capture book poisoned");
                if claude_like && book.active.contains(&pane_id) {
                    deps.history.merge(&pane_id, &content);
                }
                book.inflight.remove(&pane_id);
            }
        }
        .instrument(span),
    );
}

/// `handleTransition` (server.go:1549-1750) — the per-transition task.
async fn handle_transition(deps: &ProjectorDeps, transition: PaneTransition) {
    let pane_id = transition.pane_id.as_str();
    let status = transition.status.as_str();
    let revision = transition.revision;
    let transition_at = transition.observed_at;

    // `s.state.Agent(paneID)` — the live committed row at task time; the
    // `sameConversationTuple` re-check below compares the CURRENT pane
    // against this capture (`agentExists != currentExists` included —
    // the `Option` inequality covers it).
    let (agent_state, conversation_project, before_tuple) = {
        let topology = deps.handle.topology.borrow();
        let info = topology.pane_of(pane_id).cloned();
        let state = info.as_ref().map(|info| topology.agent_state(info));
        let project = info
            .as_ref()
            .map(|info| {
                normalize_project_context(
                    info.agent.as_deref().unwrap_or_default(),
                    ProjectContext {
                        cwd: info.cwd.clone().unwrap_or_default(),
                        foreground_cwd: info.foreground_cwd.clone().unwrap_or_default(),
                    },
                )
            })
            .unwrap_or_default();
        let tuple = info
            .as_ref()
            .map(crate::actions::conversation::conversation_tuple);
        (state, project, tuple)
    };

    let mut session = String::new();
    let mut session_id = String::new();
    let mut conversation_agent = transition.agent.clone();
    let mut conversation_cwd = String::new();
    let mut blocked_event_id = String::new();
    let mut blocked_content_rev = 0i64;
    if let Some(agent) = &agent_state {
        // The oracle reads `session` only when the transition is still
        // current — for `working` the fence short-circuits true.
        if status != "working"
            || deps
                .handle
                .topology
                .borrow()
                .transition_current(pane_id, status, revision)
        {
            session = agent.session.clone();
        }
        session_id = agent.agent_session_id.clone();
        conversation_agent = agent.agent.clone();
        conversation_cwd = agent.cwd.clone();
        blocked_event_id = agent.event_id.clone();
        blocked_content_rev = deps.handle.topology.borrow().content_rev_of(pane_id);
    }
    let pane_generation = if status == "blocked" {
        let (generation, active) = deps.handle.topology.borrow().pane_session(pane_id);
        if !active || blocked_event_id.is_empty() {
            return;
        }
        generation
    } else {
        0
    };

    // `transitionCurrent` — blocked fences on the cycle's event id +
    // generation; working never fences; completions fence on the
    // completion revision.
    let transition_current = || {
        let topology = deps.handle.topology.borrow();
        if status == "blocked" {
            topology.blocked_transition_current(pane_id, &blocked_event_id, pane_generation)
        } else if status == "working" {
            true
        } else {
            topology.completion_current(pane_id, revision)
        }
    };
    if !transition_current() {
        return;
    }

    if status == "working" {
        // `pushM.ResolvePaneID(ctx, paneID, "")` — retract everything.
        if let Err(error) = deps.push.resolve_pane_id(pane_id, "") {
            warn!(%pane_id, %error, "push notification retraction failed");
        }
        let summary = if transition.agent.is_empty() {
            "Agent started working".to_owned()
        } else {
            format!("{} started working", transition.agent)
        };
        record_transition_activity(
            deps,
            TransitionRow {
                kind: "working",
                status: "working",
                summary,
                pane_id,
                details: BTreeMap::from([
                    ("transition".to_owned(), serde_json::json!("working")),
                    ("transition_at".to_owned(), serde_json::json!(transition_at)),
                ]),
                agent: transition.agent.clone(),
                project: transition.project.clone(),
                session,
                extract: String::new(),
                fence: Fence::Working,
            },
        );
        return;
    }

    if status == "blocked" {
        let Some(agent_state) = agent_state else {
            return;
        };
        // `enrichBlockedTransition` — reads + classify retries, then the
        // custom-answer record/fill (`setAgentAttention`).
        let classification = enrich_blocked(deps, pane_id, &agent_state.agent).await;
        if !transition_current() {
            return;
        }
        // `CommitAttentionClassification` — the commit fence carries the
        // event id, generation, and the pre-enrich content revision.
        let Some((persisted, interaction_id, attention_rev)) =
            deps.handle.topology.borrow().commit_attention(
                pane_id,
                &blocked_event_id,
                pane_generation,
                blocked_content_rev,
                &classification,
            )
        else {
            return;
        };
        if !transition_current() {
            return;
        }
        // `classifiedCurrent` — `AttentionTransitionCurrent` on the
        // committed kind + attention revision.
        let classified_current = || {
            deps.handle.topology.borrow().attention_transition_current(
                pane_id,
                &persisted.event_id,
                pane_generation,
                &persisted.attention_kind,
                attention_rev,
            )
        };
        if !classified_current() {
            return;
        }
        if persisted.attention_kind == "chat" {
            broadcast_blocked(deps, &persisted, &interaction_id);
            handle_chat_completion(deps, &persisted).await;
            return;
        }

        let event_id = persisted.event_id.clone();
        let mut command = persisted.command.clone();
        if command.is_empty() {
            command = match persisted.attention_kind.as_str() {
                "question" => "Agent needs an answer",
                "unknown" => "Agent needs inspection",
                _ => "Agent needs approval",
            }
            .to_owned();
        }
        let activity_kind = if persisted.attention_kind == "question" {
            "question"
        } else {
            "blocked"
        };
        if !record_transition_activity(
            deps,
            TransitionRow {
                kind: activity_kind,
                status: "attention",
                summary: command.clone(),
                pane_id,
                details: BTreeMap::from([
                    ("event_id".to_owned(), serde_json::json!(event_id)),
                    (
                        "attention_kind".to_owned(),
                        serde_json::json!(persisted.attention_kind),
                    ),
                    ("transition_at".to_owned(), serde_json::json!(transition_at)),
                ]),
                agent: transition.agent.clone(),
                project: transition.project.clone(),
                session,
                extract: persisted.prompt.clone(),
                fence: Fence::Attention {
                    event_id: event_id.clone(),
                    generation: pane_generation,
                    kind: persisted.attention_kind.clone(),
                    attention_rev,
                },
            },
        ) {
            return;
        }
        if !classified_current() {
            return;
        }
        let (category, preview) = if persisted.attention_kind == "question" {
            (CATEGORY_QUESTION, PREVIEW_QUESTION)
        } else {
            (CATEGORY_ATTENTION, PREVIEW_HIDDEN)
        };
        publish_agent_push(
            deps,
            &persisted,
            &event_id,
            attention_rev,
            category,
            preview,
        );
        if !classified_current() {
            return;
        }
        broadcast_blocked(deps, &persisted, &interaction_id);
        return;
    }

    // The completion branch — `RegisterFinishedNotificationForTransition`
    // claims the cycle's finished notification.
    if !deps
        .handle
        .topology
        .borrow()
        .register_finished_notification(pane_id, revision)
    {
        return;
    }
    let event_id = finished_event_id(pane_id);
    let extract = capture_finished_pane(
        deps,
        pane_id,
        &conversation_agent,
        &conversation_cwd,
        &session_id,
        &conversation_project,
    )
    .await;
    // `transitionCurrent() || agentExists != currentExists ||
    //  (agentExists && !sameConversationTuple(agentState, currentAgent))`
    let current_tuple = {
        let topology = deps.handle.topology.borrow();
        topology
            .pane_of(pane_id)
            .map(crate::actions::conversation::conversation_tuple)
    };
    if !transition_current() || current_tuple != before_tuple {
        return;
    }
    let summary = if transition.agent.is_empty() {
        "Agent completed".to_owned()
    } else {
        format!("{} completed", transition.agent)
    };
    if !record_transition_activity(
        deps,
        TransitionRow {
            kind: "finished",
            status: "completed",
            summary,
            pane_id,
            details: BTreeMap::from([
                ("event_id".to_owned(), serde_json::json!(event_id)),
                ("transition_at".to_owned(), serde_json::json!(transition_at)),
            ]),
            agent: transition.agent.clone(),
            project: transition.project.clone(),
            session,
            extract,
            fence: Fence::Completion { revision },
        },
    ) {
        return;
    }
    if !transition_current() {
        return;
    }
    let current_agent = deps.handle.topology.borrow().agent_state_of(pane_id);
    match current_agent {
        Some(agent) => {
            publish_agent_push(
                deps,
                &agent,
                &event_id,
                revision,
                CATEGORY_FINISHED,
                PREVIEW_BRIEF,
            );
        }
        None => {
            if let Err(error) = deps.push.resolve_pane_id(pane_id, "") {
                warn!(%pane_id, %error, "push notification retraction failed");
            }
        }
    }
}

/// `enrichBlockedTransition` + `classifyBlockedTransition`
/// (server.go:1789-1880) — up to four `recent_unwrapped` 80-line reads
/// 100ms apart until the classify is decisive; a read error leaves the
/// pane `unknown` + "Agent needs inspection". `classify` runs per
/// attempt; `setAgentAttention`'s custom-answer record/fill runs once,
/// on the classification that returns.
async fn enrich_blocked(deps: &ProjectorDeps, pane_id: &str, agent: &str) -> Classification {
    for attempt in 0..CLASSIFY_ATTEMPTS {
        let read = tokio::time::timeout(
            READ_TIMEOUT,
            deps.handle.client.pane_read(
                pane_id,
                ReadSource::RecentUnwrapped,
                ENRICH_LINES,
                ReadFormat::Ansi,
            ),
        )
        .await;
        let Ok(Ok(read)) = read else {
            // Read error — the oracle's `setAgentAttention` fallback.
            return Classification {
                kind: AttentionKind::Unknown,
                prompt: "Agent needs inspection".to_owned(),
                ..Classification::default()
            };
        };
        let mut classification = classify(&read.text, agent);
        if classification.kind != AttentionKind::Unknown || attempt + 1 == CLASSIFY_ATTEMPTS {
            record_fill_answers(pane_id, &mut classification, &deps.questions);
            return classification;
        }
        // `blockedClassificationRetryDelay`, cancellable like the oracle's
        // `ctx.Done()` select arm.
        tokio::select! {
            () = deps.cancel.cancelled() => return classification,
            () = tokio::time::sleep(CLASSIFY_RETRY) => {}
        }
    }
    unreachable!("attempt loop always returns")
}

/// `poller.SetEnrich`'s per-pane leg (server.go:1236-1254): `ReadPane`
/// (recent-unwrapped, 80 lines, ansi) under the 3s bound, `Classify`,
/// then `setAgentAttention`'s record/fill bookkeeping — all inside
/// [`classify_semantics`]. A read error yields the `unknown` +
/// "Agent needs inspection" fallback the oracle writes onto the row.
fn enrich_hook(deps: &ProjectorDeps) -> crate::actor::EnrichHook {
    let client = deps.handle.client.clone();
    let questions = deps.questions.clone();
    let cancel = deps.cancel.clone();
    Arc::new(move |pane_id: String, agent: String| {
        let client = client.clone();
        let questions = questions.clone();
        let cancel = cancel.clone();
        Box::pin(async move {
            let read = tokio::select! {
                () = cancel.cancelled() => None,
                read = tokio::time::timeout(
                    READ_TIMEOUT,
                    client.pane_read(
                        &pane_id,
                        ReadSource::RecentUnwrapped,
                        ENRICH_LINES,
                        ReadFormat::Ansi,
                    ),
                ) => read.ok().and_then(|outcome| outcome.ok()),
            };
            let Some(read) = read else {
                return Classification {
                    kind: AttentionKind::Unknown,
                    prompt: "Agent needs inspection".to_owned(),
                    ..Classification::default()
                };
            };
            classify_semantics(&pane_id, &read.text, &agent, &questions)
        })
    })
}

/// `broadcastBlockedAttention` (server.go:1894-1935) — the `blocked`
/// frame built off the committed row, broadcast to every session
/// (`broadcastCommitted`; the shared ledger already carries the
/// projection, so no `agentView` merge is needed here).
fn broadcast_blocked(deps: &ProjectorDeps, agent: &AgentState, interaction_id: &str) {
    let generation = deps.handle.topology.borrow().generation_of(&agent.pane_id);
    deps.notices.send(
        Outbound::Blocked(Box::new(BlockedMessage {
            r#type: "blocked".to_owned(),
            pane_id: Some(agent.pane_id.clone()),
            raw_pane_id: Some(agent.raw_pane_id.clone()),
            terminal_id: Some(agent.terminal_id.clone()),
            tab_id: Some(agent.tab_id.clone()),
            tab_label: Some(agent.tab_label.clone()),
            tab_number: Some(agent.tab_number),
            workspace_id: Some(agent.workspace_id.clone()),
            agent: Some(agent.agent.clone()),
            name: Some(agent.name.clone()),
            status: Some("blocked".to_owned()),
            cwd: Some(agent.cwd.clone()),
            project: Some(agent.project.clone()),
            host: Some(agent.host.clone()),
            session: Some(agent.session.clone()),
            session_name: Some(agent.session_name.clone()),
            server_session_id: Some("primary".to_owned()),
            generation: Some(generation),
            agent_session_id: Some(agent.agent_session_id.clone()),
            updated_at: Some(agent.updated_at),
            event_id: Some(agent.event_id.clone()),
            attention_kind: Some(agent.attention_kind.clone()),
            prompt: Some(agent.prompt.clone()),
            command: Some(agent.command.clone()),
            options: Some(if agent.options.is_empty() {
                MaybeNull::Null
            } else {
                MaybeNull::Value(agent.options.clone())
            }),
            approval_fingerprint: Some(agent.approval_fingerprint.clone()),
            interaction: Some(match &agent.interaction {
                Some(interaction) => MaybeNull::Value(interaction.clone()),
                None => MaybeNull::Null,
            }),
            interaction_id: Some(interaction_id.to_owned()),
            question_layout: Some(agent.question_layout),
            pane_revision: Some(agent.pane_revision),
        })),
        String::new(),
    );
}

/// `handleChatCompletion` (server.go:1937-1991) — a `chat` blocked
/// classification completes the cycle inline: claim the finished
/// notification, journal `finished`, publish the finished push.
async fn handle_chat_completion(deps: &ProjectorDeps, agent: &AgentState) {
    let pane_id = agent.pane_id.as_str();
    let revision = agent.pane_revision;
    let current = || {
        deps.handle
            .topology
            .borrow()
            .completion_current(pane_id, revision)
    };
    if !current()
        || !deps
            .handle
            .topology
            .borrow()
            .register_finished_notification(pane_id, revision)
    {
        return;
    }
    let event_id = finished_event_id(pane_id);
    let summary = if agent.agent.is_empty() {
        "Agent completed".to_owned()
    } else {
        format!("{} completed", agent.agent)
    };
    if !record_transition_activity(
        deps,
        TransitionRow {
            kind: "finished",
            status: "completed",
            summary,
            pane_id,
            details: BTreeMap::from([
                ("event_id".to_owned(), serde_json::json!(event_id)),
                (
                    "attention_kind".to_owned(),
                    serde_json::json!(agent.attention_kind),
                ),
            ]),
            agent: agent.agent.clone(),
            project: agent.project.clone(),
            session: agent.session.clone(),
            extract: agent.prompt.clone(),
            fence: Fence::Completion { revision },
        },
    ) {
        return;
    }
    if !current() {
        return;
    }
    publish_agent_push(
        deps,
        agent,
        &event_id,
        revision,
        CATEGORY_FINISHED,
        PREVIEW_BRIEF,
    );
}

/// `publishAgentPush` (server.go:1518-1547) — retract the pane's other
/// keys, then queue the publish (5-minute TTL is `Publish`'s default).
fn publish_agent_push(
    deps: &ProjectorDeps,
    agent: &AgentState,
    event_id: &str,
    interaction_revision: i64,
    category: &'static str,
    preview: &'static str,
) {
    if agent.terminal_id.is_empty() || event_id.is_empty() {
        return;
    }
    let key = PushEventKey {
        server_session_id: "primary".to_owned(),
        pane_id: agent.pane_id.clone(),
        terminal_id: agent.terminal_id.clone(),
        agent_session_id: agent.agent_session_id.clone(),
        generation: agent.generation,
        event_id: event_id.to_owned(),
        interaction_revision,
        category: category.to_owned(),
        ..PushEventKey::default()
    };
    if let Err(error) = deps.push.resolve_pane_id(&agent.pane_id, event_id) {
        warn!(pane_id = %agent.pane_id, %error, "push notification retraction failed");
    }
    if let Err(error) = deps.push.publish(PublishRequest {
        key,
        preview,
        created_at: None,
        expires_at: None,
    }) {
        warn!(pane_id = %agent.pane_id, %error, "push notification queueing failed");
    }
}

/// `RecordTransitionActivity`'s fence vocabulary (dispatch.go:972-1001):
/// which `*Current` predicate gates the commit.
enum Fence<'a> {
    /// `kind == "working"` — always current.
    Working,
    /// `CompletionCurrent(paneID, revision)` — `kind == "finished"`.
    Completion { revision: i64 },
    /// `AttentionTransitionCurrent` — the 4-tuple blocked identity.
    Attention {
        event_id: String,
        generation: i64,
        kind: String,
        attention_rev: i64,
    },
    /// `TransitionCurrent(paneID, expectedStatus, revision)` — the
    /// generic fallback (unused by the transition sites today, kept so
    /// the mapping stays total).
    #[allow(dead_code)]
    Status { status: &'a str, revision: i64 },
}

/// One `RecordTransitionActivity` call distilled.
struct TransitionRow<'a> {
    /// `Entry.Kind` — `working`/`blocked`/`question`/`finished`.
    kind: &'static str,
    /// `Entry.Status` — `working`/`attention`/`completed`.
    status: &'static str,
    summary: String,
    pane_id: &'a str,
    /// `Entry.Details` verbatim.
    details: BTreeMap<String, serde_json::Value>,
    /// Transition-captured attribution (NOT the committed row's).
    agent: String,
    project: String,
    session: String,
    /// `Entry.Extract` — the prompt or captured response.
    extract: String,
    fence: Fence<'a>,
}

/// `RecordTransitionActivity` (dispatch.go:949-1039) — the fenced
/// journal write: check current → commit → re-check → discard-or-publish.
fn record_transition_activity(deps: &ProjectorDeps, row: TransitionRow) -> bool {
    let pane_id = row.pane_id;
    let current = || {
        let topology = deps.handle.topology.borrow();
        match &row.fence {
            Fence::Working => true,
            Fence::Completion { revision } => topology.completion_current(pane_id, *revision),
            Fence::Attention {
                event_id,
                generation,
                kind,
                attention_rev,
            } => topology.attention_transition_current(
                pane_id,
                event_id,
                *generation,
                kind,
                *attention_rev,
            ),
            Fence::Status { status, revision } => {
                topology.transition_current(pane_id, status, *revision)
            }
        }
    };
    if !current() {
        return false;
    }
    // `transitionAt` — the commit-time timestamp overrides the stamp.
    let timestamp = row
        .details
        .get("transition_at")
        .and_then(serde_json::Value::as_i64)
        .filter(|at| *at > 0);
    let Some(entry) = deps.activities.commit(NewEntry {
        kind: row.kind.to_owned(),
        status: row.status.to_owned(),
        summary: row.summary,
        host: crate::topology::hostname_short(),
        pane_id: pane_id.to_owned(),
        agent: row.agent,
        project: row.project,
        request_id: String::new(),
        extract: row.extract,
        session: row.session,
        details: Some(row.details),
        timestamp,
    }) else {
        return false;
    };
    if !current() {
        deps.activities.discard(&entry.id);
        return false;
    }
    deps.activities.publish(&entry);
    true
}

/// `captureFinishedPane` (server.go:2145-2186) — transcript first, then
/// the full-pane fallback (history-merged for claude-like agents) ending
/// in `LatestCompletedResponse`/`PaneSummary`.
async fn capture_finished_pane(
    deps: &ProjectorDeps,
    pane_id: &str,
    agent: &str,
    cwd: &str,
    session_id: &str,
    project: &ProjectContext,
) -> String {
    let response = latest_conversation_response(deps, agent, cwd, session_id, project).await;
    if !response.is_empty() {
        return response;
    }
    let read = tokio::time::timeout(
        READ_TIMEOUT,
        deps.handle.client.pane_read(
            pane_id,
            ReadSource::RecentUnwrapped,
            history::MAX_LINES as u32,
            ReadFormat::Ansi,
        ),
    )
    .await;
    let content = read
        .ok()
        .and_then(Result::ok)
        .map(|r| r.text)
        .unwrap_or_default();
    if content.is_empty() {
        return String::new();
    }
    let completion_content = if is_claude_like(agent) && !layout_hint(&content) {
        deps.history.merge(pane_id, &content)
    } else {
        content
    };
    let response = latest_completed_response(&completion_content);
    if !response.is_empty() {
        return response;
    }
    pane_summary(&completion_content)
}

/// `latestConversationResponse` (server.go:2126-2144) — the last
/// assistant entry of the pane's transcript, when the provider has one.
/// The reader is sync file/subprocess IO — `spawn_blocking` keeps it off
/// the runtime thread.
async fn latest_conversation_response(
    deps: &ProjectorDeps,
    agent: &str,
    cwd: &str,
    session_id: &str,
    project: &ProjectContext,
) -> String {
    if session_id.trim().is_empty() || !crate::conversation::supported(agent) {
        return String::new();
    }
    let reader = Arc::clone(&deps.conversations);
    let agent = agent.to_owned();
    let session_id = session_id.to_owned();
    let project = project.clone();
    let _ = cwd; // the project context carries cwd (see projectContextForAgent)
    tokio::task::spawn_blocking(move || {
        let page = reader.read_with_project(&agent, project, &session_id, None, 1);
        let Ok(page) = page else { return String::new() };
        if !page.available {
            return String::new();
        }
        let Some(entry) = page.entries.last() else {
            return String::new();
        };
        if entry.role != "assistant" || entry.text.trim().is_empty() {
            return String::new();
        }
        entry.text.clone()
    })
    .await
    .unwrap_or_default()
}

/// `reconcileRecoveredPush` (server.go:1467-1516) — once per process:
/// recovered `finished` keys survive only while their completion is
/// still current; blocked panes re-arm their attention/question keys per
/// subscription; everything else gets retracted by `Reconcile`.
fn reconcile_recovered_push(
    deps: &ProjectorDeps,
    topology: &Topology,
    book: &Arc<Mutex<CaptureBook>>,
) {
    {
        let book = book.lock().expect("capture book poisoned");
        if book.push_reconciled {
            return;
        }
    }
    let subscriptions = deps.push.subscriptions();
    let mut current: Vec<PushEventKey> = Vec::new();
    for key in deps.push.recovered_keys() {
        if key.category == CATEGORY_FINISHED
            && push_target_current(topology, &key.target())
            && topology.completion_current(&key.pane_id, key.interaction_revision)
        {
            current.push(key);
        }
    }
    for agent in topology.agents() {
        if agent.status != "blocked"
            || agent.event_id.is_empty()
            || agent.terminal_id.is_empty()
            || agent.attention_kind == "chat"
        {
            continue;
        }
        let category = if agent.attention_kind == "question" {
            CATEGORY_QUESTION
        } else {
            CATEGORY_ATTENTION
        };
        for subscription in &subscriptions {
            if subscription.device_id.is_empty() {
                continue;
            }
            current.push(PushEventKey {
                device_id: subscription.device_id.clone(),
                server_session_id: "primary".to_owned(),
                pane_id: agent.pane_id.clone(),
                terminal_id: agent.terminal_id.clone(),
                agent_session_id: agent.agent_session_id.clone(),
                generation: agent.generation,
                event_id: agent.event_id.clone(),
                interaction_revision: topology.attention_rev_of(&agent.pane_id),
                category: category.to_owned(),
            });
        }
    }
    if deps.push.reconcile(&current).is_err() {
        warn!("recovered push queue reconciliation failed");
        return;
    }
    book.lock().expect("capture book poisoned").push_reconciled = true;
}

/// `pushTargetCurrent` (server.go:485-494) — the exact-target tuple must
/// still resolve to the live pane.
fn push_target_current(topology: &Topology, target: &TargetRef) -> bool {
    if target.server_session_id != "primary"
        || target.pane_id.is_empty()
        || target.terminal_id.is_empty()
        || target.generation < 0
    {
        return false;
    }
    let Some(info) = topology.pane_of(&target.pane_id) else {
        return false;
    };
    info.terminal_id == target.terminal_id
        && info
            .agent_session
            .as_ref()
            .map(|s| s.value.trim() == target.agent_session_id)
            .unwrap_or_else(|| target.agent_session_id.is_empty())
        && topology.generation_of(&target.pane_id) == target.generation
}

/// Re-export for the read/watch paths — `classify` + custom answers is
/// the `setAgentAttention`/`classifyPaneResponse` shared half.
#[allow(dead_code)]
pub(crate) fn classify_for_frame(
    pane_id: &str,
    content: &str,
    agent: &str,
    questions: &Questions,
) -> Classification {
    classify_semantics(pane_id, content, agent, questions)
}

/// `kind_str` re-export — frame emitters spell attention kinds on the wire.
#[allow(dead_code)]
pub(crate) fn attention_kind_str(kind: AttentionKind) -> &'static str {
    kind_str(kind)
}

/// The wire `interaction` — `wire_interaction` re-exported for the frame
/// emitters.
#[allow(dead_code)]
pub(crate) fn frame_interaction(
    interaction: &Option<super::model::Interaction>,
) -> MaybeNull<lerdr_core::protocol::QuestionInteraction> {
    match interaction {
        Some(interaction) => MaybeNull::Value(wire_interaction(interaction)),
        None => MaybeNull::Null,
    }
}

/// `classify` re-export — kept for the projector's own enrich loop.
#[allow(dead_code)]
pub(crate) fn classify_content(text: &str, agent: &str) -> Classification {
    classify(text, agent)
}
