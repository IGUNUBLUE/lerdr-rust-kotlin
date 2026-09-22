package com.lerdr.app.ui.terminal

import com.google.common.truth.Truth.assertThat
import org.junit.Test

/**
 * Find-in-buffer semantics — the JVM mirror of the oracle's
 * `terminal-find.ts` (frontend/src/lib/terminal-find.ts): corpus join,
 * literal case-insensitive matching, row-offset mapping, fragment splits,
 * wraparound navigation.
 */
class TerminalFindTest {

    private fun rows(vararg texts: String): List<TerminalFindRow> =
        texts.map { TerminalFindRow(it) }

    @Test
    fun `corpus joins rendered rows with newline`() {
        val rows = rows("foo bar", "baz", "")
        assertThat(terminalSearchText(rows)).isEqualTo("foo bar\nbaz\n")
        assertThat(terminalRowOffsets(rows).toList()).containsExactly(0, 8, 12).inOrder()
    }

    @Test
    fun `single row has no separator cost`() {
        val rows = rows("alone")
        assertThat(terminalSearchText(rows)).isEqualTo("alone")
        assertThat(terminalRowOffsets(rows).toList()).containsExactly(0)
    }

    @Test
    fun `match spans are corpus offsets`() {
        val corpus = terminalSearchText(rows("ok go", "no ok"))
        // "ok go\nno ok" — hits at 0..2 and 9..11.
        val result = findTerminalText(corpus, "ok")
        assertThat(result.truncated).isFalse()
        assertThat(result.matches).containsExactly(
            TerminalFindMatch(0, 2),
            TerminalFindMatch(9, 11),
        ).inOrder()
    }

    @Test
    fun `search is literal — regex metacharacters are escaped`() {
        val result = findTerminalText("a.b axb a.b", "a.b")
        assertThat(result.matches).containsExactly(
            TerminalFindMatch(0, 3),
            TerminalFindMatch(8, 11),
        ).inOrder()
    }

    @Test
    fun `search is case-insensitive like the oracle's giu flag`() {
        val result = findTerminalText("Foo fOO bar", "foo")
        assertThat(result.matches).hasSize(2)
    }

    @Test
    fun `search is Unicode-aware case-insensitive`() {
        // UNICODE_CASE mirrors JS 'iu' — accented letters and the Kelvin
        // sign fold onto their ASCII counterparts.
        assertThat(findTerminalText("Résumé", "résumé").matches).hasSize(1)
        assertThat(findTerminalText("temp: \u212A", "k").matches).hasSize(1)
    }

    @Test
    fun `empty query or dead limit yields no matches`() {
        assertThat(findTerminalText("abc", "").matches).isEmpty()
        assertThat(findTerminalText("abc", "a", limit = 0).truncated).isFalse()
        assertThat(findTerminalText("abc", "a", limit = 0).matches).isEmpty()
    }

    @Test
    fun `whitespace query still searches — trimming is the caller's job`() {
        // The oracle trims at the call site (findQuery.trim()); the find
        // function itself treats a space as a literal query.
        val result = findTerminalText("a b", " ")
        assertThat(result.matches).containsExactly(TerminalFindMatch(1, 2))
    }

    @Test
    fun `matches do not overlap`() {
        // regex exec semantics — after a hit the scan resumes at its end.
        assertThat(findTerminalText("aaa", "aa").matches)
            .containsExactly(TerminalFindMatch(0, 2))
    }

    @Test
    fun `truncated only when a match beyond the limit exists`() {
        val corpus = "aa aa aa"
        val capped = findTerminalText(corpus, "aa", limit = 2)
        assertThat(capped.matches).hasSize(2)
        assertThat(capped.truncated).isTrue()
        // Exactly-at-limit is not truncated — the cap bites on the next hit.
        val exact = findTerminalText("aa aa", "aa", limit = 2)
        assertThat(exact.matches).hasSize(2)
        assertThat(exact.truncated).isFalse()
    }

    @Test
    fun `row for offset maps corpus positions to rows`() {
        val rows = rows("ab", "cd")
        val offsets = terminalRowOffsets(rows) // [0, 3] over "ab\ncd"
        assertThat(terminalRowForOffset(rows, offsets, 0)).isEqualTo(0)
        assertThat(terminalRowForOffset(rows, offsets, 1)).isEqualTo(0)
        // The '\n' slot belongs to the row it terminates.
        assertThat(terminalRowForOffset(rows, offsets, 2)).isEqualTo(0)
        assertThat(terminalRowForOffset(rows, offsets, 3)).isEqualTo(1)
        assertThat(terminalRowForOffset(rows, offsets, 4)).isEqualTo(1)
        // Past the end clamps onto the last row; empty corpus misses.
        assertThat(terminalRowForOffset(rows, offsets, 100)).isEqualTo(1)
        assertThat(terminalRowForOffset(emptyList(), IntArray(0), 0)).isEqualTo(-1)
    }

    @Test
    fun `match fragments split a match across row boundaries`() {
        val rows = rows("abc", "def")
        val offsets = terminalRowOffsets(rows)
        // "abc\ndef" — "c\nd" spans the join (only reachable via a query
        // containing a newline; the single-line field can't type one, but
        // the mapping is the oracle's verbatim).
        val fragments = terminalMatchFragments(rows, offsets, TerminalFindMatch(2, 5))
        assertThat(fragments).containsExactly(
            TerminalFindFragment(row = 0, start = 2, end = 3),
            TerminalFindFragment(row = 1, start = 0, end = 1),
        ).inOrder()
    }

    @Test
    fun `match fragments ignore zero-length and out-of-row slices`() {
        val rows = rows("abc", "def")
        val offsets = terminalRowOffsets(rows)
        assertThat(terminalMatchFragments(rows, offsets, TerminalFindMatch(1, 1))).isEmpty()
        // A match ending exactly at a row boundary leaves nothing behind.
        val fragments = terminalMatchFragments(rows, offsets, TerminalFindMatch(1, 3))
        assertThat(fragments).containsExactly(TerminalFindFragment(0, 1, 3))
    }

    @Test
    fun `find ranges group fragments by row with the active flag`() {
        val rows = rows("ok go", "no ok")
        val offsets = terminalRowOffsets(rows)
        val matches = findTerminalText(terminalSearchText(rows), "ok").matches
        val ranges = terminalFindRanges(rows, offsets, matches, activeIndex = 1)
        assertThat(ranges.keys).containsExactly(0, 1).inOrder()
        assertThat(ranges[0]).containsExactly(
            TerminalFindRange(0, 2, active = false),
        )
        assertThat(ranges[1]).containsExactly(
            TerminalFindRange(3, 5, active = true),
        )
    }

    @Test
    fun `find ranges are empty without rows or matches`() {
        val rows = rows("abc")
        val offsets = terminalRowOffsets(rows)
        assertThat(terminalFindRanges(rows, offsets, emptyList(), 0)).isEmpty()
        assertThat(
            terminalFindRanges(
                emptyList(), IntArray(0), listOf(TerminalFindMatch(0, 1)), 0,
            ),
        ).isEmpty()
    }

    @Test
    fun `navigation index wraps both directions`() {
        assertThat(wrapFindIndex(-1, 3)).isEqualTo(2)
        assertThat(wrapFindIndex(0, 3)).isEqualTo(0)
        assertThat(wrapFindIndex(2, 3)).isEqualTo(2)
        assertThat(wrapFindIndex(3, 3)).isEqualTo(0)
        assertThat(wrapFindIndex(7, 3)).isEqualTo(1)
        assertThat(wrapFindIndex(0, 0)).isEqualTo(-1)
    }

    @Test
    fun `find rows come from the rendered span text`() {
        // ANSI is resolved, tabs expanded — the corpus is what the user
        // sees, not the wire bytes.
        val uiRows = parseTerminalRows(
            listOf("a\u001b[31mb\u001b[0mc", "\tlead"),
            format = TERMINAL_FORMAT_ANSI,
        )
        val findRows = terminalFindRows(uiRows)
        assertThat(findRows[0].text).isEqualTo("abc")
        assertThat(findRows[1].text).isEqualTo("        lead")
    }
}
