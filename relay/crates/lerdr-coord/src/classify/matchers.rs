//! Row matchers — the hand-rolled ports of every `regexp.MustCompile` in
//! `internal/question/parser.go` and `attention.go`. Go regex syntax that has
//! no literal Rust equivalent is implemented as a small scanner per pattern;
//! each matcher documents the source pattern it ports.

use super::text::*;

/// `menuPattern` = `^\s*([❯›]?)\s*(\d+)\.\s+(.*?)\s*$` → (focus, number, label).
pub(crate) fn menu_match(line: &str) -> Option<(bool, i64, &str)> {
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
pub(crate) fn checkbox_match(line: &str) -> Option<(bool, i64, &str, &str)> {
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
pub(crate) fn submit_match(line: &str) -> Option<bool> {
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
pub(crate) fn chat_match(line: &str) -> Option<bool> {
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
pub(crate) fn is_other_label(label: &str) -> bool {
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
pub(crate) fn selected_mark(line: &str) -> bool {
    let trimmed = line.trim_end();
    trimmed.ends_with('✓') || trimmed.ends_with('✔')
}

/// `claudeReviewPattern` = `(?i)^\s*([❯›]?)\s*(\d+)\.\s*(submit answers|cancel)\s*$`
/// → (focus, number).
pub(crate) fn claude_review_match(line: &str) -> Option<(bool, i64)> {
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
pub(crate) fn qoder_review_match(line: &str) -> Option<bool> {
    let rest = line.trim_start();
    let (focus, rest) = match rest.strip_prefix(['❯', '›']) {
        Some(rest) => (true, rest.trim_start()),
        None => (false, rest),
    };
    let body = rest.trim();
    (eq_fold(body, "submit answers") || eq_fold(body, "cancel ask")).then_some(focus)
}

/// `codexHeaderPattern` = `(?i)^\s*question\s+(\d+)\s*/\s*(\d+)` → (current, total).
pub(crate) fn codex_header_match(line: &str) -> Option<(i64, i64)> {
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

pub(crate) fn strip_prefix_fold<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
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
pub(crate) fn codex_submit_match(line: &str) -> bool {
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
pub(crate) fn codex_footer(line: &str) -> bool {
    let lower = line.to_lowercase();
    codex_submit_match(line)
        && (lower.contains("navigate questions")
            || lower.contains("tab to add notes")
            || lower.contains("tab or esc to clear notes"))
}

/// `qoderHeader` — the Qoder "Asking User" header line.
pub(crate) fn qoder_header(line: &str) -> bool {
    if eq_fold(line.trim(), "Asking User") {
        return true;
    }
    let lower = line.to_lowercase();
    lower.contains("asking user") && line.contains('·') && lower.contains("submit")
}

/// `qoderFooter` — the Qoder nav-hint line.
pub(crate) fn qoder_footer(line: &str) -> bool {
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
pub(crate) fn qoder_active_label(raw: &str) -> Option<String> {
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
pub(crate) fn sgr_has_48(params: &str) -> bool {
    params.contains("48;") || params.contains("48:")
}

/// `openCodeFocusPattern` = `\x1b\[[^m]*48(?:;|:)2(?:;|:)30(?:;|:)30(?:;|:)30m`
/// — the OpenCode focused-row background.
pub(crate) fn opencode_focus_match(raw: &str) -> bool {
    raw_has_sgr48_truecolor(raw, "30", "30", "30")
}

/// `openCodeActivePattern` = `\x1b\[[^m]*48(?:;|:)2(?:;|:)157(?:;|:)124(?:;|:)216m([^\x1b]*)`
/// — all active-tab segments on the raw line, concatenated.
pub(crate) fn opencode_active_label(raw: &str) -> String {
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
pub(crate) fn sgr48_rgb(params: &str, r: &str, g: &str, b: &str) -> bool {
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

pub(crate) fn raw_has_sgr48_truecolor(raw: &str, r: &str, g: &str, b: &str) -> bool {
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
pub(crate) fn omp_ask_header(line: &str) -> bool {
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
pub(crate) fn omp_option_match(line: &str) -> Option<(bool, &str, &str)> {
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
pub(crate) fn omp_frame_meta_match(line: &str) -> Option<&str> {
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
pub(crate) fn omp_progress_match(line: &str) -> Option<(i64, i64)> {
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
pub(crate) fn omp_review_submit_match(line: &str) -> bool {
    let rest = line.trim_start();
    let rest = rest
        .strip_prefix(['❯', '›', '\u{f054}', '>'])
        .unwrap_or(rest)
        .trim();
    eq_fold(rest, "submit")
}

/// `ompTabIDPattern` = `^[\p{L}\p{N}_.-]+$`.
pub(crate) fn omp_tab_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '-')
}

/// `ompActiveTabPattern` = `\x1b\[1m(?:\x1b\[[0-9;:]*m)*\s*([\p{L}\p{N}_.-]+)`
/// — the bold active tab on a raw line.
pub(crate) fn omp_active_tab(raw: &str) -> Option<String> {
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
pub(crate) fn command_match(line: &str) -> Option<&str> {
    let rest = line.trim_start();
    let rest = rest.strip_prefix(['$', '>', '❯', '›'])?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let body = rest.trim();
    (!body.is_empty()).then_some(body)
}

/// `chromePattern` — nav-hint/border/status chrome skipped by summaries.
pub(crate) fn is_chrome(line: &str) -> bool {
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
pub(crate) fn is_prompt_skip(line: &str) -> bool {
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
pub(crate) fn is_turn_duration(line: &str) -> bool {
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

pub(crate) fn take_number(s: &str) -> Option<(u64, &str)> {
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    Some((digits.parse().ok()?, &s[digits.len()..]))
}

/// `responseStartPattern` = `^\s*[•●]\s+\S`; `responsePrefixPattern` strips
/// the same bullet.
pub(crate) fn response_start(line: &str) -> bool {
    let rest = line.trim_start();
    let Some(rest) = rest.strip_prefix(['•', '●']) else {
        return false;
    };
    rest.starts_with(char::is_whitespace) && !rest.trim().is_empty()
}

pub(crate) fn response_prefix_strip(line: &str) -> &str {
    let rest = line.trim_start();
    match rest.strip_prefix(['•', '●']) {
        Some(rest) if rest.starts_with(char::is_whitespace) => rest.trim_start(),
        _ => line,
    }
}

// ── attention.go matchers ───────────────────────────────────────────────

/// `approvalFocusSourcePattern` — focus markers and trailing padding are
/// presentation; strip them from the fingerprint source.
pub(crate) fn stable_approval_source(source: &str) -> String {
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
pub(crate) fn approval_footer_match(line: &str) -> bool {
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

pub(crate) fn contains_phrase(haystack: &str, first: &str, mids: &[&str], lasts: &[&str]) -> bool {
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
pub(crate) fn phrase_with_gap(haystack: &str, first: &str, last: &str) -> bool {
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
pub(crate) fn normal_prompt_match(line: &str) -> bool {
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
pub(crate) fn hermes_placeholder_match(line: &str) -> bool {
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
pub(crate) fn hermes_approval_prompt(line: &str) -> bool {
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
pub(crate) fn hermes_spinner_line(line: &str) -> bool {
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
pub(crate) fn hermes_status_line(line: &str) -> bool {
    let rest = line.trim_start();
    let Some(rest) = rest.strip_prefix('⚕') else {
        return false;
    };
    rest.starts_with(char::is_whitespace) && !rest.trim().is_empty()
}

/// `statusFooterPattern` — the status bar under a live prompt.
pub(crate) fn status_footer_match(line: &str) -> bool {
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

pub(crate) fn take_float(s: &str) -> Option<(f64, &str)> {
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
pub(crate) fn omp_plan_menu_match(line: &str) -> bool {
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
pub(crate) fn omp_tool_approval_match(line: &str) -> bool {
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
pub(crate) fn omp_plan_focus_match(line: &str) -> Option<&str> {
    let rest = line.strip_prefix(['❯', '›', '>', '\u{f054}'])?;
    if rest.starts_with(char::is_whitespace) {
        Some(rest)
    } else {
        None
    }
}

/// `ompInputHeaderPattern` = `^╭[─━═]{2}.*╮$`.
pub(crate) fn omp_input_header_match(line: &str) -> bool {
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
pub(crate) fn omp_input_footer_match(line: &str) -> bool {
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
pub(crate) fn opencode_input_prompt_match(line: &str) -> bool {
    let lower = line.to_lowercase();
    lower.contains("ask anything...") || lower.contains("ask anything…")
}

/// `contextUsageStatusPattern` = `(?i)\d+(?:\.\d+)?%/\d+[km]\b`.
pub(crate) fn context_usage_status_match(line: &str) -> bool {
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
pub(crate) fn terminal_rule_match(line: &str) -> bool {
    let count = line
        .chars()
        .take_while(|c| matches!(c, '─' | '━' | '═' | '_' | '—'))
        .count();
    count >= 8 && count == line.chars().count()
}

/// `\b(?:yes|allow|approve|proceed|trust)\b` / `\b(?:no|deny|reject|cancel|exit)\b`
/// — the first/last approval label sanity check.
pub(crate) fn approval_labels(rows: &[ApprovalMenuRow]) -> bool {
    let Some(first) = rows.first() else {
        return false;
    };
    let Some(last) = rows.last() else {
        return false;
    };
    pub(crate) fn word_boundary(haystack: &str, words: &[&str]) -> bool {
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
pub(crate) fn approval_continuation(line: &str) -> bool {
    line.starts_with(' ')
        || approval_footer_match(line)
        || line.to_lowercase().contains("esc to cancel")
}

/// `hermesApprovalChromeLine`.
pub(crate) fn hermes_approval_chrome_line(line: &str) -> bool {
    hermes_approval_prompt(line)
        || hermes_spinner_line(line)
        || hermes_status_line(line)
        || approval_footer_match(line)
}

/// `hermesApprovalAuxiliaryOption`.
pub(crate) fn hermes_approval_auxiliary(label: &str) -> bool {
    matches!(
        label.trim().to_lowercase().as_str(),
        "show full command" | "view full command"
    )
}

/// `qoderApprovalTailLine`.
pub(crate) fn qoder_approval_tail_line(line: &str) -> bool {
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
pub(crate) fn omp_border_line(line: &str) -> bool {
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
pub(crate) fn omp_marker_selected(marker: &str) -> bool {
    matches!(
        marker.to_lowercase().as_str(),
        "☑" | "◉" | "\u{f0ca}" | "\u{f192}" | "[x]" | "(o)"
    )
}

/// `ompCheckboxMarker`.
pub(crate) fn omp_checkbox_marker(marker: &str) -> bool {
    matches!(
        marker.to_lowercase().as_str(),
        "☑" | "☐" | "\u{f0ca}" | "\u{f096}" | "[x]" | "[ ]"
    )
}

// ── approval menu rows ────────────────────────────────────────────────

#[derive(Debug, Clone)]
/// `latestApprovalMenu`/`approvalMenu` row — a numbered approval choice line.
pub(crate) struct ApprovalMenuRow {
    pub(crate) line: usize,
    pub(crate) focus: bool,
    pub(crate) label: String,
}
