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
//! - **No scroll harvesting** — `recent`/`recent-unwrapped` reads scroll the
//!   operator's real pane on Herdr; the emulator keeps scrollback locally
//!   and never touches the host pane.
//! - **Real VT semantics** — alt screen, scroll regions, and cursor state
//!   are resolved by the parser instead of re-read snapshots racing the
//!   writer.
//!
//! Fallback: spawn failure, EOF, or `terminal.closed` degrade to the
//! snapshot path for the rest of the watch — the stream is a strict
//! improvement when available and invisible when not.
//!
//! `LERDR_PANE_STREAM=off` disables the stream entirely (reads always poll).

use lerdr_herdr::observe::{ObserveEvent, ObserveStream, TerminalFrame};
use lerdr_herdr::ReadSource;
use tracing::warn;

use crate::watches::cap_pane_content_lines;

/// Scrollback kept by the emulator — comfortably past any `lines` budget
/// the router clamps to (`MAX_PANE_LINES` is far below this).
const SCROLLBACK_ROWS: usize = 4096;

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
    /// Spawn the observer; `None` when the CLI/socket aren't reachable or
    /// the stream is env-disabled. `client` supplies the resolved `herdr`
    /// binary and the transport's socket path.
    pub(crate) fn spawn(client: &lerdr_herdr::Client, pane_id: &str) -> Option<Self> {
        if !streams_enabled() {
            return None;
        }
        let socket_path = client.socket_path_hint()?;
        let bin = client.resolved_herdr_bin();
        Some(PaneStream {
            stream: ObserveStream::spawn(&bin, pane_id, &socket_path)?,
            emu: PaneEmulator::new(),
        })
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
    pub(crate) fn render_parts(
        &self,
        source: ReadSource,
        ansi: bool,
        lines: u32,
    ) -> (String, bool) {
        self.emu.render_parts(source, ansi, lines)
    }
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
}
