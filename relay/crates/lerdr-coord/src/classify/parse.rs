//! Structured question parsing — `internal/question/parser.go`. `Parse` turns
//! live pane text into the agent-family interaction model.

use std::collections::HashMap;

use super::matchers::*;
use super::model::*;
use super::text::*;

/// `Supports` — the agent families with structured question parsing.
pub(crate) fn supports(agent: &str) -> bool {
    let agent = agent.to_lowercase();
    agent.contains("claude")
        || agent.contains("codex")
        || omp_ask_agent(&agent)
        || agent.contains("opencode")
        || agent.contains("qoder")
        || agent.contains("hermes")
}

/// `ompAskAgent` — omp and pi spellings share the Ask frame parser.
pub(crate) fn omp_ask_agent(agent: &str) -> bool {
    let agent = agent.to_lowercase();
    let agent = agent.trim();
    agent == "omp"
        || agent.starts_with("omp-")
        || agent == "pi"
        || agent.starts_with("pi-")
        || agent.contains("oh-my-pi")
}

/// `Parse` — layout gate first, then the agent-family parser.
pub(crate) fn parse_question(text: &str, agent: &str) -> Option<Interaction> {
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
pub(crate) fn layout_hint(text: &str) -> bool {
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
pub(crate) fn omp_layout_hint(text: &str) -> bool {
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
pub(crate) fn opencode_layout_hint(text: &str) -> bool {
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

pub(crate) fn opencode_tail_is_empty(lines: &[String], footer: usize) -> bool {
    lines[footer + 1..]
        .iter()
        .all(|line| line.trim().is_empty())
}

/// `openCodeFooter` — the OpenCode nav-hint line.
pub(crate) fn opencode_footer(line: &str) -> bool {
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
pub(crate) fn parse_claude(text: &str) -> Option<Interaction> {
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

pub(crate) fn strip_selected_mark(label: &str) -> &str {
    match label.trim_end().strip_suffix(['✓', '✔']) {
        Some(body) => body.trim_end(),
        None => label.trim(),
    }
}

/// `parseClaudeReview` — the "Review your answers" screen.
pub(crate) fn parse_claude_review(text: &str, lines: &[String]) -> Option<Interaction> {
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
pub(crate) fn claude_position(text: &str) -> (i64, i64) {
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
pub(crate) fn ansi_48_span(raw: &str) -> Option<(usize, usize)> {
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

pub(crate) fn count_marks(value: &str) -> i64 {
    value
        .chars()
        .filter(|c| matches!(c, '☐' | '☒' | '☑' | '✓' | '✔'))
        .count() as i64
}

// ── parseCodex ──────────────────────────────────────────────────────────

pub(crate) struct CodexRow {
    pub(crate) line: usize,
    pub(crate) focus: bool,
    pub(crate) body: String,
}

/// `parseCodex` — `question N/M` header + numbered menu + footer hint.
pub(crate) fn parse_codex(text: &str) -> Option<Interaction> {
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
pub(crate) fn codex_description_column(lines: &[String], rows: &[CodexRow]) -> i64 {
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
pub(crate) fn codex_parts(
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

pub(crate) fn byte_of_rune(line: &str, rune: usize) -> usize {
    line.char_indices()
        .nth(rune)
        .map(|(index, _)| index)
        .unwrap_or(line.len())
}

// ── parseQoder ──────────────────────────────────────────────────────────

/// `parseQoder` — `Asking User` header + checkbox/menu rows + footer hint.
pub(crate) fn parse_qoder(text: &str) -> Option<Interaction> {
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
pub(crate) fn qoder_position(raw: &str) -> (i64, i64) {
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
pub(crate) fn parse_qoder_review(
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
pub(crate) fn parse_opencode(text: &str) -> Option<Interaction> {
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
pub(crate) fn parse_opencode_review(
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
pub(crate) fn opencode_position(
    raw_lines: &[String],
    lines: &[String],
    footer_index: usize,
) -> (i64, i64) {
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
pub(crate) fn opencode_tabs(line: &str) -> Vec<String> {
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

pub(crate) struct OmpRow {
    pub(crate) line: usize,
    pub(crate) focus: bool,
    pub(crate) marker: String,
    pub(crate) label: String,
    pub(crate) selected: bool,
}

/// `parseOMP` — the `╭ Ask` frame with marker-row options.
pub(crate) fn parse_omp(text: &str) -> Option<Interaction> {
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
pub(crate) fn finish_omp(
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
pub(crate) fn parse_omp_review(lines: &[String], start: usize, end: usize) -> Option<Interaction> {
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
pub(crate) fn omp_question(lines: &[String], start: usize, first_option: usize) -> String {
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
pub(crate) fn omp_description(lines: &[String], start: usize, end: usize) -> String {
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
pub(crate) fn omp_tab_ids(lines: &[String], ask_start: usize) -> (Option<Vec<String>>, usize) {
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
pub(crate) fn omp_active_tab_in(raw_lines: &[String], first: usize, last: usize) -> String {
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
pub(crate) fn omp_position(
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
pub(crate) fn description(lines: &[String], start: usize, end: usize) -> String {
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
pub(crate) fn prompt(lines: &[String], first_option: usize) -> String {
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
pub(crate) fn summary_lines(entries: &[SummaryEntry], limit: usize) -> String {
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
pub(crate) fn split_summary_entry(line: &str) -> SummaryEntry {
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
