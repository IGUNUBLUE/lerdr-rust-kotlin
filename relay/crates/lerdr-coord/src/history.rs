//! Per-pane transcript history — `internal/history` + `internal/seqmatch`.
//!
//! Herdr's `pane.read` answers a bounded viewport; a Claude/Qoder-style
//! agent's real transcript is longer than any single read. The manager
//! accumulates each read's body lines into a per-pane history and merges
//! new snapshots on top — `classifyPaneResponse` serves the merged tail
//! (`MergeLimited`), the projector's capture loop keeps it warm for
//! claude-like panes (`Merge`), and `captureFinishedPane` consults it for
//! the completion extract.
//!
//! Merge rules (history.go:49-103): a snapshot identical to the last body
//! returns the joined history untouched; a tail-overlapping snapshot
//! extends the history in place; otherwise a difflib-style sequence match
//! (`seqmatch`) rebases the divergent tail — with a stale-refusal counter
//! guarding ambiguous overlaps — and, failing both, the body appends.
//!
//! Persistence mirrors the oracle: one JSON file per pane under
//! `<dir>/claude-history/` (`paneID` with `/`/`:` → `_`), written through
//! a `.tmp` rename at most every `SAVE_INTERVAL`, loaded lazily on first
//! touch, and reaped by [`Manager::reconcile`]/[`Manager::discard`].
//!
//! Sequence matching is `seqmatch.go`: a faithful port of Python
//! `difflib.SequenceMatcher` with `autojunk=False` — the queue-recursion
//! `get_matching_blocks` and the insertion-sorted merge.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// `history.MaxLines` — the merge history bound and the `pane.read` line
/// ceiling (`watches::MAX_PANE_LINES` shares the value).
pub(crate) const MAX_LINES: usize = 10_000;

/// `history.FooterLines` — the trailing rows split out of every snapshot
/// (agent chrome/status lines are not transcript history).
const FOOTER_LINES: usize = 6;

/// `history.CaptureInterval` — the projector's per-pane capture period.
pub(crate) const CAPTURE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(4);

/// `history.SaveInterval` — per-pane persist throttle inside `maybeSave`.
const SAVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// `history.PaneState` — the persisted per-pane record.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PaneState {
    history: Vec<String>,
    footer: Vec<String>,
    stale_refusals: i64,
    last_hash: String,
}

/// `seqmatch.Match` — `a[a..a+size] == b[b..b+size]`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Match {
    a: usize,
    b: usize,
    size: usize,
}

/// `seqmatch.Matcher` — Python's `difflib.SequenceMatcher` with
/// `autojunk=False`: `b` is indexed once, `find_longest_match` runs the
/// Ratcliff/Obershelp longest-contiguous-match over the window.
struct Matcher<'a> {
    a: &'a [String],
    b: &'a [String],
    b2j: HashMap<&'a str, Vec<usize>>,
}

impl<'a> Matcher<'a> {
    fn new(a: &'a [String], b: &'a [String]) -> Self {
        let mut b2j: HashMap<&str, Vec<usize>> = HashMap::with_capacity(b.len());
        for (j, elem) in b.iter().enumerate() {
            b2j.entry(elem.as_str()).or_default().push(j);
        }
        Matcher { a, b, b2j }
    }

    /// `FindLongestMatch` — longest match in `a[alo..ahi]`/`b[blo..bhi]`.
    fn find_longest_match(&self, alo: usize, ahi: usize, blo: usize, bhi: usize) -> Match {
        let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0usize);
        let mut j2len: HashMap<usize, usize> = HashMap::new();
        for (i, elem) in self.a.iter().enumerate().take(ahi).skip(alo) {
            let mut newj2len: HashMap<usize, usize> = HashMap::new();
            if let Some(js) = self.b2j.get(elem.as_str()) {
                for &j in js {
                    if j < blo {
                        continue;
                    }
                    if j >= bhi {
                        break;
                    }
                    let k = j2len.get(&(j.wrapping_sub(1))).copied().unwrap_or(0) + 1;
                    newj2len.insert(j, k);
                    if k > bestsize {
                        // `i - k + 1`/`j - k + 1` written as `i + 1 - k`/
                        // `j + 1 - k`: the match ending at (i, j) can be
                        // at most i + 1 / j + 1 long, so the reorder is
                        // exact — the oracle's signed ints compute
                        // `0 - 1 + 1 = 0` for a first-line match, where a
                        // literal `j - k` would underflow.
                        besti = i + 1 - k;
                        bestj = j + 1 - k;
                        bestsize = k;
                    }
                }
            }
            j2len = newj2len;
        }
        Match {
            a: besti,
            b: bestj,
            size: bestsize,
        }
    }

    /// `GetMatchingBlocks` — matching blocks sorted by position with the
    /// `(len(a), len(b), 0)` sentinel, adjacent blocks merged — the Go
    /// port preserves difflib's queue ordering (a stack, not a deque).
    fn matching_blocks(&self) -> Vec<Match> {
        let (la, lb) = (self.a.len(), self.b.len());
        let mut queue = vec![(0usize, la, 0usize, lb)];
        let mut blocks = Vec::new();
        while let Some((alo, ahi, blo, bhi)) = queue.pop() {
            let m = self.find_longest_match(alo, ahi, blo, bhi);
            if m.size > 0 {
                blocks.push(m);
                if alo < m.a && blo < m.b {
                    queue.push((alo, m.a, blo, m.b));
                }
                if m.a + m.size < ahi && m.b + m.size < bhi {
                    queue.push((m.a + m.size, ahi, m.b + m.size, bhi));
                }
            }
        }
        // `sortMatches` — insertion sort on (a, b).
        for i in 1..blocks.len() {
            let key = blocks[i];
            let mut j = i;
            while j > 0 && (blocks[j - 1].a, blocks[j - 1].b) > (key.a, key.b) {
                blocks[j] = blocks[j - 1];
                j -= 1;
            }
            blocks[j] = key;
        }
        // Merge adjacent blocks, then append the sentinel.
        let mut merged: Vec<Match> = Vec::with_capacity(blocks.len() + 1);
        let (mut i1, mut j1, mut k1) = (0usize, 0usize, 0usize);
        for m in blocks {
            if k1 > 0 && i1 + k1 == m.a && j1 + k1 == m.b {
                k1 += m.size;
            } else {
                if k1 > 0 {
                    merged.push(Match {
                        a: i1,
                        b: j1,
                        size: k1,
                    });
                }
                i1 = m.a;
                j1 = m.b;
                k1 = m.size;
            }
        }
        if k1 > 0 {
            merged.push(Match {
                a: i1,
                b: j1,
                size: k1,
            });
        }
        merged.push(Match {
            a: la,
            b: lb,
            size: 0,
        });
        merged
    }
}

/// `ansiRe` (history.go:21) — a *narrower* ANSI stripper than
/// `classify::text::strip_ansi`: CSI with only `[0-9;]` params and a
/// letter final, OSC terminated by BEL, the `\x1b()`/`[0-9A-B]` charset
/// selects, `\x1b[>=<]`, and the `?`-mode CSI ending `[hlJKHfG]`. Kept
/// separate deliberately — history normalization must erase exactly the
/// sequences the oracle's regex does.
fn strip_ansi_history(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != 0x1b {
            // Copy the full UTF-8 scalar — `\x1b` is ASCII so a
            // non-escape byte never splits a sequence.
            let len = utf8_len(bytes[i]);
            let end = (i + len).min(bytes.len());
            out.push_str(&line[i..end]);
            i = end;
            continue;
        }
        // `\x1b[` — CSI: `[0-9;]*[a-zA-Z]` first (leftmost alternative),
        // then `\??[0-9;]*[hlJKHfG]`.
        if bytes.get(i + 1) == Some(&b'[') {
            let mut j = i + 2;
            let start = j;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
                j += 1;
            }
            if j < bytes.len() && bytes[j].is_ascii_alphabetic() {
                i = j + 1;
                continue;
            }
            // `\x1b\[\??[0-9;]*[hlJKHfG]`
            j = start;
            if bytes.get(j) == Some(&b'?') {
                j += 1;
            }
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
                j += 1;
            }
            if j < bytes.len() && matches!(bytes[j], b'h' | b'l' | b'J' | b'K' | b'H' | b'f' | b'G')
            {
                i = j + 1;
                continue;
            }
            out.push('\x1b');
            i += 1;
            continue;
        }
        // `\x1b\][^\x07]*\x07` — OSC to BEL.
        if bytes.get(i + 1) == Some(&b']') {
            let mut j = i + 2;
            while j < bytes.len() && bytes[j] != 0x07 {
                j += 1;
            }
            if j < bytes.len() {
                i = j + 1;
                continue;
            }
            out.push('\x1b');
            i += 1;
            continue;
        }
        // `\x1b[()][0-9A-B]` — charset selects.
        if matches!(bytes.get(i + 1), Some(b'(') | Some(b')'))
            && bytes
                .get(i + 2)
                .is_some_and(|b| b.is_ascii_digit() || (b'A'..=b'B').contains(b))
        {
            i += 3;
            continue;
        }
        // `\x1b[>=<]`
        if matches!(bytes.get(i + 1), Some(b'>') | Some(b'=') | Some(b'<')) {
            i += 2;
            continue;
        }
        out.push('\x1b');
        i += 1;
    }
    out
}

/// `utf8_len` — the scalar's byte length from its lead byte.
fn utf8_len(lead: u8) -> usize {
    match lead {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

/// `NormalizeLine` (history.go:241-245) — `ansiRe` strip *first* (a `\r`
/// inside an OSC/CSI span is consumed by the sequence), then drop `\r`,
/// then right-trim ` \t\n`.
pub(crate) fn normalize_line(line: &str) -> String {
    strip_ansi_history(line)
        .replace('\r', "")
        .trim_end_matches([' ', '\t', '\n'])
        .to_owned()
}

/// `splitSnapshot` — lines minus the trailing 6-row footer.
fn split_snapshot(raw: &str) -> (Vec<String>, Vec<String>) {
    let lines: Vec<String> = raw.split('\n').map(str::to_owned).collect();
    if lines.len() <= FOOTER_LINES {
        return (lines, Vec::new());
    }
    (
        lines[..lines.len() - FOOTER_LINES].to_vec(),
        lines[lines.len() - FOOTER_LINES..].to_vec(),
    )
}

/// `normalizeLines` — elementwise [`normalize_line`].
fn normalize_lines(lines: &[String]) -> Vec<String> {
    lines.iter().map(|l| normalize_line(l)).collect()
}

/// `tailOverlap` — the largest k where `history`'s last k lines equal
/// `current`'s first k.
fn tail_overlap(history: &[String], current: &[String]) -> usize {
    let mut k = history.len().min(current.len());
    while k > 0 {
        if history[history.len() - k..] == current[..k] {
            return k;
        }
        k -= 1;
    }
    0
}

/// `sequenceMatch` — the largest qualifying matching block between the
/// normalized sequences: `size >= 2` with at least two non-empty history
/// lines; ties prefer the later `a`.
fn sequence_match(history: &[String], current: &[String]) -> Match {
    if history.is_empty() || current.is_empty() {
        return Match::default();
    }
    let matcher = Matcher::new(history, current);
    let mut best = Match::default();
    for block in matcher.matching_blocks() {
        if block.size < 2 {
            continue;
        }
        let non_empty = history[block.a..block.a + block.size]
            .iter()
            .filter(|l| !l.is_empty())
            .count();
        if non_empty < 2 {
            continue;
        }
        if block.size > best.size || (block.size == best.size && block.a > best.a) {
            best = block;
        }
    }
    best
}

/// `hashLines` — `h = h*31 + rune` per line plus `h*31 + '\n'`, hex-16.
fn hash_lines(lines: &[String]) -> String {
    let mut h: u64 = 0;
    for line in lines {
        for c in line.chars() {
            h = h.wrapping_mul(31).wrapping_add(u64::from(c as u32));
        }
        h = h.wrapping_mul(31).wrapping_add(u64::from('\n'));
    }
    format!("{h:016x}")
}

/// `joinContent` — history + footer joined for display, clipped to
/// `limit` lines from the tail when positive (`footer` survives first —
/// a limit under the footer drops the history entirely).
fn join_content(state: &PaneState, limit: usize) -> (String, bool) {
    let mut history_lines: &[String] = &state.history;
    let mut footer_lines: &[String] = &state.footer;
    let truncated = limit > 0 && history_lines.len() + footer_lines.len() > limit;
    if truncated {
        if footer_lines.len() >= limit {
            history_lines = &[];
            footer_lines = &footer_lines[footer_lines.len() - limit..];
        } else {
            history_lines = &history_lines[history_lines.len() - (limit - footer_lines.len())..];
        }
    }
    let mut parts: Vec<String> = Vec::new();
    if !history_lines.is_empty() {
        parts.push(history_lines.join("\n"));
    }
    if !footer_lines.is_empty() {
        parts.push(footer_lines.join("\n"));
    }
    (parts.join("\n"), truncated)
}

/// `applyMatch` (history.go:148-184) — rebase the divergent tail onto the
/// matched block; ambiguous divergent tails cost a stale refusal.
fn apply_match(state: &mut PaneState, body: &[String], m: Match) {
    let history_end = m.a + m.size;
    let current_end = m.b + m.size;
    let size = m.size.min(body.len() - m.b).min(state.history.len() - m.a);
    state.history[m.a..m.a + size].clone_from_slice(&body[m.b..m.b + size]);
    let current_suffix = &body[current_end..];
    let history_tail = state.history.len() - history_end;
    if current_suffix.is_empty() {
        // Scrolled-up viewport re-showing known content.
        state.stale_refusals = 0;
    } else if history_tail >= body.len() {
        // Match implausibly deep — treat the whole frame as new.
        state.history.extend_from_slice(body);
        state.stale_refusals = 0;
    } else if history_tail <= 3 {
        // Normal case — rebase.
        state.history.truncate(history_end);
        state.history.extend_from_slice(current_suffix);
        state.stale_refusals = 0;
    } else {
        state.stale_refusals += 1;
        if state.stale_refusals >= 2 {
            state.history.truncate(history_end);
            state.history.extend_from_slice(current_suffix);
            state.stale_refusals = 0;
        }
    }
}

struct Inner {
    states: HashMap<String, PaneState>,
    last_save: HashMap<String, Instant>,
}

/// `history.Manager` — the shared per-pane merge/persist ledger.
#[derive(Clone)]
pub(crate) struct Manager {
    inner: Arc<Mutex<Inner>>,
    dir: Option<PathBuf>,
}

impl Manager {
    /// `history.NewManager(cacheDir)` — history lives under
    /// `<dir>/claude-history`. Directory creation is best-effort exactly
    /// like the oracle (`os.MkdirAll` error discarded): persistence is an
    /// optimization; the in-memory merge is the semantic part.
    pub(crate) fn new(dir: &Path) -> Self {
        let dir = dir.join("claude-history");
        let dir = fs::create_dir_all(&dir)
            .and_then(|()| set_mode_700(&dir))
            .ok()
            .map(|()| dir);
        Manager {
            inner: Arc::new(Mutex::new(Inner {
                states: HashMap::new(),
                last_save: HashMap::new(),
            })),
            dir,
        }
    }

    /// In-memory-only manager (tests and the journal-unavailable path).
    #[cfg(test)]
    pub(crate) fn in_memory() -> Self {
        Manager {
            inner: Arc::new(Mutex::new(Inner {
                states: HashMap::new(),
                last_save: HashMap::new(),
            })),
            dir: None,
        }
    }

    /// `stateFile` — `paneID` with `/`/`:` flattened to `_`.
    fn state_file(&self, pane_id: &str) -> Option<PathBuf> {
        self.dir
            .as_ref()
            .map(|dir| dir.join(format!("{}.json", pane_id.replace(['/', ':'], "_"))))
    }

    /// `loadState` — the in-memory state else the persisted file
    /// (malformed JSON reads as empty, like the oracle's discarded
    /// `Unmarshal` error). Borrows only `states` so callers can still
    /// touch `last_save` under the same lock.
    fn load_state<'a>(
        states: &'a mut HashMap<String, PaneState>,
        dir: Option<&Path>,
        pane_id: &str,
    ) -> &'a mut PaneState {
        states.entry(pane_id.to_owned()).or_insert_with(|| {
            let mut state = PaneState::default();
            if let Some(dir) = dir {
                let path = dir.join(format!("{}.json", pane_id.replace(['/', ':'], "_")));
                if let Ok(data) = fs::read(&path) {
                    if let Ok(loaded) = serde_json::from_slice::<PaneState>(&data) {
                        state = loaded;
                    }
                }
            }
            state
        })
    }

    /// `Merge` — fold one snapshot into the pane's history; returns the
    /// joined content unbounded.
    pub(crate) fn merge(&self, pane_id: &str, raw_content: &str) -> String {
        self.merge_limited(pane_id, raw_content, 0).0
    }

    /// `MergeLimited` — merge then return only the latest `limit` lines
    /// plus the truncation flag.
    pub(crate) fn merge_limited(
        &self,
        pane_id: &str,
        raw_content: &str,
        limit: usize,
    ) -> (String, bool) {
        let mut guard = self.inner.lock().expect("history poisoned");
        let Inner { states, last_save } = &mut *guard;
        let state = Self::load_state(states, self.dir.as_deref(), pane_id);

        let (body, footer) = split_snapshot(raw_content);
        if body.is_empty() {
            return join_content(state, limit);
        }
        let hash = hash_lines(&body);
        if hash == state.last_hash && state.stale_refusals == 0 {
            return join_content(state, limit);
        }
        state.last_hash = hash;
        state.footer = footer;

        let normalized = normalize_lines(&body);
        let hist_norm = normalize_lines(&state.history);

        let overlap = tail_overlap(&hist_norm, &normalized);
        if overlap > 0 {
            let history_start = state.history.len() - overlap;
            state.history[history_start..].clone_from_slice(&body[..overlap]);
            state.history.extend_from_slice(&body[overlap..]);
        } else {
            let m = sequence_match(&hist_norm, &normalized);
            if m.size >= 2 {
                apply_match(state, &body, m);
            } else {
                state.history.extend_from_slice(&body);
                state.stale_refusals = 0;
            }
        }

        if state.history.len() > MAX_LINES {
            let drop = state.history.len() - MAX_LINES;
            state.history.drain(..drop);
        }

        // `maybeSave` — the 10s per-pane persist throttle.
        let due = last_save
            .get(pane_id)
            .is_none_or(|last| last.elapsed() >= SAVE_INTERVAL);
        if due {
            if let Some(path) = self.state_file(pane_id) {
                save_state(&path, state);
                last_save.insert(pane_id.to_owned(), Instant::now());
            }
        }
        join_content(state, limit)
    }

    /// `Content` — the joined history without a fresh snapshot.
    #[allow(dead_code)]
    pub(crate) fn content(&self, pane_id: &str, limit: usize) -> String {
        let mut guard = self.inner.lock().expect("history poisoned");
        let state = Self::load_state(&mut guard.states, self.dir.as_deref(), pane_id);
        join_content(state, limit).0
    }

    /// `Discard` — drop the pane's history (state + file).
    pub(crate) fn discard(&self, pane_id: &str) {
        let mut inner = self.inner.lock().expect("history poisoned");
        inner.states.remove(pane_id);
        inner.last_save.remove(pane_id);
        if let Some(path) = self.state_file(pane_id) {
            let _ = fs::remove_file(path);
        }
    }

    /// `SaveAll` — flush every in-memory state.
    pub(crate) fn save_all(&self) {
        let inner = self.inner.lock().expect("history poisoned");
        for (pane_id, state) in &inner.states {
            if let Some(path) = self.state_file(pane_id) {
                save_state(&path, state);
            }
        }
    }

    /// `Reconcile` — drop persisted + in-memory history for panes absent
    /// from the snapshot.
    pub(crate) fn reconcile(&self, active_pane_ids: &HashSet<String>) {
        let mut inner = self.inner.lock().expect("history poisoned");
        if let Some(dir) = &self.dir {
            let active_files: HashSet<String> = active_pane_ids
                .iter()
                .map(|id| format!("{}.json", id.replace(['/', ':'], "_")))
                .collect();
            if let Ok(entries) = fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let path = entry.path();
                    if path.is_dir() || !name.ends_with(".json") || active_files.contains(&name) {
                        continue;
                    }
                    let _ = fs::remove_file(path);
                }
            }
        }
        inner
            .states
            .retain(|pane_id, _| active_pane_ids.contains(pane_id));
        inner
            .last_save
            .retain(|pane_id, _| active_pane_ids.contains(pane_id));
    }
}

/// `saveState` — atomic write: `<pane>.json.tmp` then rename, `0o600`.
fn save_state(path: &Path, state: &PaneState) {
    let Ok(data) = serde_json::to_vec(state) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if fs::write(&tmp, data).is_err() {
        return;
    }
    let _ = set_file_mode_600(&tmp);
    let _ = fs::rename(&tmp, path);
}

#[cfg(unix)]
fn set_mode_700(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_mode_700(_dir: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_file_mode_600(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_file_mode_600(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(prefix: &str, range: std::ops::RangeInclusive<usize>) -> String {
        range
            .map(|i| format!("{prefix} {i:02}"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    }

    #[test]
    fn merge_appends_fresh_content() {
        let m = Manager::in_memory();
        let first = m.merge("p1", &lines("line", 1..=10));
        // body = lines 1..4, footer = 5..10 → joined = raw.
        assert_eq!(first, "line 01\nline 02\nline 03\nline 04\nline 05\nline 06\nline 07\nline 08\nline 09\nline 10\n");
        let second = m.merge("p1", &lines("line", 3..=12));
        let expected = lines("line", 1..=12);
        assert_eq!(second, expected);
    }

    /// Regression: `find_longest_match` computed `besti = i - k + 1` /
    /// `bestj = j - k + 1` — a length-1 match at `i == 0` or `j == 0`
    /// underflows the subtraction before the `+1`. The oracle's signed
    /// ints produce `0` there; the merged content must survive too.
    #[test]
    fn sequence_match_first_line_match_does_not_underflow() {
        let m = Manager::in_memory();
        // Seed a history whose body shares no tail with the next
        // snapshot (so `tail_overlap` misses and `sequence_match` runs),
        // then merge a snapshot whose first body line equals a line deep
        // in the history — the `j == 0` arm — and one where the match
        // starts at `a[0]`/`b[0]` — the `i == 0` arm.
        m.merge(
            "p1",
            "alpha\nbeta\ngamma\n$ npm test\nold\nrows\nf1\nf2\nf3\nf4\nf5\nf6\n",
        );
        let merged = m.merge(
            "p1",
            "$ npm test\nnew one\nnew two\nf1\nf2\nf3\nf4\nf5\nf6\n",
        );
        assert!(merged.contains("$ npm test\nnew one\nnew two"));
    }

    #[test]
    fn identical_snapshot_is_stable() {
        let m = Manager::in_memory();
        let raw = lines("x", 1..=12);
        assert_eq!(m.merge("p1", &raw), raw);
        assert_eq!(m.merge("p1", &raw), raw);
    }

    #[test]
    fn merge_limited_clips_from_the_tail() {
        let m = Manager::in_memory();
        m.merge("p1", &lines("l", 1..=20));
        let (content, truncated) = m.merge_limited("p1", &lines("l", 1..=20), 5);
        assert!(truncated);
        // Footer wins the limit — the last 5 footer rows are all that
        // fit; the trailing empty split element keeps the final newline.
        assert_eq!(content, "l 17\nl 18\nl 19\nl 20\n");
    }

    #[test]
    fn discard_forgets_the_pane() {
        let m = Manager::in_memory();
        m.merge("p1", &lines("l", 1..=10));
        m.discard("p1");
        assert_eq!(m.content("p1", 0), "");
    }

    #[test]
    fn ansi_strip_matches_the_oracle_grammar() {
        assert_eq!(normalize_line("\x1b[31mred\x1b[0m  "), "red");
        assert_eq!(normalize_line("\x1b[?25hblock\x1b[K"), "block");
        assert_eq!(
            normalize_line("\x1b]8;;http://x\x07link\x1b]8;;\x07"),
            "link"
        );
        assert_eq!(normalize_line("\x1b(Bplain\x1b>"), "plain");
        // `\x1b[0 q` — a space intermediate is NOT in ansiRe's grammar.
        assert_eq!(normalize_line("a\x1b[0 qb"), "a\x1b[0 qb");
    }
}
