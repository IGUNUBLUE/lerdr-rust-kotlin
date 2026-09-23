//! Attention classification — `internal/question/attention.go`. `Classify`
//! decides what the live control region asks of the user; approval panes
//! yield options + focus + the fingerprint inputs.

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::matchers::*;
use super::model::*;
use super::parse::*;
use super::text::*;

/// `question.AttentionKind`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum AttentionKind {
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
pub(crate) struct Classification {
    pub(crate) kind: AttentionKind,
    pub(crate) prompt: String,
    pub(crate) command: String,
    pub(crate) options: Vec<String>,
    pub(crate) approval_focus: usize,
    #[allow(dead_code)]
    pub(crate) interaction: Option<Interaction>,
    #[allow(dead_code)]
    pub(crate) question_layout: bool,
    pub(crate) approval_identity: String,
    pub(crate) approval_source: String,
}

/// `ApprovalFingerprint` — sha256 of source+prompt+command+options, or the
/// client-supplied identity when the classifier already carries one.
pub(crate) fn approval_fingerprint(classification: &Classification) -> String {
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
pub(crate) fn classify(text: &str, agent: &str) -> Classification {
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
pub(crate) fn approval_summary_lines(text: &str, agent: &str) -> Vec<String> {
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
pub(crate) fn live_approval_details(
    text: &str,
    agent: &str,
) -> Option<(Vec<String>, usize, String)> {
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
pub(crate) fn approval_dialog_source(text: &str, agent: &str) -> String {
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
pub(crate) const MAX_TRAILING_STATUS_LINES: usize = 6;

/// `ompDialogContentRow`.
pub(crate) fn omp_dialog_content_row(row: &str) -> bool {
    !row.is_empty() && !omp_border_line(row) && !approval_footer_match(row)
}

pub(crate) fn last_line_matching(
    lines: &[String],
    matcher: impl Fn(&str) -> bool,
) -> Option<usize> {
    lines.iter().rposition(|line| matcher(line))
}

/// `ompToolApprovalDetails` — the "Allow tool: bash" border box with
/// unnumbered focus-marker rows; returns the dialog's own `Command:`.
pub(crate) fn omp_tool_approval_details(lines: &[String]) -> Option<(Vec<String>, usize, String)> {
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
pub(crate) fn omp_plan_approval_details(lines: &[String]) -> (Vec<String>, usize) {
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

/// `latestApprovalMenu` — the newest numbered run with a focused row.
pub(crate) fn latest_approval_menu(lines: &[String]) -> Vec<ApprovalMenuRow> {
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
pub(crate) fn approval_header(agent: &str, header: &str) -> bool {
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
pub(crate) fn hermes_approval_rows(
    mut rows: Vec<ApprovalMenuRow>,
) -> (Vec<ApprovalMenuRow>, usize, bool) {
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
pub(crate) fn hermes_approval_tail_end(lines: &[String], last_menu_line: usize) -> usize {
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
pub(crate) fn newer_output_after_menu(
    lines: &[String],
    last_menu_line: usize,
    agent: &str,
) -> bool {
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
pub(crate) fn normal_input_prompt(text: &str, agent: &str) -> bool {
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
pub(crate) fn omp_input_frame_prompt(lines: &[String], agent: &str) -> bool {
    if !omp_ask_agent(agent) {
        return false;
    }
    rule_framed_status_prompt(lines)
}

/// `piInputFramePrompt`.
pub(crate) fn pi_input_frame_prompt(lines: &[String], agent: &str) -> bool {
    let agent = agent.trim();
    if agent != "pi" && !agent.starts_with("pi-") {
        return false;
    }
    rule_framed_status_prompt(lines)
}

/// `ruleFramedStatusPrompt`.
pub(crate) fn rule_framed_status_prompt(lines: &[String]) -> bool {
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
pub(crate) fn opencode_input_prompt(lines: &[String], agent: &str) -> bool {
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
pub(crate) fn kimi_input_frame_prompt(lines: &[String], agent: &str) -> bool {
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
pub(crate) fn pane_summary_lines(text: &str) -> Vec<String> {
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
pub(crate) fn pane_summary(text: &str) -> String {
    pane_summary_lines(text).join("\n")
}

/// `latestCompletedTurnLine` — the last `…ed/ing for Ns` status line index.
pub(crate) fn latest_completed_turn_line(lines: &[String]) -> i64 {
    for (index, line) in lines.iter().enumerate().rev() {
        if is_turn_duration(line.trim()) {
            return index as i64;
        }
    }
    -1
}

/// `LatestCompletedResponse` — the completed agent turn bounded by the
/// response bullet and the turn-duration line.
pub(crate) fn latest_completed_response(text: &str) -> String {
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
pub(crate) fn approval_options(lines: &[String]) -> Vec<String> {
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
pub(crate) fn approval_command(lines: &[String]) -> String {
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
