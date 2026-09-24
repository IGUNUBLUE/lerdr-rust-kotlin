package com.lerdr.app.ui.terminal

import androidx.compose.ui.geometry.Offset
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * Cell-range selection — the pure model: range normalization, per-row
 * cell spans for the draw pass, and cell→text extraction for copy.
 */
class TerminalSelectionTest {

    private fun rows(vararg lines: String): List<TerminalRowUi> =
        parseTerminalRows(lines.toList(), TERMINAL_FORMAT_ANSI)

    @Test
    fun `same row selects the char range`() {
        val parsed = rows("hello world")
        // 'o w' — cells 4..6 inclusive.
        assertEquals(
            "o w",
            selectedText(parsed, TerminalCell(0, 4), TerminalCell(0, 6)),
        )
    }

    @Test
    fun `multi row joins lines and trims draw padding`() {
        val parsed = rows("alpha", "beta   ", "gamma")
        assertEquals(
            "alpha\nbeta\ngamma",
            selectedText(parsed, TerminalCell(0, 0), TerminalCell(2, 4)),
        )
    }

    @Test
    fun `partial edges keep whole middle rows`() {
        val parsed = rows("alpha", "beta", "gamma")
        // From 'p' on row 0 to 'm' on row 2 — row 1 comes whole.
        assertEquals(
            "pha\nbeta\ngam",
            selectedText(parsed, TerminalCell(0, 2), TerminalCell(2, 2)),
        )
    }

    @Test
    fun `dragging backwards normalizes`() {
        val parsed = rows("hello world")
        assertEquals(
            "o w",
            selectedText(parsed, TerminalCell(0, 6), TerminalCell(0, 4)),
        )
        assertEquals(
            TerminalCell(0, 4) to TerminalCell(0, 6),
            normalizeSelection(TerminalCell(0, 6), TerminalCell(0, 4)),
        )
    }

    @Test
    fun `wide grapheme occupies both cells atomically`() {
        // 表 is a terminal-wide leaf — cells 2..3; 'c' lands at 4.
        val parsed = rows("ab表cd")
        assertEquals(
            "ab表c",
            selectedText(parsed, TerminalCell(0, 0), TerminalCell(0, 4)),
        )
        // Starting inside the wide cell pulls the whole grapheme in.
        assertEquals(
            "表c",
            selectedText(parsed, TerminalCell(0, 3), TerminalCell(0, 4)),
        )
    }

    @Test
    fun `end past the row ends at the row`() {
        val parsed = rows("short")
        assertEquals(
            "short",
            selectedText(parsed, TerminalCell(0, 0), TerminalCell(0, 200)),
        )
    }

    @Test
    fun `single cell selects one char`() {
        val parsed = rows("hello")
        assertEquals(
            "e",
            selectedText(parsed, TerminalCell(0, 1), TerminalCell(0, 1)),
        )
    }

    @Test
    fun `cell ranges span the middle rows`() {
        val parsed = rows("alpha", "beta", "gamma")
        val ranges = selectionCellRanges(
            parsed,
            TerminalCell(0, 2),
            TerminalCell(2, 2),
        )
        assertEquals(2 until 5, ranges[0])
        assertEquals(0 until 4, ranges[1])
        assertEquals(0 until 3, ranges[2])
    }

    @Test
    fun `reversed same row drag normalizes and clamps to the row`() {
        val parsed = rows("hello")
        val ranges = selectionCellRanges(
            parsed,
            TerminalCell(0, 5),
            TerminalCell(0, 2),
        )
        assertEquals(2 until 5, ranges[0])
    }

    @Test
    fun `handle points sit at the selection's outer edges`() {
        val parsed = rows("alpha", "beta", "gamma")
        val ranges = selectionCellRanges(
            parsed,
            TerminalCell(0, 2),
            TerminalCell(2, 2),
        )
        val (start, end) = selectionHandlePoints(
            ranges, cellWidth = 10f, rowHeight = 20f,
        )!!
        // Bottom-left of the first selected cell / bottom-right of the last.
        assertEquals(Offset(20f, 20f), start)
        assertEquals(Offset(30f, 60f), end)
    }

    @Test
    fun `handle points are absent for an empty range`() {
        val parsed = rows("hello")
        // Both ends past the row's width — no cell ever gets painted.
        val ranges = selectionCellRanges(
            parsed,
            TerminalCell(0, 9),
            TerminalCell(0, 12),
        )
        assertEquals(null, selectionHandlePoints(ranges, 10f, 20f))
    }
}
