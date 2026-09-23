//! Per-pane attention ledger — the `coordinator.State` halves that are
//! *not* the Herdr snapshot (state.go): committed classifications, blocked
//! event ids, the revision counters, and the unseen/ack/done bookkeeping
//! that projects `done`/`idle` display states.
//!
//! The snapshot rows live on `Topology`; everything semantic lives here.
//! One [`SharedLedger`] is shared by the actor's `Topology` and every
//! published clone, so a classification committed by the projector is
//! visible to the next `agents` projection and to mid-read fences without
//! a new snapshot.
//!
//! What the oracle keys on `*AgentState` (event id, kind, prompt, command,
//! options, fingerprint, interaction, layout, interaction id) lives on
//! [`AttentionCell::blocked`]; the rest of the cell mirrors the `State`
//! maps (`revision`/`contentRev`/`attentionRev`/`completionRev`,
//! `prevStatus`, `unseenDone`, `ackDone`, `finishedNotif`, and the
//! `LastSeenAt` ack override).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use base64::Engine;
use lerdr_core::json::MaybeNull;
use lerdr_core::protocol::{QuestionInteraction, QuestionOption, QuestionOther, SummaryEntry};

use super::attention::{approval_fingerprint, classify, AttentionKind, Classification};
use super::model::{fill_custom_answers, Interaction};
use super::noecho::match_prompt;
use crate::actions::questions::Questions;

/// `attentionStatuses` (state.go:175-178) — the statuses that still own
/// the operator's attention stream.
pub(crate) fn is_attention_status(status: &str) -> bool {
    matches!(status, "working" | "blocked")
}

/// `doneStatuses` (state.go:180-188) — Herdr's terminal vocabulary.
pub(crate) fn is_done_status(status: &str) -> bool {
    matches!(
        status,
        "done" | "complete" | "completed" | "finished" | "success" | "succeeded" | "unread"
    )
}

/// `isClaudeLike` (server.go:2179-2182) — the alternate-screen agents
/// whose transcripts the history merge handles (`claude`/`qoder`,
/// case-insensitive substring — the oracle's own match).
pub(crate) fn is_claude_like(agent: &str) -> bool {
    let lower = agent.to_lowercase();
    lower.contains("claude") || lower.contains("qoder")
}

/// The semantic halves `clearBlockedDetails`/`copyBlockedDetails` move as
/// a unit (state.go:716-736). `kind: None` is the oracle's `""` — no
/// committed classification — distinct from `AttentionKind::Unknown`
/// ("unknown"), which a blocked pane carries between minting and the
/// projector's first commit.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct BlockedDetails {
    pub event_id: String,
    pub kind: Option<AttentionKind>,
    pub prompt: String,
    pub command: String,
    pub options: Vec<String>,
    pub approval_fingerprint: String,
    pub interaction: Option<Interaction>,
    pub question_layout: bool,
    pub interaction_id: String,
}

impl BlockedDetails {
    fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// The per-pane record `s.agents[paneID]`'s semantic halves plus the
/// `State` map entries fold into.
#[derive(Debug, Clone, Default)]
pub(crate) struct AttentionCell {
    /// Committed blocked-cycle details — cleared wholesale when the pane
    /// leaves `blocked` (`applyBlockedCycleLocked`).
    pub blocked: BlockedDetails,
    /// `s.revision[paneID]` — the commit epoch this pane last moved under
    /// (`StateRevision`/`pane_revision` on the wire).
    pub state_rev: i64,
    /// `s.contentRev[paneID]` — status/name/cwd/agent/activitySeq/attention
    /// changes; the mid-read fence `HandleReadPane` checks.
    pub content_rev: i64,
    /// `s.attentionRev[paneID]` — blocked-detail changes; the push key's
    /// `interaction_revision`.
    pub attention_rev: i64,
    /// `s.completionRev[paneID]` — the revision a completion (or chat
    /// classification) is anchored to; cleared by attention churn.
    pub completion_rev: i64,
    /// `s.prevStatus[paneID]` — the status the last commit recorded.
    pub prev_status: String,
    /// `s.unseenDone[paneID]` — a completion no client has acknowledged.
    pub unseen_done: bool,
    /// `s.ackDone[paneID]` — a done-status the client acknowledged.
    pub ack_done: bool,
    /// `s.finishedNotif[paneID]` — the finished notification fired for the
    /// current completion cycle.
    pub finished_notif: bool,
    /// `LastSeenAt` — `AcknowledgePane` writes `max(now, last_active_at)`;
    /// `AgentTimes` keeps the observation-time half.
    pub last_seen_at: i64,
    /// `agent.SessionName` (server.go:509-534) — the title the
    /// `session.Resolver` resolved for the committed row's
    /// `agent_session`; `""` while unresolved. Lives on the cell (not the
    /// snapshot) so published clones project it through the shared
    /// ledger and the `!seen` removal pass reclaims it with the row.
    pub session_name: String,
}

impl AttentionCell {
    /// `clearBlockedDetails` (state.go:716-727) — drop every committed
    /// semantic field. Returns whether anything was set (the
    /// `!blockedDetailsEqual` half of `attentionChanged`).
    pub(crate) fn clear_blocked(&mut self) -> bool {
        let changed = !self.blocked.is_empty();
        self.blocked = BlockedDetails::default();
        changed
    }

    /// The `replaced` wipe (state.go:460-470): a pane-session replacement
    /// clears the done/ack/notification ledgers and the committed blocked
    /// state, but *not* the revision counters or the generation.
    pub(crate) fn reset_on_replacement(&mut self) {
        self.blocked = BlockedDetails::default();
        self.prev_status.clear();
        self.unseen_done = false;
        self.ack_done = false;
        self.finished_notif = false;
        self.completion_rev = 0;
        self.session_name.clear();
    }

    /// `applyBlockedCycleLocked` (state.go:687-708) — entering blocked
    /// mints a fresh event id and seeds `unknown`; leaving clears every
    /// blocked field; staying blocked keeps the committed values.
    /// Returns `attentionChanged` — the `!blockedDetailsEqual(existing,
    /// &cp)` bit that drives `attentionRev` and the approval refire.
    pub(crate) fn sync_cycle(&mut self, status: &str, mint: &mut dyn FnMut() -> String) -> bool {
        if status != "blocked" {
            return self.clear_blocked();
        }
        let mut changed = false;
        if self.blocked.kind.is_none() {
            self.blocked.kind = Some(AttentionKind::Unknown);
            changed = true;
        }
        if self.blocked.event_id.is_empty() {
            self.blocked.event_id = mint();
            changed = true;
        }
        changed
    }

    /// `applyAttentionClassification` (state.go:884-921) — write a
    /// classification into the committed halves, kind-gating `options`
    /// (approval), `interaction`/`question_layout`/`interaction_id`
    /// (question), and `approval_fingerprint`. Returns whether the
    /// committed tuple moved.
    pub(crate) fn apply_classification(&mut self, classification: &Classification) -> bool {
        let options = if classification.kind == AttentionKind::Approval {
            classification.options.clone()
        } else {
            Vec::new()
        };
        let fingerprint = if classification.kind == AttentionKind::Approval {
            approval_fingerprint(classification)
        } else {
            String::new()
        };
        let (interaction, question_layout, interaction_id) =
            if classification.kind == AttentionKind::Question {
                let interaction = classification.interaction.clone();
                let interaction_id = interaction
                    .as_ref()
                    .map(|interaction| interaction.id.clone())
                    .unwrap_or_default();
                (interaction, classification.question_layout, interaction_id)
            } else {
                (None, false, String::new())
            };
        // `interactionsEqual` — identity by `ID` only (state.go:950-955).
        let same_interaction = match (&self.blocked.interaction, &interaction) {
            (None, None) => true,
            (Some(left), Some(right)) => left.id == right.id,
            _ => false,
        };
        let changed = self.blocked.kind != Some(classification.kind)
            || self.blocked.prompt != classification.prompt
            || self.blocked.command != classification.command
            || self.blocked.options != options
            || self.blocked.approval_fingerprint != fingerprint
            || !same_interaction
            || self.blocked.question_layout != question_layout
            || self.blocked.interaction_id != interaction_id;
        self.blocked.kind = Some(classification.kind);
        self.blocked.prompt = classification.prompt.clone();
        self.blocked.command = classification.command.clone();
        self.blocked.options = options;
        self.blocked.approval_fingerprint = fingerprint;
        self.blocked.interaction = interaction;
        self.blocked.question_layout = question_layout;
        self.blocked.interaction_id = interaction_id;
        changed
    }

    /// `syncAttentionCompletionLocked` (state.go:824-839) — entering
    /// `chat` arms a completion cycle at the current state revision;
    /// leaving `chat` drops it.
    pub(crate) fn sync_attention_completion(
        &mut self,
        previous: Option<AttentionKind>,
        current: Option<AttentionKind>,
        state_rev: i64,
    ) {
        if current == Some(AttentionKind::Chat) {
            if previous != Some(AttentionKind::Chat) {
                self.finished_notif = false;
                self.completion_rev = state_rev;
            }
            return;
        }
        if previous == Some(AttentionKind::Chat) {
            self.finished_notif = false;
            self.completion_rev = 0;
        }
    }

    /// `DisplayedStatus` (state.go:976-991) — the status clients see:
    /// an acknowledged done reads `idle`; an unacknowledged idle that
    /// completed reads `done`.
    pub(crate) fn displayed_status(&self, status: &str) -> String {
        if is_done_status(status) && self.ack_done {
            return "idle".to_owned();
        }
        if status == "idle" && self.unseen_done {
            return "done".to_owned();
        }
        status.to_owned()
    }

    /// `AcknowledgePane` (state.go:957-974): `LastSeenAt` moves to
    /// `max(now, last_active_at)`; a done-status or pending unseen
    /// completion is consumed into `ack_done`.
    pub(crate) fn acknowledge(&mut self, status: &str, last_active_at: i64, now: i64) {
        self.last_seen_at = now.max(last_active_at);
        if self.unseen_done || is_done_status(status) {
            self.unseen_done = false;
            self.ack_done = true;
        }
    }

    /// `registerTransition` (state.go:760-822) — the once-per-cycle
    /// notification state machine. `revision` is this commit's epoch.
    /// Returns the transition the projector should run, if any.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn register_transition(
        &mut self,
        pane_id: &str,
        agent: &str,
        project: &str,
        prev: &str,
        status: &str,
        previous_attention: Option<AttentionKind>,
        revision: i64,
    ) -> Option<PaneTransition> {
        let transition = |status: &str, revision: i64| {
            Some(PaneTransition {
                pane_id: pane_id.to_owned(),
                agent: agent.to_owned(),
                project: project.to_owned(),
                status: status.to_owned(),
                revision,
                observed_at: now_millis(),
            })
        };
        if is_attention_status(status) {
            if prev != status {
                self.unseen_done = false;
                self.ack_done = false;
                self.finished_notif = false;
                self.completion_rev = 0;
            }
            if (status == "blocked" && prev != "blocked")
                || (status == "working" && prev != "working")
            {
                return transition(status, revision);
            }
            return None;
        }
        if is_done_status(status) {
            if is_attention_status(prev) {
                self.ack_done = false;
                self.unseen_done = true;
                self.completion_rev = revision;
                return transition(status, revision);
            }
            return None;
        }
        // §9.8: working/blocked → idle is the common completion path for
        // agents that don't emit an explicit done status.
        if status == "idle" && is_attention_status(prev) {
            // blocked(chat) → idle already fired its finished
            // notification at classification time — nothing re-arms.
            if prev == "blocked" && previous_attention == Some(AttentionKind::Chat) {
                return None;
            }
            self.ack_done = false;
            self.unseen_done = true;
            self.completion_rev = revision;
            return transition(status, revision);
        }
        None
    }
}

/// `preservesChatCompletion` (state.go:841-848) — blocked(chat) → idle
/// keeps the chat completion cycle instead of re-arming a generic one.
pub(crate) fn preserves_chat_completion(
    previous_status: &str,
    current_status: &str,
    previous_attention: Option<AttentionKind>,
) -> bool {
    previous_status == "blocked"
        && current_status == "idle"
        && previous_attention == Some(AttentionKind::Chat)
}

/// `newBlockedEventIDLocked` (state.go:710-714) — 96 bits of randomness
/// as unpadded base64url. The oracle's `blocked-{nanos}-{seq}` counter
/// fallback only runs when `rand.Read` fails; `ThreadRng::fill` is
/// infallible, so the fallback is unreachable here.
pub(crate) fn mint_blocked_event_id() -> String {
    let mut bytes = [0u8; 12];
    rand::Rng::fill(&mut rand::rng(), &mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// One `onTransition` record — `commitInventoryLocked` hands the
/// projector `(paneID, agent, project, status, revision)`; `observed_at`
/// is the commit-time `transitionAt` the activity row stamps.
#[derive(Debug, Clone)]
pub(crate) struct PaneTransition {
    pub pane_id: String,
    /// `cp.Agent` at commit time — the display name (`activity.agent`
    /// attribution and the push payload's agent name).
    pub agent: String,
    /// `cp.Project` — basename(cwd) at commit time.
    pub project: String,
    /// `cp.Status` — `"blocked"`, `"working"`, a done status, or `"idle"`.
    pub status: String,
    /// `s.revision[paneID]` — the commit epoch (state_rev).
    pub revision: i64,
    /// `transitionAt` — the accept-time timestamp activity rows stamp.
    pub observed_at: i64,
}

/// The ledger — `BTreeMap` for the same reason the oracle sorts
/// `snapshotLocked`: deterministic iteration when debugging/tests dump it.
#[derive(Debug, Default)]
pub(crate) struct AttentionLedger {
    cells: BTreeMap<String, AttentionCell>,
}

/// Shared ownership: the actor's `Topology` and every published clone
/// see the same cells — commits land once, readers always see the latest.
pub(crate) type SharedLedger = Arc<Mutex<AttentionLedger>>;

impl AttentionLedger {
    pub(crate) fn shared() -> SharedLedger {
        Arc::new(Mutex::new(Self::default()))
    }

    /// `s.agents[paneID]`-side cell access — inserts on write paths.
    pub(crate) fn cell_mut(&mut self, pane_id: &str) -> &mut AttentionCell {
        self.cells.entry(pane_id.to_owned()).or_default()
    }

    /// Read-side access without inserting.
    pub(crate) fn cell(&self, pane_id: &str) -> Option<&AttentionCell> {
        self.cells.get(pane_id)
    }

    /// The removal pass (`commitInventoryLocked`'s `!seen` loop):
    /// everything keyed by the pane goes except `generation` (which lives
    /// on `Topology`) — including the custom-answer keys the caller
    /// forwards to `Questions::forget_pane`.
    pub(crate) fn remove(&mut self, pane_id: &str) {
        self.cells.remove(pane_id);
    }
}

/// The classification + no-echo halves `preparePaneResponse` writes onto
/// a successful pane frame (`pane_content`/`pane_delta`) —
/// `attention_kind`/`prompt`/`command`/`options`/`interaction`/
/// `question_layout`/`no_echo`/`no_echo_prompt`.
///
/// `classify_semantics` runs on the content the oracle classifies (the
/// raw read — `classifyPaneResponse`, server.go:2771-2817);
/// `no_echo_semantics` runs on the final rendered bytes after the
/// history merge (`noecho.Match` on `preparePaneResponse`'s tail,
/// server.go:2757). `pane_semantics` is the single-content convenience
/// for callers that skip the merge.
#[derive(Debug, Clone, Default)]
pub(crate) struct PaneSemantics {
    pub attention_kind: &'static str,
    pub prompt: String,
    pub command: String,
    /// `classification.Options` — `null` on the wire when empty (the Go
    /// slice is nil for every non-approval kind).
    pub options: Vec<String>,
    /// The wire-shaped interaction when the pane shows a structured
    /// question (custom answers already filled).
    pub interaction: Option<QuestionInteraction>,
    pub question_layout: bool,
    /// `no_echo` — always emitted on a successful frame.
    pub no_echo: bool,
    /// `no_echo_prompt` — emitted only when the tail is a secret prompt.
    pub no_echo_prompt: Option<String>,
}

/// `question.Classify` + the custom-answer record/fill bookkeeping
/// (`RecordCustomAnswer` then `FillCustomAnswers`, so the just-typed
/// answer lands in this frame's review summary too). Runs on the
/// pre-merge classified content.
pub(crate) fn classify_semantics(
    pane_id: &str,
    classified_content: &str,
    agent: &str,
    questions: &Questions,
) -> Classification {
    let mut classification = classify(classified_content, agent);
    record_fill_answers(pane_id, &mut classification, questions);
    classification
}

/// The `setAgentAttention` record/fill half (server.go:1860-1877) —
/// `RecordCustomAnswer` the typed `other` text, then `FillCustomAnswers`
/// the interaction from the pane's stored answers. Split out so the
/// projector's retry loop runs `classify` per attempt but pays the
/// bookkeeping once, on the classification that actually commits.
pub(crate) fn record_fill_answers(
    pane_id: &str,
    classification: &mut Classification,
    questions: &Questions,
) {
    if let Some(interaction) = &mut classification.interaction {
        let text = interaction.other.text.trim();
        if !text.is_empty() {
            questions.record_custom_answer(pane_id, &interaction.question, text);
        }
        let answers = questions.custom_answers(pane_id);
        fill_custom_answers(interaction, &answers);
    }
}

/// The `noecho.Match` tail of `preparePaneResponse` — runs on the
/// post-history-merge rendered content so full reads, deltas, and
/// history frames agree on the tail.
pub(crate) fn no_echo_semantics(rendered_content: &str) -> (bool, Option<String>) {
    match match_prompt(rendered_content) {
        Some(prompt) => (true, Some(prompt)),
        None => (false, None),
    }
}

/// `classification` → `PaneSemantics` minus the no-echo pair.
pub(crate) fn semantics_of(classification: Classification) -> PaneSemantics {
    PaneSemantics {
        attention_kind: kind_str(classification.kind),
        prompt: classification.prompt,
        command: classification.command,
        options: classification.options,
        interaction: classification.interaction.as_ref().map(wire_interaction),
        question_layout: classification.question_layout,
        ..PaneSemantics::default()
    }
}

/// `preparePaneResponse`'s output halves — the final rendered `content`
/// (history-merged when the agent is claude-like), the `truncated` flag
/// (Herdr's OR the merge's), and the semantic field set.
#[derive(Debug, Default)]
pub(crate) struct PreparedPane {
    pub content: String,
    pub truncated: bool,
    pub semantics: PaneSemantics,
}

/// `preparePaneResponse` (server.go:2757-2817) on a successful read.
/// `classified` is the capped raw read; the merge condition is the
/// oracle's exactly — claude-like agents merge their transcript history
/// only when no structured interaction is showing and the read isn't a
/// lease-shaped viewport. `no_echo` runs on the final rendered bytes.
#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_pane_response(
    pane_id: &str,
    classified: &str,
    truncated: bool,
    agent: &str,
    viewport_only: bool,
    lines: u32,
    questions: &Questions,
    history: &crate::history::Manager,
) -> PreparedPane {
    let classification = classify_semantics(pane_id, classified, agent, questions);
    let mut semantics = semantics_of(classification);
    let (content, merged_truncated) =
        if semantics.interaction.is_none() && is_claude_like(agent) && !viewport_only {
            history.merge_limited(pane_id, classified, lines as usize)
        } else {
            (classified.to_owned(), false)
        };
    let truncated = truncated || merged_truncated;
    let (no_echo, no_echo_prompt) = no_echo_semantics(&content);
    semantics.no_echo = no_echo;
    semantics.no_echo_prompt = no_echo_prompt;
    PreparedPane {
        content,
        truncated,
        semantics,
    }
}

/// `question.AttentionKind` → wire string.
pub(crate) fn kind_str(kind: AttentionKind) -> &'static str {
    match kind {
        AttentionKind::Approval => "approval",
        AttentionKind::Question => "question",
        AttentionKind::Chat => "chat",
        AttentionKind::Unknown => "unknown",
    }
}

/// `classify::Interaction` → the wire `QuestionInteraction`. `options` is
/// always `Value` (never `null`): every Go parser that can emit an empty
/// slice builds it non-nil (`make`/`all[:len-1]` — the `var options`
/// review paths bail before producing an interaction), so an empty Vec
/// marshals `[]` exactly like the oracle.
pub(crate) fn wire_interaction(interaction: &Interaction) -> QuestionInteraction {
    QuestionInteraction {
        id: interaction.id.clone(),
        kind: interaction.kind.clone(),
        question: interaction.question.clone(),
        options: MaybeNull::Value(
            interaction
                .options
                .iter()
                .map(|option| QuestionOption {
                    index: option.index,
                    label: option.label.clone(),
                    description: option.description.clone(),
                    selected: option.selected,
                    summary: (!option.summary.is_empty()).then(|| {
                        option
                            .summary
                            .iter()
                            .map(|entry| SummaryEntry {
                                q: entry.question.clone(),
                                a: entry.answer.clone(),
                            })
                            .collect()
                    }),
                })
                .collect(),
        ),
        other: QuestionOther {
            selected: interaction.other.selected,
            text: interaction.other.text.clone(),
            label: interaction.other.label.clone(),
            placeholder: interaction.other.placeholder.clone(),
            allow_empty: interaction.other.allow_empty,
            hidden: interaction.other.hidden,
        },
        submit_label: interaction.submit_label.clone(),
        can_chat: interaction.can_chat,
        can_go_back: interaction.can_go_back,
        question_index: interaction.question_index,
        question_total: interaction.question_total,
    }
}
