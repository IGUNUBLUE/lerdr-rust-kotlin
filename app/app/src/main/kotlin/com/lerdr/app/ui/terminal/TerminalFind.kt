package com.lerdr.app.ui.terminal

import java.util.regex.Pattern

/**
 * Find-in-buffer — pure port of the oracle's `terminal-find.ts`
 * (frontend/src/lib/terminal-find.ts). The corpus joins every rendered row
 * with `'\n'`; matches are literal-text hits over that corpus and map back
 * to per-row character spans for highlighting.
 *
 * Nothing here touches Compose — [TerminalScreen] wires the state and
 * [TerminalSurface] draws the fragments. `Pattern.CASE_INSENSITIVE |
 * UNICODE_CASE` is the JVM equivalent of the oracle's `giu` regex flags
 * (plain IGNORE_CASE is ASCII-only; `iu` is Unicode-aware).
 */

/** One rendered scrollback row — [text] is the row as the user sees it. */
data class TerminalFindRow(val text: String)

/** A query hit in the joined corpus — [start, end) character offsets. */
data class TerminalFindMatch(val start: Int, val end: Int)

data class TerminalFindResult(
    val matches: List<TerminalFindMatch>,
    /** True when the corpus had more hits than the match cap could hold. */
    val truncated: Boolean,
)

/** The slice of a match that lands on one row — [start, end) in row text. */
data class TerminalFindFragment(val row: Int, val start: Int, val end: Int)

/**
 * A highlight span on one rendered row — [start, end) are character offsets
 * into the row text (the same offsets the row's `AnnotatedString` uses).
 * [active] marks the fragment of the current match (`mark.active` in the
 * oracle) so the renderer can paint it stronger.
 */
data class TerminalFindRange(val start: Int, val end: Int, val active: Boolean)

const val TERMINAL_FIND_MATCH_LIMIT = 1_000

/** `terminalSearchText` — rendered rows joined with `'\n'`. */
fun terminalSearchText(rows: List<TerminalFindRow>): String =
    rows.joinToString("\n") { it.text }

/**
 * `findTerminalText` — literal, case-insensitive search. The oracle escapes
 * the query into a `giu` regex; `Pattern.quote` is the same literal match.
 * Callers pass the trimmed query (the oracle searches `findQuery.trim()`);
 * an empty query or a non-positive limit short-circuits to no matches.
 */
fun findTerminalText(
    text: String,
    query: String,
    limit: Int = TERMINAL_FIND_MATCH_LIMIT,
): TerminalFindResult {
    if (query.isEmpty() || limit < 1) {
        return TerminalFindResult(matches = emptyList(), truncated = false)
    }
    val matcher = Pattern
        .compile(Pattern.quote(query), Pattern.CASE_INSENSITIVE or Pattern.UNICODE_CASE)
        .matcher(text)
    val matches = ArrayList<TerminalFindMatch>()
    while (matcher.find()) {
        if (matches.size >= limit) {
            return TerminalFindResult(matches, truncated = true)
        }
        matches.add(TerminalFindMatch(matcher.start(), matcher.end()))
    }
    return TerminalFindResult(matches, truncated = false)
}

/** `terminalRowOffsets` — corpus offset where each row's text begins. */
fun terminalRowOffsets(rows: List<TerminalFindRow>): IntArray {
    val offsets = IntArray(rows.size)
    var offset = 0
    for (index in rows.indices) {
        offsets[index] = offset
        offset += rows[index].text.length + (if (index < rows.size - 1) 1 else 0)
    }
    return offsets
}

/**
 * `terminalRowForOffset` — binary search for the row containing a corpus
 * offset. Boundary semantics mirror the oracle exactly: the `'\n'` slot
 * after a row's text belongs to that row (`offset == end` returns middle),
 * and offsets past the last row's text still resolve to the last row.
 */
fun terminalRowForOffset(rows: List<TerminalFindRow>, offsets: IntArray, offset: Int): Int {
    if (rows.isEmpty()) return -1
    var low = 0
    var high = rows.size - 1
    while (low <= high) {
        val middle = (low + high) / 2
        val start = offsets[middle]
        val end = start + rows[middle].text.length
        if (offset < start) {
            high = middle - 1
        } else if (offset > end && middle < rows.size - 1) {
            low = middle + 1
        } else {
            return middle
        }
    }
    return maxOf(0, minOf(rows.size - 1, low))
}

/**
 * `terminalMatchFragments` — split a corpus match into per-row slices. A
 * query containing `'\n'` can span rows (the corpus joins rows with it);
 * the find bar's single-line field never produces one, but the mapping is
 * ported verbatim for completeness.
 */
fun terminalMatchFragments(
    rows: List<TerminalFindRow>,
    offsets: IntArray,
    match: TerminalFindMatch,
): List<TerminalFindFragment> {
    if (rows.isEmpty() || match.end <= match.start) return emptyList()
    val first = terminalRowForOffset(rows, offsets, match.start)
    val last = terminalRowForOffset(rows, offsets, maxOf(match.start, match.end - 1))
    val fragments = ArrayList<TerminalFindFragment>()
    for (row in first..last) {
        val rowStart = offsets[row]
        val start = maxOf(0, match.start - rowStart)
        val end = minOf(rows[row].text.length, match.end - rowStart)
        if (end > start) fragments.add(TerminalFindFragment(row, start, end))
    }
    return fragments
}

/**
 * The `byRow` map the oracle builds in `applyTerminalFindHighlights` —
 * every match's fragments grouped by row, each range tagged [active] when
 * its match is the current one. Rows with no matches are absent.
 */
fun terminalFindRanges(
    rows: List<TerminalFindRow>,
    offsets: IntArray,
    matches: List<TerminalFindMatch>,
    activeIndex: Int,
): Map<Int, List<TerminalFindRange>> {
    if (rows.isEmpty() || matches.isEmpty()) return emptyMap()
    val byRow = LinkedHashMap<Int, MutableList<TerminalFindRange>>()
    matches.forEachIndexed { index, match ->
        for (fragment in terminalMatchFragments(rows, offsets, match)) {
            byRow.getOrPut(fragment.row) { mutableListOf() }
                .add(TerminalFindRange(fragment.start, fragment.end, active = index == activeIndex))
        }
    }
    return byRow
}

/**
 * The oracle's index normalizer (`((i % n) + n) % n`) — wraps both
 * directions. Returns -1 when there is nothing to select.
 */
fun wrapFindIndex(index: Int, count: Int): Int =
    if (count < 1) -1 else Math.floorMod(index, count)

/** Rendered row → find corpus row: the text the user actually sees. */
fun terminalFindRows(rows: List<TerminalRowUi>): List<TerminalFindRow> =
    rows.map { row -> TerminalFindRow(row.spans.joinToString("") { it.text }) }
