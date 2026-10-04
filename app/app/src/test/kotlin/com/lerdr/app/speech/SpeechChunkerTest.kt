package com.lerdr.app.speech

import com.google.common.truth.Truth.assertThat
import org.junit.Test

/**
 * `speechChunks` / `speakableText` parity tests — Lerdr's
 * the protocol contract and the protocol contract behavior,
 * including its edge cases (word-boundary cuts, CJK sentence splits,
 * empty-sentence joins).
 */
class SpeechChunkerTest {

    // ── speechChunks ────────────────────────────────────────────────────

    @Test
    fun `empty text yields no chunks`() {
        assertThat(SpeechChunker.speechChunks("")).isEmpty()
    }

    @Test
    fun `short text is one chunk`() {
        assertThat(SpeechChunker.speechChunks("Hello world."))
            .containsExactly("Hello world.")
    }

    @Test
    fun `sentences pack into one chunk joined by a single space`() {
        // Western punctuation + whitespace is the split point.
        assertThat(SpeechChunker.speechChunks("First.  Second? Third!"))
            .containsExactly("First. Second? Third!")
    }

    @Test
    fun `newline after punctuation splits the sentence`() {
        assertThat(SpeechChunker.speechChunks("One.\nTwo."))
            .containsExactly("One. Two.")
    }

    @Test
    fun `cjk sentences separate at punctuation when they fill the budget`() {
        assertThat(SpeechChunker.speechChunks("你好。再见！谢谢。", limit = 3))
            .containsExactly("你好。", "再见！", "谢谢。").inOrder()
    }

    @Test
    fun `overlong piece cuts at the last space inside the limit`() {
        // "aaaa bbbb cccc": last space at or before index 9 sits at 9.
        assertThat(SpeechChunker.speechChunks("aaaa bbbb cccc", limit = 9))
            .containsExactly("aaaa bbbb", "cccc")
            .inOrder()
    }

    @Test
    fun `piece with no space inside the limit hard-cuts`() {
        assertThat(SpeechChunker.speechChunks("aaaaaaaaaaaa", limit = 9))
            .containsExactly("aaaaaaaaa", "aaa")
            .inOrder()
    }
    @Test
    fun `hard cuts preserve supplementary characters across the speech boundary`() {
        val text = "x".repeat(239) + "😀" + "tail"
        val chunks = SpeechChunker.speechChunks(text, limit = 240)
        assertThat(chunks).containsExactly("x".repeat(239), "😀tail").inOrder()
        assertThat(chunks.joinToString("")).isEqualTo(text)
    }

    @Test(expected = IllegalArgumentException::class)
    fun `zero budget is rejected instead of looping forever`() {
        SpeechChunker.speechChunks("hello", limit = 0)
    }

    @Test(expected = IllegalArgumentException::class)
    fun `a budget too small for one supplementary character is rejected`() {
        SpeechChunker.speechChunks("😀", limit = 1)
    }


    @Test
    fun `a space at index zero is not a usable cut point`() {
        // lastIndexOf(' ', 5) == 0 → Lerdr takes the hard `limit` cut
        // (the leading space stays on the remainder).
        assertThat(SpeechChunker.speechChunks(" abcdefghijkl", limit = 5))
            .containsExactly(" abcd", "efghi", "jkl")
            .inOrder()
    }

    @Test
    fun `a full current chunk flushes before the next sentence packs`() {
        // current="aa." (3) + " " + piece(10) = 14 > 8 → flush first.
        val chunks = SpeechChunker.speechChunks("aa. bbbbbbbbbb", limit = 8)
        assertThat(chunks).containsExactly("aa.", "bbbbbbbb", "bb").inOrder()
    }

    @Test
    fun `chunk boundary at exactly the limit does not cut`() {
        assertThat(SpeechChunker.speechChunks("12345678", limit = 8))
            .containsExactly("12345678")
    }

    @Test
    fun `sentence boundary trims the joining whitespace`() {
        // The split consumes the whitespace; sentences re-join on one space.
        assertThat(SpeechChunker.speechChunks("A.    B.", limit = 100))
            .containsExactly("A. B.")
    }

    @Test
    fun `long prose respects the 240 char speak budget`() {
        val words = (1..60).joinToString(" ") { "w%03d".format(it) } + "."
        val chunks = SpeechChunker.speechChunks(words, SpeechChunker.SPEAK_CHUNK_LIMIT)
        assertThat(chunks.size).isGreaterThan(1)
        chunks.forEach { assertThat(it.length).isAtMost(SpeechChunker.SPEAK_CHUNK_LIMIT) }
        // Every boundary lands on a word edge — no mid-word cuts here.
        chunks.drop(1).forEach { assertThat(it.first()).isEqualTo('w') }
    }

    @Test
    fun `speak limit packs a long response into fragments`() {
        // 1200 chars at the 240-char speak budget → several fragments.
        val sentence = "Word. ".repeat(200)
        val chunks = SpeechChunker.speechChunks(
            sentence,
            SpeechChunker.SPEAK_CHUNK_LIMIT,
        )
        assertThat(chunks.size).isGreaterThan(1)
        chunks.forEach {
            assertThat(it.length).isAtMost(SpeechChunker.SPEAK_CHUNK_LIMIT)
        }
    }


    // ── speakableText ───────────────────────────────────────────────────

    @Test
    fun `plain text passes through`() {
        assertThat(SpeechChunker.speakableText("Just a sentence."))
            .isEqualTo("Just a sentence.")
    }

    @Test
    fun `fenced code collapses to an omission notice`() {
        val md = "Here is code:\n```kotlin\nval x = 1\n```\nDone."
        assertThat(SpeechChunker.speakableText(md))
            .isEqualTo("Here is code:\nCode block omitted.\nDone.")
    }

    @Test
    fun `unterminated fence drops the rest`() {
        val md = "Keep this.\n```\nval x = 1\nnever read"
        assertThat(SpeechChunker.speakableText(md))
            .isEqualTo("Keep this.\nCode block omitted.")
    }

    @Test
    fun `inline formatting unwraps to its text`() {
        assertThat(SpeechChunker.speakableText("**bold** `code` _em_ ~~gone~~"))
            .isEqualTo("bold code em gone")
    }

    @Test
    fun `links keep their label and bare urls drop`() {
        assertThat(SpeechChunker.speakableText("See [the docs](https://example.com) or https://a.b/c"))
            .isEqualTo("See the docs or")
    }

    @Test
    fun `headings quotes and list markers strip`() {
        val md = "## Title\n> a note\n- first\n- second\n3. third"
        assertThat(SpeechChunker.speakableText(md))
            .isEqualTo("Title\na note\nfirst\nsecond\nthird")
    }

    @Test
    fun `four-digit list markers stay literal`() {
        // `\d{1,3}` bound in Lerdr — 1234. is not a list marker.
        assertThat(SpeechChunker.speakableText("1234. not a marker"))
            .isEqualTo("1234. not a marker")
    }

    @Test
    fun `horizontal rules drop`() {
        assertThat(SpeechChunker.speakableText("Above\n---\nBelow"))
            .isEqualTo("Above\nBelow")
    }

    @Test
    fun `tables read as comma separated cells`() {
        val md = "| Name | Size |\n| --- | --- |\n| a | 63 MB |"
        assertThat(SpeechChunker.speakableText(md))
            .isEqualTo("Name, Size\na, 63 MB")
    }

    @Test
    fun `whitespace runs collapse after stripping`() {
        assertThat(SpeechChunker.speakableText("  spaced   out  "))
            .isEqualTo("spaced out")
    }

    @Test
    fun `empty input stays empty`() {
        assertThat(SpeechChunker.speakableText("")).isEmpty()
        assertThat(SpeechChunker.speakableText("   \n  ")).isEmpty()
    }

    @Test
    fun `markdown-only text empties so the caller falls back`() {
        // Only a bare URL: nothing speakable survives.
        assertThat(SpeechChunker.speakableText("https://example.com")).isEmpty()
    }

    // ── language catalog ────────────────────────────────────────────────

    @Test
    fun `only the five offered codes are speech languages`() {
        assertThat(isSpeechLanguage("en")).isTrue()
        assertThat(isSpeechLanguage("ja")).isFalse()
        assertThat(isSpeechLanguage(null)).isFalse()
        assertThat(isSpeechLanguage("")).isFalse()
    }

    @Test
    fun `language label falls back to the raw code`() {
        assertThat(speechLanguageLabel("fr")).isEqualTo("French")
        assertThat(speechLanguageLabel("xx")).isEqualTo("xx")
    }
}
