package com.lerdr.app.ui.terminal

import androidx.compose.ui.graphics.Color
import com.google.common.truth.Truth.assertThat
import org.junit.Test

/**
 * Snapshot → render-row mapping — the pure half of the terminal renderer
 * (no Compose runtime: `Color` is a value class, so these run on the JVM).
 */
class TerminalRenderModelTest {

    @Test
    fun `ansi line parses SGR colors and flags`() {
        val rows = parseTerminalRows(
            listOf("\u001b[31m\u001b[1merr\u001b[0m plain \u001b[4munder"),
            format = TERMINAL_FORMAT_ANSI,
        )
        val spans = rows.single().spans
        assertThat(spans).hasSize(3)
        with(spans[0]) {
            assertThat(text).isEqualTo("err")
            assertThat(fg).isEqualTo(Color(0xFFFF5F5F))
            assertThat(bold).isTrue()
        }
        with(spans[1]) {
            assertThat(text).isEqualTo(" plain ")
            assertThat(fg).isNull()
            assertThat(bold).isFalse()
        }
        with(spans[2]) {
            assertThat(text).isEqualTo("under")
            assertThat(underline).isTrue()
        }
    }

    @Test
    fun `background colors and dim resolve`() {
        val rows = parseTerminalRows(
            listOf("\u001b[41m\u001b[2mfaded"),
            format = TERMINAL_FORMAT_ANSI,
        )
        val span = rows.single().spans.single()
        assertThat(span.bg).isEqualTo(Color(0xFFFF5F5F))
        assertThat(span.dim).isTrue()
    }

    @Test
    fun `non-ansi format keeps the line literal`() {
        val rows = parseTerminalRows(
            listOf("\u001b[31mnot-styled"),
            format = "plain",
        )
        val span = rows.single().spans.single()
        assertThat(span.text).isEqualTo("\u001b[31mnot-styled")
        assertThat(span.fg).isNull()
    }

    @Test
    fun `cell widths expand tabs and wide graphemes`() {
        val rows = parseTerminalRows(
            listOf("\ta", "あいう"),
            format = TERMINAL_FORMAT_ANSI,
        )
        // Tab at column 0 expands to 8 cells; 'a' rides the same run.
        assertThat(rows[0].cells).isEqualTo(9)
        // CJK wide graphemes count 2 cells each.
        assertThat(rows[1].cells).isEqualTo(6)
    }

    @Test
    fun `links surface as href spans`() {
        val rows = parseTerminalRows(
            listOf("see https://example.com/docs for details"),
            format = TERMINAL_FORMAT_ANSI,
        )
        val link = rows.single().spans.first { it.href != null }
        assertThat(link.href).isEqualTo("https://example.com/docs")
    }

    @Test
    fun `bold+italic without a color takes the scheme heading accent`() {
        val rows = parseTerminalRows(
            listOf("\u001b[1m\u001b[3mheading"),
            format = TERMINAL_FORMAT_ANSI,
        )
        val span = rows.single().spans.single()
        assertThat(span.bold).isTrue()
        assertThat(span.italic).isTrue()
        // TerminalScheme.DARK.headingAccent = #3daee9.
        assertThat(span.fg).isEqualTo(Color(0xFF3DAEE9))
    }

    @Test
    fun `cursor rides the last row one cell past its content`() {
        val rows = parseTerminalRows(
            listOf("build finished", "$ "),
            format = TERMINAL_FORMAT_ANSI,
        )
        assertThat(terminalCursor(rows)).isEqualTo(TerminalCursorUi(row = 1, column = 2))
    }

    @Test
    fun `empty input yields no cursor`() {
        assertThat(terminalCursor(emptyList())).isNull()
        assertThat(parseTerminalRows(emptyList(), TERMINAL_FORMAT_ANSI)).isEmpty()
    }

    @Test
    fun `trailing newline leaves a last empty row for the cursor`() {
        val rows = parseTerminalRows(
            listOf("done", ""),
            format = TERMINAL_FORMAT_ANSI,
        )
        assertThat(terminalCursor(rows)).isEqualTo(TerminalCursorUi(row = 1, column = 0))
    }

    @Test
    fun `terminalColor resolves the wire encodings`() {
        assertThat(terminalColor("#ff5f5f")).isEqualTo(Color(0xFFFF5F5F))
        assertThat(terminalColor("#fff")).isEqualTo(Color(0xFFFFFFFF))
        assertThat(terminalColor("rgb(1,2,3)")).isEqualTo(Color(1, 2, 3))
        assertThat(terminalColor(null)).isNull()
        // CSS-syntax values only appear under normalization flags we never set.
        assertThat(terminalColor("var(--terminal-text)")).isNull()
        assertThat(terminalColor("not-a-color")).isNull()
    }
}
