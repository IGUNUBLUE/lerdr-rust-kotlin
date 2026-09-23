//! Terminal text helpers — `ansiPattern`/`edgePattern` line cleaning shared
//! by the parsers and the attention classifier (`internal/question/parser.go`,
//! `attention.go`).

/// `ansiPattern` = `\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07]*(?:\x07|\x1b\\)|\x9b[0-9;?]*[ -/]*[@-~]`
/// — CSI (both `ESC [` and the C1 `\x9b` spelling), OSC with BEL or ST
/// terminator.
pub(crate) fn strip_ansi(line: &str) -> String {
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

pub(crate) fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

/// `edgePattern` = `^[\s│|]+|[\s│|]+$` — one border column strip per pass.
pub(crate) fn strip_edge_once(line: &str) -> &str {
    let line = line.strip_prefix(|c| c == '│' || c == '|').unwrap_or(line);
    let line = line.strip_suffix(|c| c == '│' || c == '|').unwrap_or(line);
    line
}

/// `cleanLines`/`cleanLine` — ANSI strip, `TrimSpace`, then repeat edge-strip
/// + `TrimSpace` to a fixpoint.
pub(crate) fn clean_lines(text: &str) -> Vec<String> {
    text.replace("\r\n", "\n")
        .split('\n')
        .map(clean_line)
        .collect()
}

pub(crate) fn clean_line(line: &str) -> String {
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
pub(crate) fn clean_codex_line(line: &str) -> String {
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
pub(crate) fn clean_opencode_line(line: &str) -> String {
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
pub(crate) fn omp_clean_lines(text: &str) -> Vec<String> {
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
pub(crate) fn first_run_of_spaces(line: &str, n: usize) -> Option<(usize, usize)> {
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
pub(crate) fn fields(line: &str) -> Vec<&str> {
    line.split_whitespace().collect()
}

/// `compact` — collapse whitespace runs, truncate at `limit` runes.
pub(crate) fn compact(value: &str, limit: usize) -> String {
    let joined: String = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.chars().count() > limit {
        return joined.chars().take(limit).collect();
    }
    joined
}

/// `title` — lowercase then uppercase the first byte.
pub(crate) fn title(value: &str) -> String {
    let lower = value.to_lowercase();
    if lower.is_empty() {
        return lower;
    }
    let mut chars = lower.chars();
    let first = chars.next().expect("non-empty").to_uppercase().to_string();
    first + chars.as_str()
}

pub(crate) fn default_string<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.is_empty() {
        fallback
    } else {
        value
    }
}

pub(crate) fn eq_fold(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

pub(crate) fn contains_fold(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

// ── row matchers ────────────────────────────────────────────────────────
