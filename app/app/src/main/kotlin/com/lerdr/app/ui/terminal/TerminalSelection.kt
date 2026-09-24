package com.lerdr.app.ui.terminal

import androidx.compose.ui.geometry.Offset

/**
 * Cell-range selection over the committed pane rows — the long-press-drag
 * gesture selects terminal cells, not text offsets, so the model works in
 * `(row, col)` space and maps back to characters only when copying.
 *
 * Both ends are **inclusive**: the cell under the finger on press and on
 * release are part of the selection. The mapping is approximate by
 * construction: `preserveTerminalCells` emits one leaf per grapheme
 * cluster tagged with its cell width, so a leaf span covers `cells`
 * columns atomically, while untagged runs map one column per code point.
 * Wide graphemes (CJK/emoji) therefore occupy their true two columns
 * without splitting.
 */

/** A cell in the rendered grid — `row` indexes `rows`, `col` the column. */
data class TerminalCell(val row: Int, val col: Int)

/** Row-major ordering — selection works dragging up or down. */
fun normalizeSelection(
    anchor: TerminalCell,
    cursor: TerminalCell,
): Pair<TerminalCell, TerminalCell> =
    if (anchor.row < cursor.row ||
        (anchor.row == cursor.row && anchor.col <= cursor.col)
    ) {
        anchor to cursor
    } else {
        cursor to anchor
    }

/**
 * Per-row cell ranges to paint — `row index → [startCol, endColExclusive)`
 * columns. The first row covers `start.col` through `end.row`'s inclusive
 * `end.col`; middle rows span their full width. Degenerate selections
 * (end before start on the same row) map to an empty map.
 */
fun selectionCellRanges(
    rows: List<TerminalRowUi>,
    anchor: TerminalCell,
    cursor: TerminalCell,
): Map<Int, IntRange> {
    val (start, end) = normalizeSelection(anchor, cursor)
    if (start.row == end.row && start.col > end.col) return emptyMap()
    val ranges = LinkedHashMap<Int, IntRange>()
    for (index in start.row..end.row) {
        val row = rows.getOrNull(index) ?: continue
        val from = if (index == start.row) start.col else 0
        // Inclusive end → exclusive bound one column past it.
        val to = if (index == end.row) {
            minOf(end.col + 1, row.cells)
        } else {
            row.cells
        }
        if (to > from) ranges[index] = from until to
    }
    return ranges
}

/**
 * The adjustment teardrops' edge points — bottom-left of the first
 * selected cell and bottom-right of the last, in content pixels. The
 * drawn handle hangs below its edge; null for an empty range map.
 */
fun selectionHandlePoints(
    ranges: Map<Int, IntRange>,
    cellWidth: Float,
    rowHeight: Float,
): Pair<Offset, Offset>? {
    val firstRow = ranges.keys.minOrNull() ?: return null
    val lastRow = ranges.keys.maxOrNull() ?: return null
    return Offset(
        ranges.getValue(firstRow).first * cellWidth,
        (firstRow + 1) * rowHeight,
    ) to Offset(
        (ranges.getValue(lastRow).last + 1) * cellWidth,
        (lastRow + 1) * rowHeight,
    )
}

/** The selected text — rows joined by `'\n'`, trailing draw-padding trimmed. */
fun selectedText(
    rows: List<TerminalRowUi>,
    anchor: TerminalCell,
    cursor: TerminalCell,
): String {
    val (start, end) = normalizeSelection(anchor, cursor)
    if (start.row == end.row && start.col > end.col) return ""
    val out = StringBuilder()
    for (index in start.row..end.row) {
        val row = rows.getOrNull(index) ?: continue
        val from = if (index == start.row) start.col else 0
        val to = if (index == end.row) end.col else row.cells - 1
        if (to < from) continue
        val fromChar = charOffsetForCell(row, from, includeCell = false)
        val toChar = charOffsetForCell(row, to, includeCell = true)
        if (toChar <= fromChar) continue
        if (out.isNotEmpty()) out.append('\n')
        out.append(row.joinedText().substring(fromChar, toChar).trimEnd())
    }
    return out.toString()
}

/**
 * Cell column → UTF-16 offset into the row's joined span text.
 * [includeCell] picks the boundary after the grapheme covering [col]
 * (selection end) versus before it (selection start). Columns beyond the
 * row's cells resolve to the row's end.
 */
internal fun charOffsetForCell(
    row: TerminalRowUi,
    col: Int,
    includeCell: Boolean,
): Int {
    if (col < 0) return 0
    var cellCursor = 0
    var charCursor = 0
    for (span in row.spans) {
        val text = span.text
        if (text.isEmpty()) continue
        val codePoints = text.codePointCount(0, text.length)
        if (span.cells != codePoints) {
            // Cell leaf — one grapheme cluster covering `cells` columns
            // atomically (wide chars = 2, plain/box graphemes = 1, so a
            // multi-codepoint cluster never splits).
            val width = span.cells
            if (col < cellCursor + width) {
                return if (includeCell) charCursor + text.length else charCursor
            }
            cellCursor += width
            charCursor += text.length
        } else {
            // Untagged run — one column per code point.
            var offset = 0
            while (offset < text.length) {
                val width = Character.charCount(text.codePointAt(offset))
                if (col <= cellCursor) {
                    return if (includeCell) {
                        charCursor + offset + width
                    } else {
                        charCursor + offset
                    }
                }
                cellCursor += 1
                offset += width
            }
            charCursor += text.length
        }
    }
    return row.spans.sumOf { it.text.length }
}

/** The row's joined span text — the coordinate space offsets map into. */
private fun TerminalRowUi.joinedText(): String =
    spans.joinToString("") { it.text }
