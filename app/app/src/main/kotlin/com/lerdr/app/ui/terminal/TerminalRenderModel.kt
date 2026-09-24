package com.lerdr.app.ui.terminal

import androidx.compose.runtime.Immutable
import androidx.compose.ui.graphics.Color
import lerdr.core.terminal.AnsiSpans
import lerdr.core.terminal.TerminalScheme
import lerdr.core.terminal.TerminalSpan

/**
 * Render model for [TerminalSurface] — the committed `PaneSurface.Snapshot`
 * mapped once per commit into stable, Compose-ready rows.
 *
 * Parsing lives on the ViewModel side (UDF: the composable is stateless);
 * every model entering composition is `@Immutable` so a new snapshot only
 * recomposes what actually changed.
 */

/** One styled run inside a row — `TerminalSpan` resolved to Compose colors. */
@Immutable
data class TerminalSpanUi(
    val text: String,
    /** Resolved fg color; null = the pane's default text color. */
    val fg: Color? = null,
    /** Resolved bg color; null = transparent (the pane surface shows). */
    val bg: Color? = null,
    val bold: Boolean = false,
    val italic: Boolean = false,
    val underline: Boolean = false,
    /** SGR 2 — the oracle renders it as 70% opacity on the fg color. */
    val dim: Boolean = false,
    /** `terminal-link` leaf — the normalized http(s) target. */
    val href: String? = null,
    /**
     * Cell width of this span on the grid — a leaf carries its real
     * width (wide graphemes = 2); a run maps one column per code point.
     * Selection maps cell columns back to text through it.
     */
    val cells: Int = 0,
)

/**
 * One committed scrollback row. [cells] is the grid width in terminal cells
 * (tabs already expanded, wide graphemes counting 2) — it may exceed the
 * lease width when the pane was captured at a larger size.
 */
@Immutable
data class TerminalRowUi(
    val index: Int,
    val spans: List<TerminalSpanUi>,
    val cells: Int,
)

/**
 * The write cursor: last row, one cell past its content — the position a
 * real terminal would append at. The wire frame carries no cursor
 * coordinates (the pane is a text capture), so this is the approximation
 * the oracle's capture model implies.
 */
@Immutable
data class TerminalCursorUi(
    val row: Int,
    val column: Int,
)

/** `renderTerminalContent` — `format == "ansi"` parses SGR, else literal. */
fun parseTerminalRows(
    lines: List<String>,
    format: String,
    scheme: TerminalScheme = TerminalScheme.DARK,
): List<TerminalRowUi> = lines.mapIndexed { index, line ->
    val spans = if (format == TERMINAL_FORMAT_ANSI) {
        // preserveTerminalCells expands tabs at 8-cell stops and tags runs
        // with their cell width — the fixed-grid contract the renderer draws.
        AnsiSpans.parse(line, scheme = scheme, preserveTerminalCells = true)
    } else {
        listOf(TerminalSpan(text = line))
    }
    val uiSpans = spans.map { span -> span.toUi() }
    TerminalRowUi(index = index, spans = uiSpans, cells = spans.sumOf(::spanCells))
}

/** Cursor for a parsed row set — `null` rows means no committed content. */
fun terminalCursor(rows: List<TerminalRowUi>): TerminalCursorUi? {
    val last = rows.lastOrNull() ?: return null
    return TerminalCursorUi(row = last.index, column = last.cells)
}

private fun TerminalSpan.toUi(): TerminalSpanUi = TerminalSpanUi(
    text = text,
    fg = terminalColor(fg),
    bg = terminalColor(bg),
    bold = bold,
    italic = italic,
    underline = underline,
    dim = dim,
    href = href,
    cells = spanCells(this),
)

/**
 * Cell width of one span. `preserveTerminalCells` tags run/horizontal
 * leaves with `width_cells`; individual cell leaves carry no width —
 * `terminal-cell-wide` is 2 cells, every other cell leaf is one grapheme
 * at 1 column (a multi-codepoint cluster like `e`+́ still occupies one
 * cell). Zero-width marks never appear standalone — `BreakIterator` keeps
 * them attached to their base cluster. Plain-format spans carry no tags —
 * one column per code point.
 */
private fun spanCells(span: TerminalSpan): Int {
    span.widthCells?.let { return it }
    if (span.text.isEmpty()) return 0
    val className = span.className ?: return span.text.codePointCount(0, span.text.length)
    if (className.contains("terminal-cell-wide")) return 2
    if (className.contains("terminal-cell")) return 1
    return span.text.codePointCount(0, span.text.length)
}

/**
 * `#rgb` / `#rrggbb` / `rgb(r,g,b)` → Compose [Color], via the shared ANSI
 * channel parser. CSS-syntax values (`var(--terminal-text)`, `color-mix`)
 * only appear when normalization flags run — the pane surface is always
 * dark, so they are not expected; they resolve to null (default color).
 */
internal fun terminalColor(value: String?): Color? {
    if (value == null) return null
    val channels = AnsiSpans.ansiColorChannels(value) ?: return null
    return Color(red = channels[0], green = channels[1], blue = channels[2])
}

const val TERMINAL_FORMAT_ANSI = "ansi"
