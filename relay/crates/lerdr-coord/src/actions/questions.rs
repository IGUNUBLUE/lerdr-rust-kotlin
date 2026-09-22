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
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;

use super::{dispatch_failure, ActionContext, Outcome};

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
/// `promptMaxChars` — the `other_text` rune bound.
const OTHER_TEXT_MAX_RUNES: usize = 100_000;
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
// Inbound payloads — `approvalPayload`/`questionPayload` decoding and the
// field validation the oracle applies before the state machine runs.
// ═══════════════════════════════════════════════════════════════════════

/// `approvalPayload`.
#[derive(Debug, Clone, PartialEq, Serialize)]
struct ApprovalPayload {
    event_id: String,
    fingerprint: String,
    choice: String,
    index: i64,
    total: i64,
}

impl ApprovalPayload {
    /// `handleApproval`'s field validation, verbatim messages.
    fn decode(message: &Inbound) -> Result<(Self, String), &'static str> {
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
struct QuestionPayload {
    interaction_id: String,
    selected: Vec<i64>,
    other_selected: bool,
    other_text: String,
    clarify: bool,
    navigation: String,
}

impl QuestionPayload {
    /// `decodeQuestionPayload` — validation order and messages preserved.
    fn decode_answer(message: &Inbound) -> Result<Self, &'static str> {
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
    fn decode_clarify(message: &Inbound) -> Result<Self, &'static str> {
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
    fn decode_navigate(message: &Inbound) -> Result<Self, &'static str> {
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
fn validate_question_payload(
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
fn sorted_unique(values: &[i64]) -> Vec<i64> {
    let mut out = values.to_vec();
    out.sort_unstable();
    out.dedup();
    out
}

/// `hashPayload` — the scheduler's in-memory conflict key (hex of the JSON
/// encoding; not an integrity digest, same as the oracle).
fn hash_payload<T: Serialize>(payload: &T) -> String {
    hex::encode(serde_json::to_vec(payload).unwrap_or_default())
}

// ═══════════════════════════════════════════════════════════════════════
// Terminal text — `ansiPattern`/`edgePattern` line cleaning and the
// hand-rolled matchers for every `regexp.MustCompile` in parser.go and
// attention.go. Go regex syntax that has no literal Rust equivalent is
// implemented as a small scanner per pattern; each matcher documents the
// source pattern it ports.
// ═══════════════════════════════════════════════════════════════════════

/// `ansiPattern` = `\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07]*(?:\x07|\x1b\\)|\x9b[0-9;?]*[ -/]*[@-~]`
/// — CSI (both `ESC [` and the C1 `\x9b` spelling), OSC with BEL or ST
/// terminator.
fn strip_ansi(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            0x1b if index + 1 < bytes.len() && bytes[index + 1] == b'[' => {
                index += 2;
                while index < bytes.len() && (0x30..=0x3f).contains(&bytes[index]) {
                    index += 1;
                }
                while index < bytes.len() && (0x20..=0x2f).contains(&bytes[index]) {
                    index += 1;
                }
                if index < bytes.len() && (0x40..=0x7e).contains(&bytes[index]) {
                    index += 1;
                }
            }
            0x1b if index + 1 < bytes.len() && bytes[index + 1] == b']' => {
                index += 2;
                while index < bytes.len() && bytes[index] != 0x07 {
                    if bytes[index] == 0x1b && index + 1 < bytes.len() && bytes[index + 1] == b'\\'
                    {
                        index += 2;
                        break;
                    }
                    index += 1;
                }
                if index < bytes.len() && bytes[index] == 0x07 {
                    index += 1;
                }
            }
            0x9b => {
                index += 1;
                while index < bytes.len() && (0x30..=0x3f).contains(&bytes[index]) {
                    index += 1;
                }
                while index < bytes.len() && (0x20..=0x2f).contains(&bytes[index]) {
                    index += 1;
                }
                if index < bytes.len() && (0x40..=0x7e).contains(&bytes[index]) {
                    index += 1;
                }
            }
            _ => {
                // Copy one UTF-8 rune verbatim.
                let width = utf8_len(bytes[index]);
                let end = (index + width).min(bytes.len());
                out.push_str(&line[index..end]);
                index = end;
            }
        }
    }
    out
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

/// `edgePattern` = `^[\s│|]+|[\s│|]+$` — one border column strip per pass.
fn strip_edge_once(line: &str) -> &str {
    let line = line.strip_prefix(|c| c == '│' || c == '|').unwrap_or(line);
    let line = line.strip_suffix(|c| c == '│' || c == '|').unwrap_or(line);
    line
}

/// `cleanLines`/`cleanLine` — ANSI strip, `TrimSpace`, then repeat edge-strip
/// + `TrimSpace` to a fixpoint.
fn clean_lines(text: &str) -> Vec<String> {
    text.replace("\r\n", "\n")
        .split('\n')
        .map(clean_line)
        .collect()
}

fn clean_line(line: &str) -> String {
    let mut line = strip_ansi(&line.replace('\r', "")).trim().to_owned();
    loop {
        let next = strip_edge_once(&line).trim().to_owned();
        if next == line {
            return line;
        }
        line = next;
    }
}

/// `cleanCodexLine` — ANSI strip + right-trim, then repeat right-side
/// edge-strip to a fixpoint (left edge preserved: the codex layout keeps
/// its indentation).
fn clean_codex_line(line: &str) -> String {
    let mut line = strip_ansi(&line.replace('\r', ""))
        .trim_end_matches([' ', '\t'])
        .to_owned();
    loop {
        let next = strip_edge_once(&line)
            .trim_end_matches([' ', '\t'])
            .to_owned();
        if next == line {
            return line;
        }
        line = next;
    }
}

/// `cleanOpenCodeLine` — ANSI strip + right-trim; lines not starting with
/// the `┃` frame column are blanked; the body ends at the first 20-column
/// gap (scrollbar gutter).
fn clean_opencode_line(line: &str) -> String {
    let stripped = strip_ansi(&line.replace('\r', ""));
    let stripped = stripped.trim_end_matches([' ', '\t']);
    let trimmed = stripped.trim_start_matches([' ', '\t']);
    let Some(body) = trimmed.strip_prefix('┃') else {
        return String::new();
    };
    let body = match first_run_of_spaces(body, 20) {
        Some((start, _)) => &body[..start],
        None => body,
    };
    body.trim().to_owned()
}

/// `ompCleanLines` — `cleanLines` plus the Ask frame's inner scrollbar
/// column on the right.
fn omp_clean_lines(text: &str) -> Vec<String> {
    clean_lines(text)
        .into_iter()
        .map(|line| {
            line.trim_end_matches([
                ' ', '\t', '│', '█', '▉', '▊', '▋', '▌', '▍', '▎', '▏', '▁', '▂', '▃', '▄', '▅',
                '▆', '▇', '▀',
            ])
            .to_owned()
        })
        .collect()
}

/// First run of at least `n` consecutive whitespace runes → (byte start,
/// byte end). Ports `\s{20,}`/`columnGapPattern` (`\s{2,}`) searches.
fn first_run_of_spaces(line: &str, n: usize) -> Option<(usize, usize)> {
    let mut start = None;
    let mut count = 0usize;
    for (index, ch) in line.char_indices() {
        if ch.is_whitespace() {
            if start.is_none() {
                start = Some(index);
            }
            count += 1;
            if count == n {
                return Some((start.expect("run start"), index + ch.len_utf8()));
            }
        } else {
            start = None;
            count = 0;
        }
    }
    None
}

/// `strings.Fields` — split on whitespace runs.
fn fields(line: &str) -> Vec<&str> {
    line.split_whitespace().collect()
}

/// `compact` — collapse whitespace runs, truncate at `limit` runes.
fn compact(value: &str, limit: usize) -> String {
    let joined: String = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.chars().count() > limit {
        return joined.chars().take(limit).collect();
    }
    joined
}

/// `title` — lowercase then uppercase the first byte.
fn title(value: &str) -> String {
    let lower = value.to_lowercase();
    if lower.is_empty() {
        return lower;
    }
    let mut chars = lower.chars();
    let first = chars.next().expect("non-empty").to_uppercase().to_string();
    first + chars.as_str()
}

fn default_string<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.is_empty() {
        fallback
    } else {
        value
    }
}

fn eq_fold(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

fn contains_fold(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

// ── row matchers ────────────────────────────────────────────────────────

/// `menuPattern` = `^\s*([❯›]?)\s*(\d+)\.\s+(.*?)\s*$` → (focus, number, label).
fn menu_match(line: &str) -> Option<(bool, i64, &str)> {
    let rest = line.trim_start();
    let (focus, rest) = match rest.strip_prefix(['❯', '›']) {
        Some(rest) => (true, rest.trim_start()),
        None => (false, rest),
    };
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let rest = rest[digits.len()..].strip_prefix('.')?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let label = rest.trim();
    Some((focus, digits.parse().ok()?, label))
}

/// `checkboxPattern` = `^\s*([❯›]?)\s*(\d+)\.\s*\[([^\]]*)\]\s*(.*?)\s*$`
/// → (focus, number, mark, label).
fn checkbox_match(line: &str) -> Option<(bool, i64, &str, &str)> {
    let rest = line.trim_start();
    let (focus, rest) = match rest.strip_prefix(['❯', '›']) {
        Some(rest) => (true, rest.trim_start()),
        None => (false, rest),
    };
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let rest = rest[digits.len()..].strip_prefix('.')?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('[')?;
    let close = rest.find(']')?;
    let mark = &rest[..close];
    let label = rest[close + 1..].trim();
    Some((focus, digits.parse().ok()?, mark, label))
}

/// `submitPattern` = `(?i)^\s*([❯›]?)\s*(?:\d+\.\s*)?(submit|next)\s*$`
/// → focus.
fn submit_match(line: &str) -> Option<bool> {
    let rest = line.trim_start();
    let (focus, rest) = match rest.strip_prefix(['❯', '›']) {
        Some(rest) => (true, rest.trim_start()),
        None => (false, rest),
    };
    let rest = rest.trim();
    let mut body = rest;
    let digits: String = body.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() {
        if let Some(after) = body[digits.len()..].strip_prefix('.') {
            body = after.trim();
        }
    }
    if eq_fold(body, "submit") || eq_fold(body, "next") {
        return Some(focus);
    }
    None
}

/// `chatPattern` = `(?i)^\s*([❯›]?)\s*(?:\d+\.\s*)?chat about this\s*$`.
fn chat_match(line: &str) -> Option<bool> {
    let rest = line.trim_start();
    let (focus, rest) = match rest.strip_prefix(['❯', '›']) {
        Some(rest) => (true, rest.trim_start()),
        None => (false, rest),
    };
    let rest = rest.trim();
    let mut body = rest;
    let digits: String = body.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() {
        if let Some(after) = body[digits.len()..].strip_prefix('.') {
            body = after.trim();
        }
    }
    eq_fold(body, "chat about this").then_some(focus)
}

/// `otherPattern` = `(?i)^(?:type something\.?|type your own answer|none of the above|other)\b`.
fn is_other_label(label: &str) -> bool {
    let lower = label.to_lowercase();
    for prefix in [
        "type something",
        "type your own answer",
        "none of the above",
        "other",
    ] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            if prefix == "type something" {
                if let Some(rest) = rest.strip_prefix('.') {
                    return rest.is_empty()
                        || !rest.chars().next().is_some_and(|c| c.is_alphanumeric());
                }
                return rest.is_empty()
                    || rest.starts_with(char::is_whitespace)
                    || !rest.chars().next().is_some_and(|c| c.is_alphanumeric());
            }
            return rest.is_empty() || !rest.chars().next().is_some_and(|c| c.is_alphanumeric());
        }
    }
    false
}

/// `selectedPattern` = `\s*[✓✔]\s*$` — a trailing check mark.
fn selected_mark(line: &str) -> bool {
    let trimmed = line.trim_end();
    trimmed.ends_with('✓') || trimmed.ends_with('✔')
}

/// `claudeReviewPattern` = `(?i)^\s*([❯›]?)\s*(\d+)\.\s*(submit answers|cancel)\s*$`
/// → (focus, number).
fn claude_review_match(line: &str) -> Option<(bool, i64)> {
    let rest = line.trim_start();
    let (focus, rest) = match rest.strip_prefix(['❯', '›']) {
        Some(rest) => (true, rest.trim_start()),
        None => (false, rest),
    };
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let rest = rest[digits.len()..].strip_prefix('.')?;
    let body = rest.trim();
    if !(eq_fold(body, "submit answers") || eq_fold(body, "cancel")) {
        return None;
    }
    digits.parse().ok().map(|number| (focus, number))
}

/// `qoderReviewPattern` = `(?i)^\s*([❯›]?)\s*(submit answers|cancel ask)\s*$`.
fn qoder_review_match(line: &str) -> Option<bool> {
    let rest = line.trim_start();
    let (focus, rest) = match rest.strip_prefix(['❯', '›']) {
        Some(rest) => (true, rest.trim_start()),
        None => (false, rest),
    };
    let body = rest.trim();
    (eq_fold(body, "submit answers") || eq_fold(body, "cancel ask")).then_some(focus)
}

/// `codexHeaderPattern` = `(?i)^\s*question\s+(\d+)\s*/\s*(\d+)` → (current, total).
fn codex_header_match(line: &str) -> Option<(i64, i64)> {
    let rest = line.trim_start();
    let rest = strip_prefix_fold(rest, "question")?;
    let rest = rest.trim_start();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let rest = rest[digits.len()..].trim_start();
    let rest = rest.strip_prefix('/')?.trim_start();
    let total: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if total.is_empty() {
        return None;
    }
    Some((digits.parse().ok()?, total.parse().ok()?))
}

fn strip_prefix_fold<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    // `get` guards both the length and the char boundary — a multibyte
    // lead (box-drawing, markers) can never match an ASCII prefix.
    let head = line.get(..prefix.len())?;
    if head.eq_ignore_ascii_case(prefix) {
        Some(&line[prefix.len()..])
    } else {
        None
    }
}

/// `codexSubmitPattern` = `(?i)\benter\s+to\s+submit\s+(answer|answers|all)\b`.
fn codex_submit_match(line: &str) -> bool {
    let lower = line.to_lowercase();
    for (index, _) in lower.match_indices("enter") {
        let after = lower[index + "enter".len()..].trim_start();
        let Some(after) = after.strip_prefix("to") else {
            continue;
        };
        if !after.starts_with(char::is_whitespace) {
            continue;
        }
        let after = after.trim_start();
        let Some(after) = after.strip_prefix("submit") else {
            continue;
        };
        if !after.starts_with(char::is_whitespace) {
            continue;
        }
        let after = after.trim_start();
        for tail in ["answers", "answer", "all"] {
            if let Some(rest) = after.strip_prefix(tail) {
                if rest.is_empty() || !rest.chars().next().is_some_and(|c| c.is_alphanumeric()) {
                    return true;
                }
            }
        }
    }
    false
}

/// `codexFooter` — the codex nav-hint line.
fn codex_footer(line: &str) -> bool {
    let lower = line.to_lowercase();
    codex_submit_match(line)
        && (lower.contains("navigate questions")
            || lower.contains("tab to add notes")
            || lower.contains("tab or esc to clear notes"))
}

/// `qoderHeader` — the Qoder "Asking User" header line.
fn qoder_header(line: &str) -> bool {
    if eq_fold(line.trim(), "Asking User") {
        return true;
    }
    let lower = line.to_lowercase();
    lower.contains("asking user") && line.contains('·') && lower.contains("submit")
}

/// `qoderFooter` — the Qoder nav-hint line.
fn qoder_footer(line: &str) -> bool {
    let lower = line.to_lowercase();
    if lower.contains("esc back")
        && lower.contains("enter")
        && (lower.contains("navigate") || lower.contains("enter submit"))
    {
        return true;
    }
    lower.contains("switch")
        && (lower.contains("enter select")
            || lower.contains("enter toggle")
            || lower.contains("enter submit"))
        && (lower.contains("esc back") || lower.contains("esc cancel"))
        && (line.contains('←') || lower.contains("tab/"))
}

/// `qoderActivePattern` = `\x1b\[[^m]*48(?:;|:)[^m]*m\s*([^\x1b]+)` — the
/// background-colored active tab segment on a raw line.
fn qoder_active_label(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != 0x1b || index + 1 >= bytes.len() || bytes[index + 1] != b'[' {
            index += 1;
            continue;
        }
        let mut cursor = index + 2;
        let start = cursor;
        while cursor < bytes.len() && bytes[cursor] != b'm' && bytes[cursor] != 0x1b {
            cursor += 1;
        }
        if cursor >= bytes.len() || bytes[cursor] != b'm' {
            index += 1;
            continue;
        }
        let params = &raw[start..cursor];
        if sgr_has_48(params) {
            let after = &raw[cursor + 1..];
            let label: String = after
                .chars()
                .skip_while(|c| c.is_whitespace())
                .take_while(|c| *c != '\x1b')
                .collect();
            if label.is_empty() {
                return None;
            }
            return Some(label);
        }
        index = cursor + 1;
    }
    None
}

/// Whether an SGR parameter run carries `48` followed by a separator —
/// `\x1b\[[^m]*48(?:;|:)[^m]*m`.
fn sgr_has_48(params: &str) -> bool {
    params.contains("48;") || params.contains("48:")
}

/// `openCodeFocusPattern` = `\x1b\[[^m]*48(?:;|:)2(?:;|:)30(?:;|:)30(?:;|:)30m`
/// — the OpenCode focused-row background.
fn opencode_focus_match(raw: &str) -> bool {
    raw_has_sgr48_truecolor(raw, "30", "30", "30")
}

/// `openCodeActivePattern` = `\x1b\[[^m]*48(?:;|:)2(?:;|:)157(?:;|:)124(?:;|:)216m([^\x1b]*)`
/// — all active-tab segments on the raw line, concatenated.
fn opencode_active_label(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut parts = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == 0x1b && index + 1 < bytes.len() && bytes[index + 1] == b'[' {
            let mut cursor = index + 2;
            let start = cursor;
            while cursor < bytes.len() && bytes[cursor] != b'm' && bytes[cursor] != 0x1b {
                cursor += 1;
            }
            if cursor < bytes.len() && bytes[cursor] == b'm' {
                let params = &raw[start..cursor];
                if sgr48_rgb(params, "157", "124", "216") {
                    let label: String = raw[cursor + 1..]
                        .chars()
                        .take_while(|c| *c != '\x1b')
                        .collect();
                    parts.push(label);
                    index = cursor + 1;
                    continue;
                }
            }
        }
        index += 1;
    }
    compact(&parts.join(""), 500)
}

/// SGR `48;2;r;g;b` (or colon-separated) parameter match on a sequence that
/// may carry other params — `[^m]*48` prefix semantics: `48` appears as a
/// parameter somewhere in the sequence.
fn sgr48_rgb(params: &str, r: &str, g: &str, b: &str) -> bool {
    let parts: Vec<&str> = params.split([';', ':']).collect();
    // Exact 5-parameter subsequence: 48 ; 2 ; r ; g ; b.
    for window in parts.windows(5) {
        if window[0].trim() == "48"
            && window[1].trim() == "2"
            && window[2].trim() == r
            && window[3].trim() == g
            && window[4].trim() == b
        {
            return true;
        }
    }
    false
}

fn raw_has_sgr48_truecolor(raw: &str, r: &str, g: &str, b: &str) -> bool {
    let bytes = raw.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == 0x1b && index + 1 < bytes.len() && bytes[index + 1] == b'[' {
            let mut cursor = index + 2;
            let start = cursor;
            while cursor < bytes.len() && bytes[cursor] != b'm' && bytes[cursor] != 0x1b {
                cursor += 1;
            }
            if cursor < bytes.len()
                && bytes[cursor] == b'm'
                && sgr48_rgb(&raw[start..cursor], r, g, b)
            {
                return true;
            }
        }
        index += 1;
    }
    false
}

/// `ompAskHeaderPattern` = `(?i)^╭[─━═_—\s]*Ask(?:[─━═_—\s]|$)`.
fn omp_ask_header(line: &str) -> bool {
    let Some(rest) = line.strip_prefix('╭') else {
        return false;
    };
    let frame: String = rest
        .chars()
        .take_while(|c| matches!(c, '─' | '━' | '═' | '_' | '—') || c.is_whitespace())
        .collect();
    let rest = &rest[frame.len()..];
    let Some(rest) = strip_prefix_fold(rest, "ask") else {
        return false;
    };
    rest.is_empty()
        || rest
            .chars()
            .next()
            .is_some_and(|c| matches!(c, '─' | '━' | '═' | '_' | '—') || c.is_whitespace())
}

/// `ompOptionPattern` = `^\s*([❯›>]?)\s*(☑|☐|◉|○|||||\[[xX ]\]|\([oO ]\))\s+(.+?)\s*$`
/// → (focus, marker, label).
fn omp_option_match(line: &str) -> Option<(bool, &str, &str)> {
    let rest = line.trim_start();
    let (focus, rest) = match rest.strip_prefix(['❯', '›', '\u{f054}', '>']) {
        Some(rest) => (true, rest.trim_start()),
        None => (false, rest),
    };
    let markers = [
        "☑", "☐", "◉", "○", "\u{f0ca}", "\u{f096}", "\u{f192}", "\u{f10c}", "[x]", "[X]", "[ ]",
        "(o)", "(O)", "( )",
    ];
    for marker in markers {
        if let Some(after) = rest.strip_prefix(marker) {
            if !after.starts_with(char::is_whitespace) {
                continue;
            }
            let label = after.trim();
            if label.is_empty() {
                continue;
            }
            return Some((focus, marker, label));
        }
    }
    None
}

/// `ompFrameMetaPattern` = `\[\s*([\p{L}\p{N}_-]+)\s*\]\s*[·•]\s*options:\s*\d+`
/// → the frame's question id.
fn omp_frame_meta_match(line: &str) -> Option<&str> {
    let start = line.find('[')?;
    let inner_start = start + 1;
    let close = line[inner_start..].find(']')? + inner_start;
    let id = line[inner_start..close].trim();
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    let rest = line[close + 1..].trim_start();
    let rest = rest.strip_prefix(['·', '•'])?.trim_start();
    let rest = strip_prefix_fold(rest, "options:")?.trim_start();
    if !rest.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(id)
}

/// `ompProgressPattern` = `\s+\((\d+)\s*/\s*(\d+)\)\s*$` → (current, total).
fn omp_progress_match(line: &str) -> Option<(i64, i64)> {
    let trimmed = line.trim_end();
    let open = trimmed.rfind('(')?;
    if !trimmed.ends_with(')') {
        return None;
    }
    let inner = &trimmed[open + 1..trimmed.len() - 1];
    let (left, right) = inner.split_once('/')?;
    let current: i64 = left.trim().parse().ok()?;
    let total: i64 = right.trim().parse().ok()?;
    // `\s+` before the paren — the progress must not be flush against text.
    if open == 0 || !trimmed[..open].ends_with(char::is_whitespace) {
        return None;
    }
    Some((current, total))
}

/// `ompReviewSubmitPattern` = `(?i)^\s*[❯›>]?\s*submit\s*$`.
fn omp_review_submit_match(line: &str) -> bool {
    let rest = line.trim_start();
    let rest = rest
        .strip_prefix(['❯', '›', '\u{f054}', '>'])
        .unwrap_or(rest)
        .trim();
    eq_fold(rest, "submit")
}

/// `ompTabIDPattern` = `^[\p{L}\p{N}_.-]+$`.
fn omp_tab_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '-')
}

/// `ompActiveTabPattern` = `\x1b\[1m(?:\x1b\[[0-9;:]*m)*\s*([\p{L}\p{N}_.-]+)`
/// — the bold active tab on a raw line.
fn omp_active_tab(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == 0x1b
            && index + 3 < bytes.len()
            && bytes[index + 1] == b'['
            && bytes[index + 2] == b'1'
            && bytes[index + 3] == b'm'
        {
            let mut cursor = index + 4;
            // Consume any following SGR sequences.
            loop {
                if cursor + 1 < bytes.len() && bytes[cursor] == 0x1b && bytes[cursor + 1] == b'[' {
                    let mut end = cursor + 2;
                    while end < bytes.len() && bytes[end].is_ascii_digit()
                        || end < bytes.len() && (bytes[end] == b';' || bytes[end] == b':')
                    {
                        end += 1;
                    }
                    if end < bytes.len() && bytes[end] == b'm' {
                        cursor = end + 1;
                        continue;
                    }
                }
                break;
            }
            let after = &raw[cursor..];
            let label: String = after
                .chars()
                .skip_while(|c| c.is_whitespace())
                .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '-'))
                .collect();
            if !label.is_empty() {
                return Some(label);
            }
        }
        index += 1;
    }
    None
}

/// `commandPattern` = `^\s*[$>❯›]\s+(.+?)\s*$` — a shell-glyph command line.
fn command_match(line: &str) -> Option<&str> {
    let rest = line.trim_start();
    let rest = rest.strip_prefix(['$', '>', '❯', '›'])?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let body = rest.trim();
    (!body.is_empty()).then_some(body)
}

/// `chromePattern` — nav-hint/border/status chrome skipped by summaries.
fn is_chrome(line: &str) -> bool {
    let lower = line.to_lowercase();
    if lower.contains("esc to cancel") || lower.contains("type to queue") {
        return true;
    }
    if line.is_empty() {
        return false;
    }
    if line.chars().all(|c| {
        matches!(
            c,
            '─' | '━'
                | '═'
                | '_'
                | '—'
                | '│'
                | '|'
                | '◔'
                | '◑'
                | '◕'
                | '●'
                | '┃'
                | '┆'
                | '┊'
                | '╭'
                | '╮'
                | '╯'
                | '╰'
                | '├'
                | '┤'
                | '┬'
                | '┴'
                | '┼'
                | '┌'
                | '┐'
                | '└'
                | '┘'
        ) || c.is_whitespace()
    }) {
        return true;
    }
    let trimmed = line.trim_start();
    for spinner in ['◔', '◑', '◕', '●'] {
        if let Some(rest) = trimmed.strip_prefix(spinner) {
            let rest = rest.trim_start();
            if strip_prefix_fold(rest, "shell").is_some()
                || strip_prefix_fold(rest, "bash").is_some()
            {
                return true;
            }
        }
    }
    false
}

/// `promptSkipPattern` — approval-dialog lines that never join the summary.
fn is_prompt_skip(line: &str) -> bool {
    let lower = line.to_lowercase();
    let trimmed = lower.trim();
    trimmed == "bash command"
        || trimmed == "do you want to proceed"
        || trimmed == "do you want to proceed?"
        || trimmed.starts_with("would you like to run")
        || (trimmed.starts_with("environment:")
            && trimmed["environment:".len()..]
                .trim_start()
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_')
            && trimmed["environment:".len()..]
                .trim_start()
                .chars()
                .next()
                .is_some())
        || trimmed.starts_with("press enter to confirm")
        || trimmed.starts_with("esc to cancel")
}

/// `turnDurationPattern` = `(?i)^[^\p{L}\p{N}]*\p{L}+(?:ed|ing)\s+for\s+(?:\d+h\s*)?(?:\d+m\s*)?\d+s\b`.
fn is_turn_duration(line: &str) -> bool {
    let trimmed = line.trim_start_matches(|c: char| !c.is_alphanumeric());
    let lower = trimmed.to_lowercase();
    for (index, _) in lower.match_indices("for") {
        let word_end = index;
        // `\s+for\s+` — whitespace on both sides of "for".
        if word_end == 0
            || !lower[..word_end]
                .chars()
                .last()
                .is_some_and(|c| c.is_whitespace())
        {
            continue;
        }
        let after = &lower[index + 3..];
        if !after.starts_with(char::is_whitespace) {
            continue;
        }
        let word = lower[..word_end].trim_end();
        if !(word.ends_with("ed") || word.ends_with("ing")) || word.len() < 3 {
            continue;
        }
        if !word.chars().all(|c| c.is_alphabetic()) {
            continue;
        }
        let after = after.trim_start();
        let mut rest = after;
        // `(?:\d+h\s*)?(?:\d+m\s*)?\d+s\b` — an optional unit consumes its
        // digits only when its letter follows (regex backtracking).
        if let Some((_, tail)) = take_number(rest) {
            let tail = tail.trim_start();
            if let Some(tail) = tail.strip_prefix('h') {
                rest = tail.trim_start();
            }
        }
        if let Some((_, tail)) = take_number(rest) {
            let tail = tail.trim_start();
            if let Some(tail) = tail.strip_prefix('m') {
                rest = tail.trim_start();
            }
        }
        if let Some((_, tail)) = take_number(rest) {
            let tail = tail.trim_start();
            if let Some(tail) = tail.strip_prefix('s') {
                // `\b` — '_' is a word character like in Go.
                return !tail
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_');
            }
        }
    }
    false
}

fn take_number(s: &str) -> Option<(u64, &str)> {
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    Some((digits.parse().ok()?, &s[digits.len()..]))
}

/// `responseStartPattern` = `^\s*[•●]\s+\S`; `responsePrefixPattern` strips
/// the same bullet.
fn response_start(line: &str) -> bool {
    let rest = line.trim_start();
    let Some(rest) = rest.strip_prefix(['•', '●']) else {
        return false;
    };
    rest.starts_with(char::is_whitespace) && !rest.trim().is_empty()
}

fn response_prefix_strip(line: &str) -> &str {
    let rest = line.trim_start();
    match rest.strip_prefix(['•', '●']) {
        Some(rest) if rest.starts_with(char::is_whitespace) => rest.trim_start(),
        _ => line,
    }
}

// ── attention.go matchers ───────────────────────────────────────────────

/// `approvalFocusSourcePattern` — focus markers and trailing padding are
/// presentation; strip them from the fingerprint source.
fn stable_approval_source(source: &str) -> String {
    source
        .lines()
        .map(|line| {
            let trimmed_start = line.trim_start_matches([' ', '\t']);
            let stripped = trimmed_start
                .strip_prefix(|c| matches!(c, '❯' | '›' | '>' | '\u{f054}'))
                .map(|rest| rest.trim_start_matches([' ', '\t']))
                .unwrap_or(trimmed_start);
            stripped.trim_end_matches([' ', '\t'])
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `approvalFooterPattern` — the approval nav-hint line.
fn approval_footer_match(line: &str) -> bool {
    let lower = line.to_lowercase();
    if contains_phrase(&lower, "enter", &["to ", ""], &["select", "confirm"]) {
        return true;
    }
    for esc in ["esc", "escape"] {
        for tail in ["cancel", "reject", "deny", "exit"] {
            if phrase_with_gap(&lower, esc, tail) {
                return true;
            }
        }
    }
    if (lower.contains("↑/↓") || lower.contains("up/down"))
        && (lower.contains("navigate") || lower.contains("select"))
    {
        return true;
    }
    if lower.contains("tab")
        && (phrase_with_gap(&lower, "tab", "edit") || phrase_with_gap(&lower, "tab", "amend"))
    {
        return true;
    }
    false
}

fn contains_phrase(haystack: &str, first: &str, mids: &[&str], lasts: &[&str]) -> bool {
    for (index, _) in haystack.match_indices(first) {
        if index > 0 && haystack.as_bytes()[index - 1].is_ascii_alphanumeric() {
            continue;
        }
        let after = &haystack[index + first.len()..];
        if after.starts_with(char::is_alphanumeric) {
            continue;
        }
        for mid in mids {
            if let Some(rest) = after.trim_start().strip_prefix(mid.trim_end()) {
                let rest = rest.trim_start();
                for last in lasts {
                    if let Some(tail) = rest.strip_prefix(last) {
                        if tail.is_empty()
                            || !tail.chars().next().is_some_and(|c| c.is_alphanumeric())
                        {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

/// `<first>\s+(?:to\s+)?<last>`-ish: two words separated by whitespace and
/// an optional "to".
fn phrase_with_gap(haystack: &str, first: &str, last: &str) -> bool {
    for (index, _) in haystack.match_indices(first) {
        if index > 0 && haystack.as_bytes()[index - 1].is_ascii_alphanumeric() {
            continue;
        }
        let mut after = haystack[index + first.len()..].trim_start();
        if let Some(rest) = after.strip_prefix("to") {
            if rest.starts_with(char::is_whitespace) {
                after = rest.trim_start();
            }
        }
        if let Some(tail) = after.strip_prefix(last) {
            if tail.is_empty() || !tail.chars().next().is_some_and(|c| c.is_alphanumeric()) {
                return true;
            }
        }
    }
    false
}

/// `normalPromptPattern` — the agent's idle `❯`/`›`/`>` input prompt.
fn normal_prompt_match(line: &str) -> bool {
    let rest = line.trim_start();
    // Optional [name] or bare name prefix.
    let rest = if let Some(rest) = rest.strip_prefix('[') {
        match rest.find(']') {
            Some(end)
                if !rest[..end].is_empty()
                    && rest[..end].chars().all(|c| {
                        c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-'
                    }) =>
            {
                rest[end + 1..].trim_start()
            }
            _ => return false,
        }
    } else {
        let name_len: usize = rest
            .chars()
            .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_' || *c == '-')
            .map(char::len_utf8)
            .sum();
        if name_len > 0 && rest[name_len..].starts_with(char::is_whitespace) {
            rest[name_len..].trim_start()
        } else {
            rest
        }
    };
    let Some(rest) = rest.strip_prefix(['❯', '›', '>']) else {
        return false;
    };
    if rest.is_empty() || rest.trim().is_empty() {
        return rest.is_empty() || rest.trim().is_empty();
    }
    if !rest.starts_with(char::is_whitespace) {
        return false;
    }
    let body = rest.trim_start().to_lowercase();
    for head in ["ask", "describe", "type", "send", "use"] {
        if let Some(tail) = body.strip_prefix(head) {
            return tail.is_empty() || !tail.chars().next().is_some_and(|c| c.is_alphanumeric());
        }
    }
    false
}

/// `hermesPlaceholderPattern` — Hermes' rotating placeholder prompts.
fn hermes_placeholder_match(line: &str) -> bool {
    let rest = line.trim_start();
    let rest = if let Some(rest) = rest.strip_prefix('[') {
        match rest.find(']') {
            Some(end)
                if !rest[..end].is_empty()
                    && rest[..end].chars().all(|c| {
                        c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-'
                    }) =>
            {
                rest[end + 1..].trim_start()
            }
            _ => rest,
        }
    } else {
        let name_len: usize = rest
            .chars()
            .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_' || *c == '-')
            .map(char::len_utf8)
            .sum();
        if name_len > 0 && rest[name_len..].starts_with(char::is_whitespace) {
            rest[name_len..].trim_start()
        } else {
            rest
        }
    };
    let Some(rest) = rest.strip_prefix(['❯', '›', '>']) else {
        return false;
    };
    let body = rest.trim().to_lowercase();
    const PLACEHOLDERS: &[&str] = &[
        "ask anything, or type / for commands",
        "summarize what's in this folder",
        "draft a reply to the last email in my inbox",
        "plan a feature, then build it step by step",
        "find and fix a failing test",
        "research this topic and write me a brief",
        "what changed in this repo recently?",
        "turn these notes into a to-do list",
        "explain this error and how to fix it",
        "set a reminder or schedule a recurring task",
        "type / to browse commands, or ctrl+p for the palette",
    ];
    PLACEHOLDERS.iter().any(|placeholder| {
        let body = body
            .trim_end_matches(['…'])
            .trim_end_matches("...")
            .trim_end();
        body == placeholder.trim_end_matches(['…']).trim_end_matches("...")
    })
}

/// `hermesApprovalPromptPattern` = `(?i)^\s*⚠\x{fe0f}?(?:\s+[❯›>])?\s*$`.
fn hermes_approval_prompt(line: &str) -> bool {
    let rest = line.trim();
    let Some(rest) = rest.strip_prefix('⚠') else {
        return false;
    };
    let rest = rest.strip_prefix('\u{fe0f}').unwrap_or(rest).trim();
    rest.is_empty()
        || rest
            .strip_prefix(['❯', '›', '>'])
            .is_some_and(|r| r.trim().is_empty())
}

/// `hermesSpinnerLinePattern` — the 💻 elapsed-time spinner line.
fn hermes_spinner_line(line: &str) -> bool {
    let rest = line.trim_start();
    let Some(rest) = rest.strip_prefix('💻') else {
        return false;
    };
    let rest = rest.trim();
    if rest.is_empty() {
        return false;
    }
    let Some(open) = rest.rfind('(') else {
        return false;
    };
    if !rest.ends_with(')') {
        return false;
    }
    let inner = rest[open + 1..rest.len() - 1].trim();
    // `\d+(?:\.\d+)?s` or `\d+m\d+s`, optionally ` · ↓/↑ N tok`.
    let duration = inner.split('·').next().unwrap_or("").trim();
    let duration_ok = match duration.strip_suffix('s') {
        Some(body) => {
            let body = body.trim();
            if let Some((mins, secs)) = body.split_once('m') {
                !mins.is_empty()
                    && mins.chars().all(|c| c.is_ascii_digit())
                    && secs.chars().all(|c| c.is_ascii_digit() || c == '.')
                    && secs.chars().any(|c| c.is_ascii_digit())
            } else {
                !body.is_empty()
                    && body.chars().all(|c| c.is_ascii_digit() || c == '.')
                    && body.chars().any(|c| c.is_ascii_digit())
            }
        }
        None => false,
    };
    if !duration_ok {
        return false;
    }
    if let Some(tok) = inner.split('·').nth(1) {
        let tok = tok.trim();
        let Some(tok) = tok.strip_prefix(['↓', '↑']) else {
            return false;
        };
        return !tok.trim().is_empty() && tok.trim().ends_with("tok");
    }
    true
}

/// `hermesStatusLinePattern` = `^\s*⚕\s+\S+(?:\s+(?:│|·)\s*.*)?\s*$`.
fn hermes_status_line(line: &str) -> bool {
    let rest = line.trim_start();
    let Some(rest) = rest.strip_prefix('⚕') else {
        return false;
    };
    rest.starts_with(char::is_whitespace) && !rest.trim().is_empty()
}

/// `statusFooterPattern` — the status bar under a live prompt.
fn status_footer_match(line: &str) -> bool {
    let lower = line.to_lowercase();
    let word = |needle: &str| -> bool {
        lower.match_indices(needle).any(|(index, _)| {
            (index == 0 || !lower.as_bytes()[index - 1].is_ascii_alphanumeric())
                && !lower[index + needle.len()..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphanumeric())
        })
    };
    // `context N% used`
    if let Some(index) = lower.find("context") {
        let after = lower[index + "context".len()..].trim_start();
        if let Some((num, tail)) = take_float(after) {
            let _ = num;
            let tail = tail.trim_start();
            if tail.starts_with('%') && tail[1..].trim_start().starts_with("used") {
                return true;
            }
        }
    }
    // `ctx :? N%` or `ctx -+`
    for (index, _) in lower.match_indices("ctx") {
        if index > 0 && lower.as_bytes()[index - 1].is_ascii_alphanumeric() {
            continue;
        }
        let mut after = lower[index + 3..].trim_start();
        if let Some(rest) = after.strip_prefix(':') {
            after = rest.trim_start();
        }
        if after.starts_with('%') || after.chars().all(|c| c == '-') && !after.is_empty() {
            return true;
        }
        if let Some((_, tail)) = take_float(after) {
            if tail.trim_start().starts_with('%') {
                return true;
            }
        }
    }
    if lower.contains("? for shortcuts") && word("? for shortcuts") {
        return true;
    }
    if word("manual mode") || word("plan mode") {
        return true;
    }
    if lower.contains("shift+tab") || lower.contains("ctrl+") || lower.contains("cmd+") {
        return true;
    }
    // `\b\d+ agents?\b`
    let parts = fields(&lower);
    for pair in parts.windows(2) {
        if pair[0].chars().all(|c| c.is_ascii_digit())
            && !pair[0].is_empty()
            && (pair[1] == "agent" || pair[1] == "agents")
        {
            return true;
        }
    }
    false
}

fn take_float(s: &str) -> Option<(f64, &str)> {
    let mut end = 0;
    let mut dot = false;
    for (index, ch) in s.char_indices() {
        if ch.is_ascii_digit() {
            end = index + 1;
        } else if ch == '.' && !dot {
            dot = true;
            end = index + 1;
        } else {
            break;
        }
    }
    if end == 0 {
        return None;
    }
    s[..end].parse::<f64>().ok().map(|n| (n, &s[end..]))
}

/// `ompPlanMenuPattern` = `(?i)^plan mode\s*[-–—]\s*next step$`.
fn omp_plan_menu_match(line: &str) -> bool {
    let lower = line.to_lowercase();
    let Some(rest) = lower.strip_prefix("plan mode") else {
        return false;
    };
    let rest = rest.trim_start();
    let Some(rest) = rest.strip_prefix(['-', '–', '—']) else {
        return false;
    };
    rest.trim() == "next step"
}

/// `ompToolApprovalPattern` = `(?i)^╭[─━═\s]*allow tool:\s*\S`.
fn omp_tool_approval_match(line: &str) -> bool {
    let Some(rest) = line.strip_prefix('╭') else {
        return false;
    };
    let frame: String = rest
        .chars()
        .take_while(|c| matches!(c, '─' | '━' | '═') || c.is_whitespace())
        .collect();
    let rest = &rest[frame.len()..];
    let Some(rest) = strip_prefix_fold(rest, "allow tool:") else {
        return false;
    };
    rest.trim_start()
        .chars()
        .next()
        .is_some_and(|c| !c.is_whitespace())
}

/// `ompPlanFocusPattern` = `^[❯›>\x{f054}]\s+` — the row focus marker.
fn omp_plan_focus_match(line: &str) -> Option<&str> {
    let rest = line.strip_prefix(['❯', '›', '>', '\u{f054}'])?;
    if rest.starts_with(char::is_whitespace) {
        Some(rest)
    } else {
        None
    }
}

/// `ompInputHeaderPattern` = `^╭[─━═]{2}.*╮$`.
fn omp_input_header_match(line: &str) -> bool {
    let Some(rest) = line.strip_prefix('╭') else {
        return false;
    };
    if rest.len() < 2 || !rest.starts_with(['─', '━', '═']) {
        return false;
    }
    let frame: String = rest
        .chars()
        .take_while(|c| matches!(c, '─' | '━' | '═'))
        .collect();
    frame.chars().count() >= 2 && line.ends_with('╮')
}

/// `ompInputFooterPattern` = `^╰[─━═].*[─━═]╯$`.
fn omp_input_footer_match(line: &str) -> bool {
    let Some(rest) = line.strip_prefix('╰') else {
        return false;
    };
    if !rest.starts_with(['─', '━', '═']) || !line.ends_with('╯') {
        return false;
    }
    let inner = &line['╰'.len_utf8()..line.len() - '╯'.len_utf8()];
    inner.ends_with('─') || inner.ends_with('━') || inner.ends_with('═')
}

/// `openCodeInputPromptPattern` = `(?i)\bask anything\.\.\.`
fn opencode_input_prompt_match(line: &str) -> bool {
    let lower = line.to_lowercase();
    lower.contains("ask anything...") || lower.contains("ask anything…")
}

/// `contextUsageStatusPattern` = `(?i)\d+(?:\.\d+)?%/\d+[km]\b`.
fn context_usage_status_match(line: &str) -> bool {
    let trimmed = line.trim();
    for (index, _) in trimmed.match_indices("%/") {
        let before = &trimmed[..index];
        let num: String = before
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        if num.is_empty() || !num.chars().any(|c| c.is_ascii_digit()) {
            continue;
        }
        let after = &trimmed[index + 2..];
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            continue;
        }
        if let Some(rest) = after[digits.len()..].strip_prefix(['k', 'K', 'm', 'M']) {
            if rest.is_empty() || !rest.chars().next().is_some_and(|c| c.is_alphanumeric()) {
                return true;
            }
        }
    }
    false
}

/// `terminalRulePattern` = `^[─━═_—]{8,}$`.
fn terminal_rule_match(line: &str) -> bool {
    let count = line
        .chars()
        .take_while(|c| matches!(c, '─' | '━' | '═' | '_' | '—'))
        .count();
    count >= 8 && count == line.chars().count()
}

/// `\b(?:yes|allow|approve|proceed|trust)\b` / `\b(?:no|deny|reject|cancel|exit)\b`
/// — the first/last approval label sanity check.
fn approval_labels(rows: &[ApprovalMenuRow]) -> bool {
    let Some(first) = rows.first() else {
        return false;
    };
    let Some(last) = rows.last() else {
        return false;
    };
    fn word_boundary(haystack: &str, words: &[&str]) -> bool {
        let lower = haystack.to_lowercase();
        for word in words {
            for (index, _) in lower.match_indices(word) {
                let left_ok = index == 0 || !lower.as_bytes()[index - 1].is_ascii_alphanumeric();
                let right_ok = !lower[index + word.len()..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphanumeric());
                if left_ok && right_ok {
                    return true;
                }
            }
        }
        false
    }
    word_boundary(
        &first.label,
        &["yes", "allow", "approve", "proceed", "trust"],
    ) && word_boundary(&last.label, &["no", "deny", "reject", "cancel", "exit"])
}

/// `approvalContinuation` — a non-menu line that does not end a menu run.
fn approval_continuation(line: &str) -> bool {
    line.starts_with(' ')
        || approval_footer_match(line)
        || line.to_lowercase().contains("esc to cancel")
}

/// `hermesApprovalChromeLine`.
fn hermes_approval_chrome_line(line: &str) -> bool {
    hermes_approval_prompt(line)
        || hermes_spinner_line(line)
        || hermes_status_line(line)
        || approval_footer_match(line)
}

/// `hermesApprovalAuxiliaryOption`.
fn hermes_approval_auxiliary(label: &str) -> bool {
    matches!(
        label.trim().to_lowercase().as_str(),
        "show full command" | "view full command"
    )
}

/// `qoderApprovalTailLine`.
fn qoder_approval_tail_line(line: &str) -> bool {
    let trimmed = line.trim();
    if eq_fold(trimmed, "Ctrl+X to edit plan") {
        return true;
    }
    if !line.starts_with(' ') || trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_lowercase();
    lower.starts_with("reject this plan") && lower.contains("without providing feedback")
}

/// `ompBorderLine` — a line that is only border/padding runes.
fn omp_border_line(line: &str) -> bool {
    line.trim_matches(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '─' | '━'
                    | '═'
                    | '_'
                    | '—'
                    | '│'
                    | '|'
                    | '├'
                    | '┤'
                    | '╭'
                    | '╮'
                    | '╰'
                    | '╯'
                    | '┬'
                    | '┴'
                    | '┼'
            )
    })
    .is_empty()
}

/// `ompMarkerSelected`.
fn omp_marker_selected(marker: &str) -> bool {
    matches!(
        marker.to_lowercase().as_str(),
        "☑" | "◉" | "\u{f0ca}" | "\u{f192}" | "[x]" | "(o)"
    )
}

/// `ompCheckboxMarker`.
fn omp_checkbox_marker(marker: &str) -> bool {
    matches!(
        marker.to_lowercase().as_str(),
        "☑" | "☐" | "\u{f0ca}" | "\u{f096}" | "[x]" | "[ ]"
    )
}

// ═══════════════════════════════════════════════════════════════════════
// Structured question model — `internal/question`'s Interaction shape,
// serialized for `data.interaction` on the wire (Go JSON field names).
// ═══════════════════════════════════════════════════════════════════════

/// `question.SummaryEntry` — one answered question in a review option.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
struct SummaryEntry {
    #[serde(rename = "q")]
    question: String,
    #[serde(rename = "a")]
    answer: String,
}

/// `question.Option`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
struct QuestionOption {
    index: i64,
    label: String,
    description: String,
    selected: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    summary: Vec<SummaryEntry>,
}

/// `question.Other`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
struct QuestionOther {
    selected: bool,
    text: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    label: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    placeholder: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    allow_empty: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    hidden: bool,
}

/// `question.Focus` — the renderer's focus (drives navigation keys).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct QuestionFocus {
    kind: FocusKind,
    index: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum FocusKind {
    #[default]
    Option,
    Other,
    Submit,
    Chat,
}

/// `question.Interaction` — `Focus`/`AllOptionCount`/`Agent`/`NotesActive`
/// stay internal (Go `json:"-"`); the rest serialize wire-identically.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
struct Interaction {
    id: String,
    kind: String,
    question: String,
    options: Vec<QuestionOption>,
    other: QuestionOther,
    submit_label: String,
    can_chat: bool,
    can_go_back: bool,
    #[serde(skip_serializing_if = "is_zero_i64")]
    question_index: i64,
    #[serde(skip_serializing_if = "is_zero_i64")]
    question_total: i64,
    #[serde(skip)]
    focus: QuestionFocus,
    #[serde(skip)]
    all_option_count: usize,
    #[serde(skip)]
    agent: String,
    #[serde(skip)]
    notes_active: bool,
}

fn is_zero_i64(value: &i64) -> bool {
    *value == 0
}

/// `interactionID` — sha256 of the canonical identity JSON, 20 hex chars.
/// The bytes must match Go's `json.Marshal` exactly (`lerdr_core::json` —
/// HTML-safe escapes), and a nil `Options`/`Position` marshals `null`/
/// omits like the oracle's untagged slices.
fn interaction_id(interaction: &Interaction) -> String {
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

fn finish_interaction(mut interaction: Interaction) -> Option<Interaction> {
    interaction.id = interaction_id(&interaction);
    Some(interaction)
}

/// `Supports` — the agent families with structured question parsing.
fn supports(agent: &str) -> bool {
    let agent = agent.to_lowercase();
    agent.contains("claude")
        || agent.contains("codex")
        || omp_ask_agent(&agent)
        || agent.contains("opencode")
        || agent.contains("qoder")
        || agent.contains("hermes")
}

/// `ompAskAgent` — omp and pi spellings share the Ask frame parser.
fn omp_ask_agent(agent: &str) -> bool {
    let agent = agent.to_lowercase();
    let agent = agent.trim();
    agent == "omp"
        || agent.starts_with("omp-")
        || agent == "pi"
        || agent.starts_with("pi-")
        || agent.contains("oh-my-pi")
}

/// `Parse` — layout gate first, then the agent-family parser.
fn parse_question(text: &str, agent: &str) -> Option<Interaction> {
    if !layout_hint(text) {
        return None;
    }
    let normalized = agent.to_lowercase();
    if omp_ask_agent(&normalized) {
        return parse_omp(text);
    }
    if normalized.contains("codex") {
        return parse_codex(text);
    }
    if normalized.contains("claude") {
        return parse_claude(text);
    }
    if normalized.contains("qoder") {
        return parse_qoder(text);
    }
    if normalized.contains("opencode") {
        return parse_opencode(text);
    }
    if let Some(interaction) = parse_codex(text) {
        return Some(interaction);
    }
    if let Some(interaction) = parse_claude(text) {
        return Some(interaction);
    }
    parse_qoder(text)
}

/// `LayoutHint` — cheap structural gate before the family parsers.
fn layout_hint(text: &str) -> bool {
    if opencode_layout_hint(text) || omp_layout_hint(text) {
        return true;
    }
    let lines = clean_lines(text);
    let (mut has_checkbox, mut has_submit, mut has_chat) = (false, false, false);
    let (mut has_codex_header, mut has_codex_footer) = (false, false);
    let (mut has_qoder_header, mut has_qoder_footer) = (false, false);
    let mut has_review = false;
    let mut last_control: i64 = -1;
    for (index, line) in lines.iter().enumerate() {
        let lower = line.to_lowercase();
        if checkbox_match(line).is_some() {
            has_checkbox = true;
        } else if submit_match(line).is_some() {
            has_submit = true;
            last_control = index as i64;
        } else if chat_match(line).is_some() {
            has_chat = true;
            last_control = index as i64;
        }
        if codex_header_match(line).is_some() {
            has_codex_header = true;
        }
        if codex_footer(line) {
            has_codex_footer = true;
            last_control = index as i64;
        }
        if qoder_header(line) {
            has_qoder_header = true;
        }
        if qoder_footer(line) {
            has_qoder_footer = true;
            last_control = index as i64;
        }
        if lower.contains("enter to select")
            && (line.contains("↑/↓") || lower.contains("arrow keys"))
        {
            last_control = index as i64;
        }
        if lower.contains("review your answers") {
            has_review = true;
            last_control = index as i64;
        }
        if has_review && (qoder_review_match(line).is_some() || claude_review_match(line).is_some())
        {
            last_control = index as i64;
        }
    }
    let has_layout = (has_checkbox && (has_submit || has_chat))
        || has_chat
        || (has_codex_header && has_codex_footer)
        || (has_qoder_header && has_qoder_footer)
        || has_review;
    if !has_layout || last_control < 0 {
        return false;
    }
    for line in &lines[last_control as usize + 1..] {
        if has_codex_header && eq_fold(line.trim(), "esc to interrupt") {
            continue;
        }
        if !line.is_empty()
            && !line
                .trim_matches(|c| matches!(c, '─' | '━' | '═' | '_' | '—' | '│' | '|' | ' '))
                .is_empty()
        {
            return false;
        }
    }
    true
}

/// `ompLayoutHint` — the Ask frame must sit above its footer hint with only
/// border/blank lines below.
fn omp_layout_hint(text: &str) -> bool {
    let lines = clean_lines(text);
    let mut start: i64 = -1;
    for (index, line) in lines.iter().enumerate() {
        if omp_ask_header(line) {
            start = index as i64;
        }
    }
    if start < 0 {
        return false;
    }
    let start = start as usize;
    let mut footer: i64 = -1;
    let mut options = 0;
    let mut review = false;
    let mut review_submit = false;
    for (index, line) in lines.iter().enumerate().skip(start + 1) {
        if omp_option_match(line).is_some() {
            options += 1;
        }
        if eq_fold(line, "review answers") {
            review = true;
        }
        if review && omp_review_submit_match(line) {
            review_submit = true;
        }
        let lower = line.to_lowercase();
        if lower.contains("enter select") || lower.contains("enter submit") {
            footer = index as i64;
        }
    }
    if footer < 0 || (options < 2 && !review_submit) {
        return false;
    }
    for line in &lines[footer as usize + 1..] {
        if !line.is_empty() && !omp_border_line(line) {
            return false;
        }
    }
    true
}

/// `openCodeLayoutHint` — footer hint present and the tail below it blank.
fn opencode_layout_hint(text: &str) -> bool {
    let raw_lines: Vec<String> = text
        .replace("\r\n", "\n")
        .split('\n')
        .map(String::from)
        .collect();
    let mut footer: i64 = -1;
    let mut lines = Vec::with_capacity(raw_lines.len());
    for raw in &raw_lines {
        let line = clean_opencode_line(raw);
        if opencode_footer(&line) {
            footer = lines.len() as i64;
        }
        lines.push(line);
    }
    footer >= 0 && opencode_tail_is_empty(&lines, footer as usize)
}

fn opencode_tail_is_empty(lines: &[String], footer: usize) -> bool {
    lines[footer + 1..]
        .iter()
        .all(|line| line.trim().is_empty())
}

/// `openCodeFooter` — the OpenCode nav-hint line.
fn opencode_footer(line: &str) -> bool {
    let lower = line.to_lowercase();
    if !lower.contains("esc dismiss") {
        return false;
    }
    if lower.contains("enter submit") {
        return true;
    }
    lower.contains("↑↓ select")
        && (lower.contains("enter confirm") || lower.contains("enter toggle"))
}

// ── parseClaude ─────────────────────────────────────────────────────────

/// `parseClaude` — checkbox multi-select first, numbered single-select
/// (requires the chat row), else the review screen.
fn parse_claude(text: &str) -> Option<Interaction> {
    let lines = clean_lines(text);
    struct Row {
        line: usize,
        focus: bool,
        label: String,
        selected: bool,
    }
    let mut checkbox_rows: Vec<Row> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let Some((focus, _number, mark, label)) = checkbox_match(line) else {
            continue;
        };
        checkbox_rows.push(Row {
            line: index,
            focus,
            label: compact(label, 500),
            selected: !mark.trim().is_empty(),
        });
    }
    let (mut submit_index, mut submit_focus, mut submit_label) = (-1i64, false, String::new());
    let (mut chat_index, mut chat_focus) = (-1i64, false);
    for (index, line) in lines.iter().enumerate() {
        if let Some(focus) = submit_match(line) {
            submit_index = index as i64;
            submit_focus = focus;
            let body = line
                .trim_start()
                .trim_start_matches(['❯', '›'])
                .trim_start();
            let digits: String = body.chars().take_while(|c| c.is_ascii_digit()).collect();
            let body = if !digits.is_empty() {
                body[digits.len()..]
                    .strip_prefix('.')
                    .unwrap_or(body)
                    .trim()
            } else {
                body.trim()
            };
            submit_label = title(body);
        }
        if let Some(focus) = chat_match(line) {
            chat_index = index as i64;
            chat_focus = focus;
        }
    }

    if checkbox_rows.len() >= 2 && submit_index >= 0 {
        let mut end = submit_index as usize;
        if chat_index >= 0 && (chat_index as usize) < end {
            end = chat_index as usize;
        }
        let mut all: Vec<QuestionOption> = Vec::with_capacity(checkbox_rows.len());
        let mut focus = QuestionFocus::default();
        for (index, item) in checkbox_rows.iter().enumerate() {
            let row_end = checkbox_rows
                .get(index + 1)
                .map(|row| row.line)
                .unwrap_or(end);
            all.push(QuestionOption {
                index: index as i64,
                label: strip_selected_mark(&item.label).to_owned(),
                description: description(&lines, item.line, row_end),
                selected: item.selected || selected_mark(&item.label),
                summary: Vec::new(),
            });
            if item.focus {
                focus = QuestionFocus {
                    kind: FocusKind::Option,
                    index,
                };
            }
        }
        if submit_focus {
            focus = QuestionFocus {
                kind: FocusKind::Submit,
                index: 0,
            };
        }
        if chat_focus {
            focus = QuestionFocus {
                kind: FocusKind::Chat,
                index: 0,
            };
        }
        let other_item = all.pop().expect("checkbox rows >= 2");
        let other_text = if !is_other_label(&other_item.label) {
            other_item.label.clone()
        } else {
            String::new()
        };
        let question = prompt(&lines, checkbox_rows[0].line);
        let (current, total) = claude_position(text);
        if submit_label == "Submit" && current > 0 && current < total {
            submit_label = "Next".to_owned();
        }
        let all_count = all.len() + 1;
        return finish_interaction(Interaction {
            id: String::new(),
            kind: "multi_select".to_owned(),
            question,
            options: all,
            other: QuestionOther {
                selected: other_item.selected,
                text: other_text,
                ..QuestionOther::default()
            },
            submit_label: default_string(&submit_label, "Submit").to_owned(),
            can_chat: chat_index >= 0,
            can_go_back: current > 1,
            question_index: current,
            question_total: total,
            focus,
            all_option_count: all_count,
            agent: "claude".to_owned(),
            notes_active: false,
        });
    }

    if chat_index < 0 {
        return parse_claude_review(text, &lines);
    }
    let chat_index = chat_index as usize;
    let mut rows: Vec<Row> = Vec::new();
    let mut expected: i64 = 1;
    for (index, line) in lines[..chat_index].iter().enumerate() {
        let Some((focus, number, label)) = menu_match(line) else {
            continue;
        };
        if number == 1 {
            rows.clear();
            expected = 1;
        }
        if number != expected {
            continue;
        }
        let label = compact(label, 500);
        rows.push(Row {
            line: index,
            focus,
            label: strip_selected_mark(&label).to_owned(),
            selected: selected_mark(&label),
        });
        expected += 1;
    }
    if rows.len() < 3 {
        return None;
    }
    let mut all: Vec<QuestionOption> = Vec::with_capacity(rows.len());
    let mut focus = QuestionFocus::default();
    for (index, item) in rows.iter().enumerate() {
        let row_end = rows
            .get(index + 1)
            .map(|row| row.line)
            .unwrap_or(chat_index);
        all.push(QuestionOption {
            index: index as i64,
            label: item.label.clone(),
            description: description(&lines, item.line, row_end),
            selected: item.selected,
            summary: Vec::new(),
        });
        if item.focus {
            focus = QuestionFocus {
                kind: FocusKind::Option,
                index,
            };
        }
    }
    if chat_focus {
        focus = QuestionFocus {
            kind: FocusKind::Chat,
            index: 0,
        };
    }
    let other_item = all.pop().expect("rows >= 3");
    let other_text = if !is_other_label(&other_item.label) {
        other_item.label.clone()
    } else {
        String::new()
    };
    let (current, total) = claude_position(text);
    let submit_label = if current > 0 && current < total {
        "Next"
    } else {
        "Submit"
    };
    let mut interaction = Interaction {
        id: String::new(),
        kind: "single_select".to_owned(),
        question: prompt(&lines, rows[0].line),
        options: all,
        other: QuestionOther {
            selected: other_item.selected,
            text: other_text,
            ..QuestionOther::default()
        },
        submit_label: submit_label.to_owned(),
        can_chat: true,
        can_go_back: current > 1,
        question_index: current,
        question_total: total,
        focus,
        all_option_count: rows.len(),
        agent: "claude".to_owned(),
        notes_active: false,
    };
    // Leftover typed text only marks the custom answer as chosen while no
    // option row carries the confirmed selection; otherwise a stale note
    // from an earlier visit would override the real answer.
    if !interaction.other.text.is_empty() && !interaction.other.selected {
        let selected_elsewhere = interaction.options.iter().any(|option| option.selected);
        if !selected_elsewhere {
            interaction.other.selected = true;
        }
    }
    finish_interaction(interaction)
}

fn strip_selected_mark(label: &str) -> &str {
    match label.trim_end().strip_suffix(['✓', '✔']) {
        Some(body) => body.trim_end(),
        None => label.trim(),
    }
}

/// `parseClaudeReview` — the "Review your answers" screen.
fn parse_claude_review(text: &str, lines: &[String]) -> Option<Interaction> {
    let mut review_index: i64 = -1;
    for (index, line) in lines.iter().enumerate().rev() {
        if eq_fold(line.trim(), "Review your answers") {
            review_index = index as i64;
            break;
        }
    }
    if review_index < 0 {
        return None;
    }
    let review_index = review_index as usize;
    let mut options: Vec<QuestionOption> = Vec::new();
    let mut focus = QuestionFocus::default();
    for line in lines.iter().skip(review_index + 1) {
        let Some((row_focus, number)) = claude_review_match(line) else {
            continue;
        };
        if number != options.len() as i64 + 1 {
            return None;
        }
        let option_index = options.len();
        let body = line
            .trim_start()
            .trim_start_matches(['❯', '›'])
            .trim_start();
        let digits: String = body.chars().take_while(|c| c.is_ascii_digit()).collect();
        let label = body[digits.len()..]
            .strip_prefix('.')
            .unwrap_or(&body[digits.len()..])
            .trim();
        options.push(QuestionOption {
            index: option_index as i64,
            label: compact(label, 500),
            ..QuestionOption::default()
        });
        if row_focus {
            focus = QuestionFocus {
                kind: FocusKind::Option,
                index: option_index,
            };
        }
    }
    if options.len() != 2
        || !eq_fold(&options[0].label, "Submit answers")
        || !eq_fold(&options[1].label, "Cancel")
    {
        return None;
    }

    let mut summary: Vec<SummaryEntry> = Vec::new();
    let mut prompt_text = String::new();
    let mut answer_open = false;
    for line in &lines[review_index + 1..] {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix('●') {
            prompt_text = rest.trim().to_owned();
            answer_open = false;
        } else if trimmed.starts_with('→') && !prompt_text.is_empty() {
            let mut answer = trimmed.trim_start_matches('→').trim().to_owned();
            if answer == "__other__" {
                answer = CUSTOM_ANSWER_PLACEHOLDER.to_owned();
            }
            summary.push(SummaryEntry {
                question: prompt_text.trim_end_matches('?').to_owned(),
                answer,
            });
            prompt_text.clear();
            answer_open = true;
        } else if trimmed.is_empty() || claude_review_match(line).is_some() {
            prompt_text.clear();
            answer_open = false;
        } else if !prompt_text.is_empty() {
            // Long prompts wrap onto continuation lines in the terminal.
            prompt_text.push(' ');
            prompt_text.push_str(trimmed);
        } else if answer_open && !summary.is_empty() {
            let last = summary.last_mut().expect("non-empty");
            last.answer.push(' ');
            last.answer.push_str(trimmed);
        }
    }
    options[0].description = summary_lines(&summary, 1000);
    options[0].summary = summary.clone();

    let (mut current, mut total) = claude_position(text);
    if current < 1 || total < current {
        current = summary.len() as i64 + 1;
        total = current;
    }
    finish_interaction(Interaction {
        id: String::new(),
        kind: "single_select".to_owned(),
        question: "Review your answers and choose what to do".to_owned(),
        options,
        other: QuestionOther {
            hidden: true,
            ..QuestionOther::default()
        },
        submit_label: "Continue".to_owned(),
        can_chat: false,
        can_go_back: text.contains('←'),
        question_index: current,
        question_total: total,
        focus,
        all_option_count: 2,
        agent: "claude".to_owned(),
        notes_active: false,
    })
}

/// `claudePosition` — the multi-question progress comes from the active-tab
/// background highlight on the raw `... → Submit →` tab row.
fn claude_position(text: &str) -> (i64, i64) {
    for raw in text.split('\n') {
        let clean = clean_line(raw);
        if !clean.contains('→') || !clean.to_lowercase().contains("submit") {
            continue;
        }
        let Some((active_start, active_end)) = ansi_48_span(raw) else {
            return (0, 0);
        };
        let prefix = clean_line(&raw[..active_start]);
        let current = count_marks(&prefix) + 1;
        let mut before_submit = clean.clone();
        if let Some(index) = before_submit.to_lowercase().find("submit") {
            before_submit = before_submit[..index].to_owned();
        }
        let mut total = count_marks(&before_submit);
        let active_text = clean_line(&raw[active_end..]);
        if !active_text
            .chars()
            .any(|c| matches!(c, '☐' | '☒' | '☑' | '✓' | '✔'))
            || total < current
        {
            total += 1;
        }
        if current >= 1 && total >= current {
            return (current, total);
        }
    }
    (0, 0)
}

/// `\x1b\[[^m]*48[^m]*m` — the first background-color SGR span on a raw line.
fn ansi_48_span(raw: &str) -> Option<(usize, usize)> {
    let bytes = raw.as_bytes();
    let mut index = 0;
    while index + 1 < bytes.len() {
        if bytes[index] == 0x1b && bytes[index + 1] == b'[' {
            let mut cursor = index + 2;
            while cursor < bytes.len() && bytes[cursor] != b'm' {
                cursor += 1;
            }
            if cursor < bytes.len() && raw[index + 2..cursor].contains("48") {
                return Some((index, cursor + 1));
            }
        }
        index += 1;
    }
    None
}

fn count_marks(value: &str) -> i64 {
    value
        .chars()
        .filter(|c| matches!(c, '☐' | '☒' | '☑' | '✓' | '✔'))
        .count() as i64
}

// ── parseCodex ──────────────────────────────────────────────────────────

struct CodexRow {
    line: usize,
    focus: bool,
    body: String,
}

/// `parseCodex` — `question N/M` header + numbered menu + footer hint.
fn parse_codex(text: &str) -> Option<Interaction> {
    let raw_lines: Vec<String> = text
        .replace("\r\n", "\n")
        .split('\n')
        .map(String::from)
        .collect();
    let mut lines = Vec::with_capacity(raw_lines.len());
    let (mut header_index, mut current, mut total) = (-1i64, 0i64, 0i64);
    for (index, raw) in raw_lines.iter().enumerate() {
        let line = clean_codex_line(raw);
        if let Some((c, t)) = codex_header_match(&line) {
            header_index = index as i64;
            current = c;
            total = t;
        }
        lines.push(line);
    }
    if header_index < 0 {
        return None;
    }
    let header_index = header_index as usize;
    let mut footer_index: i64 = -1;
    for (index, line) in lines.iter().enumerate().skip(header_index + 1) {
        if codex_footer(line) {
            footer_index = index as i64;
            break;
        }
    }
    if footer_index < 0 {
        return None;
    }
    let footer_index = footer_index as usize;
    let mut rows: Vec<CodexRow> = Vec::new();
    let mut expected: i64 = 1;
    for (index, line) in lines[header_index + 1..footer_index].iter().enumerate() {
        let index = index + header_index + 1;
        let Some((focus, number, label)) = menu_match(line) else {
            continue;
        };
        if number != expected {
            continue;
        }
        rows.push(CodexRow {
            line: index,
            focus,
            body: label.to_owned(),
        });
        expected += 1;
    }
    if rows.len() < 3 {
        return None;
    }
    let first_option = rows[0].line;
    let question_parts: Vec<&str> = lines[header_index + 1..first_option]
        .iter()
        .filter(|line| !line.is_empty())
        .map(String::as_str)
        .collect();
    let question_text = compact(&question_parts.join(" "), 1000);
    if question_text.is_empty() {
        return None;
    }
    let mut notes_start = footer_index;
    let notes_active = lines[footer_index].to_lowercase().contains("clear notes");
    if notes_active {
        for (index, line) in lines
            .iter()
            .enumerate()
            .take(footer_index)
            .skip(rows.last().expect("rows").line + 1)
        {
            if !line.trim().is_empty() {
                notes_start = index;
                break;
            }
        }
    }
    let description_column = codex_description_column(&lines, &rows);
    let mut all: Vec<QuestionOption> = Vec::with_capacity(rows.len());
    let mut focus = QuestionFocus::default();
    for (index, item) in rows.iter().enumerate() {
        let mut end = footer_index;
        if index + 1 < rows.len() {
            end = rows[index + 1].line;
        } else if notes_start < footer_index {
            end = notes_start;
        }
        let (label, desc) = codex_parts(&lines, item, end, description_column);
        if label.is_empty() {
            return None;
        }
        all.push(QuestionOption {
            index: index as i64,
            label,
            description: desc,
            selected: false,
            summary: Vec::new(),
        });
        if item.focus {
            focus = QuestionFocus {
                kind: FocusKind::Option,
                index,
            };
        }
    }
    if all.is_empty() || !is_other_label(&all[all.len() - 1].label) {
        return None;
    }
    let mut other_item = all.pop().expect("non-empty");
    let options_len = all.len();
    let mut notes = String::new();
    for line in &lines[notes_start..footer_index] {
        let trimmed = line.trim().trim_start_matches('›').trim();
        if !trimmed.is_empty() {
            notes = compact(trimmed, 20000);
        }
    }
    if notes_active {
        focus = QuestionFocus {
            kind: FocusKind::Option,
            index: options_len,
        };
    }
    if text.contains("\x1b[")
        && !raw_lines[header_index + 1..first_option]
            .join("\n")
            .contains("\x1b[38;5;6m")
        && focus.kind == FocusKind::Option
    {
        if focus.index < options_len {
            all[focus.index].selected = true;
        } else {
            other_item.selected = true;
        }
    }
    let submit_label = if current < total { "Next" } else { "Submit" };
    let all_count = options_len + 1;
    finish_interaction(Interaction {
        id: String::new(),
        kind: "single_select".to_owned(),
        question: question_text,
        options: all,
        other: QuestionOther {
            selected: other_item.selected || notes_active,
            text: notes,
            label: other_item.label.clone(),
            placeholder: "Optional notes".to_owned(),
            allow_empty: true,
            hidden: false,
        },
        submit_label: submit_label.to_owned(),
        can_chat: false,
        can_go_back: current > 1,
        question_index: current,
        question_total: total,
        focus,
        all_option_count: all_count,
        agent: "codex".to_owned(),
        notes_active,
    })
}

/// `codexDescriptionColumn` — the right-hand description column is the
/// most common 2+ space gap position across menu rows.
fn codex_description_column(lines: &[String], rows: &[CodexRow]) -> i64 {
    let mut counts: HashMap<i64, i64> = HashMap::new();
    for item in rows {
        let Some(body_start) = lines[item.line].find(&item.body) else {
            continue;
        };
        if let Some((_gap_start, gap_end)) = first_run_of_spaces(&item.body, 2) {
            let prefix_column = lines[item.line][..body_start].chars().count() as i64;
            let description_column = prefix_column + item.body[..gap_end].chars().count() as i64;
            *counts.entry(description_column).or_insert(0) += 1;
        }
    }
    let (mut best, mut best_count) = (-1i64, 0i64);
    for (column, count) in counts {
        if count > best_count || (count == best_count && (best < 0 || column < best)) {
            best = column;
            best_count = count;
        }
    }
    best
}

/// `codexParts` — split each row line at the description column.
fn codex_parts(
    lines: &[String],
    item: &CodexRow,
    end: usize,
    description_column: i64,
) -> (String, String) {
    let (mut labels, mut descriptions) = (Vec::new(), Vec::new());
    for (index, line) in lines.iter().enumerate().take(end).skip(item.line) {
        if line.is_empty() {
            continue;
        }
        if index == item.line {
            let (mut left, mut right) = (item.body.as_str(), "");
            if let Some((gap_start, gap_end)) = first_run_of_spaces(&item.body, 2) {
                left = &item.body[..gap_start];
                right = &item.body[gap_end..];
            }
            if !left.trim().is_empty() {
                labels.push(left.trim().to_owned());
            }
            if !right.trim().is_empty() {
                descriptions.push(right.trim().to_owned());
            }
            continue;
        }
        let (mut left, mut right) = (line.as_str(), "");
        if description_column >= 0 {
            let runes: Vec<char> = line.chars().collect();
            if runes.len() < description_column as usize {
                left = line;
            } else {
                left = &line[..byte_of_rune(line, description_column as usize)];
                right = &line[byte_of_rune(line, description_column as usize)..];
            }
        }
        if !left.trim().is_empty() {
            labels.push(left.trim().to_owned());
        }
        if !right.trim().is_empty() {
            descriptions.push(right.trim().to_owned());
        }
    }
    (
        compact(&labels.join(" "), 500),
        compact(&descriptions.join(" "), 500),
    )
}

fn byte_of_rune(line: &str, rune: usize) -> usize {
    line.char_indices()
        .nth(rune)
        .map(|(index, _)| index)
        .unwrap_or(line.len())
}

// ── parseQoder ──────────────────────────────────────────────────────────

/// `parseQoder` — `Asking User` header + checkbox/menu rows + footer hint.
fn parse_qoder(text: &str) -> Option<Interaction> {
    let raw_lines: Vec<String> = text
        .replace("\r\n", "\n")
        .split('\n')
        .map(String::from)
        .collect();
    let mut lines = Vec::with_capacity(raw_lines.len());
    let (mut header_index, mut footer_index) = (-1i64, -1i64);
    let (mut current, mut total) = (0i64, 0i64);
    for (index, raw) in raw_lines.iter().enumerate() {
        let line = clean_line(raw);
        if qoder_header(&line) {
            header_index = index as i64;
            let (c, t) = qoder_position(raw);
            current = c;
            total = t;
            if current == 0 && total == 0 {
                current = 1;
                total = 1;
            }
        }
        if header_index >= 0 && qoder_footer(&line) {
            footer_index = index as i64;
        }
        lines.push(line);
    }
    if header_index < 0 || footer_index <= header_index || current < 1 || total < current {
        if header_index < 0 || footer_index <= header_index {
            return None;
        }
        return parse_qoder_review(&lines, header_index as usize, footer_index as usize, total);
    }
    let header_index = header_index as usize;
    let footer_index = footer_index as usize;

    struct Row {
        line: usize,
        focus: bool,
        label: String,
        selected: bool,
    }
    let (mut checkbox_rows, mut menu_rows): (Vec<Row>, Vec<Row>) = (Vec::new(), Vec::new());
    let mut expected: i64 = 1;
    for (index, line) in lines[header_index + 1..footer_index].iter().enumerate() {
        let index = index + header_index + 1;
        if let Some((focus, number, mark, label)) = checkbox_match(line) {
            if number != expected {
                continue;
            }
            checkbox_rows.push(Row {
                line: index,
                focus,
                label: compact(label, 500),
                selected: !mark.trim().is_empty(),
            });
            expected += 1;
            continue;
        }
        let Some((focus, number, label)) = menu_match(line) else {
            continue;
        };
        if number != expected {
            continue;
        }
        menu_rows.push(Row {
            line: index,
            focus,
            label: compact(label, 500),
            selected: false,
        });
        expected += 1;
    }

    let mut kind = "single_select";
    let mut rows = &menu_rows;
    let mut submit_row: Option<&Row> = None;
    if checkbox_rows.len() >= 2 {
        kind = "multi_select";
        rows = &checkbox_rows;
        for item in &menu_rows {
            let label = item.label.trim_end_matches('→').trim();
            if eq_fold(label, "next") || eq_fold(label, "submit") {
                submit_row = Some(item);
            }
        }
    }
    let mut other_row: Option<&Row> = None;
    for item in &menu_rows {
        if is_other_label(&item.label) {
            other_row = Some(item);
        }
    }
    if rows.len() < 2 || other_row.is_none() {
        return None;
    }
    let other_row = other_row.expect("checked");
    let option_rows: Vec<&Row> = if kind == "single_select" {
        if !is_other_label(&rows[rows.len() - 1].label) {
            return None;
        }
        rows[..rows.len() - 1].iter().collect()
    } else {
        submit_row?;
        rows.iter().collect()
    };

    let first_option = option_rows[0].line;
    let mut question_text = String::new();
    for index in (header_index + 1..first_option).rev() {
        let candidate = lines[index].trim();
        if candidate.is_empty()
            || candidate
                .trim_matches(|c| matches!(c, '─' | '━' | '═' | '_' | '—' | '│' | '|' | ' '))
                .is_empty()
        {
            continue;
        }
        let lower = candidate.to_lowercase();
        if candidate.starts_with('(') && lower.contains("select all") {
            continue;
        }
        question_text = compact(candidate, 1000);
        break;
    }
    if question_text.is_empty() {
        return None;
    }

    let mut options: Vec<QuestionOption> = Vec::with_capacity(option_rows.len());
    let mut focus = QuestionFocus::default();
    for (index, item) in option_rows.iter().enumerate() {
        let mut end = other_row.line;
        if index + 1 < option_rows.len() {
            end = option_rows[index + 1].line;
        } else if let Some(submit) = submit_row {
            end = submit.line;
        }
        options.push(QuestionOption {
            index: index as i64,
            label: item.label.clone(),
            description: description(&lines, item.line, end),
            selected: item.selected,
            summary: Vec::new(),
        });
        if item.focus {
            focus = QuestionFocus {
                kind: FocusKind::Option,
                index,
            };
        }
    }
    if submit_row.is_some_and(|row| row.focus) {
        focus = QuestionFocus {
            kind: FocusKind::Submit,
            index: 0,
        };
    }
    if other_row.focus {
        focus = QuestionFocus {
            kind: FocusKind::Other,
            index: 0,
        };
    }
    let mut notes = String::new();
    let notes_active =
        other_row.focus && lines[footer_index].to_lowercase().contains("enter submit");
    if notes_active {
        let note_lines: Vec<String> = lines[other_row.line + 1..footer_index]
            .iter()
            .filter(|line| !line.is_empty())
            .map(|line| line.trim().to_owned())
            .collect();
        notes = compact(&note_lines.join(" "), 20000);
    }
    let option_count = options.len();
    let mut interaction = Interaction {
        id: String::new(),
        kind: kind.to_owned(),
        question: question_text,
        options,
        other: QuestionOther {
            selected: other_row.selected || notes_active,
            text: notes,
            label: other_row.label.clone(),
            placeholder: "Type an answer".to_owned(),
            allow_empty: false,
            hidden: false,
        },
        submit_label: "Next".to_owned(),
        can_chat: false,
        can_go_back: current > 1,
        question_index: current,
        question_total: total,
        focus,
        all_option_count: option_count + 1,
        agent: "qoder".to_owned(),
        notes_active,
    };
    if current == total {
        interaction.submit_label = "Submit".to_owned();
    }
    finish_interaction(interaction)
}

/// `qoderPosition` — tab labels after the `·` on the raw header line; the
/// active one carries the background highlight.
fn qoder_position(raw: &str) -> (i64, i64) {
    let clean = clean_line(raw);
    let Some(dot) = clean.find('·') else {
        return (0, 0);
    };
    let parts = clean[dot + '·'.len_utf8()..].split('>');
    let mut tabs: Vec<String> = Vec::new();
    for part in parts {
        let label = part.trim();
        if label.is_empty() || eq_fold(label, "submit") {
            continue;
        }
        tabs.push(label.to_owned());
    }
    let Some(active) = qoder_active_label(raw) else {
        return (0, tabs.len() as i64);
    };
    let active_label = clean_line(&active);
    let active_label = active_label.trim();
    for (index, label) in tabs.iter().enumerate() {
        if label == active_label {
            return (index as i64 + 1, tabs.len() as i64);
        }
    }
    (0, tabs.len() as i64)
}

/// `parseQoderReview` — the "Review your answers:" screen.
fn parse_qoder_review(
    lines: &[String],
    header_index: usize,
    footer_index: usize,
    question_total: i64,
) -> Option<Interaction> {
    let mut review_index: i64 = -1;
    for (index, line) in lines
        .iter()
        .enumerate()
        .take(footer_index)
        .skip(header_index + 1)
    {
        if eq_fold(line.trim(), "Review your answers:") {
            review_index = index as i64;
            break;
        }
    }
    if review_index < 0 {
        return None;
    }
    let review_index = review_index as usize;
    let mut summary: Vec<SummaryEntry> = Vec::new();
    let mut options: Vec<QuestionOption> = Vec::new();
    let mut focus = QuestionFocus::default();
    for line in lines
        .iter()
        .take(footer_index)
        .skip(review_index + 1)
        .map(|line| line.trim())
    {
        if let Some(row_focus) = qoder_review_match(line) {
            let option_index = options.len();
            let body = line.trim_start().trim_start_matches(['❯', '›']).trim();
            options.push(QuestionOption {
                index: option_index as i64,
                label: title(body),
                ..QuestionOption::default()
            });
            if row_focus {
                focus = QuestionFocus {
                    kind: FocusKind::Option,
                    index: option_index,
                };
            }
            continue;
        }
        if line.contains('→') {
            let mut parts = line.splitn(2, '→');
            summary.push(SummaryEntry {
                question: parts.next().unwrap_or("").trim().to_owned(),
                answer: parts.next().unwrap_or("").trim().to_owned(),
            });
        }
    }
    if options.len() != 2 {
        return None;
    }
    options[0].description = summary_lines(&summary, 1000);
    options[0].summary = summary;
    let step = question_total + 1;
    finish_interaction(Interaction {
        id: String::new(),
        kind: "single_select".to_owned(),
        question: "Review your answers and choose what to do".to_owned(),
        options,
        other: QuestionOther {
            hidden: true,
            ..QuestionOther::default()
        },
        submit_label: "Continue".to_owned(),
        can_chat: false,
        can_go_back: true,
        question_index: step,
        question_total: step,
        focus,
        all_option_count: 2,
        agent: "qoder".to_owned(),
        notes_active: false,
    })
}

// ── parseOpenCode ───────────────────────────────────────────────────────

/// `parseOpenCode` — the `┃`-framed question with the `esc dismiss` footer.
fn parse_opencode(text: &str) -> Option<Interaction> {
    let raw_lines: Vec<String> = text
        .replace("\r\n", "\n")
        .split('\n')
        .map(String::from)
        .collect();
    let mut footer_index: i64 = -1;
    let mut lines = Vec::with_capacity(raw_lines.len());
    for raw in &raw_lines {
        let line = clean_opencode_line(raw);
        if opencode_footer(&line) {
            footer_index = lines.len() as i64;
        }
        lines.push(line);
    }
    if footer_index < 0 || !opencode_tail_is_empty(&lines, footer_index as usize) {
        return None;
    }
    let footer_index = footer_index as usize;
    let (mut current, mut total) = opencode_position(&raw_lines, &lines, footer_index);
    if let Some(interaction) = parse_opencode_review(&lines, footer_index, current, total) {
        return Some(interaction);
    }

    struct Row {
        line: usize,
        focus: bool,
        label: String,
        selected: bool,
    }
    let mut runs: Vec<Vec<Row>> = Vec::new();
    let mut current_run: Vec<Row> = Vec::new();
    let mut expected: i64 = 1;
    macro_rules! flush {
        () => {
            if !current_run.is_empty() {
                runs.push(std::mem::take(&mut current_run));
            }
        };
    }
    for index in 0..footer_index {
        let matched = checkbox_match(&lines[index]);
        let (number, label, mark_selected) = match (matched, menu_match(&lines[index])) {
            (Some((_, n, mark, label)), _) => (n, label, !mark.trim().is_empty()),
            (None, Some((_, n, label))) => (n, label, false),
            _ => continue,
        };
        if number == 1 {
            flush!();
            expected = 1;
        }
        if number != expected {
            flush!();
            expected = 1;
            continue;
        }
        let mut label = compact(label, 500);
        let mut selected = mark_selected;
        if selected_mark(&label) {
            selected = true;
            label = strip_selected_mark(&label).to_owned();
        }
        current_run.push(Row {
            line: index,
            focus: opencode_focus_match(&raw_lines[index]),
            label,
            selected,
        });
        expected += 1;
    }
    if !current_run.is_empty() {
        runs.push(current_run);
    }
    let rows = runs.pop()?;
    if rows.len() < 2 || !is_other_label(&rows[rows.len() - 1].label) {
        return None;
    }

    let first_option = rows[0].line;
    let mut question_text = String::new();
    for index in (0..first_option).rev() {
        let candidate = lines[index].trim();
        if candidate.is_empty() {
            continue;
        }
        question_text = compact(candidate, 1000);
        break;
    }
    if question_text.is_empty() {
        return None;
    }

    let mut all: Vec<QuestionOption> = Vec::with_capacity(rows.len());
    let mut focus = QuestionFocus::default();
    for (index, item) in rows.iter().enumerate() {
        let end = rows
            .get(index + 1)
            .map(|row| row.line)
            .unwrap_or(footer_index);
        all.push(QuestionOption {
            index: index as i64,
            label: item.label.clone(),
            description: description(&lines, item.line, end),
            selected: item.selected,
            summary: Vec::new(),
        });
        if item.focus {
            focus = QuestionFocus {
                kind: FocusKind::Option,
                index,
            };
        }
    }
    let mut other_item = all.pop().expect("rows >= 2");
    let options_len = all.len();
    let mut other_text = other_item.description.clone();
    if eq_fold(&other_text, &other_item.label) {
        other_text = String::new();
    }
    other_item.description = String::new();
    let notes_active = focus
        == QuestionFocus {
            kind: FocusKind::Option,
            index: options_len,
        }
        && !other_item.selected;
    if notes_active {
        focus = QuestionFocus {
            kind: FocusKind::Other,
            index: 0,
        };
    }

    let kind = if checkbox_match(&lines[rows[0].line]).is_some() {
        "multi_select"
    } else {
        "single_select"
    };
    if current < 1 || total < current {
        current = 1;
        total = 1;
    }
    let submit_label = if current == total { "Submit" } else { "Next" };
    let all_count = options_len + 1;
    finish_interaction(Interaction {
        id: String::new(),
        kind: kind.to_owned(),
        question: question_text,
        options: all,
        other: QuestionOther {
            selected: other_item.selected,
            text: other_text,
            label: other_item.label.clone(),
            placeholder: "Type your own answer".to_owned(),
            allow_empty: false,
            hidden: false,
        },
        submit_label: submit_label.to_owned(),
        can_chat: false,
        can_go_back: current > 1,
        question_index: current,
        question_total: total,
        focus,
        all_option_count: all_count,
        agent: "opencode".to_owned(),
        notes_active,
    })
}

/// `parseOpenCodeReview` — the "Review" tab screen.
fn parse_opencode_review(
    lines: &[String],
    footer_index: usize,
    _current: i64,
    total: i64,
) -> Option<Interaction> {
    let mut review_index: i64 = -1;
    for index in (0..footer_index).rev() {
        if eq_fold(lines[index].trim(), "Review") {
            review_index = index as i64;
            break;
        }
    }
    if review_index < 0 {
        return None;
    }
    let review_index = review_index as usize;
    let mut summary: Vec<SummaryEntry> = Vec::new();
    for line in &lines[review_index + 1..footer_index] {
        let line = line.trim();
        if line.is_empty() || !line.contains(':') {
            continue;
        }
        summary.push(split_summary_entry(line));
    }
    if summary.is_empty() {
        return None;
    }
    let mut total = total;
    if total < 1 {
        total = summary.len() as i64;
    }
    let current = total + 1;
    let total = current;
    finish_interaction(Interaction {
        id: String::new(),
        kind: "single_select".to_owned(),
        question: "Review your answers and choose what to do".to_owned(),
        options: vec![QuestionOption {
            index: 0,
            label: "Submit answers".to_owned(),
            description: summary_lines(&summary, 1000),
            selected: false,
            summary,
        }],
        other: QuestionOther {
            hidden: true,
            ..QuestionOther::default()
        },
        submit_label: "Continue".to_owned(),
        can_chat: false,
        can_go_back: true,
        question_index: current,
        question_total: total,
        focus: QuestionFocus::default(),
        all_option_count: 1,
        agent: "opencode".to_owned(),
        notes_active: false,
    })
}

/// `openCodePosition` — the `question N/M` position comes from the tab row
/// ("Question 1 … Confirm") just above the footer; the active tab carries
/// the highlight color.
fn opencode_position(raw_lines: &[String], lines: &[String], footer_index: usize) -> (i64, i64) {
    for index in (0..footer_index).rev() {
        if !lines[index].to_lowercase().contains("confirm") {
            continue;
        }
        let labels = opencode_tabs(&lines[index]);
        if labels.len() < 2 {
            continue;
        }
        let active = opencode_active_label(&raw_lines[index]);
        if active.is_empty() {
            return (0, labels.len() as i64 - 1);
        }
        for (tab_index, label) in labels.iter().enumerate() {
            if eq_fold(&active, label) {
                if eq_fold(label, "confirm") {
                    return (labels.len() as i64, labels.len() as i64 - 1);
                }
                return (tab_index as i64 + 1, labels.len() as i64 - 1);
            }
        }
    }
    (0, 0)
}

/// `openCodeTabs` — tab labels are separated by 2+ space runs.
fn opencode_tabs(line: &str) -> Vec<String> {
    let mut labels = Vec::new();
    let mut rest = line.trim();
    while !rest.is_empty() {
        match first_run_of_spaces(rest, 2) {
            Some((start, end)) => {
                let label = rest[..start].trim();
                if !label.is_empty() {
                    labels.push(label.to_owned());
                }
                rest = &rest[end..];
            }
            None => {
                labels.push(rest.trim().to_owned());
                break;
            }
        }
    }
    labels
        .into_iter()
        .filter(|label| !label.is_empty())
        .collect()
}

// ── parseOMP ────────────────────────────────────────────────────────────

struct OmpRow {
    line: usize,
    focus: bool,
    marker: String,
    label: String,
    selected: bool,
}

/// `parseOMP` — the `╭ Ask` frame with marker-row options.
fn parse_omp(text: &str) -> Option<Interaction> {
    let lines = omp_clean_lines(text);
    let raw_lines: Vec<String> = text
        .replace("\r\n", "\n")
        .split('\n')
        .map(String::from)
        .collect();
    let mut start: i64 = -1;
    for (index, line) in lines.iter().enumerate() {
        if omp_ask_header(line) {
            start = index as i64;
        }
    }
    if start < 0 {
        return None;
    }
    let start = start as usize;
    let mut end = lines.len();
    for (index, line) in lines.iter().enumerate().skip(start + 1) {
        let lower = line.to_lowercase();
        if lower.contains("enter select") || lower.contains("enter submit") {
            end = index;
            break;
        }
    }
    let mut rows: Vec<OmpRow> = Vec::new();
    for (index, line) in lines.iter().enumerate().take(end).skip(start + 1) {
        let Some((focus, marker, label)) = omp_option_match(line) else {
            continue;
        };
        rows.push(OmpRow {
            line: index,
            focus,
            marker: marker.to_owned(),
            label: compact(label, 500),
            selected: omp_marker_selected(marker),
        });
    }
    if rows.len() < 2 {
        return parse_omp_review(&lines, start, end);
    }

    let question = omp_question(&lines, start, rows[0].line);
    let (mut current, mut total) = omp_position(&lines, &raw_lines, start, &question);
    if let Some((c, t)) = omp_progress_match(&question) {
        current = c;
        total = t;
        // Strip the trailing " (c / t)" run the matcher consumed.
        let progress_len = question.rfind('(');
        let mut trimmed = question.trim().to_owned();
        if let Some(open) = progress_len {
            trimmed = trimmed[..open].trim_end().to_owned();
        }
        return finish_omp(&lines, &rows, trimmed, (current, total), start, end);
    }
    finish_omp(&lines, &rows, question, (current, total), start, end)
}

/// Shared tail of `parseOMP` — assembles the interaction once the question
/// text and position are known.
fn finish_omp(
    lines: &[String],
    rows: &[OmpRow],
    question: String,
    position: (i64, i64),
    _start: usize,
    end: usize,
) -> Option<Interaction> {
    let (mut current, mut total) = position;
    let kind = if omp_checkbox_marker(&rows[0].marker) {
        "multi_select"
    } else {
        "single_select"
    };
    let mut options: Vec<QuestionOption> = Vec::with_capacity(rows.len() - 1);
    let mut other = QuestionOther {
        hidden: true,
        ..QuestionOther::default()
    };
    let mut focus = QuestionFocus::default();
    for (row_index, row) in rows.iter().enumerate() {
        let label = &row.label;
        if eq_fold(label, "done selecting") || contains_fold(label, "done selecting") {
            if row.focus {
                focus = QuestionFocus {
                    kind: FocusKind::Submit,
                    index: 0,
                };
            }
            continue;
        }
        let row_end = rows.get(row_index + 1).map(|row| row.line).unwrap_or(end);
        if is_other_label(label) {
            other = QuestionOther {
                selected: row.selected,
                text: omp_description(lines, row.line, row_end),
                label: label.clone(),
                placeholder: String::new(),
                allow_empty: false,
                hidden: false,
            };
            if row.focus {
                focus = QuestionFocus {
                    kind: FocusKind::Option,
                    index: options.len(),
                };
            }
            continue;
        }
        let option_index = options.len();
        options.push(QuestionOption {
            index: option_index as i64,
            label: label.clone(),
            description: omp_description(lines, row.line, row_end),
            selected: row.selected,
            summary: Vec::new(),
        });
        if row.focus {
            focus = QuestionFocus {
                kind: FocusKind::Option,
                index: option_index,
            };
        }
    }
    if options.is_empty() || (other.hidden && options.len() < 2) {
        return None;
    }
    let mut all_options = options.len();
    if !other.hidden {
        all_options += 1;
    }
    if total == 0 {
        current = 1;
        total = 1;
    }
    let submit_label = if current < total { "Next" } else { "Submit" };
    finish_interaction(Interaction {
        id: String::new(),
        kind: kind.to_owned(),
        question,
        options,
        other,
        submit_label: submit_label.to_owned(),
        can_chat: false,
        can_go_back: current > 1,
        question_index: current,
        question_total: total,
        focus,
        all_option_count: all_options,
        agent: "omp".to_owned(),
        notes_active: false,
    })
}

/// `parseOMPReview` — the "Review answers" screen inside the Ask frame.
fn parse_omp_review(lines: &[String], start: usize, end: usize) -> Option<Interaction> {
    let (mut review, mut submit) = (-1i64, -1i64);
    for (index, line) in lines
        .iter()
        .enumerate()
        .take(end)
        .skip(start + 1)
        .map(|(index, line)| (index, line.trim()))
    {
        if eq_fold(line, "review answers") {
            review = index as i64;
            continue;
        }
        if review >= 0 && omp_review_submit_match(line) {
            submit = index as i64;
            break;
        }
    }
    if review < 0 || submit < 0 {
        return None;
    }
    let mut summary: Vec<SummaryEntry> = Vec::new();
    for line in &lines[review as usize + 1..submit as usize] {
        if let Some((_focus, _number, label)) = menu_match(line) {
            summary.push(split_summary_entry(label));
        }
    }
    let mut question_total = 1i64;
    if let Some(ids) = omp_tab_ids(lines, start).0 {
        if !ids.is_empty() {
            question_total = ids.len() as i64 + 1;
        }
    }
    finish_interaction(Interaction {
        id: String::new(),
        kind: "single_select".to_owned(),
        question: "Review answers".to_owned(),
        options: vec![QuestionOption {
            index: 0,
            label: "Submit answers".to_owned(),
            description: summary_lines(&summary, 1000),
            selected: true,
            summary,
        }],
        other: QuestionOther {
            hidden: true,
            ..QuestionOther::default()
        },
        submit_label: "Submit".to_owned(),
        can_chat: false,
        can_go_back: question_total > 1,
        question_index: question_total,
        question_total,
        focus: QuestionFocus::default(),
        all_option_count: 1,
        agent: "omp".to_owned(),
        notes_active: false,
    })
}

/// `ompQuestion` — the nearest content line above the first option.
fn omp_question(lines: &[String], start: usize, first_option: usize) -> String {
    for index in (start + 1..first_option).rev() {
        let line = &lines[index];
        if line.is_empty() || omp_border_line(line) {
            continue;
        }
        return compact(line, 1000);
    }
    "OMP needs an answer".to_owned()
}

/// `ompDescription` — `↳` detail lines under an option row.
fn omp_description(lines: &[String], start: usize, end: usize) -> String {
    let mut parts = Vec::new();
    for line in &lines[start + 1..end] {
        if line.is_empty() || omp_border_line(line) || line.to_lowercase().contains("enter select")
        {
            continue;
        }
        parts.push(line.trim_start_matches('↳').trim().to_owned());
    }
    compact(&parts.join(" "), 500)
}

/// `ompTabIDs` — question ids on the Ask header's tab row (wrapping onto
/// continuation lines); the `Submit` tab ends the row.
fn omp_tab_ids(lines: &[String], ask_start: usize) -> (Option<Vec<String>>, usize) {
    let mut ids = Vec::new();
    for (index, line) in lines
        .iter()
        .enumerate()
        .take(lines.len().min(ask_start + 5))
        .skip(ask_start + 1)
    {
        for field in fields(line) {
            if eq_fold(field, "submit") {
                return (Some(ids), index);
            }
            if !omp_tab_id(field) {
                return (None, usize::MAX);
            }
            ids.push(field.to_owned());
        }
    }
    (None, usize::MAX)
}

/// `ompActiveTab` — the tab rendered with the bold active style (raw only);
/// scans the tab-row span for `ompActiveTabPattern`'s single-line matcher.
fn omp_active_tab_in(raw_lines: &[String], first: usize, last: usize) -> String {
    for line in raw_lines
        .iter()
        .take(last.min(raw_lines.len().saturating_sub(1)) + 1)
        .skip(first)
    {
        if let Some(label) = omp_active_tab(line) {
            return label;
        }
    }
    String::new()
}

/// `ompPosition` — current/total question position from the tab row or the
/// surrounding meta frame's `[id] · options:` prompt map.
fn omp_position(
    lines: &[String],
    raw_lines: &[String],
    ask_start: usize,
    question: &str,
) -> (i64, i64) {
    let (ids, tab_end) = omp_tab_ids(lines, ask_start);
    if let Some(ids) = &ids {
        if !ids.is_empty() {
            let active = omp_active_tab_in(raw_lines, ask_start + 1, tab_end);
            if !active.is_empty() {
                for (index, id) in ids.iter().enumerate() {
                    if eq_fold(id, &active) {
                        return (index as i64 + 1, ids.len() as i64);
                    }
                }
            }
        }
    }
    let mut frame_end: i64 = -1;
    for index in (0..ask_start).rev() {
        if lines[index].starts_with('╰') {
            frame_end = index as i64;
            break;
        }
    }
    let mut frame_start: i64 = -1;
    if frame_end >= 0 {
        for index in (0..frame_end as usize).rev() {
            if lines[index].starts_with('╭') {
                frame_start = index as i64;
                break;
            }
        }
    }

    let mut meta_ids: Vec<String> = Vec::new();
    let mut prompts: Vec<String> = Vec::new();
    if frame_start >= 0 {
        for (index, line) in lines
            .iter()
            .enumerate()
            .take(frame_end as usize)
            .skip(frame_start as usize + 1)
        {
            let Some(id) = omp_frame_meta_match(line) else {
                continue;
            };
            let mut prompt = String::new();
            for candidate in lines.iter().take(frame_end as usize).skip(index + 1) {
                if omp_frame_meta_match(candidate).is_some() {
                    break;
                }
                if candidate.is_empty() || omp_border_line(candidate) {
                    continue;
                }
                prompt = compact(candidate, 1000);
                break;
            }
            meta_ids.push(id.to_owned());
            prompts.push(prompt);
        }
    }

    let mut current_id = String::new();
    for (index, prompt) in prompts.iter().enumerate() {
        if prompt == question {
            current_id = meta_ids[index].clone();
            break;
        }
    }
    if !current_id.is_empty() {
        if let Some(ids) = &ids {
            for (index, id) in ids.iter().enumerate() {
                if eq_fold(id, &current_id) {
                    return (index as i64 + 1, ids.len() as i64);
                }
            }
        }
    }
    for (index, prompt) in prompts.iter().enumerate() {
        if prompt == question {
            return (index as i64 + 1, prompts.len() as i64);
        }
    }
    if let Some(ids) = &ids {
        if !ids.is_empty() {
            return (0, ids.len() as i64);
        }
    }
    (0, prompts.len() as i64)
}

// ── shared parse helpers ────────────────────────────────────────────────

/// `description` — non-control lines under a menu row form its description.
fn description(lines: &[String], start: usize, end: usize) -> String {
    let mut parts = Vec::new();
    for line in &lines[start + 1..end.min(lines.len())] {
        if line.is_empty()
            || checkbox_match(line).is_some()
            || menu_match(line).is_some()
            || submit_match(line).is_some()
            || chat_match(line).is_some()
            || line.to_lowercase().contains("enter to select")
        {
            continue;
        }
        parts.push(line.clone());
    }
    compact(&parts.join(" "), 500)
}

/// `prompt` — the question text above the first option row.
fn prompt(lines: &[String], first_option: usize) -> String {
    let (mut start, mut end) = (-1i64, -1i64);
    for index in (0..first_option).rev() {
        let line = &lines[index];
        let lower = line.to_lowercase();
        let boundary = line.is_empty()
            || submit_match(line).is_some()
            || chat_match(line).is_some()
            || lower.contains("enter to select")
            || (line.contains("Submit") && line.contains('→'));
        if boundary {
            if end >= 0 {
                break;
            }
            continue;
        }
        if end < 0 {
            end = index as i64;
        }
        start = index as i64;
    }
    if end < 0 {
        return "Claude Code needs an answer".to_owned();
    }
    compact(&lines[start as usize..end as usize + 1].join(" "), 1000)
}

/// `summaryLines` — one compacted answer per line for the review option's
/// description.
fn summary_lines(entries: &[SummaryEntry], limit: usize) -> String {
    let mut parts = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let mut line = format!("{}. {}", index + 1, compact(&entry.question, limit));
        if !entry.answer.is_empty() {
            line.push_str(": ");
            line.push_str(&compact(&entry.answer, limit));
        }
        parts.push(line);
    }
    parts.join("\n")
}

/// `splitSummaryEntry` — a `"label: value"` review line.
fn split_summary_entry(line: &str) -> SummaryEntry {
    if let Some((question, answer)) = line.split_once(": ") {
        return SummaryEntry {
            question: question.to_owned(),
            answer: answer.to_owned(),
        };
    }
    SummaryEntry {
        question: line.to_owned(),
        answer: String::new(),
    }
}

/// `CustomAnswerPlaceholder`.
const CUSTOM_ANSWER_PLACEHOLDER: &str = "custom answer";

/// `SummaryKey` — normalize a question so review entries match the views
/// free text was typed into.
fn summary_key(value: &str) -> String {
    compact(value.trim().trim_end_matches('?'), 500).to_lowercase()
}

/// `FillCustomAnswers` — swap placeholder review answers for the recorded
/// free text and refresh the description.
fn fill_custom_answers(interaction: &mut Interaction, answers: &HashMap<String, String>) {
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

// ═══════════════════════════════════════════════════════════════════════
// Attention classification — `internal/question/attention.go`. `Classify`
// decides what the live control region asks of the user; approval panes
// yield options + focus + the fingerprint inputs.
// ═══════════════════════════════════════════════════════════════════════

/// `question.AttentionKind`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum AttentionKind {
    Approval,
    Question,
    Chat,
    #[default]
    Unknown,
}

/// `question.Classification` — the `interaction`/`question_layout` fields
/// mirror the oracle's attention shape for the projection that lands with
/// the session watcher; the handlers drive `parse_question` directly and
/// read only `kind`/`options`/`approval_focus` for now.
#[derive(Debug, Clone, Default)]
struct Classification {
    kind: AttentionKind,
    prompt: String,
    command: String,
    options: Vec<String>,
    approval_focus: usize,
    #[allow(dead_code)]
    interaction: Option<Interaction>,
    #[allow(dead_code)]
    question_layout: bool,
    approval_identity: String,
    approval_source: String,
}

/// `ApprovalFingerprint` — sha256 of source+prompt+command+options, or the
/// client-supplied identity when the classifier already carries one.
fn approval_fingerprint(classification: &Classification) -> String {
    if classification.kind != AttentionKind::Approval || classification.options.len() < 2 {
        return String::new();
    }
    if !classification.approval_identity.is_empty() {
        return classification.approval_identity.clone();
    }
    #[derive(Serialize)]
    struct Fingerprint<'a> {
        #[serde(skip_serializing_if = "String::is_empty")]
        source: String,
        prompt: &'a str,
        command: &'a str,
        options: &'a [String],
    }
    // `json.Marshal` — `lerdr_core::json` reproduces Go's HTML-safe
    // escaping so commands like `foo && bar` hash identically.
    let encoded = lerdr_core::json::to_vec(&Fingerprint {
        source: stable_approval_source(&classification.approval_source),
        prompt: &classification.prompt,
        command: &classification.command,
        options: &classification.options,
    })
    .unwrap_or_default();
    hex::encode(Sha256::digest(&encoded))
}

/// `Classify` — question first for structured agents, approval details
/// next, the idle prompt last; blocked agents are not inputs.
fn classify(text: &str, agent: &str) -> Classification {
    let structured_controls = supports(agent);
    let input_ready = normal_input_prompt(text, agent);
    if !structured_controls && !input_ready {
        return Classification {
            kind: AttentionKind::Unknown,
            prompt: compact(&pane_summary(text), 500),
            ..Classification::default()
        };
    }
    if structured_controls {
        if let Some(interaction) = parse_question(text, agent) {
            return Classification {
                kind: AttentionKind::Question,
                prompt: interaction.question.clone(),
                command: interaction.question.clone(),
                interaction: Some(interaction),
                question_layout: true,
                ..Classification::default()
            };
        }
        if let Some((options, focus, command)) = live_approval_details(text, agent) {
            if !options.is_empty() {
                let summary = approval_summary_lines(text, agent);
                let command = if command.is_empty() {
                    approval_command(&summary)
                } else {
                    command
                };
                return Classification {
                    kind: AttentionKind::Approval,
                    prompt: compact(&summary.join("\n"), 500),
                    command: compact(&command, 240),
                    options,
                    approval_focus: focus,
                    approval_source: approval_dialog_source(text, agent),
                    ..Classification::default()
                };
            }
        }
    }
    if input_ready {
        let mut response = latest_completed_response(text);
        if response.is_empty() {
            response = pane_summary(text);
        }
        return Classification {
            kind: AttentionKind::Chat,
            prompt: response,
            ..Classification::default()
        };
    }
    Classification {
        kind: AttentionKind::Unknown,
        prompt: compact(&pane_summary(text), 500),
        ..Classification::default()
    }
}

/// `approvalSummaryLines` — the 12-line summary tail; Hermes drops its
/// volatile chrome first so repaints do not shift the fingerprint inputs.
fn approval_summary_lines(text: &str, agent: &str) -> Vec<String> {
    if !agent.to_lowercase().contains("hermes") {
        return pane_summary_lines(text);
    }
    let mut filtered = Vec::new();
    for line in clean_lines(text) {
        if line.is_empty()
            || is_chrome(&line)
            || is_prompt_skip(&line)
            || hermes_approval_chrome_line(&line)
        {
            continue;
        }
        let mut line = line;
        if let Some((_focus, number, label)) = menu_match(&line) {
            let label = compact(label, 500);
            if hermes_approval_auxiliary(&label) {
                continue;
            }
            // Menu focus and the native number-column padding are presentation.
            line = format!("{}. {}", number, label).trim().to_owned();
        }
        filtered.push(line);
    }
    if filtered.len() > 12 {
        filtered = filtered[filtered.len() - 12..].to_vec();
    }
    filtered
}

/// `liveApprovalDetails` — options + focus + an inline command for the
/// agent's live approval dialog, or none when the menu is not approvable.
fn live_approval_details(text: &str, agent: &str) -> Option<(Vec<String>, usize, String)> {
    let normalized = agent.to_lowercase();
    if normalized.contains("opencode") {
        return None;
    }
    let lines = clean_lines(text);
    if omp_ask_agent(&normalized) {
        if let Some((options, focus, command)) = omp_tool_approval_details(&lines) {
            if !options.is_empty() {
                return Some((options, focus, command));
            }
        }
        let (options, focus) = omp_plan_approval_details(&lines);
        return Some((options, focus, String::new()));
    }
    let raw_lines: Vec<String> = text
        .replace("\r\n", "\n")
        .split('\n')
        .map(String::from)
        .collect();
    let menu_lines: Vec<String> = raw_lines
        .iter()
        .map(|line| clean_codex_line(line))
        .collect();
    let rows = latest_approval_menu(&menu_lines);
    if rows.len() < 2 {
        return None;
    }
    let menu_end = rows[rows.len() - 1].line;
    let is_hermes = normalized.contains("hermes");
    let mut focus = 0usize;
    let mut rows = rows;
    if is_hermes {
        let (kept, kept_focus, ok) = hermes_approval_rows(rows);
        if !ok {
            return None;
        }
        rows = kept;
        focus = kept_focus;
    } else if !approval_labels(&rows) {
        return None;
    }

    let latest_completed = latest_completed_turn_line(&lines);
    if (rows[0].line as i64) <= latest_completed {
        return None;
    }
    let mut header_start = (latest_completed + 1).max(0) as usize;
    if rows[0].line > 16 && rows[0].line - 16 > header_start {
        header_start = rows[0].line - 16;
    }
    let header = lines[header_start.min(lines.len())..rows[0].line.min(lines.len())].join("\n");
    if !approval_header(&normalized, &header) {
        return None;
    }
    if newer_output_after_menu(&menu_lines, menu_end, &normalized) {
        return None;
    }

    let mut options = Vec::with_capacity(rows.len());
    for row in &rows {
        options.push(row.label.clone());
        if !is_hermes && row.focus {
            focus = options.len() - 1;
        }
    }
    Some((options, focus, String::new()))
}

/// `approvalDialogSource` — the stable text the fingerprint hashes: the
/// dialog's header+menu for menu agents, the bordered box for OMP.
fn approval_dialog_source(text: &str, agent: &str) -> String {
    let normalized = agent.to_lowercase();
    if normalized.contains("opencode") {
        return String::new();
    }
    if omp_ask_agent(&normalized) {
        let lines = clean_lines(text);
        if let Some(header) = last_line_matching(&lines, omp_tool_approval_match) {
            for end in header + 1..lines.len() {
                if lines[end].starts_with('╰') {
                    return lines[header..end + 1].join("\n");
                }
            }
        }
        let Some(header) = last_line_matching(&lines, omp_plan_menu_match) else {
            return String::new();
        };
        for end in header + 1..lines.len() {
            if lines[end].is_empty() || omp_border_line(&lines[end]) {
                return lines[header..end].join("\n");
            }
        }
        return String::new();
    }
    let raw_lines: Vec<String> = text
        .replace("\r\n", "\n")
        .split('\n')
        .map(String::from)
        .collect();
    let menu_lines: Vec<String> = raw_lines
        .iter()
        .map(|line| clean_codex_line(line))
        .collect();
    let mut rows = latest_approval_menu(&menu_lines);
    if rows.len() < 2 {
        return String::new();
    }
    let mut end = rows[rows.len() - 1].line;
    if normalized.contains("hermes") {
        let (kept, _focus, ok) = hermes_approval_rows(rows);
        if !ok {
            return String::new();
        }
        rows = kept;
        end = rows[rows.len() - 1].line;
    }
    let mut header_start = (latest_completed_turn_line(&clean_lines(text)) + 1).max(0) as usize;
    if rows[0].line > 16 && rows[0].line - 16 > header_start {
        header_start = rows[0].line - 16;
    }
    menu_lines[header_start.min(menu_lines.len())..end.min(menu_lines.len().saturating_sub(1)) + 1]
        .join("\n")
}

/// `maxTrailingStatusLines` — a live dialog sits directly above the status
/// line; more trailing content means the box scrolled into history.
const MAX_TRAILING_STATUS_LINES: usize = 6;

/// `ompDialogContentRow`.
fn omp_dialog_content_row(row: &str) -> bool {
    !row.is_empty() && !omp_border_line(row) && !approval_footer_match(row)
}

fn last_line_matching(lines: &[String], matcher: impl Fn(&str) -> bool) -> Option<usize> {
    lines.iter().rposition(|line| matcher(line))
}

/// `ompToolApprovalDetails` — the "Allow tool: bash" border box with
/// unnumbered focus-marker rows; returns the dialog's own `Command:`.
fn omp_tool_approval_details(lines: &[String]) -> Option<(Vec<String>, usize, String)> {
    let header = last_line_matching(lines, omp_tool_approval_match)?;
    let mut end: i64 = -1;
    for (index, line) in lines.iter().enumerate().skip(header + 1) {
        if line.starts_with('╰') {
            end = index as i64;
            break;
        }
    }
    if end < 0 {
        return None;
    }
    let end = end as usize;
    if latest_completed_turn_line(lines) > end as i64 {
        return None;
    }
    let trailing = lines[end + 1..]
        .iter()
        .filter(|line| !line.is_empty())
        .count();
    if trailing > MAX_TRAILING_STATUS_LINES {
        return None;
    }
    let rows = &lines[header + 1..end];
    let mut command = String::new();
    let mut command_end: i64 = -1;
    for (index, row) in rows.iter().enumerate() {
        if row.to_lowercase().starts_with("command:") {
            command = row["command:".len()..].trim().to_owned();
            let mut cursor = index;
            while cursor + 1 < rows.len() {
                let next = &rows[cursor + 1];
                if !omp_dialog_content_row(next) || omp_plan_focus_match(next).is_some() {
                    break;
                }
                // continuation row of a command wider than the dialog box
                command.push(' ');
                command.push_str(next);
                cursor += 1;
            }
            command_end = cursor as i64;
            break;
        }
    }
    let marker = last_line_matching(rows, |line| omp_plan_focus_match(line).is_some())?;
    // Options are the contiguous run of rows around the focus marker; detail
    // rows (Command:, Path:, wrapped values) sit in their own
    // blank-delimited blocks above it.
    let mut start = marker;
    let mut stop = marker;
    while start as i64 - 1 > command_end && start > 0 && omp_dialog_content_row(&rows[start - 1]) {
        start -= 1;
    }
    while stop + 1 < rows.len() && omp_dialog_content_row(&rows[stop + 1]) {
        stop += 1;
    }
    // A run not blank- or border-delimited above means detail rows rendered
    // flush against the controls; refuse rather than emit a mis-indexed menu.
    if start > 0 && !rows[start - 1].is_empty() && !omp_border_line(&rows[start - 1]) {
        return None;
    }
    let mut options = Vec::with_capacity(stop - start + 1);
    let mut focus = 0usize;
    for (index, row) in rows.iter().enumerate().take(stop + 1).skip(start) {
        let mut row = row.as_str();
        if let Some(marked) = omp_plan_focus_match(row) {
            focus = options.len();
            row = marked.trim_start();
            let _ = index;
        }
        options.push(compact(row, 500));
    }
    if options.len() < 2
        || !approval_labels(&[
            ApprovalMenuRow {
                line: 0,
                focus: false,
                label: options[0].clone(),
            },
            ApprovalMenuRow {
                line: 0,
                focus: false,
                label: options[options.len() - 1].clone(),
            },
        ])
    {
        return None;
    }
    if command.is_empty() {
        // No Command: row (write/edit dialogs): the detail rows above the
        // options describe the action better than any pane-wide fallback.
        let details: Vec<&str> = rows[..start]
            .iter()
            .filter(|row| omp_dialog_content_row(row))
            .map(String::as_str)
            .collect();
        command = details.join(" ");
    }
    Some((options, focus, command))
}

/// `ompPlanApprovalDetails` — the plan-review action menu (unnumbered
/// focus-marker rows under `plan mode - next step`).
fn omp_plan_approval_details(lines: &[String]) -> (Vec<String>, usize) {
    let mut header: i64 = -1;
    for (index, line) in lines.iter().enumerate() {
        if omp_plan_menu_match(line) {
            header = index as i64;
        }
    }
    if header < 0 {
        return (Vec::new(), 0);
    }
    let header = header as usize;
    let mut options = Vec::with_capacity(4);
    let mut focus = 0usize;
    let mut end: i64 = -1;
    for (index, line) in lines.iter().enumerate().skip(header + 1) {
        if line.is_empty() || omp_border_line(line) {
            end = index as i64;
            break;
        }
        let lower = line.to_lowercase();
        if lower.starts_with("continue with") || line.starts_with('↳') {
            continue;
        }
        let mut line = line.as_str();
        if let Some(marked) = omp_plan_focus_match(line) {
            focus = options.len();
            line = marked.trim_start();
        }
        options.push(compact(line, 500));
    }
    if end < 0 || options.len() < 2 {
        return (Vec::new(), 0);
    }
    for line in &lines[end as usize..] {
        if line.is_empty()
            || omp_border_line(line)
            || line.contains('·')
            || line.to_lowercase().contains("scroll")
        {
            continue;
        }
        return (Vec::new(), 0);
    }
    (options, focus)
}

/// `approvalMenuRow`.
#[derive(Debug, Clone)]
struct ApprovalMenuRow {
    line: usize,
    focus: bool,
    label: String,
}

/// `latestApprovalMenu` — the newest numbered run with a focused row.
fn latest_approval_menu(lines: &[String]) -> Vec<ApprovalMenuRow> {
    let mut runs: Vec<Vec<ApprovalMenuRow>> = Vec::new();
    let mut current: Vec<ApprovalMenuRow> = Vec::new();
    let mut expected: i64 = 1;
    macro_rules! flush {
        () => {
            if !current.is_empty() {
                runs.push(std::mem::take(&mut current));
            }
        };
    }
    for (index, line) in lines.iter().enumerate() {
        let Some((focus, number, label)) = menu_match(line) else {
            if !current.is_empty() && !line.is_empty() && !approval_continuation(line) {
                flush!();
                expected = 1;
            }
            continue;
        };
        let label = compact(label, 500);
        if number == 1 {
            flush!();
            current = vec![ApprovalMenuRow {
                line: index,
                focus,
                label,
            }];
            expected = 2;
        } else if !current.is_empty() && number == expected {
            current.push(ApprovalMenuRow {
                line: index,
                focus,
                label,
            });
            expected += 1;
        } else {
            flush!();
            expected = 1;
        }
    }
    if !current.is_empty() {
        runs.push(current);
    }
    for run in runs.iter().rev() {
        if run.len() < 2 {
            continue;
        }
        if run.iter().any(|row| row.focus) {
            return run.clone();
        }
    }
    Vec::new()
}

/// `approvalHeader` — the header window above the menu must carry the
/// agent's approval phrasing.
fn approval_header(agent: &str, header: &str) -> bool {
    let lower = header.to_lowercase();
    if agent.contains("hermes") {
        return lower.contains("dangerous command")
            || lower.contains("permission required")
            || lower.contains("allow once")
            || lower.contains("allow for this session");
    }
    if agent.contains("codex") {
        return (lower.contains("would you like to")
            || lower.contains("do you want to")
            || lower.contains("implement this plan")
            || lower.contains("approve all pending")
            || lower.contains("requested permission")
            || (lower.contains("approve")
                && (lower.contains("subagent")
                    || lower.contains("pending")
                    || lower.contains("permission"))))
            && (lower.contains("run")
                || lower.contains("proceed")
                || lower.contains("permission")
                || lower.contains("subagent")
                || lower.contains("agent")
                || lower.contains("plan")
                || lower.contains("command")
                || lower.contains("tool"));
    }
    if agent.contains("claude") {
        return lower.contains("do you want to proceed")
            || lower.contains("would you like to proceed")
            || (lower.contains("do you want to")
                && (lower.contains("create")
                    || lower.contains("edit")
                    || lower.contains("delete")
                    || lower.contains("run")))
            || (lower.contains("allow")
                && (lower.contains("permission")
                    || lower.contains("tool")
                    || lower.contains("command")
                    || lower.contains("action")))
            || ((lower.contains("needs your permission")
                || lower.contains("requested permission"))
                && (lower.contains("tool")
                    || lower.contains("bash")
                    || lower.contains("command")
                    || lower.contains("action")));
    }
    if agent.contains("qoder") {
        return (lower.contains("permission required")
            && (lower.contains("apply this change")
                || lower.contains("tool:")
                || lower.contains("file:")))
            || (lower.contains("would you like to proceed")
                && (lower.contains("ready to execute") || lower.contains("plan approval")))
            || (lower.contains("allow")
                && (lower.contains("action")
                    || lower.contains("command")
                    || lower.contains("tool")));
    }
    false
}

/// `hermesApprovalRows` — drop trailing auxiliary rows, keep the native
/// focus index.
fn hermes_approval_rows(mut rows: Vec<ApprovalMenuRow>) -> (Vec<ApprovalMenuRow>, usize, bool) {
    let mut focus = 0usize;
    for (index, row) in rows.iter().enumerate() {
        if row.focus {
            // Keep the native row index even when a trailing auxiliary action
            // is omitted from the consent choices. The dispatcher must still
            // navigate away from that action before pressing Enter.
            focus = index;
            break;
        }
    }
    while rows
        .last()
        .is_some_and(|row| hermes_approval_auxiliary(&row.label))
    {
        rows.pop();
    }
    if rows.len() < 2 || !approval_labels(&rows) {
        return (Vec::new(), 0, false);
    }
    (rows, focus, true)
}

/// `hermesApprovalTailEnd` — the guard explanation inside the same bordered
/// panel counts as dialog content, not newer output.
fn hermes_approval_tail_end(lines: &[String], last_menu_line: usize) -> usize {
    const MAX_TAIL_LINES: usize = 8;
    let limit = (last_menu_line + 1 + MAX_TAIL_LINES).min(lines.len());
    for (index, line) in lines
        .iter()
        .enumerate()
        .take(limit)
        .skip(last_menu_line + 1)
    {
        if line.trim_start().starts_with('╰') && is_chrome(line) {
            return index;
        }
    }
    last_menu_line
}

/// `newerOutputAfterMenu` — any non-chrome line below the menu means the
/// dialog already scrolled into history.
fn newer_output_after_menu(lines: &[String], last_menu_line: usize, agent: &str) -> bool {
    let hermes_tail_end = if agent.contains("hermes") {
        hermes_approval_tail_end(lines, last_menu_line)
    } else {
        last_menu_line
    };
    for (index, line) in lines.iter().enumerate().skip(last_menu_line + 1) {
        if index <= hermes_tail_end
            || line.is_empty()
            || is_chrome(line)
            || approval_footer_match(line)
        {
            continue;
        }
        if agent.contains("hermes") && hermes_approval_chrome_line(line) {
            continue;
        }
        if agent.contains("qoder") && qoder_approval_tail_line(line) {
            continue;
        }
        return true;
    }
    false
}

/// `normalInputPrompt` — the agent's idle input prompt is showing.
fn normal_input_prompt(text: &str, agent: &str) -> bool {
    let normalized = agent.to_lowercase();
    if !supports(&normalized) && !normalized.contains("kimi") {
        return false;
    }
    let lines = clean_lines(text);
    if omp_input_frame_prompt(&lines, &normalized)
        || pi_input_frame_prompt(&lines, &normalized)
        || opencode_input_prompt(&lines, &normalized)
        || kimi_input_frame_prompt(&lines, &normalized)
    {
        return true;
    }
    let bottom = lines.len().saturating_sub(10);
    for index in (bottom..lines.len()).rev() {
        let line = &lines[index];
        let is_prompt = normal_prompt_match(line)
            || (normalized.contains("hermes") && hermes_placeholder_match(line));
        if !is_prompt {
            continue;
        }
        let mut valid_tail = true;
        for tail in &lines[index + 1..] {
            if tail.is_empty() || is_chrome(tail) || status_footer_match(tail) {
                continue;
            }
            valid_tail = false;
            break;
        }
        if valid_tail {
            return true;
        }
    }
    false
}

/// `ompInputFramePrompt` — the idle composer: a bare input line between two
/// full-width rules with the context-usage status line at the bottom.
fn omp_input_frame_prompt(lines: &[String], agent: &str) -> bool {
    if !omp_ask_agent(agent) {
        return false;
    }
    rule_framed_status_prompt(lines)
}

/// `piInputFramePrompt`.
fn pi_input_frame_prompt(lines: &[String], agent: &str) -> bool {
    let agent = agent.trim();
    if agent != "pi" && !agent.starts_with("pi-") {
        return false;
    }
    rule_framed_status_prompt(lines)
}

/// `ruleFramedStatusPrompt`.
fn rule_framed_status_prompt(lines: &[String]) -> bool {
    let mut last = lines.len() as i64 - 1;
    while last >= 0 && lines[last as usize].is_empty() {
        last -= 1;
    }
    if last < 1 || !context_usage_status_match(&lines[last as usize]) {
        return false;
    }
    let mut rules = 0;
    let bottom = (last - 6).max(0) as usize;
    for index in (bottom..last as usize).rev() {
        if !terminal_rule_match(&lines[index]) {
            continue;
        }
        rules += 1;
        if rules == 2 {
            return true;
        }
    }
    false
}

/// `openCodeInputPrompt`.
fn opencode_input_prompt(lines: &[String], agent: &str) -> bool {
    if !agent.contains("opencode") {
        return false;
    }
    let bottom = lines.len().saturating_sub(12);
    for line in &lines[bottom..] {
        if opencode_input_prompt_match(line) {
            return true;
        }
    }
    false
}

/// `kimiInputFramePrompt` — the bordered `╭─…╮ / > / ╰─…╯` composer.
fn kimi_input_frame_prompt(lines: &[String], agent: &str) -> bool {
    if !agent.contains("kimi") {
        return false;
    }
    let bottom = lines.len().saturating_sub(12);
    for footer in (bottom.max(2)..lines.len()).rev() {
        if !omp_input_footer_match(&lines[footer]) {
            continue;
        }
        let mut prompt = footer as i64 - 1;
        while prompt >= 0 && lines[prompt as usize].is_empty() {
            prompt -= 1;
        }
        if prompt < 1 || !normal_prompt_match(&lines[prompt as usize]) {
            continue;
        }
        let mut header = prompt - 1;
        while header >= 0 && lines[header as usize].is_empty() {
            header -= 1;
        }
        if header >= 0 && omp_input_header_match(&lines[header as usize]) {
            return true;
        }
    }
    false
}

// ── summaries ───────────────────────────────────────────────────────────

/// `paneSummaryLines` — the last 12 meaningful lines (no chrome/prompt-skip).
fn pane_summary_lines(text: &str) -> Vec<String> {
    let lines = clean_lines(text);
    let mut summary = Vec::new();
    for line in lines {
        if line.is_empty() || is_chrome(&line) || is_prompt_skip(&line) {
            continue;
        }
        summary.push(line);
    }
    if summary.len() > 12 {
        summary = summary[summary.len() - 12..].to_vec();
    }
    summary
}

/// `PaneSummary`.
fn pane_summary(text: &str) -> String {
    pane_summary_lines(text).join("\n")
}

/// `latestCompletedTurnLine` — the last `…ed/ing for Ns` status line index.
fn latest_completed_turn_line(lines: &[String]) -> i64 {
    for (index, line) in lines.iter().enumerate().rev() {
        if is_turn_duration(line.trim()) {
            return index as i64;
        }
    }
    -1
}

/// `LatestCompletedResponse` — the completed agent turn bounded by the
/// response bullet and the turn-duration line.
fn latest_completed_response(text: &str) -> String {
    let raw_lines: Vec<String> = text
        .replace('\r', "")
        .split('\n')
        .map(String::from)
        .collect();
    let lines: Vec<String> = raw_lines
        .iter()
        .map(|line| strip_ansi(line).trim_end_matches([' ', '\t']).to_owned())
        .collect();

    let mut end: i64 = -1;
    for index in (0..lines.len()).rev() {
        if is_turn_duration(lines[index].trim()) {
            end = index as i64;
            break;
        }
    }
    if end < 0 {
        return String::new();
    }
    let mut start: i64 = -1;
    for index in (0..end as usize).rev() {
        if response_start(&lines[index]) {
            start = index as i64;
            break;
        }
        if is_turn_duration(lines[index].trim()) {
            break;
        }
    }
    if start < 0 {
        return String::new();
    }
    let mut response: Vec<String> = lines[start as usize..end as usize].to_vec();
    response[0] = response_prefix_strip(&response[0]).to_owned();
    for line in response.iter_mut().skip(1) {
        *line = line.strip_prefix("  ").unwrap_or(line).to_owned();
    }
    while response.last().is_some_and(|line| line.trim().is_empty()) {
        response.pop();
    }
    response.join("\n").trim().to_owned()
}

/// `approvalOptions` — the final sequential numbered run (≥2) in the pane.
/// Kept for the attention projection (`agent.Options`) that lands with the
/// session watcher; the live-classify path derives options itself.
#[allow(dead_code)]
fn approval_options(lines: &[String]) -> Vec<String> {
    let mut runs: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut expected: i64 = 1;
    for line in lines {
        let Some((_focus, number, label)) = menu_match(line) else {
            if !current.is_empty() {
                runs.push(std::mem::take(&mut current));
                expected = 1;
            }
            continue;
        };
        let label = label.trim().to_owned();
        if number == 1 {
            if !current.is_empty() {
                runs.push(std::mem::take(&mut current));
            }
            current = vec![label];
            expected = 2;
        } else if !current.is_empty() && number == expected {
            current.push(label);
            expected += 1;
        } else {
            if !current.is_empty() {
                runs.push(std::mem::take(&mut current));
            }
            expected = 1;
        }
    }
    if !current.is_empty() {
        runs.push(current);
    }
    for run in runs.iter().rev() {
        if run.len() >= 2 {
            return run.clone();
        }
    }
    Vec::new()
}

/// `approvalCommand` — the shell-glyph command line, else the last
/// non-menu, non-chrome line of the summary.
fn approval_command(lines: &[String]) -> String {
    let (mut command, mut fallback) = (String::new(), String::new());
    for line in lines {
        if line.is_empty() || menu_match(line).is_some() || is_chrome(line) || is_prompt_skip(line)
        {
            continue;
        }
        if let Some(body) = command_match(line) {
            command = body.trim().to_owned();
            continue;
        }
        fallback = line.clone();
    }
    if !command.is_empty() {
        command
    } else {
        fallback
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Input planning — `internal/question/input.go`. `plan_input` translates
// the shared question protocol into the keyboard contract of the detected
// terminal form; steps preserve the dispatch-uncertainty boundary.
// ═══════════════════════════════════════════════════════════════════════

/// `question.InputStep` — one ordered terminal operation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct InputStep {
    keys: Vec<String>,
    text: String,
}

impl InputStep {
    fn keys(keys: Vec<String>) -> Self {
        Self {
            keys,
            text: String::new(),
        }
    }

    fn text(text: impl Into<String>) -> Self {
        Self {
            keys: Vec::new(),
            text: text.into(),
        }
    }
}

/// `question.PlanInput` — navigation and clarification short-circuit the
/// per-agent planners.
fn plan_input(interaction: &Interaction, payload: &QuestionPayload) -> Vec<InputStep> {
    match payload.navigation.as_str() {
        "previous" => {
            if interaction.agent == "opencode" {
                return vec![InputStep::keys(vec!["Shift+Tab".to_owned()])];
            }
            return vec![InputStep::keys(vec!["Left".to_owned()])];
        }
        "next" => {
            if interaction.agent == "opencode" {
                return vec![InputStep::keys(vec!["Tab".to_owned()])];
            }
            return vec![InputStep::keys(vec!["Right".to_owned()])];
        }
        _ => {}
    }
    if payload.clarify {
        let mut keys = navigation_keys(
            interaction,
            &QuestionFocus {
                kind: FocusKind::Chat,
                index: 0,
            },
        );
        keys.push("Enter".to_owned());
        return vec![InputStep::keys(keys)];
    }

    match interaction.agent.as_str() {
        "codex" => plan_codex_input(interaction, payload),
        "qoder" => plan_qoder_input(interaction, payload),
        "opencode" => plan_opencode_input(interaction, payload),
        "omp" => plan_omp_input(interaction, payload),
        _ => plan_claude_input(interaction, payload),
    }
}

/// `planCodexInput` — single-select only; `selected[0]` navigates+Enters,
/// otherwise the notes row gets the text + Enter.
fn plan_codex_input(interaction: &Interaction, payload: &QuestionPayload) -> Vec<InputStep> {
    if !payload.selected.is_empty() {
        let target = QuestionFocus {
            kind: FocusKind::Option,
            index: payload.selected[0] as usize,
        };
        let mut keys = navigation_keys(interaction, &target);
        keys.push("Enter".to_owned());
        return vec![InputStep::keys(keys)];
    }
    let target = QuestionFocus {
        kind: FocusKind::Option,
        index: interaction.all_option_count - 1,
    };
    let keys = navigation_keys(interaction, &target);
    if payload.other_text.is_empty() {
        let mut keys = keys;
        keys.push("Enter".to_owned());
        return vec![InputStep::keys(keys)];
    }
    let keys = if interaction.notes_active {
        vec!["Ctrl+U".to_owned()]
    } else {
        let mut keys = keys;
        keys.push("Tab".to_owned());
        keys
    };
    let mut steps = Vec::with_capacity(3);
    if !keys.is_empty() {
        steps.push(InputStep::keys(keys));
    }
    steps.push(InputStep::text(payload.other_text.clone()));
    steps.push(InputStep::keys(vec!["Enter".to_owned()]));
    steps
}

/// `planQoderInput`.
fn plan_qoder_input(interaction: &Interaction, payload: &QuestionPayload) -> Vec<InputStep> {
    if interaction.kind == "single_select" {
        if interaction.notes_active {
            let mut steps = vec![InputStep::keys(vec!["Ctrl+U".to_owned()])];
            if payload.other_selected && !payload.other_text.is_empty() {
                steps.push(InputStep::text(payload.other_text.clone()));
            }
            steps.push(InputStep::keys(vec!["Enter".to_owned()]));
            if payload.other_selected {
                return steps;
            }
            let mut current = interaction.clone();
            current.focus = QuestionFocus {
                kind: FocusKind::Other,
                index: 0,
            };
            let target = QuestionFocus {
                kind: FocusKind::Option,
                index: payload.selected[0] as usize,
            };
            let mut keys = qoder_navigation_keys(&current, &target);
            keys.push("Enter".to_owned());
            steps.push(InputStep::keys(keys));
            return steps;
        }
        if !payload.selected.is_empty() {
            let target = QuestionFocus {
                kind: FocusKind::Option,
                index: payload.selected[0] as usize,
            };
            let mut keys = qoder_navigation_keys(interaction, &target);
            keys.push("Enter".to_owned());
            return vec![InputStep::keys(keys)];
        }
        let target = QuestionFocus {
            kind: FocusKind::Other,
            index: 0,
        };
        let mut keys = qoder_navigation_keys(interaction, &target);
        keys.push("Enter".to_owned());
        keys.push("Ctrl+U".to_owned());
        let mut steps = vec![InputStep::keys(keys)];
        if !payload.other_text.is_empty() {
            steps.push(InputStep::text(payload.other_text.clone()));
        }
        steps.push(InputStep::keys(vec!["Enter".to_owned()]));
        return steps;
    }
    plan_qoder_multi_input(interaction, payload)
}

/// `planQoderMultiInput`.
fn plan_qoder_multi_input(interaction: &Interaction, payload: &QuestionPayload) -> Vec<InputStep> {
    let mut current = interaction.clone();
    let mut steps: Vec<InputStep> = Vec::new();
    let mut notes_handled = false;
    if current.notes_active {
        steps.push(InputStep::keys(vec!["Ctrl+U".to_owned()]));
        if payload.other_selected && !payload.other_text.is_empty() {
            steps.push(InputStep::text(payload.other_text.clone()));
        }
        steps.push(InputStep::keys(vec!["Enter".to_owned()]));
        current.focus = QuestionFocus {
            kind: FocusKind::Other,
            index: 0,
        };
        current.notes_active = false;
        current.other.selected = true;
        current.other.text = payload.other_text.clone();
        notes_handled = true;
    }
    for index in 0..current.options.len() {
        let desired = payload.selected.contains(&(index as i64));
        if current.options[index].selected == desired {
            continue;
        }
        let target = QuestionFocus {
            kind: FocusKind::Option,
            index,
        };
        let mut keys = qoder_navigation_keys(&current, &target);
        keys.push("Enter".to_owned());
        steps.push(InputStep::keys(keys));
        current.focus = target;
        current.options[index].selected = desired;
    }
    let other_target = QuestionFocus {
        kind: FocusKind::Other,
        index: 0,
    };
    if payload.other_selected && !notes_handled {
        let mut keys = qoder_navigation_keys(&current, &other_target);
        keys.push("Enter".to_owned());
        keys.push("Ctrl+U".to_owned());
        steps.push(InputStep::keys(keys));
        if !payload.other_text.is_empty() {
            steps.push(InputStep::text(payload.other_text.clone()));
        }
        steps.push(InputStep::keys(vec!["Enter".to_owned()]));
        current.focus = other_target;
    } else if !payload.other_selected && current.other.selected {
        let mut keys = qoder_navigation_keys(&current, &other_target);
        keys.push("Enter".to_owned());
        steps.push(InputStep::keys(keys));
        current.focus = other_target;
    }
    let submit = QuestionFocus {
        kind: FocusKind::Submit,
        index: 0,
    };
    let mut keys = qoder_navigation_keys(&current, &submit);
    keys.push("Enter".to_owned());
    steps.push(InputStep::keys(keys));
    steps
}

/// `planOpenCodeInput`.
fn plan_opencode_input(interaction: &Interaction, payload: &QuestionPayload) -> Vec<InputStep> {
    if interaction.other.hidden {
        return vec![InputStep::keys(vec!["Enter".to_owned()])];
    }
    if interaction.kind == "multi_select" {
        return plan_opencode_multi_input(interaction, payload);
    }
    if interaction.notes_active {
        if payload.other_selected {
            let mut steps = vec![InputStep::keys(vec!["Ctrl+U".to_owned()])];
            if !payload.other_text.is_empty() {
                steps.push(InputStep::text(payload.other_text.clone()));
            }
            steps.push(InputStep::keys(vec!["Enter".to_owned()]));
            steps.push(InputStep::keys(vec!["Enter".to_owned()]));
            return steps;
        }
        let mut current = interaction.clone();
        current.focus = QuestionFocus {
            kind: FocusKind::Other,
            index: 0,
        };
        let target = QuestionFocus {
            kind: FocusKind::Option,
            index: payload.selected[0] as usize,
        };
        let mut keys = opencode_navigation_keys(&current, &target);
        keys.push("Enter".to_owned());
        return vec![
            InputStep::keys(vec!["Escape".to_owned()]),
            InputStep::keys(keys),
        ];
    }
    if !payload.selected.is_empty() {
        let target = QuestionFocus {
            kind: FocusKind::Option,
            index: payload.selected[0] as usize,
        };
        let mut keys = opencode_navigation_keys(interaction, &target);
        keys.push("Enter".to_owned());
        return vec![InputStep::keys(keys)];
    }
    let target = QuestionFocus {
        kind: FocusKind::Other,
        index: 0,
    };
    let mut keys = opencode_navigation_keys(interaction, &target);
    keys.push("Enter".to_owned());
    keys.push("Ctrl+U".to_owned());
    let mut steps = vec![InputStep::keys(keys)];
    if !payload.other_text.is_empty() {
        steps.push(InputStep::text(payload.other_text.clone()));
    }
    steps.push(InputStep::keys(vec!["Enter".to_owned()]));
    steps.push(InputStep::keys(vec!["Enter".to_owned()]));
    steps
}

/// `planOpenCodeMultiInput`.
fn plan_opencode_multi_input(
    interaction: &Interaction,
    payload: &QuestionPayload,
) -> Vec<InputStep> {
    let mut current = interaction.clone();
    let mut steps: Vec<InputStep> = Vec::new();
    if current.notes_active {
        if payload.other_selected {
            steps.push(InputStep::keys(vec!["Ctrl+U".to_owned()]));
            if !payload.other_text.is_empty() {
                steps.push(InputStep::text(payload.other_text.clone()));
            }
            steps.push(InputStep::keys(vec!["Enter".to_owned()]));
            current.other.selected = true;
            current.other.text = payload.other_text.clone();
        } else {
            steps.push(InputStep::keys(vec!["Escape".to_owned()]));
        }
        current.focus = QuestionFocus {
            kind: FocusKind::Other,
            index: 0,
        };
        current.notes_active = false;
    }
    for index in 0..current.options.len() {
        let desired = payload.selected.contains(&(index as i64));
        if current.options[index].selected == desired {
            continue;
        }
        let target = QuestionFocus {
            kind: FocusKind::Option,
            index,
        };
        let mut keys = opencode_navigation_keys(&current, &target);
        keys.push("Enter".to_owned());
        steps.push(InputStep::keys(keys));
        current.focus = target;
        current.options[index].selected = desired;
    }

    let other_target = QuestionFocus {
        kind: FocusKind::Other,
        index: 0,
    };
    if payload.other_selected
        && (!current.other.selected || current.other.text != payload.other_text)
    {
        let mut keys = opencode_navigation_keys(&current, &other_target);
        keys.push("Enter".to_owned());
        keys.push("Ctrl+U".to_owned());
        steps.push(InputStep::keys(keys));
        if !payload.other_text.is_empty() {
            steps.push(InputStep::text(payload.other_text.clone()));
        }
        steps.push(InputStep::keys(vec!["Enter".to_owned()]));
        current.focus = other_target;
    } else if !payload.other_selected && current.other.selected {
        let mut keys = opencode_navigation_keys(&current, &other_target);
        keys.push("Enter".to_owned());
        steps.push(InputStep::keys(keys));
        current.focus = other_target;
    }
    steps.push(InputStep::keys(vec!["Tab".to_owned()]));
    steps
}

/// `planOMPInput`.
fn plan_omp_input(interaction: &Interaction, payload: &QuestionPayload) -> Vec<InputStep> {
    if interaction.kind == "single_select" {
        if !payload.selected.is_empty() {
            let target = QuestionFocus {
                kind: FocusKind::Option,
                index: payload.selected[0] as usize,
            };
            let mut keys = navigation_keys(interaction, &target);
            keys.push("Enter".to_owned());
            return vec![InputStep::keys(keys)];
        }
        let target = QuestionFocus {
            kind: FocusKind::Option,
            index: interaction.all_option_count - 1,
        };
        let mut keys = navigation_keys(interaction, &target);
        keys.push("Enter".to_owned());
        keys.push("Ctrl+U".to_owned());
        let mut steps = vec![InputStep::keys(keys)];
        if !payload.other_text.is_empty() {
            steps.push(InputStep::text(payload.other_text.clone()));
        }
        steps.push(InputStep::keys(vec!["Enter".to_owned()]));
        return steps;
    }

    let mut current = interaction.clone();
    let mut steps: Vec<InputStep> = Vec::new();
    for index in 0..current.options.len() {
        let desired = payload.selected.contains(&(index as i64));
        if current.options[index].selected == desired {
            continue;
        }
        let target = QuestionFocus {
            kind: FocusKind::Option,
            index,
        };
        let mut keys = navigation_keys(&current, &target);
        keys.push("Enter".to_owned());
        steps.push(InputStep::keys(keys));
        current.focus = target;
    }
    if payload.other_selected {
        let target = QuestionFocus {
            kind: FocusKind::Option,
            index: current.all_option_count - 1,
        };
        let mut keys = navigation_keys(&current, &target);
        keys.push("Enter".to_owned());
        keys.push("Ctrl+U".to_owned());
        steps.push(InputStep::keys(keys));
        if !payload.other_text.is_empty() {
            steps.push(InputStep::text(payload.other_text.clone()));
        }
        steps.push(InputStep::keys(vec!["Enter".to_owned()]));
        return steps;
    }
    if current.question_total > 1 {
        steps.push(InputStep::keys(vec!["Right".to_owned()]));
        return steps;
    }
    if payload.selected.is_empty() {
        return steps;
    }
    let distance = current.options.len() - current.focus.index;
    let mut keys = Vec::with_capacity(distance + 1);
    for _ in 0..distance {
        keys.push("Down".to_owned());
    }
    keys.push("Enter".to_owned());
    steps.push(InputStep::keys(keys));
    steps
}

/// `planClaudeInput`.
fn plan_claude_input(interaction: &Interaction, payload: &QuestionPayload) -> Vec<InputStep> {
    if interaction.kind == "single_select" {
        if !payload.selected.is_empty() {
            let mut current = interaction.clone();
            let mut steps: Vec<InputStep> = Vec::new();
            if !current.other.hidden
                && !current.other.text.is_empty()
                && payload.other_text.is_empty()
            {
                let other_target = QuestionFocus {
                    kind: FocusKind::Option,
                    index: current.all_option_count - 1,
                };
                let mut keys = navigation_keys(&current, &other_target);
                keys.push("Ctrl+U".to_owned());
                steps.push(InputStep::keys(keys));
                current.focus = other_target;
            }
            let target = QuestionFocus {
                kind: FocusKind::Option,
                index: payload.selected[0] as usize,
            };
            let mut keys = navigation_keys(&current, &target);
            keys.push("Enter".to_owned());
            steps.push(InputStep::keys(keys));
            return steps;
        }
        let target = QuestionFocus {
            kind: FocusKind::Option,
            index: interaction.all_option_count - 1,
        };
        let mut keys = navigation_keys(interaction, &target);
        keys.push("Ctrl+U".to_owned());
        let mut steps = vec![InputStep::keys(keys)];
        if !payload.other_text.is_empty() {
            steps.push(InputStep::text(payload.other_text.clone()));
        }
        steps.push(InputStep::keys(vec!["Enter".to_owned()]));
        return steps;
    }

    let mut current = interaction.clone();
    let mut steps: Vec<InputStep> = Vec::new();
    for index in 0..current.options.len() {
        let desired = payload.selected.contains(&(index as i64));
        if current.options[index].selected == desired {
            continue;
        }
        let target = QuestionFocus {
            kind: FocusKind::Option,
            index,
        };
        let mut keys = navigation_keys(&current, &target);
        keys.push("Enter".to_owned());
        steps.push(InputStep::keys(keys));
        current.focus = target;
        current.options[index].selected = desired;
    }
    let other_target = QuestionFocus {
        kind: FocusKind::Option,
        index: current.all_option_count - 1,
    };
    if current.other.text != payload.other_text {
        let mut keys = navigation_keys(&current, &other_target);
        keys.push("Ctrl+U".to_owned());
        steps.push(InputStep::keys(keys));
        current.focus = other_target.clone();
        if !payload.other_text.is_empty() {
            steps.push(InputStep::text(payload.other_text.clone()));
        }
    }
    if current.other.selected != payload.other_selected {
        let mut keys = navigation_keys(&current, &other_target);
        keys.push("Enter".to_owned());
        steps.push(InputStep::keys(keys));
        current.focus = other_target;
    }
    let submit = QuestionFocus {
        kind: FocusKind::Submit,
        index: 0,
    };
    let mut keys = navigation_keys(&current, &submit);
    keys.push("Enter".to_owned());
    steps.push(InputStep::keys(keys));
    steps
}

/// `navigationKeys` — Up/Down travel between focus positions (option rows,
/// then submit, then chat for multi-select Claude).
fn navigation_keys(interaction: &Interaction, target: &QuestionFocus) -> Vec<String> {
    let position = |focus: &QuestionFocus| -> i64 {
        match focus.kind {
            FocusKind::Option => focus.index as i64,
            FocusKind::Submit => {
                if interaction.kind == "multi_select" {
                    interaction.all_option_count as i64
                } else {
                    0
                }
            }
            FocusKind::Chat => {
                let mut position = interaction.all_option_count as i64;
                if interaction.kind == "multi_select" {
                    position += 1;
                }
                position
            }
            FocusKind::Other => 0,
        }
    };
    let mut distance = position(target) - position(&interaction.focus);
    let mut key = "Down";
    if distance < 0 {
        key = "Up";
        distance = -distance;
    }
    vec![key.to_owned(); distance as usize]
}

/// `qoderNavigationKeys` — qoder's positions: options, then other, then
/// submit (multi-select gets one more slot).
fn qoder_navigation_keys(interaction: &Interaction, target: &QuestionFocus) -> Vec<String> {
    let position = |focus: &QuestionFocus| -> i64 {
        match focus.kind {
            FocusKind::Option => focus.index as i64,
            FocusKind::Submit => interaction.options.len() as i64,
            FocusKind::Other => {
                let mut position = interaction.options.len() as i64;
                if interaction.kind == "multi_select" {
                    position += 1;
                }
                position
            }
            FocusKind::Chat => 0,
        }
    };
    let mut distance = position(target) - position(&interaction.focus);
    let mut key = "Down";
    if distance < 0 {
        key = "Up";
        distance = -distance;
    }
    vec![key.to_owned(); distance as usize]
}

/// `openCodeNavigationKeys` — `other` is the row after the options.
fn opencode_navigation_keys(interaction: &Interaction, target: &QuestionFocus) -> Vec<String> {
    let position = |focus: &QuestionFocus| -> i64 {
        if focus.kind == FocusKind::Other {
            return interaction.options.len() as i64;
        }
        focus.index as i64
    };
    let mut distance = position(target) - position(&interaction.focus);
    let mut key = "Down";
    if distance < 0 {
        key = "Up";
        distance = -distance;
    }
    vec![key.to_owned(); distance as usize]
}

/// `approvalKeys` — move focus from the classified row to the chosen one,
/// then Enter.
fn approval_keys(target: usize, current: usize) -> Vec<String> {
    let mut distance = target as i64 - current as i64;
    let mut key = "Down";
    if distance < 0 {
        key = "Up";
        distance = -distance;
    }
    let mut keys = Vec::with_capacity(distance as usize + 1);
    for _ in 0..distance {
        keys.push(key.to_owned());
    }
    keys.push("Enter".to_owned());
    keys
}

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
    fn record_custom_answer(&self, pane_id: &str, question: &str, text: &str) {
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
    fn custom_answers(&self, pane_id: &str) -> HashMap<String, String> {
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
