//! Server-side pane delta codec — a byte-exact port of
//! `internal/panedelta/delta.go`.
//!
//! Lines come from Go's `strings.SplitAfter(s, "\n")`, which keeps the `\n`
//! inside each element and — crucially — leaves a trailing `""` element when
//! `s` ends with `\n` (and a single `""` for empty input). That phantom line
//! is part of the wire semantics: it anchors tail copy segments
//! (`copy_lines` counts it) and makes `Build("", "")` emit a single
//! empty-text literal `{}` instead of an empty segment list.
//!
//! Segment JSON uses `omitempty` on all fields, so a zero-value segment
//! serializes as `{}` and a copy from position 0 emits only `copy_lines`.

use serde::{Deserialize, Serialize};

use crate::json::de_default;

/// `minimumCopyLines` — a copy segment must cover at least this many lines.
pub const MINIMUM_COPY_LINES: usize = 3;
/// `maxCandidates` — per-anchor candidate cap; later duplicates are dropped.
pub const MAX_CANDIDATES: usize = 64;
/// Per-segment overhead charged by [`efficient`].
pub const SEGMENT_OVERHEAD_BYTES: usize = 64;

/// One delta segment. `copy_lines > 0` makes it a copy from `previous` at
/// `copy_start`; otherwise `text` is a literal insertion.
///
/// Signed ints because `Apply` must observe and reject negative positions —
/// the Go codec takes `int` and bounds-checks `copy_start < 0`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "crate::delta::is_zero"
    )]
    pub copy_start: i64,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "crate::delta::is_zero"
    )]
    pub copy_lines: i64,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub text: String,
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

/// `strings.SplitAfter(s, "\n")`: each element keeps its `\n`; a final `\n`
/// produces a trailing `""`; empty input yields `[""]`.
fn split_after(s: &str) -> Vec<&str> {
    let mut parts: Vec<&str> = s.split_inclusive('\n').collect();
    if parts.is_empty() || s.ends_with('\n') {
        parts.push("");
    }
    parts
}

type LineKey = [String; MINIMUM_COPY_LINES];

fn key_at(lines: &[&str], index: usize) -> LineKey {
    [
        lines[index].to_owned(),
        lines[index + 1].to_owned(),
        lines[index + 2].to_owned(),
    ]
}

/// Longest run of equal lines starting at `(previous_index, current_index)`.
fn matching_lines(
    previous: &[&str],
    current: &[&str],
    previous_index: usize,
    current_index: usize,
) -> usize {
    let mut matched = 0;
    while previous_index + matched < previous.len()
        && current_index + matched < current.len()
        && previous[previous_index + matched] == current[current_index + matched]
    {
        matched += 1;
    }
    matched
}

/// `Build(previous, current)` — greedy left-to-right anchor matching.
///
/// Every 3-line anchor in `previous` is indexed (first 64 occurrences only).
/// At each `current` position the longest candidate match wins; ties keep the
/// earliest candidate because the comparison is strict `>`.
pub fn build(previous: &str, current: &str) -> Vec<Segment> {
    let previous_lines = split_after(previous);
    let current_lines = split_after(current);
    let mut matches: std::collections::HashMap<LineKey, Vec<usize>> =
        std::collections::HashMap::with_capacity(previous_lines.len());
    for index in 0..previous_lines.len().saturating_sub(MINIMUM_COPY_LINES - 1) {
        let key = key_at(&previous_lines, index);
        let candidates = matches.entry(key).or_default();
        if candidates.len() < MAX_CANDIDATES {
            candidates.push(index);
        }
    }

    let mut segments: Vec<Segment> = Vec::with_capacity(8);
    let mut literal_start = 0usize;

    let mut current_index = 0usize;
    while current_index < current_lines.len() {
        if current_index + MINIMUM_COPY_LINES > current_lines.len() {
            break;
        }
        let mut best_start = 0usize;
        let mut best_lines = 0usize;
        if let Some(candidates) = matches.get(&key_at(&current_lines, current_index)) {
            for &previous_index in candidates {
                let matched = matching_lines(
                    &previous_lines,
                    &current_lines,
                    previous_index,
                    current_index,
                );
                if matched > best_lines {
                    best_start = previous_index;
                    best_lines = matched;
                }
            }
        }
        if best_lines < MINIMUM_COPY_LINES {
            current_index += 1;
            continue;
        }
        flush_literal(&mut segments, &current_lines, literal_start, current_index);
        segments.push(Segment {
            copy_start: best_start as i64,
            copy_lines: best_lines as i64,
            ..Segment::default()
        });
        current_index += best_lines;
        literal_start = current_index;
    }
    flush_literal(
        &mut segments,
        &current_lines,
        literal_start,
        current_lines.len(),
    );
    segments
}

/// Emit the pending literal between `literal_start` and `end`, if non-empty.
fn flush_literal(
    segments: &mut Vec<Segment>,
    current_lines: &[&str],
    literal_start: usize,
    end: usize,
) {
    if end <= literal_start {
        return;
    }
    segments.push(Segment {
        text: current_lines[literal_start..end].concat(),
        ..Segment::default()
    });
}

/// `Efficient` — sender-side gate: `literalBytes + 64*segments < len(current)*3/4`.
pub fn efficient(segments: &[Segment], current: &str) -> bool {
    let literal_bytes: usize = segments.iter().map(|s| s.text.len()).sum();
    literal_bytes + segments.len() * SEGMENT_OVERHEAD_BYTES < current.len() * 3 / 4
}

/// `Apply` — splice `segments` onto `previous`. `None` on out-of-range copy
/// (negative start, overflow, or end beyond the previous line count).
pub fn apply(previous: &str, segments: &[Segment]) -> Option<String> {
    let lines = split_after(previous);
    let mut output = String::new();
    for segment in segments {
        if segment.copy_lines > 0 {
            // Go uses wrapping `int` arithmetic; checked_add rejects the same
            // inputs (a wrapped end fails `end < copy_start` anyway).
            let end = segment.copy_start.checked_add(segment.copy_lines)?;
            if segment.copy_start < 0 || end < segment.copy_start || end > lines.len() as i64 {
                return None;
            }
            for line in &lines[segment.copy_start as usize..end as usize] {
                output.push_str(line);
            }
            continue;
        }
        output.push_str(&segment.text);
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_after_keeps_phantom_tail() {
        assert_eq!(split_after(""), vec![""]);
        assert_eq!(split_after("a"), vec!["a"]);
        assert_eq!(split_after("a\n"), vec!["a\n", ""]);
        assert_eq!(split_after("a\nb\n"), vec!["a\n", "b\n", ""]);
        assert_eq!(split_after("\n"), vec!["\n", ""]);
    }

    #[test]
    fn empty_to_empty_is_one_empty_literal() {
        let segments = build("", "");
        assert_eq!(segments, vec![Segment::default()]);
        assert_eq!(apply("", &segments), Some(String::new()));
    }

    #[test]
    fn identical_frame_counts_phantom_line() {
        let segments = build("one\ntwo\nthree\nfour\n", "one\ntwo\nthree\nfour\n");
        assert_eq!(
            segments,
            vec![Segment {
                copy_start: 0,
                copy_lines: 5,
                ..Segment::default()
            }]
        );
    }

    #[test]
    fn apply_rejects_out_of_range_copy() {
        let segments = vec![Segment {
            copy_start: -1,
            copy_lines: 2,
            ..Segment::default()
        }];
        assert_eq!(apply("a\nb\nc\n", &segments), None);
        let segments = vec![Segment {
            copy_start: 0,
            copy_lines: 99,
            ..Segment::default()
        }];
        assert_eq!(apply("a\nb\nc\n", &segments), None);
    }
}
