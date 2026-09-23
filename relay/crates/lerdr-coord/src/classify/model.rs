//! Question/approval value types — the inbound payloads, the wire-shaped
//! `question.Interaction` model, interaction identity, and the custom-answer
//! bookkeeping (`internal/question/question.go`, `custom.go`).

use std::collections::HashMap;

use lerdr_core::protocol::Inbound;
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::parse::summary_lines;
use super::text::*;

/// `promptMaxChars` — the `other_text` rune bound.
pub(crate) const OTHER_TEXT_MAX_RUNES: usize = 100_000;

// ═══════════════════════════════════════════════════════════════════════
// Inbound payloads — `approvalPayload`/`questionPayload` decoding and the
// field validation the oracle applies before the state machine runs.
// ═══════════════════════════════════════════════════════════════════════

/// `approvalPayload`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ApprovalPayload {
    pub(crate) event_id: String,
    pub(crate) fingerprint: String,
    pub(crate) choice: String,
    pub(crate) index: i64,
    pub(crate) total: i64,
}

impl ApprovalPayload {
    /// `handleApproval`'s field validation, verbatim messages.
    pub(crate) fn decode(message: &Inbound) -> Result<(Self, String), &'static str> {
        let payload = Self {
            event_id: message.event_id.clone(),
            fingerprint: message.approval_fingerprint.clone(),
            choice: message.choice.clone(),
            index: message.index.unwrap_or(0),
            total: message.total.unwrap_or(2),
        };
        if message.pane_id.is_empty()
            || payload.event_id.is_empty()
            || payload.fingerprint.is_empty()
            || payload.choice.is_empty()
        {
            return Err("Agent and exact approval identity are required");
        }
        if !(2..=20).contains(&payload.total) || payload.index < 0 || payload.index >= payload.total
        {
            return Err("Approval choice is no longer available");
        }
        Ok((payload, message.pane_id.clone()))
    }
}

/// `questionPayload` — one payload type serves `answer_question`,
/// `clarify_question`, and `navigate_question` exactly like the oracle.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct QuestionPayload {
    pub(crate) interaction_id: String,
    pub(crate) selected: Vec<i64>,
    pub(crate) other_selected: bool,
    pub(crate) other_text: String,
    pub(crate) clarify: bool,
    pub(crate) navigation: String,
}

impl QuestionPayload {
    /// `decodeQuestionPayload` — validation order and messages preserved.
    pub(crate) fn decode_answer(message: &Inbound) -> Result<Self, &'static str> {
        let payload = Self {
            interaction_id: message.interaction_id.clone(),
            selected: sorted_unique(&message.selected_indices),
            other_selected: message.other_selected,
            other_text: message.other_text.clone(),
            clarify: false,
            navigation: String::new(),
        };
        if payload.interaction_id.is_empty() {
            return Err("agent and question are required");
        }
        if payload.other_text.chars().count() > OTHER_TEXT_MAX_RUNES {
            return Err("other answer is longer than 100,000 characters");
        }
        if payload.selected.iter().any(|index| *index < 0) {
            return Err("invalid question selection");
        }
        if payload.selected.is_empty() && payload.other_text.is_empty() && !payload.other_selected {
            return Err("choose an answer or enter an Other answer");
        }
        if !payload.other_text.is_empty() && !payload.other_selected {
            return Err("other text must be selected");
        }
        Ok(payload)
    }

    /// `handleClarifyQuestion`.
    pub(crate) fn decode_clarify(message: &Inbound) -> Result<Self, &'static str> {
        if message.interaction_id.is_empty() {
            return Err("Agent and question are required");
        }
        Ok(Self {
            interaction_id: message.interaction_id.clone(),
            selected: Vec::new(),
            other_selected: false,
            other_text: String::new(),
            clarify: true,
            navigation: String::new(),
        })
    }

    /// `handleNavigateQuestion` — direction is validated before the
    /// interaction id, matching the oracle's order.
    pub(crate) fn decode_navigate(message: &Inbound) -> Result<Self, &'static str> {
        if message.direction != "previous" && message.direction != "next" {
            return Err("Question navigation is no longer available");
        }
        if message.interaction_id.is_empty() {
            return Err("Question is required");
        }
        Ok(Self {
            interaction_id: message.interaction_id.clone(),
            selected: Vec::new(),
            other_selected: false,
            other_text: String::new(),
            clarify: false,
            navigation: message.direction.clone(),
        })
    }
}

/// `validateQuestionPayload` — payload-vs-live-interaction checks, run
/// after the interaction id matches (verbatim messages).
pub(crate) fn validate_question_payload(
    interaction: &Interaction,
    payload: &QuestionPayload,
) -> Result<(), &'static str> {
    if payload.navigation == "previous" && !interaction.can_go_back {
        return Err("there is no previous question to open");
    }
    if payload.navigation == "next"
        && (interaction.question_index == 0
            || interaction.question_index >= interaction.question_total)
    {
        return Err("there is no next question to open");
    }
    if payload.clarify && (!interaction.can_chat || interaction.agent != "claude") {
        return Err("this question can no longer be discussed");
    }
    if !payload.navigation.is_empty() || payload.clarify {
        return Ok(());
    }
    for index in &payload.selected {
        if *index < 0 || *index >= interaction.options.len() as i64 {
            return Err("question selection is no longer available");
        }
    }
    let other_is_choice = payload.other_selected
        && (!payload.other_text.trim().is_empty() || interaction.other.allow_empty);
    if interaction.other.hidden && payload.other_selected {
        return Err("this question does not accept a custom answer");
    }
    if interaction.kind == "single_select"
        && payload.selected.len() + usize::from(other_is_choice) != 1
    {
        return Err("choose one answer or enter an Other answer");
    }
    Ok(())
}

/// `sort.Ints` + `uniqueInts` — sorted consecutive dedup.
pub(crate) fn sorted_unique(values: &[i64]) -> Vec<i64> {
    let mut out = values.to_vec();
    out.sort_unstable();
    out.dedup();
    out
}

/// `hashPayload` — the scheduler's in-memory conflict key (hex of the JSON
/// encoding; not an integrity digest, same as the oracle).
pub(crate) fn hash_payload<T: Serialize>(payload: &T) -> String {
    hex::encode(serde_json::to_vec(payload).unwrap_or_default())
}

// ═══════════════════════════════════════════════════════════════════════
// Structured question model — `internal/question`'s Interaction shape,
// serialized for `data.interaction` on the wire (Go JSON field names).
// ═══════════════════════════════════════════════════════════════════════

/// `question.SummaryEntry` — one answered question in a review option.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct SummaryEntry {
    #[serde(rename = "q")]
    pub(crate) question: String,
    #[serde(rename = "a")]
    pub(crate) answer: String,
}

/// `question.Option`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct QuestionOption {
    pub(crate) index: i64,
    pub(crate) label: String,
    pub(crate) description: String,
    pub(crate) selected: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) summary: Vec<SummaryEntry>,
}

/// `question.Other`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct QuestionOther {
    pub(crate) selected: bool,
    pub(crate) text: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) label: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) placeholder: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) allow_empty: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) hidden: bool,
}

/// `question.Focus` — the renderer's focus (drives navigation keys).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct QuestionFocus {
    pub(crate) kind: FocusKind,
    pub(crate) index: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum FocusKind {
    #[default]
    Option,
    Other,
    Submit,
    Chat,
}

/// `question.Interaction` — `Focus`/`AllOptionCount`/`Agent`/`NotesActive`
/// stay internal (Go `json:"-"`); the rest serialize wire-identically.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Interaction {
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) question: String,
    pub(crate) options: Vec<QuestionOption>,
    pub(crate) other: QuestionOther,
    pub(crate) submit_label: String,
    pub(crate) can_chat: bool,
    pub(crate) can_go_back: bool,
    #[serde(skip_serializing_if = "is_zero_i64")]
    pub(crate) question_index: i64,
    #[serde(skip_serializing_if = "is_zero_i64")]
    pub(crate) question_total: i64,
    #[serde(skip)]
    pub(crate) focus: QuestionFocus,
    #[serde(skip)]
    pub(crate) all_option_count: usize,
    #[serde(skip)]
    pub(crate) agent: String,
    #[serde(skip)]
    pub(crate) notes_active: bool,
}

pub(crate) fn is_zero_i64(value: &i64) -> bool {
    *value == 0
}

/// `interactionID` — sha256 of the canonical identity JSON, 20 hex chars.
/// The bytes must match Go's `json.Marshal` exactly (`lerdr_core::json` —
/// HTML-safe escapes), and a nil `Options`/`Position` marshals `null`/
/// omits like the oracle's untagged slices.
pub(crate) fn interaction_id(interaction: &Interaction) -> String {
    #[derive(Serialize)]
    struct Identity<'a> {
        kind: &'a str,
        question: &'a str,
        options: Option<Vec<&'a str>>,
        submit_label: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        position: Option<Vec<i64>>,
    }
    let options: Vec<&str> = interaction
        .options
        .iter()
        .map(|option| option.label.as_str())
        .collect();
    let value = Identity {
        kind: &interaction.kind,
        question: &interaction.question,
        options: (!options.is_empty()).then_some(options),
        submit_label: &interaction.submit_label,
        position: (interaction.question_index > 0 && interaction.question_total > 0)
            .then(|| vec![interaction.question_index, interaction.question_total]),
    };
    let data = lerdr_core::json::to_vec(&value).unwrap_or_default();
    let sum = Sha256::digest(&data);
    hex::encode(sum)[..20].to_owned()
}

pub(crate) fn finish_interaction(mut interaction: Interaction) -> Option<Interaction> {
    interaction.id = interaction_id(&interaction);
    Some(interaction)
}

pub(crate) const CUSTOM_ANSWER_PLACEHOLDER: &str = "custom answer";

/// `SummaryKey` — normalize a question so review entries match the views
/// free text was typed into.
pub(crate) fn summary_key(value: &str) -> String {
    compact(value.trim().trim_end_matches('?'), 500).to_lowercase()
}

/// `FillCustomAnswers` — swap placeholder review answers for the recorded
/// free text and refresh the description.
pub(crate) fn fill_custom_answers(
    interaction: &mut Interaction,
    answers: &HashMap<String, String>,
) {
    if answers.is_empty() {
        return;
    }
    for option in &mut interaction.options {
        let mut changed = false;
        for entry in &mut option.summary {
            if entry.answer != CUSTOM_ANSWER_PLACEHOLDER {
                continue;
            }
            if let Some(text) = answers.get(&summary_key(&entry.question)) {
                if !text.is_empty() {
                    entry.answer = text.clone();
                    changed = true;
                }
            }
        }
        if changed {
            option.description = summary_lines(&option.summary, 1000);
        }
    }
}
