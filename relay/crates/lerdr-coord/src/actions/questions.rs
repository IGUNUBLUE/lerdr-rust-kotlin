//! Question/approval actions — the `approval.go` state machine port.
//!
//! The oracle's coordinator keeps a per-pane ledger of pending approval and
//! structured-question operations so `respond`, `answer_question`,
//! `clarify_question`, and `navigate_question` answer exactly the
//! interaction the client saw: stale answers, conflicting submissions, and
//! late retries are refused before they reach the pane, matching duplicates
//! replay or attach to the in-flight operation, and a successful dispatch
//! resolves as `accepted` followed by a confirmation-watch result.
//!
//! This module carries the three oracle subsystems the port needs in one
//! place because nothing else in the crate may reference them yet:
//!
//! - the terminal parser (`internal/question/parser.go` + `attention.go`)
//!   that classifies live pane content into approvals and structured
//!   questions for the five supported agent families;
//! - the input planner (`internal/question/input.go`) that turns the shared
//!   question protocol into per-agent keyboard contracts;
//! - the coordinator state machine (`internal/coordinator/approval.go` and
//!   the scheduler semantics it leans on) — ledger admission, payload
//!   conflicts, replay, in-flight attach, per-command deadlines, pane
//!   session checks, and the confirmation watcher.
//!
//! Where the oracle relies on coordinator state (blocked event ids,
//! attention kind, pane generations) that this crate does not project yet,
//! the port validates the pane directly: every handler re-reads the pane
//! through Herdr, re-classifies it, and compares against the client-supplied
//! identity (approval fingerprint / interaction id) before dispatching —
//! strictly stronger than trusting a possibly stale projection, at the cost
//! of one extra `pane.read` per request. The pane identity surrogate is the
//! session tuple Herdr publishes, documented at [`PaneToken`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lerdr_core::json::{MaybeNull, RawJson};
use lerdr_core::protocol::{
    action_receipt_response, ActionReceipt, ActionReceiptPhase, CommandResultMessage, Inbound,
    Outbound,
};
use lerdr_herdr::{
    AgentInfo, AgentStatus, HerdrError, PaneReadParams, PaneReadResult, ReadFormat, ReadSource,
};

use crate::topology::Topology;
use serde::Serialize;
use tokio::sync::oneshot;

use super::{dispatch_failure, record_activity, ActionContext, Outcome};

/// `approvalDeadline` — the per-command effect budget for `respond`
/// (the oracle's `9 * time.Second`; shared across the pane read, classify,
/// and send inside one request).
const APPROVAL_DEADLINE: Duration = Duration::from_secs(9);
/// `questionDeadline` — the per-command effect budget for the question
/// actions (the oracle's `16 * time.Second`; multi-step key plans need the
/// larger window).
const QUESTION_DEADLINE: Duration = Duration::from_secs(16);
/// `approvalPollTimeout` — how long the confirmation watcher runs before
/// resolving `unconfirmed`.
const WATCH_TIMEOUT: Duration = Duration::from_secs(5);
/// `approvalPollInterval` — the watcher's poll cadence.
const WATCH_INTERVAL: Duration = Duration::from_millis(350);
/// `questionKeyDelay` — the oracle's inter-key pause inside a question plan.
const KEY_DELAY: Duration = Duration::from_millis(150);
/// `maxLedgerEntries` — the scheduler ledger bound.
const MAX_LEDGER_ENTRIES: usize = 256;
/// `ledgerRetention` — how long a finished ledger entry stays replayable.
const LEDGER_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);
/// `pane.read` line budget for classify/parse (the oracle reads 80).
const PANE_READ_LINES: u32 = 80;
/// `shiftTabSequence` — a lone `shift+tab` rides `pane.send_text` as the
/// backtab escape, matching `Client.SendKeys`.
const SHIFT_TAB_SEQUENCE: &str = "\x1b[Z";

/// `ErrPaneReplaced` — the pane identity changed under an in-flight
/// operation; the oracle's `paneSessionError` classification.
const ERR_PANE_REPLACED: &str = "pane session was replaced";

/// The shared "unhandled action" receipt stays referenced from here —
/// the router's baseline arm still links it for kinds without a backend.
const _: fn(&str, &str) -> Vec<Outbound> = super::unknown;

// ═══════════════════════════════════════════════════════════════════════
// Classifier — the pure terminal parser, attention classifier, and input
// planner live in `crate::classify`; this module keeps the coordinator
// halves: payload decoding rides `classify::model`, classification and
// input planning ride `classify::{attention,input,parse}`.
// ═══════════════════════════════════════════════════════════════════════

use crate::classify::*;

// ═══════════════════════════════════════════════════════════════════════
// Shared state — the coordinator's per-pane ledger and the pane-session
// surrogate for the oracle's `PaneGeneration`.
// ═══════════════════════════════════════════════════════════════════════

/// `PaneToken` — the pane-lifetime identity the scheduler's generation
/// carries in the oracle. This crate's projection does not publish
/// generations yet, so the token is the tuple Herdr itself supplies:
/// terminal + tab identity pin the terminal pane, and the agent-session
/// `source`/`value` pair pins the conversation bound to it. A pane
/// replacement, re-attach, or session rebind changes the token, so an
/// in-flight command for the old pane can never write into the new one.
/// Residual gap vs the oracle: a respawn that reuses every identity field
/// (same terminal, tab, and agent session) is indistinguishable — the
/// generation counter the projection will land closes that window.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PaneToken {
    terminal_id: String,
    tab_id: String,
    session_source: String,
    session_value: String,
}

impl PaneToken {
    fn of(agent: &AgentInfo) -> Self {
        Self {
            terminal_id: agent.terminal_id.clone(),
            tab_id: agent.tab_id.clone(),
            session_source: agent
                .agent_session
                .as_ref()
                .map(|session| session.source.clone())
                .unwrap_or_default(),
            session_value: agent
                .agent_session
                .as_ref()
                .map(|session| session.value.clone())
                .unwrap_or_default(),
        }
    }
}

/// The pane's token from a topology snapshot (`None` = pane absent — the
/// oracle's zero-generation stand-in).
fn pane_token(topology: &Topology, pane_id: &str) -> Option<PaneToken> {
    topology.pane_of(pane_id).map(PaneToken::of)
}

/// `paneSessionError` — the token must still resolve to itself on the live
/// topology.
fn token_current(ctx: &ActionContext, pane_id: &str, token: &PaneToken) -> bool {
    let topology = ctx.handle.topology.borrow().clone();
    pane_token(&topology, pane_id).is_some_and(|current| current == *token)
}

/// `CommandResult` stored in the ledger — `Outcome`'s fields verbatim so a
/// replay rebuilds the wire result byte-identically under a new
/// `request_id` (`replayed` never reaches the wire in either
/// implementation; a replay just re-emits the stored result).
#[derive(Debug, Clone)]
struct StoredResult {
    action: String,
    ok: bool,
    phase: &'static str,
    error: String,
    pane_id: String,
    data: Option<serde_json::Value>,
}

impl StoredResult {
    fn new(action: &str, outcome: &Outcome) -> Self {
        Self {
            action: action.to_owned(),
            ok: outcome.ok,
            phase: outcome.phase,
            error: outcome.error.clone(),
            pane_id: outcome.pane_id.clone(),
            data: outcome.data.clone(),
        }
    }

    /// Rebuild the wire `command_result` for a fresh `request_id`.
    fn frame(&self, request_id: &str) -> Outbound {
        Outbound::CommandResult(CommandResultMessage {
            r#type: "command_result".to_owned(),
            request_id: (!request_id.is_empty()).then(|| request_id.to_owned()),
            action: Some(self.action.clone()),
            ok: Some(self.ok),
            phase: Some(self.phase.to_owned()),
            error: Some(self.error.clone()),
            pane_id: Some(self.pane_id.clone()),
            data: self.data.as_ref().and_then(|value| {
                serde_json::value::to_raw_value(value)
                    .ok()
                    .map(|raw| MaybeNull::Value(RawJson(raw)))
            }),
        })
    }
}

/// `ledgerEntry` — one admission slot keyed by pane+request identity.
/// (`command_id`/`pane_id` from the oracle's entry are folded into the key
/// and the token check — `finish` verifies the slot by key+generation.)
struct LedgerEntry {
    payload_hash: String,
    result: Option<StoredResult>,
    token: PaneToken,
    waiters: Vec<oneshot::Sender<StoredResult>>,
    done: bool,
    created_at: Instant,
}

/// `ReplayLedger`'s read-only admission lookup.
enum Lookup {
    /// No matching entry (or a stale-generation entry just evicted).
    Miss,
    /// A matching operation is still running — the caller's request
    /// resolves with the stored result once the operation finishes.
    Attach(oneshot::Receiver<StoredResult>),
    /// A finished operation with the identical payload — replay its result.
    Replay(StoredResult),
    /// Same key, different payload — refuse.
    Conflict,
}

/// `schedule`'s ledger-slot claim.
enum Admission {
    /// The caller owns a new in-flight slot via [`Flight`].
    Fresh(Flight),
    /// The slot filled between the replay check and the claim — attach.
    Attach(oneshot::Receiver<StoredResult>),
    /// The slot finished between the checks — replay.
    Replay(StoredResult),
    /// Same key, different payload — refuse.
    Conflict,
}

/// `PendingApproval` — the admission-time approval identity the oracle
/// keeps on `BlockedEventID`/`Options`/`ApprovalFingerprint` while a pane
/// is blocked. The Rust projection does not carry those fields, so the
/// first verified `respond` for a blocked pane records them here; later
/// requests are gated against the record before touching the ledger.
#[derive(Debug, Clone)]
struct PendingApproval {
    event_id: String,
    fingerprint: String,
    token: PaneToken,
}

#[derive(Default)]
struct QuestionsInner {
    ledger: HashMap<String, LedgerEntry>,
    /// Recorded `other_text` answers per pane — `summary_key(question)` →
    /// text (the oracle keeps `customAnswers` per pane, refusing a new key
    /// once 32 entries exist).
    custom_answers: HashMap<String, Vec<(String, String)>>,
    /// Per-pane effect serialization — the scheduler's single
    /// `runActionOp` slot per pane. Weak refs so idle panes free the lock.
    pane_locks: HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>,
    /// Verified approval identity per pane while blocked.
    pending_approvals: HashMap<String, PendingApproval>,
}

/// `MAX_CUSTOM_ANSWERS` per pane.
const MAX_CUSTOM_ANSWERS: usize = 32;

/// Shared per-relay question/approval state — the [`ActionContext`]
/// member `questions`, constructed once by the router and cloned into
/// every action session.
#[derive(Clone, Default)]
pub(crate) struct Questions {
    inner: Arc<Mutex<QuestionsInner>>,
}

/// `Flight` — the in-flight operation a `Fresh` admission owns. `finish`
/// stores the terminal result and releases attached waiters; dropping
/// un-finished removes the slot (the oracle's cancelled-operation path)
/// so a dead task can never wedge a ledger key.
struct Flight {
    inner: Arc<Mutex<QuestionsInner>>,
    key: String,
    token: PaneToken,
    armed: bool,
}

impl Flight {
    /// `commitAndBroadcastResult` — store the result and wake waiters.
    /// Returns `false` when the pane's token moved on (the oracle drops
    /// the watcher update in that case).
    fn finish(mut self, result: StoredResult) -> bool {
        self.armed = false;
        let mut inner = self.inner.lock().expect("questions poisoned");
        let Some(entry) = inner.ledger.get_mut(&self.key) else {
            return false;
        };
        if entry.token != self.token {
            return false;
        }
        entry.done = true;
        entry.result = Some(result.clone());
        for waiter in entry.waiters.drain(..) {
            let _ = waiter.send(result.clone());
        }
        true
    }

    /// Record the in-flight `accepted` result mid-operation and release
    /// attached waiters with it — the oracle's `reply(op, result)` runs
    /// when the effect completes, before the watcher commits the terminal
    /// result via `UpdateLedgerResult`.
    fn mark_accepted(&self, result: StoredResult) {
        let mut inner = self.inner.lock().expect("questions poisoned");
        if let Some(entry) = inner.ledger.get_mut(&self.key) {
            if entry.token == self.token {
                entry.result = Some(result.clone());
                for waiter in entry.waiters.drain(..) {
                    let _ = waiter.send(result.clone());
                }
            }
        }
    }
}

impl Drop for Flight {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut inner = self.inner.lock().expect("questions poisoned");
        inner.ledger.remove(&self.key);
    }
}

impl Questions {
    /// `ReplayLedger` — the read-only admission lookup. A token mismatch
    /// deletes the entry like the oracle's generation check; an in-flight
    /// match attaches; a finished match replays.
    fn replay(&self, key: &str, hash: &str, token: &PaneToken) -> Lookup {
        let mut inner = self.inner.lock().expect("questions poisoned");
        self.prune_locked(&mut inner);
        let Some(entry) = inner.ledger.get_mut(key) else {
            return Lookup::Miss;
        };
        if entry.token != *token {
            inner.ledger.remove(key);
            return Lookup::Miss;
        }
        if entry.payload_hash != hash {
            return Lookup::Conflict;
        }
        // `existing.result != nil` — the oracle replays whatever result
        // the ledger already holds (`accepted` before the watcher lands,
        // the terminal phase afterwards); only a still-running effect
        // attaches as a waiter.
        if let Some(result) = &entry.result {
            return Lookup::Replay(result.clone());
        }
        let (tx, rx) = oneshot::channel();
        entry.waiters.push(tx);
        Lookup::Attach(rx)
    }

    /// `schedule` — claim the ledger slot for a fresh operation, with the
    /// oracle's re-check under the lock for the admission→effect race.
    fn schedule(&self, key: &str, hash: &str, token: &PaneToken) -> Admission {
        let mut inner = self.inner.lock().expect("questions poisoned");
        self.prune_locked(&mut inner);
        if let Some(entry) = inner.ledger.get_mut(key) {
            if entry.token != *token {
                inner.ledger.remove(key);
            } else if entry.payload_hash != hash {
                return Admission::Conflict;
            } else if let Some(result) = &entry.result {
                // `replyReplayed` — the stored result (`accepted` or the
                // watcher terminal) replays verbatim under this caller's
                // request id.
                return Admission::Replay(result.clone());
            } else {
                // In flight — this caller joins `existing.operation.waiters`
                // and resolves with the effect's result.
                let (tx, rx) = oneshot::channel();
                entry.waiters.push(tx);
                return Admission::Attach(rx);
            }
        }
        inner.ledger.insert(
            key.to_owned(),
            LedgerEntry {
                payload_hash: hash.to_owned(),
                result: None,
                token: token.clone(),
                waiters: Vec::new(),
                done: false,
                created_at: Instant::now(),
            },
        );
        Admission::Fresh(Flight {
            inner: self.inner.clone(),
            key: key.to_owned(),
            token: token.clone(),
            armed: true,
        })
    }

    /// `pruneLocked` — finished entries expire after the retention window;
    /// the count bound drops the oldest finished entries.
    fn prune_locked(&self, inner: &mut QuestionsInner) {
        let now = Instant::now();
        inner.ledger.retain(|_, entry| {
            !entry.done || now.duration_since(entry.created_at) < LEDGER_RETENTION
        });
        while inner.ledger.len() > MAX_LEDGER_ENTRIES {
            let oldest = inner
                .ledger
                .iter()
                .filter(|(_, entry)| entry.done)
                .min_by_key(|(_, entry)| entry.created_at)
                .map(|(key, _)| key.clone());
            match oldest {
                Some(key) => {
                    inner.ledger.remove(&key);
                }
                None => break,
            }
        }
    }

    /// `recordCustomAnswer` — remember typed `other` text so review-screen
    /// summaries can swap the `custom answer` placeholder back. The text
    /// is trimmed like the oracle's `strings.TrimSpace`.
    pub(crate) fn record_custom_answer(&self, pane_id: &str, question: &str, text: &str) {
        let key = summary_key(question);
        let text = text.trim();
        if pane_id.is_empty() || key.is_empty() || text.is_empty() {
            return;
        }
        let mut inner = self.inner.lock().expect("questions poisoned");
        let answers = inner.custom_answers.entry(pane_id.to_owned()).or_default();
        // `len(answers) >= 32` — a full map refuses a *new* key; rewriting
        // an existing one always succeeds.
        if answers.len() >= MAX_CUSTOM_ANSWERS
            && !answers.iter().any(|(existing, _)| existing == &key)
        {
            return;
        }
        answers.retain(|(existing, _)| existing != &key);
        answers.push((key, text.to_owned()));
    }

    /// `CustomAnswers` snapshot for `fill_custom_answers`.
    pub(crate) fn custom_answers(&self, pane_id: &str) -> HashMap<String, String> {
        let inner = self.inner.lock().expect("questions poisoned");
        inner
            .custom_answers
            .get(pane_id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect()
    }

    /// The per-pane effect lock — the scheduler's one-op-at-a-time pane
    /// slot.
    fn pane_lock(&self, pane_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut inner = self.inner.lock().expect("questions poisoned");
        if let Some(lock) = inner
            .pane_locks
            .get(pane_id)
            .and_then(std::sync::Weak::upgrade)
        {
            return lock;
        }
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        inner
            .pane_locks
            .insert(pane_id.to_owned(), Arc::downgrade(&lock));
        lock
    }

    /// `PendingApprovals` — the verified approval identity for a pane, or
    /// none when nothing has been verified since the pane (re)appeared.
    fn pending_approval(&self, pane_id: &str, token: &PaneToken) -> Option<PendingApproval> {
        let inner = self.inner.lock().expect("questions poisoned");
        let pending = inner.pending_approvals.get(pane_id)?;
        (pending.token == *token).then(|| pending.clone())
    }

    fn record_pending_approval(&self, pane_id: &str, pending: PendingApproval) {
        let mut inner = self.inner.lock().expect("questions poisoned");
        inner.pending_approvals.insert(pane_id.to_owned(), pending);
    }

    /// Clear the pending approval once the watcher observes resolution so
    /// a later request cannot replay a stale identity.
    fn clear_pending_approval(&self, pane_id: &str, fingerprint: &str) {
        let mut inner = self.inner.lock().expect("questions poisoned");
        if inner
            .pending_approvals
            .get(pane_id)
            .is_some_and(|pending| pending.fingerprint == fingerprint)
        {
            inner.pending_approvals.remove(pane_id);
        }
    }

    /// Attention-projection hook — the oracle's blocked-event record,
    /// kept for the session watcher that lands with the attention
    /// projection. The ledger itself is authoritative until then.
    #[allow(dead_code)]
    pub(crate) fn note_blocked(&self, pane_id: &str, event_id: String) {
        let mut inner = self.inner.lock().expect("questions poisoned");
        if let Some(pending) = inner.pending_approvals.get_mut(pane_id) {
            pending.event_id = event_id;
        }
    }

    /// `commitInventoryLocked`'s removal pass (state.go:578-579) — the
    /// pane's `customAnswers` and pending-approval records drop with every
    /// other per-pane ledger when the pane disappears.
    pub(crate) fn forget_pane(&self, pane_id: &str) {
        let mut inner = self.inner.lock().expect("questions poisoned");
        inner.custom_answers.remove(pane_id);
        inner.pending_approvals.remove(pane_id);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Effects + watchers — the dispatched halves of `handleApproval` and
// `handleQuestion`, plus the confirmation polls of `WatchApproval` and
// `watchQuestion`.
// ═══════════════════════════════════════════════════════════════════════

/// Remaining effect budget; `None` once the oracle's per-command deadline
/// has passed (mapped to the deadline-exceeded failure instead of a
/// zero-length RPC timeout).
fn remaining(deadline: Instant) -> Option<Duration> {
    deadline.checked_duration_since(Instant::now())
}

/// `readPane` — one `pane.read` bounded by the shared effect deadline.
async fn read_pane_text(
    ctx: &ActionContext,
    pane_id: &str,
    deadline: Instant,
) -> Result<String, HerdrError> {
    let Some(budget) = remaining(deadline) else {
        return Err(HerdrError::dispatched_msg("command deadline exceeded"));
    };
    let params = PaneReadParams::new(
        pane_id,
        ReadSource::RecentUnwrapped,
        PANE_READ_LINES,
        ReadFormat::Ansi,
    );
    let value = ctx
        .client
        .call_with_timeout("pane.read", &params, Some(budget))
        .await?;
    let read = value.get("read").cloned().unwrap_or_default();
    let read: PaneReadResult =
        serde_json::from_value(read).map_err(|err| HerdrError::dispatched_msg(err.to_string()))?;
    Ok(read.text)
}

/// `SendKeys`/`SendText` — one terminal write bounded by the deadline.
/// Shift+Tab rides `send_text` as the backtab escape like the Go client.
async fn send_key(
    ctx: &ActionContext,
    pane_id: &str,
    key: &str,
    deadline: Instant,
) -> Result<(), HerdrError> {
    let Some(budget) = remaining(deadline) else {
        return Err(HerdrError::dispatched_msg("command deadline exceeded"));
    };
    if key.eq_ignore_ascii_case("shift+tab") {
        #[derive(Serialize)]
        struct Text {
            pane_id: String,
            text: String,
        }
        ctx.client
            .call_with_timeout(
                "pane.send_text",
                &Text {
                    pane_id: pane_id.to_owned(),
                    text: SHIFT_TAB_SEQUENCE.to_owned(),
                },
                Some(budget),
            )
            .await?;
        return Ok(());
    }
    #[derive(Serialize)]
    struct Keys {
        pane_id: String,
        keys: Vec<String>,
    }
    ctx.client
        .call_with_timeout(
            "pane.send_keys",
            &Keys {
                pane_id: pane_id.to_owned(),
                keys: vec![key.to_owned()],
            },
            Some(budget),
        )
        .await?;
    Ok(())
}

/// `SendKeys` — the approval path's single call carrying the whole key
/// list (navigation + Enter dispatch together).
async fn send_keys(
    ctx: &ActionContext,
    pane_id: &str,
    keys: &[String],
    deadline: Instant,
) -> Result<(), HerdrError> {
    let Some(budget) = remaining(deadline) else {
        return Err(HerdrError::dispatched_msg("command deadline exceeded"));
    };
    #[derive(Serialize)]
    struct Keys {
        pane_id: String,
        keys: Vec<String>,
    }
    ctx.client
        .call_with_timeout(
            "pane.send_keys",
            &Keys {
                pane_id: pane_id.to_owned(),
                keys: keys.to_vec(),
            },
            Some(budget),
        )
        .await?;
    Ok(())
}

/// `SendText` — a free-text step of a question plan.
async fn send_text(
    ctx: &ActionContext,
    pane_id: &str,
    text: &str,
    deadline: Instant,
) -> Result<(), HerdrError> {
    let Some(budget) = remaining(deadline) else {
        return Err(HerdrError::dispatched_msg("command deadline exceeded"));
    };
    #[derive(Serialize)]
    struct Text {
        pane_id: String,
        text: String,
    }
    ctx.client
        .call_with_timeout(
            "pane.send_text",
            &Text {
                pane_id: pane_id.to_owned(),
                text: text.to_owned(),
            },
            Some(budget),
        )
        .await?;
    Ok(())
}

/// `sendQuestionKeysForSession`/`executeQuestion` — every key of a plan
/// step is its own `pane.send_keys` call with the oracle's 150ms pause
/// between keys *and* between steps; `paneSessionError` runs before each
/// write so a replaced pane stops mid-plan (`partiallyApplied` once any
/// request bytes reached Herdr).
async fn run_question_steps(
    ctx: &ActionContext,
    pane_id: &str,
    token: &PaneToken,
    steps: &[InputStep],
    deadline: Instant,
) -> Result<(), Box<Outcome>> {
    let mut dispatched = false;
    for (step_index, step) in steps.iter().enumerate() {
        if !step.text.is_empty() {
            if !token_current(ctx, pane_id, token) {
                return Err(step_stale(pane_id, dispatched));
            }
            match send_text(ctx, pane_id, &step.text, deadline).await {
                Ok(()) => dispatched = true,
                Err(err) => return Err(Box::new(step_failure(pane_id, &err, dispatched))),
            }
        }
        for (index, key) in step.keys.iter().enumerate() {
            if !token_current(ctx, pane_id, token) {
                return Err(step_stale(pane_id, dispatched));
            }
            match send_key(ctx, pane_id, key, deadline).await {
                Ok(()) => dispatched = true,
                Err(err) => {
                    return Err(Box::new(step_failure(
                        pane_id,
                        &err,
                        dispatched || err.may_have_applied(),
                    )));
                }
            }
            if index + 1 < step.keys.len() {
                tokio::time::sleep(KEY_DELAY).await;
            }
        }
        if step_index + 1 < steps.len() {
            tokio::time::sleep(KEY_DELAY).await;
        }
    }
    Ok(())
}

/// The `paneSessionError` mid-plan — `ErrPaneReplaced` resolves `failed`
/// while nothing was sent and `partiallyApplied` once it was.
fn step_stale(pane_id: &str, dispatched: bool) -> Box<Outcome> {
    if dispatched {
        return Box::new(Outcome::partially_applied(pane_id, ERR_PANE_REPLACED));
    }
    Box::new(Outcome::failed(pane_id, ERR_PANE_REPLACED))
}

/// The dispatch-boundary mapping for a failed step: unknown-after-dispatch
/// resolves `dispatched_unknown` (`partiallyApplied` wording), a
/// pre-dispatch failure falls back to `dispatch_failure`'s phases.
fn step_failure(pane_id: &str, err: &HerdrError, dispatched: bool) -> Outcome {
    if dispatched || err.may_have_applied() {
        return Outcome::partially_applied(pane_id, &err.to_string());
    }
    dispatch_failure(pane_id, err)
}

/// The `accepted` result the effect commits before the watcher runs —
/// `phase:"accepted"` + `awaiting_evidence` receipt.
fn accepted_outcome(pane_id: &str, data: Option<serde_json::Value>) -> Outcome {
    Outcome {
        ok: true,
        phase: "accepted",
        error: String::new(),
        pane_id: pane_id.to_owned(),
        data,
        receipt_phase: ActionReceiptPhase::AWAITING_EVIDENCE,
        receipt_error: None,
    }
}

/// Emit `[command_result(accepted), action_receipt(awaiting_evidence),
/// command_result(final)]` — the oracle's two-result command shape the
/// Kotlin client already understands.
fn watched_frames(
    request_id: &str,
    action: &str,
    action_id: &str,
    accepted: &Outcome,
    terminal: &Outcome,
) -> Vec<Outbound> {
    vec![
        command_result_frame(request_id, action, accepted),
        Outbound::ActionReceipt(action_receipt_response(
            request_id,
            ActionReceipt {
                action_id: action_id.to_owned(),
                phase: ActionReceiptPhase::from(accepted.receipt_phase),
                error: None,
            },
        )),
        command_result_frame(request_id, action, terminal),
    ]
}

/// `command_result_frame` copy — the private mod.rs builder, needed here
/// for the extra terminal frame of the accepted→final sequence.
fn command_result_frame(request_id: &str, action: &str, outcome: &Outcome) -> Outbound {
    Outbound::CommandResult(CommandResultMessage {
        r#type: "command_result".to_owned(),
        request_id: (!request_id.is_empty()).then(|| request_id.to_owned()),
        action: Some(action.to_owned()),
        ok: Some(outcome.ok),
        phase: Some(outcome.phase.to_owned()),
        error: Some(outcome.error.clone()),
        pane_id: Some(outcome.pane_id.clone()),
        data: outcome.data.as_ref().and_then(|value| {
            serde_json::value::to_raw_value(value)
                .ok()
                .map(|raw| MaybeNull::Value(RawJson(raw)))
        }),
    })
}

/// An attach (or replay) resolution — emit the stored result frame only;
/// the oracle's waiters get the `CommandResult`, not a fresh receipt pair.
fn attached_frames(request_id: &str, result: &StoredResult) -> Vec<Outbound> {
    vec![result.frame(request_id)]
}

/// `ErrClosed`-style resolution when a flight's sender disappears — the
/// oracle resolves attached waiters `not_started`.
fn attach_failed_frames(request_id: &str, action: &str, action_id: &str) -> Vec<Outbound> {
    Outcome::not_started_refusal("closed", "command was not sent; retry is safe", None)
        .frames(request_id, action, action_id)
}

// ── respond (`handleApproval`) ──────────────────────────────────────────

/// `handleApproval` — validate, gate on the pane's blocked state, run the
/// ledger, dispatch the keys, and watch for resolution.
pub(crate) async fn respond(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    let action = "approval";
    let (payload, pane_id) = match ApprovalPayload::decode(message) {
        Ok(decoded) => decoded,
        Err(error) => {
            return Outcome::failed(&message.pane_id, error).frames(request_id, action, action_id)
        }
    };

    let agent = ctx.topology.pane_of(&pane_id);
    let blocked = agent.is_some_and(|agent| agent.agent_status == AgentStatus::Blocked);
    let token = pane_token(&ctx.topology, &pane_id).unwrap_or_default();

    if blocked {
        // The oracle compares `agent.BlockedEventID` + the projected
        // options/fingerprint; here the verified pending record carries
        // the same identity until the watcher clears it.
        if let Some(pending) = ctx.questions.pending_approval(&pane_id, &token) {
            if pending.event_id != payload.event_id {
                return Outcome::failed(&pane_id, "This approval request is no longer current")
                    .frames(request_id, action, action_id);
            }
            if pending.fingerprint != payload.fingerprint {
                return Outcome::failed(&pane_id, "Approval choices are no longer available")
                    .frames(request_id, action, action_id);
            }
        }
    }

    let key = format!("approval\0{}\0{}", pane_id, payload.event_id);
    let hash = hash_payload(&payload);
    match ctx.questions.replay(&key, &hash, &token) {
        Lookup::Miss => {}
        Lookup::Replay(result) => return attached_frames(request_id, &result),
        Lookup::Conflict => {
            return Outcome::failed(&pane_id, "A different response was already submitted")
                .frames(request_id, action, action_id)
        }
        Lookup::Attach(rx) => {
            return match rx.await {
                Ok(result) => attached_frames(request_id, &result),
                Err(_) => attach_failed_frames(request_id, action, action_id),
            }
        }
    }

    if !blocked {
        return Outcome::failed(&pane_id, "Agent is no longer waiting for approval")
            .frames(request_id, action, action_id);
    }

    let flight = match ctx.questions.schedule(&key, &hash, &token) {
        Admission::Fresh(flight) => flight,
        Admission::Replay(result) => return attached_frames(request_id, &result),
        Admission::Conflict => {
            return Outcome::failed(&pane_id, "A different response was already submitted")
                .frames(request_id, action, action_id)
        }
        Admission::Attach(rx) => {
            return match rx.await {
                Ok(result) => attached_frames(request_id, &result),
                Err(_) => attach_failed_frames(request_id, action, action_id),
            }
        }
    };

    // ── effect — serialized per pane ──────────────────────────────────
    let lock = ctx.questions.pane_lock(&pane_id);
    let _guard = lock.lock().await;
    let deadline = Instant::now() + APPROVAL_DEADLINE;

    let effect = approval_effect(&ctx, &pane_id, &payload, &token, deadline).await;
    let (accepted, fingerprint) = match effect {
        Ok((accepted, fingerprint)) => (accepted, fingerprint),
        Err(outcome) => {
            let stored = StoredResult::new(action, &outcome);
            flight.finish(stored);
            return outcome.frames(request_id, action, action_id);
        }
    };
    flight.mark_accepted(StoredResult::new(action, &accepted));
    // `recordActivity("approval","approved",…)` — before the watcher arm.
    record_activity(
        &ctx,
        "approval",
        "approved",
        format!("Approved option {}", payload.index + 1),
        &pane_id,
        request_id,
    );
    drop(_guard);

    // ── watcher ───────────────────────────────────────────────────────
    let terminal = watch_approval(&ctx, &pane_id, &payload.event_id, &token, &fingerprint).await;
    // `commitAndBroadcastResult` — the terminal frame only goes out when
    // the ledger still owns this generation (`UpdateLedgerResult` → true).
    if let Some(outcome) = terminal {
        if flight.finish(StoredResult::new(action, &outcome)) {
            ctx.questions.clear_pending_approval(&pane_id, &fingerprint);
            return watched_frames(request_id, action, action_id, &accepted, &outcome);
        }
    }
    // Generation moved under the watcher — the oracle drops the update.
    vec![
        command_result_frame(request_id, action, &accepted),
        Outbound::ActionReceipt(action_receipt_response(
            request_id,
            ActionReceipt {
                action_id: action_id.to_owned(),
                phase: ActionReceiptPhase::from(accepted.receipt_phase),
                error: None,
            },
        )),
    ]
}

/// The `respond` effect — re-verify blocked, read + classify the pane,
/// compare the live approval identity, dispatch the keys.
async fn approval_effect(
    ctx: &ActionContext,
    pane_id: &str,
    payload: &ApprovalPayload,
    token: &PaneToken,
    deadline: Instant,
) -> Result<(Outcome, String), Box<Outcome>> {
    // `op.Run`'s state re-check — the pane must still be blocked, and a
    // previously verified approval identity (the `BlockedEventID`
    // surrogate) must still match this event.
    {
        let topology = ctx.handle.topology.borrow().clone();
        let still_current = topology.pane_of(pane_id).is_some_and(|agent| {
            agent.agent_status == AgentStatus::Blocked
                && ctx
                    .questions
                    .pending_approval(pane_id, token)
                    .is_none_or(|pending| pending.event_id == payload.event_id)
        });
        if !still_current {
            return Err(Box::new(Outcome::failed(
                pane_id,
                "This approval request is no longer current",
            )));
        }
    }
    let text = match read_pane_text(ctx, pane_id, deadline).await {
        Ok(text) => text,
        Err(err) => return Err(Box::new(dispatch_failure(pane_id, &err))),
    };
    let agent_name = ctx
        .topology
        .pane_of(pane_id)
        .and_then(|agent| agent.agent.clone())
        .unwrap_or_default();
    let classification = classify(&text, &agent_name);
    let live_fingerprint = approval_fingerprint(&classification);
    let index = payload.index as usize;
    let valid = classification.kind == AttentionKind::Approval
        && classification.options.len() == payload.total as usize
        && classification
            .options
            .get(index)
            .is_some_and(|label| label == &payload.choice)
        && live_fingerprint == payload.fingerprint;
    if !valid {
        return Err(Box::new(Outcome::failed(
            pane_id,
            "Approval choices are no longer available",
        )));
    }
    if !token_current(ctx, pane_id, token) {
        return Err(Box::new(Outcome::failed(pane_id, ERR_PANE_REPLACED)));
    }
    let keys = approval_keys(index, classification.approval_focus);
    if let Err(err) = send_keys(ctx, pane_id, &keys, deadline).await {
        return Err(Box::new(dispatch_failure(pane_id, &err)));
    }
    ctx.questions.record_pending_approval(
        pane_id,
        PendingApproval {
            event_id: payload.event_id.clone(),
            fingerprint: payload.fingerprint.clone(),
            token: token.clone(),
        },
    );
    // `completed(requestID, "approval", paneID, nil)` + `Phase="accepted"`.
    Ok((accepted_outcome(pane_id, None), payload.fingerprint.clone()))
}

/// `commitAndBroadcastPhase` — the approval watcher's terminal result is
/// `{ok: phase=="confirmed", phase}` — no error text, no data.
fn approval_terminal(pane_id: &str, confirmed: bool) -> Outcome {
    Outcome {
        ok: confirmed,
        phase: if confirmed {
            "confirmed"
        } else {
            "unconfirmed"
        },
        error: String::new(),
        pane_id: pane_id.to_owned(),
        data: None,
        receipt_phase: ActionReceiptPhase::CONFIRMED,
        receipt_error: None,
    }
}

/// `WatchApproval` — poll until the pane leaves the blocked+approval
/// state (`confirmed`), the generation moves (drop the update), or the
/// 5s window expires (`unconfirmed`). The oracle's tick order is kept:
/// generation, then the pane's blocked/event identity, then attention.
async fn watch_approval(
    ctx: &ActionContext,
    pane_id: &str,
    event_id: &str,
    token: &PaneToken,
    fingerprint: &str,
) -> Option<Outcome> {
    let deadline = Instant::now() + WATCH_TIMEOUT;
    loop {
        let agent_name;
        {
            let topology = ctx.handle.topology.borrow().clone();
            if pane_token(&topology, pane_id).is_some_and(|current| current != *token) {
                // The pane's identity moved under the request — the oracle
                // aborts the watcher and drops the update.
                return None;
            }
            let agent = topology.pane_of(pane_id);
            let resolved = match agent {
                // `!ok` — pane gone resolves the approval.
                None => true,
                Some(agent) => {
                    agent.agent_status != AgentStatus::Blocked
                        // `BlockedEventID == "" || != eventID` — the
                        // verified pending record carries the identity.
                        || ctx
                            .questions
                            .pending_approval(pane_id, token)
                            .is_none_or(|pending| pending.event_id != event_id)
                }
            };
            if resolved {
                return Some(approval_terminal(pane_id, true));
            }
            agent_name = agent
                .and_then(|agent| agent.agent.clone())
                .unwrap_or_default();
        }
        // `agent.AttentionKind != approval` — confirm the dialog is really
        // gone via classify (the live read is the same check without the
        // projection; the fingerprint compare covers a re-prompted dialog).
        if let Ok(text) = read_pane_text(ctx, pane_id, Instant::now() + WATCH_INTERVAL).await {
            let classification = classify(&text, &agent_name);
            if classification.kind != AttentionKind::Approval
                || approval_fingerprint(&classification) != fingerprint
            {
                return Some(approval_terminal(pane_id, true));
            }
        }
        let now = Instant::now();
        if now >= deadline {
            return Some(approval_terminal(pane_id, false));
        }
        tokio::time::sleep(WATCH_INTERVAL.min(deadline - now)).await;
    }
}

// ── question actions (`handleQuestion`) ─────────────────────────────────

/// Which wire action a question handler serves (`answer_question`,
/// `clarify_question`, `navigate_question`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuestionKind {
    Answer,
    Clarify,
    Navigate,
}

impl QuestionKind {
    fn action(self) -> &'static str {
        match self {
            QuestionKind::Answer => "answer_question",
            QuestionKind::Clarify => "clarify_question",
            QuestionKind::Navigate => "navigate_question",
        }
    }
}

pub(crate) async fn answer_question(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    run_question(ctx, request_id, action_id, message, QuestionKind::Answer).await
}

pub(crate) async fn clarify_question(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    run_question(ctx, request_id, action_id, message, QuestionKind::Clarify).await
}

pub(crate) async fn navigate_question(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    run_question(ctx, request_id, action_id, message, QuestionKind::Navigate).await
}

/// `handleQuestion` — the shared driver for the three question actions.
async fn run_question(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
    kind: QuestionKind,
) -> Vec<Outbound> {
    let pane_id = message.pane_id.clone();
    let action = kind.action();
    // `decodeQuestionPayload` runs before `submitQuestion`'s pane check —
    // a malformed payload reports its own error even on an empty pane id.
    let payload = match kind {
        QuestionKind::Answer => QuestionPayload::decode_answer(message),
        QuestionKind::Clarify => QuestionPayload::decode_clarify(message),
        QuestionKind::Navigate => QuestionPayload::decode_navigate(message),
    };
    let payload = match payload {
        Ok(payload) => payload,
        Err(error) => {
            return Outcome::failed(&pane_id, error).frames(request_id, action, action_id)
        }
    };
    if pane_id.is_empty() {
        return Outcome::failed("", "Agent is required").frames(
            request_id,
            "answer_question",
            action_id,
        );
    }

    let agent = ctx.topology.pane_of(&pane_id);
    let waiting = agent.is_some_and(|agent| {
        matches!(agent.agent_status, AgentStatus::Blocked | AgentStatus::Done)
    });
    let token = pane_token(&ctx.topology, &pane_id).unwrap_or_default();
    let key = question_ledger_key(action, &pane_id, &payload.interaction_id, request_id);
    let hash = hash_payload(&payload);

    match ctx.questions.replay(&key, &hash, &token) {
        Lookup::Miss => {}
        Lookup::Replay(result) => return attached_frames(request_id, &result),
        Lookup::Conflict => {
            return Outcome::failed(&pane_id, "A different response was already submitted")
                .frames(request_id, action, action_id)
        }
        Lookup::Attach(rx) => {
            return match rx.await {
                Ok(result) => attached_frames(request_id, &result),
                Err(_) => attach_failed_frames(request_id, action, action_id),
            }
        }
    }
    if !waiting {
        return Outcome::failed(&pane_id, "Agent is no longer waiting for a question")
            .frames(request_id, action, action_id);
    }

    let flight = match ctx.questions.schedule(&key, &hash, &token) {
        Admission::Fresh(flight) => flight,
        Admission::Replay(result) => return attached_frames(request_id, &result),
        Admission::Conflict => {
            return Outcome::failed(&pane_id, "A different response was already submitted")
                .frames(request_id, action, action_id)
        }
        Admission::Attach(rx) => {
            return match rx.await {
                Ok(result) => attached_frames(request_id, &result),
                Err(_) => attach_failed_frames(request_id, action, action_id),
            }
        }
    };

    // ── effect — serialized per pane ──────────────────────────────────
    let lock = ctx.questions.pane_lock(&pane_id);
    let _guard = lock.lock().await;
    let deadline = Instant::now() + QUESTION_DEADLINE;

    let effect = question_effect(&ctx, &pane_id, &payload, &token, deadline).await;
    let (accepted, submitted) = match effect {
        Ok(pair) => pair,
        Err(outcome) => {
            let stored = StoredResult::new(action, &outcome);
            flight.finish(stored);
            return outcome.frames(request_id, action, action_id);
        }
    };
    flight.mark_accepted(StoredResult::new(action, &accepted));
    drop(_guard);

    // ── watcher ───────────────────────────────────────────────────────
    let navigation = payload.navigation.clone();
    match watch_question(&ctx, &pane_id, action, &submitted, &navigation, &token).await {
        WatchVerdict::Finished(outcome) => {
            // `commitAndBroadcastResult` — the terminal frame only goes
            // out when the ledger still owns this generation.
            if flight.finish(StoredResult::new(action, &outcome)) {
                // The oracle records the committed navigation/answer.
                let summary = match navigation.as_str() {
                    "previous" => Some("Opened previous question"),
                    "next" => Some("Opened next question"),
                    _ if action == "answer_question" => Some("Answered question"),
                    _ => None,
                };
                if let Some(summary) = summary {
                    let status = if action == "answer_question" {
                        "answered"
                    } else {
                        "navigated"
                    };
                    record_activity(&ctx, "question", status, summary, &pane_id, request_id);
                }
                return watched_frames(request_id, action, action_id, &accepted, &outcome);
            }
            vec![
                command_result_frame(request_id, action, &accepted),
                Outbound::ActionReceipt(action_receipt_response(
                    request_id,
                    ActionReceipt {
                        action_id: action_id.to_owned(),
                        phase: ActionReceiptPhase::from(accepted.receipt_phase),
                        error: None,
                    },
                )),
            ]
        }
        WatchVerdict::Abort => {
            // Generation moved mid-watch — the oracle drops the update,
            // leaving the request at its accepted result.
            vec![
                command_result_frame(request_id, action, &accepted),
                Outbound::ActionReceipt(action_receipt_response(
                    request_id,
                    ActionReceipt {
                        action_id: action_id.to_owned(),
                        phase: ActionReceiptPhase::from(accepted.receipt_phase),
                        error: None,
                    },
                )),
            ]
        }
    }
}

/// `questionOperationLedgerKey` — pane+interaction+request scoped under
/// the oracle's per-family prefixes, so a retry of the same request
/// attaches while distinct submissions can never alias each other.
fn question_ledger_key(
    action: &str,
    pane_id: &str,
    interaction_id: &str,
    request_id: &str,
) -> String {
    let family = match action {
        "navigate_question" => "question-navigation",
        "clarify_question" => "question-clarification",
        _ => "question-answer",
    };
    format!(
        "{}\0{}\0{}\0{}",
        family, pane_id, interaction_id, request_id
    )
}

/// The question effect — the pane must still wait, the live parse must
/// match the submitted interaction id, the payload must apply, then the
/// planned key/text steps run under the shared deadline.
async fn question_effect(
    ctx: &ActionContext,
    pane_id: &str,
    payload: &QuestionPayload,
    token: &PaneToken,
    deadline: Instant,
) -> Result<(Outcome, Interaction), Box<Outcome>> {
    {
        let topology = ctx.handle.topology.borrow().clone();
        let waiting = topology.pane_of(pane_id).is_some_and(|agent| {
            matches!(agent.agent_status, AgentStatus::Blocked | AgentStatus::Done)
        });
        if !waiting {
            return Err(Box::new(Outcome::failed(
                pane_id,
                "The question changed before the answer was applied",
            )));
        }
    }
    let text = match read_pane_text(ctx, pane_id, deadline).await {
        Ok(text) => text,
        Err(err) => return Err(Box::new(dispatch_failure(pane_id, &err))),
    };
    let agent_name = ctx
        .topology
        .pane_of(pane_id)
        .and_then(|agent| agent.agent.clone())
        .unwrap_or_default();
    let interaction = parse_question(&text, &agent_name);
    // `RecordCustomAnswer` runs before the id check — a wrong-id
    // submission still leaves the typed answer for the review screen.
    if payload.other_selected {
        if let Some(interaction) = &interaction {
            ctx.questions
                .record_custom_answer(pane_id, &interaction.question, &payload.other_text);
        }
    }
    match interaction {
        Some(interaction) if interaction.id == payload.interaction_id => {
            if let Err(error) = validate_question_payload(&interaction, payload) {
                return Err(Box::new(Outcome::failed(pane_id, error)));
            }
            if !token_current(ctx, pane_id, token) {
                return Err(Box::new(Outcome::failed(pane_id, ERR_PANE_REPLACED)));
            }
            let steps = plan_input(&interaction, payload);
            run_question_steps(ctx, pane_id, token, &steps, deadline).await?;
            // `completed(requestID, action, paneID, nil)` +
            // `Phase="accepted"`; the submitted snapshot rides along for
            // `watchQuestion`'s `original`.
            Ok((accepted_outcome(pane_id, None), interaction))
        }
        _ => Err(Box::new(Outcome::failed(
            pane_id,
            "The question changed before the answer was applied",
        ))),
    }
}

/// `WatchVerdict` — the watcher either resolves a terminal result or
/// aborts when the pane's token moved (the oracle drops the update).
enum WatchVerdict {
    Finished(Outcome),
    Abort,
}

/// `watchQuestion` — poll until the submitted interaction leaves the
/// screen (finish the outcome), the pane's token moves (abort), or the
/// window expires (`unconfirmed`). The oracle's tick order is kept:
/// generation, pane presence, read+parse, expects-question continue.
async fn watch_question(
    ctx: &ActionContext,
    pane_id: &str,
    action: &str,
    original: &Interaction,
    navigation: &str,
    token: &PaneToken,
) -> WatchVerdict {
    let deadline = Instant::now() + WATCH_TIMEOUT;
    // `expectsQuestion` — navigation always anticipates another dialog;
    // an answer only does while the submitted question was mid-flow.
    let expects_question = !navigation.is_empty()
        || (action == "answer_question"
            && original.question_index > 0
            && original.question_index < original.question_total);
    loop {
        let agent_name;
        {
            let topology = ctx.handle.topology.borrow().clone();
            if pane_token(&topology, pane_id).is_some_and(|current| current != *token) {
                return WatchVerdict::Abort;
            }
            let Some(agent) = topology.pane_of(pane_id) else {
                return WatchVerdict::Finished(finish_question_watch(
                    pane_id, action, original, navigation, None,
                ));
            };
            agent_name = agent.agent.clone().unwrap_or_default();
        }
        let text = match read_pane_text(ctx, pane_id, Instant::now() + WATCH_INTERVAL).await {
            Ok(text) => text,
            Err(_) => {
                // `ReadPane` error — the oracle skips the tick entirely.
                if Instant::now() >= deadline {
                    return WatchVerdict::Finished(unconfirmed_question(pane_id));
                }
                tokio::time::sleep(WATCH_INTERVAL).await;
                continue;
            }
        };
        if !text.is_empty() {
            let mut current = parse_question(&text, &agent_name);
            if let Some(current) = &mut current {
                if !current.other.text.trim().is_empty() {
                    ctx.questions.record_custom_answer(
                        pane_id,
                        &current.question,
                        &current.other.text,
                    );
                }
                fill_custom_answers(current, &ctx.questions.custom_answers(pane_id));
            }
            if current.is_none() && expects_question {
                // The agent is repainting between questions — the oracle
                // keeps polling only while the pane is still `blocked`
                // with no resolved attention kind.
                let still_waiting = {
                    let topology = ctx.handle.topology.borrow().clone();
                    topology
                        .pane_of(pane_id)
                        .is_some_and(|agent| agent.agent_status == AgentStatus::Blocked)
                };
                if still_waiting {
                    let kind = classify(&text, &agent_name).kind;
                    if matches!(kind, AttentionKind::Unknown | AttentionKind::Question)
                        && Instant::now() < deadline
                    {
                        tokio::time::sleep(WATCH_INTERVAL).await;
                        continue;
                    }
                }
            }
            if current
                .as_ref()
                .is_none_or(|current| current.id != original.id)
            {
                return WatchVerdict::Finished(finish_question_watch(
                    pane_id,
                    action,
                    original,
                    navigation,
                    current.as_ref(),
                ));
            }
        }
        let now = Instant::now();
        if now >= deadline {
            return WatchVerdict::Finished(unconfirmed_question(pane_id));
        }
        tokio::time::sleep(WATCH_INTERVAL.min(deadline - now)).await;
    }
}

/// `watchQuestion`'s `ctx.Done` result — `{ok:false, phase:"unconfirmed"}`
/// with the oracle's retry hint.
fn unconfirmed_question(pane_id: &str) -> Outcome {
    Outcome {
        ok: false,
        phase: "unconfirmed",
        error: "The agent still shows the same question; try again".to_owned(),
        pane_id: pane_id.to_owned(),
        data: None,
        receipt_phase: ActionReceiptPhase::CONFIRMED,
        receipt_error: None,
    }
}

/// `finishQuestionWatch` — the terminal result once the submitted
/// interaction left the screen: the base result is a bare `confirmed`,
/// with `navigated`/`advanced`/`failed` overlays matching the oracle's
/// switch exactly (clarify always confirms).
fn finish_question_watch(
    pane_id: &str,
    action: &str,
    original: &Interaction,
    navigation: &str,
    current: Option<&Interaction>,
) -> Outcome {
    let mut outcome = Outcome {
        ok: true,
        phase: "confirmed",
        error: String::new(),
        pane_id: pane_id.to_owned(),
        data: None,
        receipt_phase: ActionReceiptPhase::CONFIRMED,
        receipt_error: None,
    };
    if !navigation.is_empty() {
        let expected = if navigation == "previous" {
            original.question_index - 1
        } else {
            original.question_index + 1
        };
        if let Some(current) = current {
            if expected > 0 && current.question_index == expected {
                outcome.phase = "navigated";
                outcome.data = Some(serde_json::json!({ "interaction": current }));
                return outcome;
            }
        }
        outcome.ok = false;
        outcome.phase = "failed";
        outcome.error =
            "The agent opened an unexpected question; the screen was refreshed".to_owned();
        if let Some(current) = current {
            outcome.data = Some(serde_json::json!({ "interaction": current }));
        }
        return outcome;
    }
    if action == "answer_question" {
        if let Some(current) = current {
            if original.question_index > 0 && current.question_index == original.question_index + 1
            {
                outcome.phase = "advanced";
            } else {
                outcome.ok = false;
                outcome.phase = "failed";
                outcome.error =
                    "The agent opened an unexpected question; the screen was refreshed".to_owned();
            }
            outcome.data = Some(serde_json::json!({ "interaction": current }));
        }
    }
    outcome
}

// ═══════════════════════════════════════════════════════════════════════
// Tests — conformance against the Go oracle's 94-vector
// `questions.interaction` fixture corpus plus the state-machine pieces
// that do not need a live Herdr.
// ═══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    /// The full `questions.interaction` suite generated from the oracle's
    /// `internal/question` package (`interaction_export_test.go`).
    fn vectors() -> Vec<Value> {
        lerdr_fixture::Suite::load("questions", "questions.interaction")
            .expect("questions.interaction fixture suite")
            .vectors
    }

    /// `pane_lines` joins with `\n` exactly like the oracle exporter's
    /// captured pane text.
    fn pane_text(vector: &Value) -> String {
        vector["pane_lines"]
            .as_array()
            .expect("pane_lines")
            .iter()
            .map(|line| line.as_str().unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn kind_str(kind: AttentionKind) -> &'static str {
        match kind {
            AttentionKind::Approval => "approval",
            AttentionKind::Question => "question",
            AttentionKind::Chat => "chat",
            AttentionKind::Unknown => "unknown",
        }
    }

    fn focus_str(kind: FocusKind) -> &'static str {
        match kind {
            FocusKind::Option => "option",
            FocusKind::Other => "other",
            FocusKind::Submit => "submit",
            FocusKind::Chat => "chat",
        }
    }

    /// Every vector's `expected_attention` must match `classify` on the
    /// captured pane text — kind, prompt, command, options, focus and the
    /// approval fingerprint all compare verbatim.
    #[test]
    fn fixture_attention_matches() {
        let mut failures = Vec::new();
        for vector in vectors() {
            let name = vector["name"].as_str().unwrap_or("?");
            let agent = vector["agent_kind"].as_str().unwrap_or_default();
            let expected = &vector["expected_attention"];
            let text = pane_text(&vector);
            let got = classify(&text, agent);
            let mut check = |field: &str, want: Value, got: Value| {
                if want != got {
                    failures.push(format!("{name}: attention.{field}: want {want}, got {got}"));
                }
            };
            check("kind", expected["kind"].clone(), json!(kind_str(got.kind)));
            check(
                "prompt",
                expected.get("prompt").cloned().unwrap_or_else(|| json!("")),
                json!(got.prompt),
            );
            check(
                "command",
                expected
                    .get("command")
                    .cloned()
                    .unwrap_or_else(|| json!("")),
                json!(got.command),
            );
            if let Some(options) = expected.get("options") {
                check("options", options.clone(), json!(got.options));
            }
            if let Some(focus) = expected.get("approval_focus") {
                check("approval_focus", focus.clone(), json!(got.approval_focus));
            }
            if let Some(fingerprint) = expected.get("approval_fingerprint") {
                check(
                    "approval_fingerprint",
                    fingerprint.clone(),
                    json!(approval_fingerprint(&got)),
                );
            }
            check(
                "question_layout",
                expected["question_layout"].clone(),
                json!(got.question_layout),
            );
            check(
                "layout_hint",
                expected["layout_hint"].clone(),
                json!(layout_hint(&text)),
            );
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// Vectors carrying `expected_interaction` must reproduce the parsed
    /// interaction byte-for-byte (the serialized `Interaction` IS the wire
    /// shape); `null` expectations must yield no interaction. The oracle
    /// exports `classification.Interaction` — unsupported agents never reach
    /// `Parse`, so the comparison goes through `classify`, not `parse`.
    #[test]
    fn fixture_interactions_match() {
        let mut failures = Vec::new();
        for vector in vectors() {
            let name = vector["name"].as_str().unwrap_or("?");
            let agent = vector["agent_kind"].as_str().unwrap_or_default();
            let expected = &vector["expected_interaction"];
            let text = pane_text(&vector);
            let got = classify(&text, agent).interaction;
            match (expected.is_null(), got) {
                (true, None) => {}
                (true, Some(got)) => {
                    failures.push(format!(
                        "{name}: expected no interaction, got {}",
                        serde_json::to_string(&got).unwrap_or_default()
                    ));
                }
                (false, None) => {
                    failures.push(format!("{name}: expected interaction, got none"));
                }
                (false, Some(got)) => {
                    let got = serde_json::to_value(&got).unwrap_or_default();
                    if got != *expected {
                        failures.push(format!(
                            "{name}: interaction mismatch\n  want {}\n  got  {}",
                            serde_json::to_string(expected).unwrap_or_default(),
                            serde_json::to_string(&got).unwrap_or_default()
                        ));
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// `interaction_internal` mirrors the fields the wire shape skips:
    /// focus kind/index, `all_option_count`, `notes_active`, agent family.
    #[test]
    fn fixture_internals_match() {
        let mut failures = Vec::new();
        for vector in vectors() {
            let name = vector["name"].as_str().unwrap_or("?");
            let Some(expected) = vector.get("interaction_internal") else {
                continue;
            };
            if expected.is_null() {
                continue;
            }
            let agent = vector["agent_kind"].as_str().unwrap_or_default();
            let text = pane_text(&vector);
            let Some(got) = parse_question(&text, agent) else {
                failures.push(format!("{name}: expected internals, got no interaction"));
                continue;
            };
            let checks: [(&str, Value, Value); 5] = [
                (
                    "focus_kind",
                    expected["focus_kind"].clone(),
                    json!(focus_str(got.focus.kind)),
                ),
                (
                    "focus_index",
                    expected["focus_index"].clone(),
                    json!(got.focus.index),
                ),
                (
                    "notes_active",
                    expected["notes_active"].clone(),
                    json!(got.notes_active),
                ),
                (
                    "all_option_count",
                    expected["all_option_count"].clone(),
                    json!(got.all_option_count),
                ),
                ("agent", expected["agent"].clone(), json!(got.agent)),
            ];
            for (field, want, got) in checks {
                if want != got {
                    failures.push(format!("{name}: internal.{field}: want {want}, got {got}"));
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    // ── payload decoding ─────────────────────────────────────────────

    /// `Inbound::decode_map` — the wire decode path, including number
    /// normalization for `index`/`total`/`selected_indices`. `type` is
    /// required on the wire, so the harness injects the command frame's.
    fn inbound(mut fields: Value) -> Inbound {
        fields
            .as_object_mut()
            .expect("object")
            .entry("type")
            .or_insert_with(|| json!("command"));
        Inbound::decode_map(fields.as_object().expect("object")).expect("inbound")
    }

    #[test]
    fn approval_decode_validates_like_the_oracle() {
        let base = || {
            inbound(json!({
                "pane_id": "p1",
                "event_id": "e1",
                "approval_fingerprint": "fp",
                "choice": "Approve",
                "index": 0,
                "total": 2,
            }))
        };
        assert!(ApprovalPayload::decode(&base()).is_ok());
        for (patch, error) in [
            (
                json!({"pane_id": ""}),
                "Agent and exact approval identity are required",
            ),
            (
                json!({"event_id": ""}),
                "Agent and exact approval identity are required",
            ),
            (
                json!({"approval_fingerprint": ""}),
                "Agent and exact approval identity are required",
            ),
            (
                json!({"choice": ""}),
                "Agent and exact approval identity are required",
            ),
            (
                json!({"total": 1}),
                "Approval choice is no longer available",
            ),
            (
                json!({"total": 21}),
                "Approval choice is no longer available",
            ),
            (
                json!({"index": -1}),
                "Approval choice is no longer available",
            ),
            (
                json!({"index": 2, "total": 2}),
                "Approval choice is no longer available",
            ),
        ] {
            let mut merged = serde_json::to_value(base()).unwrap_or_default();
            merged
                .as_object_mut()
                .expect("object")
                .extend(patch.as_object().expect("patch").clone());
            let message = inbound(merged);
            let Err(got) = ApprovalPayload::decode(&message) else {
                panic!("{patch} should fail with {error:?}");
            };
            assert_eq!(got, error, "patch {patch}");
        }
        // Defaults: missing index → 0, missing total → 2 (the oracle's
        // `intValue` fallbacks).
        let mut merged = serde_json::to_value(base()).unwrap_or_default();
        merged.as_object_mut().expect("object").remove("index");
        merged.as_object_mut().expect("object").remove("total");
        let (payload, _) = ApprovalPayload::decode(&inbound(merged)).expect("defaults");
        assert_eq!((payload.index, payload.total), (0, 2));
    }

    #[test]
    fn question_decode_answer_validates_like_the_oracle() {
        let base = || {
            inbound(json!({
                "pane_id": "p1",
                "interaction_id": "q1",
                "selected_indices": [0],
            }))
        };
        for (patch, error) in [
            (
                json!({"interaction_id": ""}),
                "agent and question are required",
            ),
            (
                json!({"selected_indices": [], "other_selected": false, "other_text": ""}),
                "choose an answer or enter an Other answer",
            ),
            (
                json!({"selected_indices": [-1]}),
                "invalid question selection",
            ),
            (
                json!({"selected_indices": [], "other_selected": false, "other_text": "hi"}),
                "other text must be selected",
            ),
        ] {
            let mut merged = serde_json::to_value(base()).unwrap_or_default();
            merged
                .as_object_mut()
                .expect("object")
                .extend(patch.as_object().expect("patch").clone());
            let Err(got) = QuestionPayload::decode_answer(&inbound(merged)) else {
                panic!("{patch} should fail with {error:?}");
            };
            assert_eq!(got, error, "patch {patch}");
        }
        // Selection dedupe+sort happens at decode (the ledger hash then
        // treats reordered duplicates as identical, like `uniqueInts`).
        let payload = QuestionPayload::decode_answer(&inbound(json!({
            "pane_id": "p1",
            "interaction_id": "q1",
            "selected_indices": [2, 0, 2],
        })))
        .expect("sorted unique");
        assert_eq!(payload.selected, vec![0, 2]);
        // other_selected alone satisfies the answer requirement.
        assert!(QuestionPayload::decode_answer(&inbound(json!({
            "pane_id": "p1",
            "interaction_id": "q1",
            "selected_indices": [],
            "other_selected": true,
        })))
        .is_ok());
    }

    #[test]
    fn question_decode_clarify_navigate_match_the_oracle() {
        assert_eq!(
            QuestionPayload::decode_clarify(&inbound(json!({"interaction_id": ""}))).unwrap_err(),
            "Agent and question are required"
        );
        assert!(QuestionPayload::decode_clarify(&inbound(json!({"interaction_id": "q"}))).is_ok());
        // Direction is checked before the interaction id.
        assert_eq!(
            QuestionPayload::decode_navigate(&inbound(json!({
                "direction": "sideways",
                "interaction_id": "",
            })))
            .unwrap_err(),
            "Question navigation is no longer available"
        );
        assert_eq!(
            QuestionPayload::decode_navigate(&inbound(json!({
                "direction": "next",
                "interaction_id": "",
            })))
            .unwrap_err(),
            "Question is required"
        );
        let payload = QuestionPayload::decode_navigate(&inbound(json!({
            "direction": "previous",
            "interaction_id": "q",
        })))
        .expect("navigate");
        assert_eq!(payload.navigation, "previous");
    }

    // ── approval key planning ────────────────────────────────────────

    #[test]
    fn approval_keys_navigate_then_enter() {
        assert_eq!(approval_keys(2, 0), vec!["Down", "Down", "Enter"]);
        assert_eq!(approval_keys(0, 2), vec!["Up", "Up", "Enter"]);
        assert_eq!(approval_keys(1, 1), vec!["Enter"]);
    }

    // ── finish_question_watch ────────────────────────────────────────

    fn original(index: i64, total: i64) -> Interaction {
        Interaction {
            id: "q1".to_owned(),
            question_index: index,
            question_total: total,
            ..Interaction::default()
        }
    }

    fn next_question(index: i64, total: i64) -> Interaction {
        Interaction {
            id: "q2".to_owned(),
            question_index: index,
            question_total: total,
            ..Interaction::default()
        }
    }

    #[test]
    fn finish_watch_navigate_compares_the_expected_index() {
        let submitted = original(2, 4);
        // "next" lands on index+1 → navigated + the new interaction.
        let outcome = finish_question_watch(
            "p",
            "navigate_question",
            &submitted,
            "next",
            Some(&next_question(3, 4)),
        );
        assert_eq!((outcome.ok, outcome.phase), (true, "navigated"));
        // Landing anywhere else (or nowhere) → failed + refresh hint.
        let outcome = finish_question_watch("p", "navigate_question", &submitted, "next", None);
        assert_eq!((outcome.ok, outcome.phase), (false, "failed"));
        let outcome = finish_question_watch(
            "p",
            "navigate_question",
            &submitted,
            "next",
            Some(&next_question(4, 4)),
        );
        assert_eq!((outcome.ok, outcome.phase), (false, "failed"));
        // "previous" from index 1 expects 0 → failed (`expected > 0`).
        let submitted = original(1, 4);
        let outcome = finish_question_watch(
            "p",
            "navigate_question",
            &submitted,
            "previous",
            Some(&next_question(0, 4)),
        );
        assert_eq!((outcome.ok, outcome.phase), (false, "failed"));
    }

    #[test]
    fn finish_watch_answer_advanced_confirmed_or_failed() {
        let original = original(1, 3);
        // Index+1 → advanced.
        let outcome = finish_question_watch(
            "p",
            "answer_question",
            &original,
            "",
            Some(&next_question(2, 3)),
        );
        assert_eq!((outcome.ok, outcome.phase), (true, "advanced"));
        // A different follow-up → failed with the unexpected-question hint.
        let outcome = finish_question_watch(
            "p",
            "answer_question",
            &original,
            "",
            Some(&next_question(1, 3)),
        );
        assert_eq!((outcome.ok, outcome.phase), (false, "failed"));
        // Screen cleared → bare confirmed, no data payload.
        let outcome = finish_question_watch("p", "answer_question", &original, "", None);
        assert_eq!((outcome.ok, outcome.phase), (true, "confirmed"));
        assert!(outcome.data.is_none());
        // Clarify always confirms once the dialog leaves.
        let outcome = finish_question_watch(
            "p",
            "clarify_question",
            &original,
            "",
            Some(&next_question(2, 3)),
        );
        assert_eq!((outcome.ok, outcome.phase), (true, "confirmed"));
    }

    // ── ledger admission ─────────────────────────────────────────────

    fn token() -> PaneToken {
        PaneToken {
            terminal_id: "t1".to_owned(),
            tab_id: "tab1".to_owned(),
            session_source: "s".to_owned(),
            session_value: "v".to_owned(),
        }
    }

    fn stored(action: &str, phase: &'static str) -> StoredResult {
        StoredResult {
            action: action.to_owned(),
            ok: phase != "failed",
            phase,
            error: String::new(),
            pane_id: "p".to_owned(),
            data: None,
        }
    }

    #[test]
    fn ledger_replays_stored_results_and_attaches_in_flight() {
        let questions = Questions::default();
        let token = token();
        let key = "k1";
        // Fresh claim → in flight; a second caller attaches.
        let Admission::Fresh(flight) = questions.schedule(key, "h1", &token) else {
            panic!("first schedule must be fresh");
        };
        match questions.replay(key, "h1", &token) {
            Lookup::Attach(_) => {}
            _ => panic!("in-flight entry must attach"),
        }
        match questions.schedule(key, "h1", &token) {
            Admission::Attach(_) => {}
            _ => panic!("in-flight entry must attach at schedule"),
        }
        // Same key, different payload → conflict.
        assert!(matches!(
            questions.replay(key, "h2", &token),
            Lookup::Conflict
        ));
        // The effect commits `accepted`: waiters resolve with it and a
        // fresh lookup replays it — the oracle replays whatever result the
        // ledger holds.
        flight.mark_accepted(stored("answer_question", "accepted"));
        match questions.replay(key, "h1", &token) {
            Lookup::Replay(result) => assert_eq!(result.phase, "accepted"),
            _ => panic!("stored accepted must replay"),
        }
        // Terminal commit replaces the stored result for later replays.
        assert!(flight.finish(stored("answer_question", "confirmed")));
        match questions.replay(key, "h1", &token) {
            Lookup::Replay(result) => assert_eq!(result.phase, "confirmed"),
            _ => panic!("stored terminal must replay"),
        }
    }

    #[test]
    fn ledger_token_mismatch_evicts_stale_entries() {
        let questions = Questions::default();
        let moved = PaneToken {
            terminal_id: "t2".to_owned(),
            ..token()
        };
        let _flight = match questions.schedule("k", "h", &token()) {
            Admission::Fresh(flight) => flight,
            _ => panic!("fresh"),
        };
        // A replacement pane's token mismatch removes the entry instead of
        // attaching the new request to the dead pane's operation.
        assert!(matches!(questions.replay("k", "h", &moved), Lookup::Miss));
    }

    #[test]
    fn ledger_dropped_flight_releases_the_slot() {
        let questions = Questions::default();
        let flight = match questions.schedule("k", "h", &token()) {
            Admission::Fresh(flight) => flight,
            _ => panic!("fresh"),
        };
        drop(flight);
        // Abandoned in-flight slots never wedge the key.
        assert!(matches!(
            questions.schedule("k", "h", &token()),
            Admission::Fresh(_)
        ));
    }

    #[test]
    fn question_ledger_key_uses_the_oracle_families() {
        assert_eq!(
            question_ledger_key("answer_question", "p", "q", "r"),
            "question-answer\0p\0q\0r"
        );
        assert_eq!(
            question_ledger_key("clarify_question", "p", "q", "r"),
            "question-clarification\0p\0q\0r"
        );
        assert_eq!(
            question_ledger_key("navigate_question", "p", "q", "r"),
            "question-navigation\0p\0q\0r"
        );
    }
}
