//! Live terminal stream path — `herdr terminal session observe` feeds ANSI
//! `terminal.frame` bytes into a `vt100` parser, and the parser's canonical
//! screen is re-rendered as the same text `pane.read` produced. Everything
//! downstream (fingerprinting, deltas, ack gating, classification) is
//! untouched: the stream only changes *where the snapshot comes from*.
//!
//! Properties vs `pane.read`:
//!
//! - **Push, not poll** — frames arrive on output, not on a 250 ms tick;
//!   intermediate states between ticks stop being lost.
//! - **Separate history** — native ANSI history stays non-invasive. An idle
//!   agent can additionally supply application-owned history through Herdr;
//!   its temporary scroll frames are discarded before observation resumes.
//! - **Real VT semantics** — alt screen, scroll regions, and cursor state
//!   are resolved by the parser instead of re-read snapshots racing the
//!   writer.
//!
//! Fallback: spawn failure, EOF, or `terminal.closed` degrade to the
//! snapshot path for the rest of the watch — the stream is a strict
//! improvement when available and invisible when not.
//!
//! `LERDR_PANE_STREAM=off` disables the stream entirely (reads always poll).

use std::sync::Arc;
use std::time::{Duration, Instant};

use lerdr_herdr::observe::{ObserveEvent, ObserveStream, TerminalFrame};
use lerdr_herdr::ReadSource;
use tracing::warn;

use crate::watches::cap_pane_content_lines;

/// Scrollback kept by the emulator — comfortably past any `lines` budget
/// the router clamps to (`MAX_PANE_LINES` is far below this).
const SCROLLBACK_ROWS: usize = 4096;

/// Re-read Herdr's history at least this often while the stream is live,
/// even when the screen looks unchanged (a row can scroll off and be
/// replaced by an identical one).
pub(crate) const HOST_HISTORY_REFRESH: Duration = Duration::from_secs(1);

/// Minimum spacing between history reads when the screen changes, so a
/// spinner redrawing every frame costs no more reads than the old snapshot
/// poll did. Rows scrolled off inside this window reappear at the next read.
pub(crate) const HOST_HISTORY_MIN_INTERVAL: Duration = Duration::from_millis(200);

/// Rows of Herdr's own scrollback, above the live screen, captured with
/// `pane.read` (`recent`, ansi). The emulator cannot supply them: it
/// starts empty at attach, the alternate screen keeps no scrollback, rows
/// scrolled inside a scroll region (a TUI with a fixed input box) never
/// reach it, and every geometry respawn starts it over.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HostHistory {
    /// History rows oldest-first, each terminated by `\n`.
    pub(crate) text: String,
    /// Herdr dropped older rows to fit the read budget.
    pub(crate) truncated: bool,
}

/// Env kill-switch for the stream path.
const DISABLE_ENV: &str = "LERDR_PANE_STREAM";

/// Streams disabled by env?
pub(crate) fn streams_enabled() -> bool {
    match std::env::var(DISABLE_ENV) {
        Ok(value) => !matches!(
            value.trim().to_lowercase().as_str(),
            "0" | "off" | "false" | "no"
        ),
        Err(_) => true,
    }
}

/// One watch's terminal stream + emulator.
pub(crate) struct PaneStream {
    stream: ObserveStream,
    /// `None` parser until the first frame lands (geometry arrives on the
    /// frame, not the spawn).
    emu: PaneEmulator,
    /// The `(cols, rows)` the observer was spawned with — `None` took
    /// Herdr's fixed default surface.
    size: Option<(u16, u16)>,
    /// Herdr's scrollback above the screen; `recent` renders prepend it.
    history: Option<HostHistory>,
    /// Last history fetch attempt — the [`HOST_HISTORY_REFRESH`] bound.
    history_attempt: Option<Instant>,
    /// The emulator screen at that attempt, as plain rows. A different
    /// screen may mean rows scrolled into Herdr's history since then.
    history_screen: Option<String>,
    /// Full plain-text transcript recovered while the application was idle.
    /// Re-locate the live screen in it on each render, never guess an overlap.
    application_history: Option<Arc<HostHistory>>,
}

/// The stream's next meaningful outcome.
#[derive(Debug)]
pub(crate) enum StreamOutcome {
    /// New bytes arrived — re-render and emit.
    Updated,
    /// `terminal.closed` — the pane exited.
    Closed,
    /// Child exited / stream broke — caller resumes snapshot polling.
    Ended,
}

impl PaneStream {
    /// Spawn the observer at `size` `(cols, rows)` — the pane's real
    /// cell geometry. `None` when the CLI/socket aren't reachable or the
    /// stream is env-disabled. `client` supplies the resolved `herdr`
    /// binary and the transport's socket path.
    ///
    /// An undersized surface silently drops pane cells: Herdr renders
    /// the pane's terminal onto the requested geometry and rows beyond
    /// it never reach the wire (`None` takes Herdr's fixed default
    /// surface — smaller than a full-size pane).
    pub(crate) fn spawn(
        client: &lerdr_herdr::Client,
        pane_id: &str,
        size: Option<(u16, u16)>,
    ) -> Option<Self> {
        if !streams_enabled() {
            return None;
        }
        let socket_path = client.socket_path_hint()?;
        let bin = client.resolved_herdr_bin();
        let (cols, rows) = size.unzip();
        Some(PaneStream {
            stream: ObserveStream::spawn_sized(&bin, pane_id, &socket_path, cols, rows)?,
            emu: PaneEmulator::new(),
            size,
            history: None,
            history_attempt: None,
            history_screen: None,
            application_history: None,
        })
    }

    /// A history refresh is due before the next render: never attempted on
    /// this stream (a fresh attach or geometry respawn),
    /// [`HOST_HISTORY_REFRESH`] passed, or the screen changed and
    /// [`HOST_HISTORY_MIN_INTERVAL`] passed. Following screen changes keeps
    /// rows that just scrolled off from staying missing; a quiet pane costs
    /// one read pair per second.
    pub(crate) fn history_due(&self, now: Instant) -> bool {
        history_refresh_due(
            self.history_attempt.map(|at| now.duration_since(at)),
            || self.history_screen.as_deref() != Some(self.screen_text().as_str()),
        )
    }

    /// Record a history fetch attempt. `None` (a failed read or an
    /// unlocatable screen) keeps the previous history rather than
    /// dropping rows the phone already shows.
    pub(crate) fn note_history(&mut self, history: Option<HostHistory>, now: Instant) {
        self.history_attempt = Some(now);
        self.history_screen = Some(self.screen_text());
        if history.is_some() {
            self.history = history;
        }
    }

    /// The emulator's screen as plain rows — the change detector for
    /// [`Self::history_due`].
    pub(crate) fn screen_text(&self) -> String {
        self.emu
            .render_parts(ReadSource::Visible, false, u32::MAX)
            .0
    }

    /// Drop queued harvest scroll frames and start at the restored viewport.
    /// The existing emulator remains visible until the new full frame arrives.
    pub(crate) fn restart_observer(
        &mut self,
        client: &lerdr_herdr::Client,
        pane_id: &str,
    ) -> Option<()> {
        let (cols, rows) = self.size.unzip();
        self.stream = ObserveStream::spawn_sized(
            &client.resolved_herdr_bin(),
            pane_id,
            &client.socket_path_hint()?,
            cols,
            rows,
        )?;
        self.emu = PaneEmulator::new();
        Some(())
    }

    pub(crate) fn set_application_history(&mut self, history: Option<Arc<HostHistory>>) {
        self.application_history = history;
    }

    /// The surface geometry this stream was spawned with — the watch
    /// respawns when the pane's real size drifts from it.
    pub(crate) fn size(&self) -> Option<(u16, u16)> {
        self.size
    }

    /// Next stream event — frames fold into the emulator; `Closed` tells
    /// the caller to emit the gone frame, `Ended` to drop the stream and
    /// resume snapshot reads. Cancel-safe: the receive is an `mpsc` recv.
    pub(crate) async fn next_render(&mut self) -> StreamOutcome {
        match self.stream.next().await {
            ObserveEvent::Frame(frame) => {
                self.emu.apply(&frame);
                StreamOutcome::Updated
            }
            ObserveEvent::Closed => StreamOutcome::Closed,
            ObserveEvent::Ended => StreamOutcome::Ended,
        }
    }

    /// Whether the emulator has seen at least one frame — before that the
    /// watch reads through `pane.read` instead.
    pub(crate) fn is_initialized(&self) -> bool {
        self.emu.is_initialized()
    }

    /// Render the surface as the `pane.read` text for `(source, ansi)` plus
    /// the `truncated` flag. `visible` is the screen; `recent` prepends
    /// scrollback rows; `recent-unwrapped` additionally joins soft-wrapped
    /// rows into logical lines. `lines` caps via the same
    /// `capPaneContentLines` the snapshot reads run through.
    ///
    /// With Herdr history captured, `recent` is that history plus the live
    /// screen; the emulator's own scrollback is not used, since the
    /// history already covers it.
    pub(crate) fn render_parts(
        &self,
        source: ReadSource,
        ansi: bool,
        lines: u32,
    ) -> (String, bool) {
        let application = self.application_history.as_ref().and_then(|recent| {
            split_application_history(&recent.text, recent.truncated, &self.screen_text())
        });
        let history = match (self.history.as_ref(), application.as_ref()) {
            (Some(native), Some(app))
                if text_rows(&native.text).len() >= text_rows(&app.text).len() =>
            {
                Some(native)
            }
            (_, Some(app)) => Some(app),
            (native, None) => native,
        };
        match (history, source) {
            (Some(history), ReadSource::Recent) => {
                self.emu.render_with_history(history, ansi, lines)
            }
            _ => self.emu.render_parts(source, ansi, lines),
        }
    }
}

/// Application transcripts can omit sticky headers shown above a virtualized
/// viewport. Find a unique run of three nonblank rows shared with the screen;
/// prepend only rows preceding that run. Repeated/unknown seams fail closed.
pub(crate) fn split_application_history(
    recent: &str,
    truncated: bool,
    visible: &str,
) -> Option<HostHistory> {
    let raw = text_rows(recent);
    let rows: Vec<_> = raw.iter().map(|row| application_row(row)).collect();
    let screen: Vec<_> = text_rows(visible)
        .into_iter()
        .map(application_row)
        .collect();
    let mut best = None;
    for start in 0..screen.len() {
        let mut nonblank = 0;
        let Some(span) = screen[start..]
            .iter()
            .position(|row| {
                nonblank += usize::from(!row.is_empty());
                nonblank == SCREEN_MATCH_ROWS
            })
            .map(|end| end + 1)
        else {
            continue;
        };
        let anchor = &screen[start..start + span];
        let mut matches = rows
            .windows(anchor.len())
            .enumerate()
            .filter_map(|(index, block)| (block == anchor).then_some(index));
        let Some(cut) = matches.next() else { continue };
        if matches.next().is_some() {
            continue;
        }
        let offset = cut.saturating_sub(start);
        if best.is_none_or(|(largest, _)| offset > largest) {
            best = Some((offset, cut));
        }
    }
    best.map(|(_, cut)| HostHistory {
        text: raw[..cut].iter().map(|row| format!("{row}\n")).collect(),
        truncated,
    })
}

/// The emulator half — a `vt100` screen fed `terminal.frame` bytes.
/// Separated from `PaneStream` so tests drive it without a child process.
///
/// The stream's `seq` is deliberately *not* exposed upstream: it is a
/// different axis than the socket's `content_revision`/`output_revision`
/// watermarks and folding it into either would corrupt their dedupe.
pub(crate) struct PaneEmulator {
    parser: Option<vt100::Parser>,
    /// Highest `terminal.frame` seq applied — the gap-detection watermark
    /// (stream-only; see the type docs).
    last_seq: u64,
}

impl PaneEmulator {
    pub(crate) fn new() -> Self {
        PaneEmulator {
            parser: None,
            last_seq: 0,
        }
    }

    pub(crate) fn is_initialized(&self) -> bool {
        self.parser.is_some()
    }

    /// Feed one frame: create the parser on first contact, resize when the
    /// reported geometry moves, then process the bytes. A `seq` gap on a
    /// partial frame means intermediate output was skipped — ANSI is
    /// cursor-addressed so the screen stays parseable and the next `full`
    /// frame heals whatever tore.
    pub(crate) fn apply(&mut self, frame: &TerminalFrame) {
        if self.last_seq != 0 && frame.seq > self.last_seq + 1 && !frame.full {
            warn!(
                last = self.last_seq,
                seq = frame.seq,
                "terminal.frame gap — screen may tear until the next full frame"
            );
        }
        self.last_seq = self.last_seq.max(frame.seq);
        let (rows, cols) = (frame.height.max(1), frame.width.max(1));
        match &mut self.parser {
            Some(parser) => {
                if parser.screen().size() != (rows, cols) {
                    parser.screen_mut().set_size(rows, cols);
                }
                parser.process(&frame.bytes);
            }
            None => {
                let mut parser = vt100::Parser::new(rows, cols, SCROLLBACK_ROWS);
                parser.process(&frame.bytes);
                self.parser = Some(parser);
            }
        }
    }

    /// `(content, truncated)` — `truncated` mirrors `pane.read`'s flag:
    /// rows existed beyond the `lines` budget and were dropped from the
    /// head of the window.
    pub(crate) fn render_parts(
        &self,
        source: ReadSource,
        ansi: bool,
        lines: u32,
    ) -> (String, bool) {
        let Some(parser) = &self.parser else {
            return (String::new(), false);
        };
        render_parser(parser, source, ansi, lines)
    }

    /// `recent` as Herdr's history followed by the live screen, capped
    /// at `lines` from the tail.
    pub(crate) fn render_with_history(
        &self,
        history: &HostHistory,
        ansi: bool,
        lines: u32,
    ) -> (String, bool) {
        let (screen, _) = self.render_parts(ReadSource::Visible, ansi, lines);
        let mut content = String::with_capacity(history.text.len() + screen.len());
        content.push_str(&history.text);
        content.push_str(&screen);
        let capped = cap_pane_content_lines(&content, lines);
        let truncated = history.truncated || capped.len() < content.len();
        (capped.to_string(), truncated)
    }
}

/// [`PaneStream::history_due`]'s pacing on its inputs: time since the last
/// attempt (`None` = never) and, lazily, whether the screen changed.
fn history_refresh_due(elapsed: Option<Duration>, screen_changed: impl FnOnce() -> bool) -> bool {
    match elapsed {
        None => true,
        Some(elapsed) if elapsed >= HOST_HISTORY_REFRESH => true,
        Some(elapsed) => elapsed >= HOST_HISTORY_MIN_INTERVAL && screen_changed(),
    }
}

/// One row reduced for comparison: escape sequences, carriage returns and
/// trailing blanks removed. Herdr's `pane.read` rows and the screen rows
/// carry the same text but not always the same escapes.
pub(crate) fn application_row(row: &str) -> String {
    let row = comparable_row(row);
    if let Some(body) = row
        .strip_suffix(['│', '┃'])
        .filter(|body| body.ends_with("  "))
    {
        body.trim_end().to_owned()
    } else {
        row
    }
}

pub(crate) fn comparable_row(row: &str) -> String {
    let mut out = String::with_capacity(row.len());
    let mut chars = row.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                // CSI: parameters/intermediates up to a final byte.
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC: up to BEL or ST (`ESC \\`).
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                // Two-byte escapes (charset selects, keypad modes, ...).
                _ => {}
            },
            '\r' => {}
            c => out.push(c),
        }
    }
    out.truncate(out.trim_end().len());
    out
}

/// Rows of `text`, oldest-first, without the empty element a trailing
/// newline leaves.
fn text_rows(text: &str) -> Vec<&str> {
    let mut rows: Vec<&str> = text.split('\n').collect();
    if rows.last().is_some_and(|row| row.is_empty()) {
        rows.pop();
    }
    rows
}

/// Rows compared when locating the screen inside `recent`.
const SCREEN_MATCH_ROWS: usize = 3;

/// Split Herdr's history out of a `recent` read using a `visible` read
/// taken right after it. `recent` is the scrollback followed by the
/// screen (trailing blank rows trimmed), so the screen normally starts
/// `visible.len()` rows from the end. Output between the two reads can
/// shift it, so nearby offsets are tried too, nearest first. `None` when
/// the screen cannot be located; the caller keeps its previous history.
pub(crate) fn split_host_history(
    recent: &str,
    recent_truncated: bool,
    visible: &str,
) -> Option<HostHistory> {
    let recent_rows = text_rows(recent);
    let mut screen: Vec<String> = text_rows(visible).into_iter().map(comparable_row).collect();
    while screen.last().is_some_and(String::is_empty) {
        screen.pop();
    }
    let cut = if screen.is_empty() {
        // A blank screen: everything Herdr returned is history.
        Some(recent_rows.len())
    } else {
        // Anchor on the first non-blank screen rows (with their offsets
        // into the screen): leading blank rows match anywhere.
        let probe: Vec<(usize, &String)> = screen
            .iter()
            .enumerate()
            .filter(|(_, row)| !row.is_empty())
            .take(SCREEN_MATCH_ROWS)
            .collect();
        // Rows past the end of `recent` are output that arrived after
        // that read; they cannot be checked, but at least one row must.
        let matches_at = |start: usize| {
            let mut checked = 0;
            let all = probe.iter().all(|&(offset, expected)| {
                recent_rows.get(start + offset).is_none_or(|row| {
                    checked += 1;
                    comparable_row(row) == *expected
                })
            });
            all && checked > 0
        };
        let expected = recent_rows.len().checked_sub(screen.len())?;
        (0..=screen.len()).find_map(|distance| {
            [
                expected.checked_sub(distance),
                expected.checked_add(distance),
            ]
            .into_iter()
            .flatten()
            .find(|&start| matches_at(start))
        })
    }?;
    let mut text = String::new();
    for row in &recent_rows[..cut] {
        text.push_str(row);
        text.push('\n');
    }
    Some(HostHistory {
        text,
        truncated: recent_truncated,
    })
}

/// `render` on a bare parser — kept separate so tests can drive `vt100`
/// without an `ObserveStream`.
fn render_parser(
    parser: &vt100::Parser,
    source: ReadSource,
    ansi: bool,
    lines: u32,
) -> (String, bool) {
    let mut screen = parser.screen().clone();
    let unwrapped = source == ReadSource::RecentUnwrapped;
    let with_scrollback = source != ReadSource::Visible;
    // Unwrapped joins drop physical rows — over-read so the post-join
    // cap still has `lines` logical lines to keep.
    let budget = if unwrapped { lines * 8 + 64 } else { lines };
    let (content, truncated) = render_rows(&mut screen, with_scrollback, ansi, unwrapped, budget);
    (
        cap_pane_content_lines(&content, lines).to_string(),
        truncated,
    )
}

/// Collect the window (scrollback + screen when `with_scrollback`,
/// screen-only otherwise) oldest-first as formatted or plain rows, capped
/// at `lines`. With `unwrapped`, soft-wrapped continuations join their
/// parent line without a newline. Trailing blank rows are dropped like
/// `pane.read`/`contents` output. The second return is `truncated`: rows
/// were dropped from the head to fit the budget.
fn render_rows(
    screen: &mut vt100::Screen,
    with_scrollback: bool,
    ansi: bool,
    unwrapped: bool,
    lines: u32,
) -> (String, bool) {
    let (rows, cols) = screen.size();
    if rows == 0 {
        return (String::new(), false);
    }
    let rows = rows as usize;
    // The scrollback *position* is a view offset — `set_scrollback` clamps
    // it to the scrollback length, so probing usize::MAX reads the max.
    // At offset `s` the viewport shows combined lines
    // `[scrollback - s, scrollback - s + rows)`.
    let scrollback = if with_scrollback {
        screen.set_scrollback(usize::MAX);
        screen.scrollback()
    } else {
        0
    };
    let total = scrollback + rows;
    let keep = (lines as usize).max(1).min(total);
    let skip = total - keep;

    // Assemble the kept lines oldest-first, viewport-tile by tile (each
    // `set_scrollback` repositions the whole view, then `rows`/`rows_formatted`
    // drain it in one pass). `blank` uses the *plain* row even in ansi
    // mode — a formatted blank row is escape bytes, not an empty string.
    let mut kept: Vec<(String, bool, bool)> = Vec::with_capacity(keep);
    let mut line = skip;
    while line < total {
        let offset = scrollback.saturating_sub(line);
        screen.set_scrollback(offset);
        // View row 0 is combined line `scrollback - offset`, which can
        // precede `line` once the view slides onto the screen — skip the
        // overlap so lines emit exactly once.
        let view_top = scrollback - offset;
        let plain: Vec<String> = screen.rows(0, cols).collect();
        let formatted: Option<Vec<String>> = ansi.then(|| {
            screen
                .rows_formatted(0, cols)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .collect()
        });
        for (view_row, text) in plain.iter().enumerate().skip(line - view_top) {
            if line >= total {
                break;
            }
            let rendered = formatted
                .as_ref()
                .and_then(|f| f.get(view_row).cloned())
                .unwrap_or_else(|| text.clone());
            kept.push((
                rendered,
                text.trim().is_empty(),
                screen.row_wrapped(view_row as u16),
            ));
            line += 1;
        }
    }
    screen.set_scrollback(0);
    while kept.last().is_some_and(|(_, blank, _)| *blank) {
        kept.pop();
    }

    let mut out = String::new();
    for (text, _, wrapped) in &kept {
        out.push_str(text);
        if !unwrapped || !wrapped {
            out.push('\n');
        }
    }
    (out, skip > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parser_with(bytes: &[u8], rows: u16, cols: u16) -> vt100::Parser {
        let mut parser = vt100::Parser::new(rows, cols, SCROLLBACK_ROWS);
        parser.process(bytes);
        parser
    }

    fn render(
        bytes: &[u8],
        rows: u16,
        cols: u16,
        source: ReadSource,
        ansi: bool,
        lines: u32,
    ) -> String {
        render_parser(&parser_with(bytes, rows, cols), source, ansi, lines).0
    }

    fn frame(seq: u64, width: u16, height: u16, bytes: &[u8]) -> TerminalFrame {
        TerminalFrame {
            seq,
            full: seq == 1,
            bytes: bytes.to_vec(),
            width,
            height,
        }
    }

    // PTY line endings are `\r\n` — a bare `\n` is a line feed that keeps
    // the cursor's column (real VT semantics, which is why every input
    // below carries the carriage return). Inputs end *without* a trailing
    // newline: a final `\r\n` would scroll the last line into scrollback
    // and leave the cursor's row blank.
    #[test]
    fn screen_text_matches_input() {
        assert_eq!(
            render(b"hello\r\nworld\r\n", 4, 10, ReadSource::Visible, false, 10),
            "hello\nworld\n"
        );
    }

    #[test]
    fn scrollback_rows_prepend_for_recent() {
        // 6 lines on a 4-row screen → 2 scrollback rows, cursor on "six".
        let bytes = b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix";
        assert_eq!(
            render(bytes, 4, 10, ReadSource::Recent, false, 10),
            "one\ntwo\nthree\nfour\nfive\nsix\n"
        );
        // `visible` keeps only the screen tail.
        assert_eq!(
            render(bytes, 4, 10, ReadSource::Visible, false, 10),
            "three\nfour\nfive\nsix\n"
        );
    }

    #[test]
    fn recent_caps_from_the_tail() {
        let bytes = b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix";
        let (text, truncated) =
            render_parser(&parser_with(bytes, 4, 10), ReadSource::Recent, false, 3);
        assert_eq!(text, "four\nfive\nsix\n");
        assert!(truncated);
        let (_, truncated) =
            render_parser(&parser_with(bytes, 4, 10), ReadSource::Recent, false, 10);
        assert!(!truncated);
    }

    #[test]
    fn recent_unwrapped_joins_soft_wraps() {
        // "abcdefghij" on a 5-col screen occupies two physical rows.
        let bytes = b"abcdefghij\r\nKLM\r\n";
        assert_eq!(
            render(bytes, 4, 5, ReadSource::RecentUnwrapped, false, 10),
            "abcdefghij\nKLM\n"
        );
        // `recent` keeps the physical split.
        let physical = render(bytes, 4, 5, ReadSource::Recent, false, 10);
        assert!(physical.contains("abcde\nfghij"), "{physical:?}");
    }

    #[test]
    fn ansi_round_trips_styles() {
        // Red fg + bold on "hi" re-render with SGR escapes.
        let text = render(
            b"\x1b[1;31mhi\x1b[0m\r\n",
            4,
            10,
            ReadSource::Visible,
            true,
            10,
        );
        assert!(text.contains("hi"), "{text:?}");
        assert!(text.contains('\u{1b}'), "{text:?}");
    }

    /// `vt100` does not reflow on resize — `set_size` clears the wrap bit
    /// and pads each row in place. That is fine for the stream: a
    /// geometry move always arrives as bytes rendered *at the new
    /// geometry*, so the grid is redrawn regardless.
    #[test]
    fn resize_preserves_rows_and_drops_wraps() {
        let mut parser = parser_with(b"abcdefghij\r\n", 4, 5);
        parser.screen_mut().set_size(4, 10);
        let (text, _) = render_parser(&parser, ReadSource::Visible, false, 10);
        assert_eq!(text, "abcde\nfghij\n");
        // Cleared wrap bits mean `recent-unwrapped` stops joining.
        let (text, _) = render_parser(&parser, ReadSource::RecentUnwrapped, false, 10);
        assert_eq!(text, "abcde\nfghij\n");
    }

    #[test]
    fn emulator_applies_incremental_frames() {
        let mut emu = PaneEmulator::new();
        assert!(!emu.is_initialized());
        emu.apply(&frame(1, 10, 4, b"first\r\n"));
        emu.apply(&frame(2, 10, 4, b"second\r\n"));
        let (text, _) = emu.render_parts(ReadSource::Visible, false, 10);
        assert_eq!(text, "first\nsecond\n");
    }

    #[test]
    fn emulator_tracks_frame_geometry() {
        let mut emu = PaneEmulator::new();
        emu.apply(&frame(1, 5, 4, b"abcde\r\n"));
        // A geometry change arrives as a full frame rendered at the new
        // size — the parser takes the new size before the bytes land.
        emu.apply(&frame(2, 10, 4, b"wider\r\n"));
        let (text, _) = emu.render_parts(ReadSource::Visible, false, 10);
        assert_eq!(text, "abcde\nwider\n");
    }

    #[test]
    fn history_refresh_pacing() {
        let ms = Duration::from_millis;
        assert!(history_refresh_due(None, || false), "first render reads");
        assert!(
            !history_refresh_due(Some(ms(50)), || true),
            "min interval holds"
        );
        assert!(history_refresh_due(Some(ms(250)), || true), "screen moved");
        assert!(
            !history_refresh_due(Some(ms(250)), || false),
            "quiet screen waits"
        );
        assert!(
            history_refresh_due(Some(ms(1000)), || false),
            "periodic bound"
        );
    }

    #[test]
    fn comparable_row_strips_escapes_and_carriage_returns() {
        assert_eq!(
            comparable_row("\x1b[0m\x1b[38;2;0;95;255m  hi\x1b[0m \r"),
            "  hi"
        );
        assert_eq!(
            comparable_row("\x1b]8;;https://x\x1b\\link\x1b]8;;\x07!"),
            "link!"
        );
        assert_eq!(comparable_row("   \r"), "");
    }

    #[test]
    fn host_history_is_recent_minus_the_screen() {
        // Herdr's `recent`: three history rows, then the screen with its
        // trailing blank rows trimmed; `visible` keeps them.
        let recent = "old 1\r\n\x1b[1mold 2\x1b[0m\nold 3\n\nscreen a\nscreen b\n";
        let visible = "\nscreen a\r\nscreen b\n\n\n";
        let history = split_host_history(recent, true, visible).unwrap();
        assert_eq!(history.text, "old 1\r\n\x1b[1mold 2\x1b[0m\nold 3\n");
        assert!(history.truncated);
    }

    #[test]
    fn application_history_keeps_rows_hidden_by_a_sticky_header() {
        let recent = (1..=100)
            .map(|n| format!("answer {n:03}\n"))
            .collect::<String>();
        let visible = format!(
            "sticky prompt\n{}",
            (75..=100)
                .map(|n| format!("\x1b[32manswer {n:03}\x1b[0m\n"))
                .collect::<String>()
        );
        let history = split_application_history(&recent, true, &visible).unwrap();
        assert_eq!(history.text.lines().count(), 74);
        assert!(history.text.starts_with("answer 001\n"));
        assert!(history.text.ends_with("answer 074\n"));
        assert!(history.truncated);
        let combined = history.text + &visible;
        for n in 1..=100 {
            assert_eq!(combined.matches(&format!("answer {n:03}")).count(), 1);
        }
        // Re-locate the seam after a viewport moves; never cache its old cut.
        let moved = (80..=100)
            .map(|n| format!("answer {n:03}\n"))
            .collect::<String>();
        assert_eq!(
            split_application_history(&recent, false, &moved)
                .unwrap()
                .text
                .lines()
                .count(),
            79
        );
    }

    #[test]
    fn application_history_matches_paragraphs_below_a_shared_fixed_header() {
        let recent =
            "fixed one\nfixed two\nfixed three\nold paragraph\n\nfirst\n\nsecond\n\nthird\n";
        let visible = "fixed one\nfixed two\nfixed three\nfirst\n\nsecond\n\nthird\n";
        let history = split_application_history(recent, false, visible).unwrap();
        assert!(history.text.contains("old paragraph"));
        assert!(!history.text.contains("first\n"));
    }

    #[test]
    fn application_history_rejects_ambiguous_or_unrelated_screens() {
        assert!(split_application_history("a\nb\nc\na\nb\nc\n", false, "a\nb\nc\n").is_none());
        assert!(split_application_history("old\na\nb\nc\n", false, "new\nx\ny\nz\n").is_none());
        assert!(split_application_history("old\na\n", false, "a\n").is_none());
    }

    #[test]
    fn host_history_tolerates_rows_scrolled_between_reads() {
        // One row scrolled off between the two reads: the screen now
        // starts one row later than `recent` placed it.
        let recent = "h1\nh2\nh3\ns1\ns2\ns3\n";
        let visible = "s2\ns3\ns4\n";
        let history = split_host_history(recent, false, visible).unwrap();
        assert_eq!(history.text, "h1\nh2\nh3\ns1\n");
    }

    #[test]
    fn host_history_blank_screen_and_unlocatable_screen() {
        let all = split_host_history("h1\nh2\n", false, "\n\n").unwrap();
        assert_eq!(all.text, "h1\nh2\n");
        // The screen never appears in `recent`: no history at all rather
        // than a guessed cut.
        assert_eq!(split_host_history("h1\nh2\nh3\n", false, "zz\n"), None);
    }

    #[test]
    fn recent_with_history_is_history_then_live_screen() {
        let mut emu = PaneEmulator::new();
        // A scroll region (rows 1-2) keeps scrolled rows out of the
        // emulator's scrollback, like a TUI with a fixed input box.
        emu.apply(&frame(
            1,
            10,
            3,
            b"\x1b[1;2r\x1b[1;1Hone\r\ntwo\r\nthree\x1b[3;1Hinput",
        ));
        let (without, _) = emu.render_parts(ReadSource::Recent, false, 50);
        assert_eq!(
            without, "two\nthree\ninput\n",
            "no scrollback without history"
        );
        let history = HostHistory {
            text: "zero\none\n".to_owned(),
            truncated: false,
        };
        let (with, truncated) = emu.render_with_history(&history, false, 50);
        assert_eq!(with, "zero\none\ntwo\nthree\ninput\n");
        assert!(!truncated);
        // The cap keeps the tail and reports the drop.
        let (capped, truncated) = emu.render_with_history(&history, false, 2);
        assert_eq!(capped, "three\ninput\n");
        assert!(truncated);
    }
}
