package com.lerdr.app.speech

import androidx.compose.runtime.Immutable
import java.util.regex.Pattern

/**
 * One read-aloud language — the oracle's `SPEECH_LANGUAGES` entries. A
 * relay synthesizes these on the computer; the audio is streamed back and
 * played as ordinary media.
 */
@Immutable
data class SpeechLanguage(val code: String, val label: String)

/**
 * `SPEECH_LANGUAGES` — the five languages the relay can read aloud, in the
 * order the settings dropdown lists them (`frontend/src/lib/speech.ts`).
 */
val SPEECH_LANGUAGES: List<SpeechLanguage> = listOf(
    SpeechLanguage("en", "English"),
    SpeechLanguage("fr", "French"),
    SpeechLanguage("de", "German"),
    SpeechLanguage("es", "Spanish"),
    SpeechLanguage("zh", "Chinese"),
)

/** `isSpeechLanguage` — only the five offered codes are valid selections. */
fun isSpeechLanguage(code: String?): Boolean =
    SPEECH_LANGUAGES.any { it.code == code }

/** `speechLanguageLabel` — the display name, falling back to the raw code. */
fun speechLanguageLabel(code: String): String =
    SPEECH_LANGUAGES.firstOrNull { it.code == code }?.label ?: code

/**
 * Text preparation for relay speech — ports of the oracle's
 * `speakableText` (`frontend/src/lib/markdown.ts`) and `speechChunks`
 * (`frontend/src/lib/speech.ts`).
 *
 * `speakableText` reduces markdown to prose worth hearing: a speech engine
 * reads formatting characters out loud — backticks became "backtick" on a
 * real phone — and a fenced code block spoken character by character is
 * noise, so structure is dropped and only human-directed text survives.
 *
 * `speechChunks` fragments the result so each relay round trip stays short:
 * reading starts almost immediately and one lost fragment costs a sentence
 * rather than the whole response. Chinese sentences end without a space,
 * so their punctuation splits too.
 */
object SpeechChunker {

    /** The 1500-char default the oracle exposes for long-form callers. */
    const val DEFAULT_CHUNK_LIMIT = 1500

    /** `speakViaRelay`'s 240-char fragment budget per `speak_text`. */
    const val SPEAK_CHUNK_LIMIT = 240

    /** `text.split(/(?<=[.!?:;\n])\s+|(?<=[。！？；：])/u)` — after western
     * punctuation followed by whitespace, or right after CJK punctuation.
     * `UNICODE_CHARACTER_CLASS` gives `\s` the oracle's `/u` coverage. */
    private val SENTENCE_SPLIT = Pattern.compile(
        """(?<=[.!?:;\n])\s+|(?<=[。！？；：])""",
        Pattern.UNICODE_CHARACTER_CLASS,
    ).toRegex()

    /**
     * `speechChunks(text, limit)` — greedy sentence packing into `limit`-char
     * fragments joined on a single space; an over-long piece cuts at the last
     * space inside the limit, else hard-cuts at the limit. Lengths count
     * UTF-16 units exactly like the oracle's `String.length`.
     */
    fun speechChunks(text: String, limit: Int = DEFAULT_CHUNK_LIMIT): List<String> {
        val chunks = mutableListOf<String>()
        var current = ""
        for (sentence in text.split(SENTENCE_SPLIT)) {
            var piece = sentence
            while (true) {
                if (current.isNotEmpty() && current.length + piece.length + 1 > limit) {
                    chunks += current
                    current = ""
                }
                if (piece.length <= limit) {
                    current = if (current.isNotEmpty()) "$current $piece" else piece
                    break
                }
                val cut = piece.lastIndexOf(' ', limit)
                if (cut > 0) {
                    chunks += piece.substring(0, cut)
                    piece = piece.substring(cut + 1)
                } else {
                    chunks += piece.substring(0, limit)
                    piece = piece.substring(limit)
                }
            }
        }
        if (current.isNotEmpty()) chunks += current
        return chunks
    }

    private val LINE_BREAKS = Regex("""\r\n?""")
    private val FENCE = Regex("""^\s*```""")
    private val HORIZONTAL_RULE = Regex("""^\s*(?:---+|___+|\*\*\*+)\s*$""")
    private val TABLE_RULE_ROW = Regex("""^\s*\|?[\s:|-]+\|?\s*$""")
    private val HEADING = Regex("""^\s*#{1,6}\s+""")
    private val QUOTE = Regex("""^\s*>\s?""")
    private val LIST_MARKER = Regex("""^\s*(?:[-*+]|\d{1,3}[.)])\s+""")
    private val LINK = Regex("""\[([^\]]+)\]\(([^)]*)\)""")
    private val BARE_URL = Regex("""https?://\S+""")
    private val INLINE_CODE = Regex("""`([^`]*)`""")
    private val STRONG = Regex("""(\*\*|__|~~)(.+?)\1""")
    private val EMPHASIS = Regex("""(^|\s)[*_](\S(?:[^*_]*\S)?)[*_](?=\s|$|[.,;:!?])""")
    private val CELL_SEPARATOR = Regex("""\s*\|\s*""")
    private val EDGE_TRIM = Regex("""^[,\s]+|[,\s]+$""")
    private val SPACE_RUN = Regex("""[ \t]+""")

    /**
     * `speakableText` — line-oriented markdown strip:
     * - fenced code blocks collapse to a single "Code block omitted." line;
     * - horizontal rules and table alignment rows drop entirely;
     * - heading/quote/list markers, links, URLs, inline code and emphasis
     *   unwrap to their inner text; table cell pipes read as ", ".
     */
    fun speakableText(value: String): String {
        val lines = mutableListOf<String>()
        var inCode = false
        for (line in value.replace(LINE_BREAKS, "\n").split('\n')) {
            if (FENCE.containsMatchIn(line)) {
                if (!inCode) lines += "Code block omitted."
                inCode = !inCode
                continue
            }
            if (inCode) continue
            if (HORIZONTAL_RULE.matches(line)) continue
            // A table's alignment row is pure formatting; content rows read
            // as comma-separated cells.
            if (TABLE_RULE_ROW.matches(line) && line.contains('-') && line.contains('|')) continue
            lines += line
                .replace(HEADING, "")
                .replace(QUOTE, "")
                .replace(LIST_MARKER, "")
                .replace(LINK, "$1")
                .replace(BARE_URL, "")
                .replace(INLINE_CODE, "$1")
                .replace(STRONG, "$2")
                .replace(EMPHASIS, "$1$2")
                .replace(CELL_SEPARATOR, ", ")
                .replace(EDGE_TRIM, "")
        }
        return lines.filter { it.isNotEmpty() }
            .joinToString("\n")
            .replace(SPACE_RUN, " ")
            .trim()
    }
}
