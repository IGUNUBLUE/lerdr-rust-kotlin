package com.lerdr.app.session.feed

import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.ClipEntry
import androidx.compose.ui.platform.LocalClipboard
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.withLink
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import com.lerdr.core.designsystem.theme.LerdrTextStyles
import com.lerdr.core.designsystem.theme.LerdrTheme
import kotlinx.coroutines.launch

/**
 * Lightweight markdown for assistant feed entries — a deliberate subset of
 * the oracle's `safeMarkdownHtml` (frontend/src/lib/markdown.ts): headings,
 * paragraphs, fenced code, bullet/ordered lists, blockquotes, rules, plus
 * inline `**bold**`, `*em*`/`_em_`, `~~strike~~`, `` `code` `` and
 * `[label](url)`/bare-URL links.
 *
 * Tables are NOT rendered (the oracle emits `<table>` markup); a table's
 * source lines fall through to plain paragraphs so nothing is lost.
 *
 * Parsing is cached module-wide on the raw text ([feedMarkdownBlocks]) so a
 * streaming tail entry does not reparse every recomposition; search
 * highlighting is applied at render time and never invalidates the cache.
 */

/** Block-level units of a parsed markdown document. */
@Immutable
internal sealed interface FeedBlock {
    /** `#{1,4}` heading — [level] 1-based like the source marker count. */
    data class Heading(val level: Int, val text: String) : FeedBlock

    /** Soft-wrapped prose; embedded `\n` keeps the oracle's `<br>` joins. */
    data class Paragraph(val text: String) : FeedBlock

    /** ```` ```lang ```` fenced block — [language] may be empty. */
    data class CodeBlock(val language: String, val code: String) : FeedBlock

    /** `-`/`*`/`+` items. */
    data class BulletList(val items: List<String>) : FeedBlock

    /** `n.`/`n)` items, keeping the source numbers (start index matters). */
    data class NumberedList(val items: List<NumberedItem>) : FeedBlock

    /** `> ` lines; consecutive quotes merge into one block. */
    data class Quote(val text: String) : FeedBlock

    /** `---`/`___`/`***`. */
    data object Rule : FeedBlock
}

/** One `n.` list row — [number] is the source ordinal, not the position. */
@Immutable
internal data class NumberedItem(val number: Int, val text: String)

private val FENCE = Regex("^\\s*```\\s*([A-Za-z0-9_+.\\-]*)\\s*$")
private val HEADING = Regex("^\\s{0,3}(#{1,4})\\s+(.+)$")
private val BULLET = Regex("^\\s*[-*+]\\s+(.+)$")
private val NUMBERED = Regex("^\\s*(\\d+)[.)]\\s+(.+)$")
private val QUOTE = Regex("^\\s*>\\s?(.*)$")
private val RULE = Regex("^\\s*(?:---+|___+|\\*\\*\\*+)\\s*$")
private val LINK = Regex("^\\[([^\\]\\n]+)]\\((https?://[^\\s)]+)\\)")
private val BARE_URL = Regex("^https?://[^\\s<>\"']+")

/** The oracle's line loop, reduced to a block list. */
internal fun parseFeedMarkdown(markdown: String): List<FeedBlock> {
    val lines = markdown.replace(Regex("\r\n?"), "\n").split('\n')
    val blocks = ArrayList<FeedBlock>()
    var inCode = false
    var codeLanguage = ""
    val codeLines = ArrayList<String>()
    val paragraph = ArrayList<String>()
    val quote = ArrayList<String>()
    var bullets: MutableList<String>? = null
    var numbered: MutableList<NumberedItem>? = null

    fun flushParagraph() {
        if (paragraph.isNotEmpty()) {
            blocks += FeedBlock.Paragraph(paragraph.joinToString("\n"))
            paragraph.clear()
        }
    }

    fun flushLists() {
        bullets?.let { blocks += FeedBlock.BulletList(it.toList()) }
        bullets = null
        numbered?.let { blocks += FeedBlock.NumberedList(it.toList()) }
        numbered = null
    }

    fun flushQuote() {
        if (quote.isNotEmpty()) {
            blocks += FeedBlock.Quote(quote.joinToString("\n"))
            quote.clear()
        }
    }

    fun flushCode() {
        blocks += FeedBlock.CodeBlock(codeLanguage, codeLines.joinToString("\n"))
        inCode = false
        codeLanguage = ""
        codeLines.clear()
    }

    for (line in lines) {
        val fence = FENCE.find(line)
        if (fence != null) {
            if (inCode) {
                flushCode()
            } else {
                flushParagraph()
                flushLists()
                flushQuote()
                inCode = true
                codeLanguage = fence.groupValues[1]
            }
            continue
        }
        if (inCode) {
            codeLines += line
            continue
        }
        if (line.isBlank()) {
            flushParagraph()
            flushLists()
            flushQuote()
            continue
        }
        val heading = HEADING.find(line)
        if (heading != null) {
            flushParagraph()
            flushLists()
            flushQuote()
            blocks += FeedBlock.Heading(heading.groupValues[1].length, heading.groupValues[2])
            continue
        }
        val bullet = BULLET.find(line)
        if (bullet != null) {
            flushParagraph()
            flushQuote()
            numbered?.let { blocks += FeedBlock.NumberedList(it.toList()) }
            numbered = null
            val list = bullets ?: ArrayList<String>().also { bullets = it }
            list += bullet.groupValues[1]
            continue
        }
        val ordered = NUMBERED.find(line)
        if (ordered != null) {
            flushParagraph()
            flushQuote()
            bullets?.let { blocks += FeedBlock.BulletList(it.toList()) }
            bullets = null
            val list = numbered ?: ArrayList<NumberedItem>().also { numbered = it }
            list += NumberedItem(
                ordered.groupValues[1].toIntOrNull() ?: (list.size + 1),
                ordered.groupValues[2],
            )
            continue
        }
        val quoted = QUOTE.find(line)
        if (quoted != null) {
            flushParagraph()
            flushLists()
            quote += quoted.groupValues[1]
            continue
        }
        if (RULE.matches(line)) {
            flushParagraph()
            flushLists()
            flushQuote()
            blocks += FeedBlock.Rule
            continue
        }
        flushLists()
        flushQuote()
        paragraph += line
    }
    flushParagraph()
    flushLists()
    flushQuote()
    if (inCode) flushCode()
    return blocks
}

/**
 * Parse cache keyed on the raw text — conversation ids are opaque, so the
 * text itself is the key like the oracle's per-message `$derived`. Bounded
 * so a long history session cannot grow it without limit.
 */
private const val MARKDOWN_CACHE_LIMIT = 128
private val markdownCache =
    object : LinkedHashMap<String, List<FeedBlock>>(MARKDOWN_CACHE_LIMIT, 0.75f, true) {
        override fun removeEldestEntry(
            eldest: MutableMap.MutableEntry<String, List<FeedBlock>>?,
        ): Boolean = size > MARKDOWN_CACHE_LIMIT
    }

internal fun feedMarkdownBlocks(markdown: String): List<FeedBlock> =
    synchronized(markdownCache) {
        markdownCache.getOrPut(markdown) { parseFeedMarkdown(markdown) }
    }

/**
 * `fencedCodeText` — every fenced block's content joined by a blank line;
 * the oracle's "Copy code" affordance reads exactly this. Null when the
 * text carries no fenced code.
 */
internal fun fencedCodeText(markdown: String): String? {
    val code = feedMarkdownBlocks(markdown)
        .filterIsInstance<FeedBlock.CodeBlock>()
        .map { it.code }
        .filter { it.isNotBlank() }
    return code.takeIf { it.isNotEmpty() }?.joinToString("\n\n")
}

/** The oracle's `trimUrlPunctuation` — trailing `.,;:!?)]` stay outside the link. */
private fun trimUrlPunctuation(value: String): Pair<String, String> {
    var url = value
    var suffix = ""
    while (url.isNotEmpty()) {
        val last = url.last()
        if (last !in ".,;:!?)]") break
        if (last == ')' && url.count { it == '(' } >= url.count { it == ')' }) break
        if (last == ']' && url.count { it == '[' } >= url.count { it == ']' }) break
        suffix = last + suffix
        url = url.dropLast(1)
    }
    return url to suffix
}

/** Case-insensitive literal scan — the `<mark>` pass in the oracle. */
private fun appendHighlighted(
    builder: AnnotatedString.Builder,
    text: String,
    needle: String,
    style: SpanStyle,
) {
    if (needle.isEmpty() || text.isEmpty()) {
        builder.append(text)
        return
    }
    var cursor = 0
    while (cursor < text.length) {
        val index = text.indexOf(needle, cursor, ignoreCase = true)
        if (index < 0) {
            builder.append(text.substring(cursor))
            return
        }
        builder.append(text.substring(cursor, index))
        builder.withStyle(style) {
            append(text.substring(index, index + needle.length))
        }
        cursor = index + needle.length
    }
}

/** Style bag the composable resolves once — keeps the parser color-free. */
@Immutable
internal class InlineStyles(
    val codeBackground: Color,
    val link: Color,
    val highlightBackground: Color,
    val highlightColor: Color,
    val baseColor: Color,
)

/**
 * Inline renderer — emits styled spans into [this]. Link spans carry a real
 * [LinkAnnotation.Url] so taps open the browser like the oracle's `<a>`.
 */
private fun AnnotatedString.Builder.appendInline(
    text: String,
    styles: InlineStyles,
    highlight: String,
) {
    val codeStyle = SpanStyle(
        fontFamily = FontFamily.Monospace,
        background = styles.codeBackground,
    )
    val highlightStyle = SpanStyle(
        background = styles.highlightBackground,
        color = styles.highlightColor,
    )
    if (styles.baseColor != Color.Unspecified) {
        pushStyle(SpanStyle(color = styles.baseColor))
    }

    fun plain(value: String) = appendHighlighted(this, value, highlight, highlightStyle)

    var index = 0
    while (index < text.length) {
        // `code` spans end at the next backtick on the same line.
        if (text[index] == '`') {
            val end = text.indexOf('`', index + 1)
            if (end > index + 1) {
                withStyle(codeStyle) { plain(text.substring(index + 1, end)) }
                index = end + 1
                continue
            }
        }
        if (text.startsWith("**", index) || text.startsWith("__", index)) {
            val marker = text.substring(index, index + 2)
            val end = text.indexOf(marker, index + 2)
            if (end > index + 2) {
                withStyle(SpanStyle(fontWeight = FontWeight.Bold)) {
                    appendInline(text.substring(index + 2, end), styles, highlight)
                }
                index = end + 2
                continue
            }
        }
        if (text.startsWith("~~", index)) {
            val end = text.indexOf("~~", index + 2)
            if (end > index + 2) {
                withStyle(SpanStyle(textDecoration = TextDecoration.LineThrough)) {
                    appendInline(text.substring(index + 2, end), styles, highlight)
                }
                index = end + 2
                continue
            }
        }
        val emph = text[index]
        if (emph == '*' || emph == '_') {
            val beforeOk = index == 0 || text[index - 1].isWhitespace() ||
                text[index - 1] in "([{\"'"
            val end = if (beforeOk) text.indexOf(emph, index + 1) else -1
            if (beforeOk && end > index + 1 &&
                !text[index + 1].isWhitespace() && !text[end - 1].isWhitespace()
            ) {
                withStyle(SpanStyle(fontStyle = FontStyle.Italic)) {
                    appendInline(text.substring(index + 1, end), styles, highlight)
                }
                index = end + 1
                continue
            }
        }
        if (text[index] == '[') {
            val link = LINK.find(text.substring(index))
            if (link != null) {
                val label = link.groupValues[1]
                val url = link.groupValues[2]
                withLink(LinkAnnotation.Url(url)) {
                    withStyle(
                        SpanStyle(
                            color = styles.link,
                            textDecoration = TextDecoration.Underline,
                        ),
                    ) { plain(label) }
                }
                index += link.value.length
                continue
            }
        }
        if (text.startsWith("http://", index) || text.startsWith("https://", index)) {
            val url = BARE_URL.find(text.substring(index))
            if (url != null) {
                val (candidate, suffix) = trimUrlPunctuation(url.value)
                withLink(LinkAnnotation.Url(candidate)) {
                    withStyle(
                        SpanStyle(
                            color = styles.link,
                            textDecoration = TextDecoration.Underline,
                        ),
                    ) { plain(candidate) }
                }
                plain(suffix)
                index += url.value.length
                continue
            }
        }
        // Plain run — copy up to the next possible marker.
        var next = index + 1
        while (next < text.length &&
            text[next] !in "`*_~[" &&
            !text.startsWith("http://", next) && !text.startsWith("https://", next)
        ) {
            next++
        }
        plain(text.substring(index, next))
        index = next
    }
    if (styles.baseColor != Color.Unspecified) {
        pop()
    }
}

/** Inline text as an [AnnotatedString] — markdown emphasis + find highlight. */
internal fun inlineAnnotated(
    text: String,
    styles: InlineStyles,
    highlight: String = "",
): AnnotatedString = buildAnnotatedString { appendInline(text, styles, highlight) }

@Composable
private fun rememberInlineStyles(baseColor: Color): InlineStyles {
    val scheme = MaterialTheme.colorScheme
    val extended = LerdrTheme.extendedColors
    return remember(scheme, extended, baseColor) {
        InlineStyles(
            codeBackground = scheme.surfaceContainerHighest,
            link = scheme.primary,
            highlightBackground = extended.attentionContainer,
            highlightColor = extended.onAttentionContainer,
            baseColor = baseColor,
        )
    }
}

/**
 * Rendered markdown body — one [Column] of blocks. [highlight] is the
 * conversation-find needle; matches get the attention `mark` styling.
 */
@Composable
internal fun FeedMarkdown(
    markdown: String,
    modifier: Modifier = Modifier,
    highlight: String = "",
    baseColor: Color = Color.Unspecified,
) {
    val blocks = remember(markdown) { feedMarkdownBlocks(markdown) }
    val styles = rememberInlineStyles(baseColor)
    Column(
        modifier = modifier,
        verticalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.extraSmall),
    ) {
        blocks.forEach { block ->
            when (block) {
                is FeedBlock.Heading -> Text(
                    inlineAnnotated(block.text, styles, highlight),
                    style = if (block.level <= 2) {
                        MaterialTheme.typography.titleMedium
                    } else {
                        MaterialTheme.typography.titleSmall
                    },
                )
                is FeedBlock.Paragraph -> Text(
                    inlineAnnotated(block.text, styles, highlight),
                    style = MaterialTheme.typography.bodyMedium,
                )
                is FeedBlock.BulletList -> Column {
                    block.items.forEach { item ->
                        Row {
                            Text(
                                "•",
                                style = MaterialTheme.typography.bodyMedium,
                                modifier = Modifier.padding(end = LerdrTheme.spacing.small),
                            )
                            Text(
                                inlineAnnotated(item, styles, highlight),
                                style = MaterialTheme.typography.bodyMedium,
                                modifier = Modifier.weight(1f),
                            )
                        }
                    }
                }
                is FeedBlock.NumberedList -> Column {
                    block.items.forEach { item ->
                        Row {
                            Text(
                                "${item.number}.",
                                style = MaterialTheme.typography.bodyMedium,
                                modifier = Modifier.padding(end = LerdrTheme.spacing.small),
                            )
                            Text(
                                inlineAnnotated(item.text, styles, highlight),
                                style = MaterialTheme.typography.bodyMedium,
                                modifier = Modifier.weight(1f),
                            )
                        }
                    }
                }
                is FeedBlock.Quote -> Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .height(IntrinsicSize.Min),
                ) {
                    Box(
                        modifier = Modifier
                            .width(3.dp)
                            .fillMaxHeight()
                            .background(MaterialTheme.colorScheme.outlineVariant),
                    )
                    Text(
                        inlineAnnotated(block.text, styles, highlight),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(start = LerdrTheme.spacing.small),
                    )
                }
                is FeedBlock.CodeBlock -> FeedCodeBlock(
                    block = block,
                    styles = styles,
                    highlight = highlight,
                )
                FeedBlock.Rule -> HorizontalDivider(
                    color = MaterialTheme.colorScheme.outlineVariant,
                )
            }
        }
    }
}

/** Fenced block — terminal surface, monospace, header row with copy. */
@Composable
private fun FeedCodeBlock(
    block: FeedBlock.CodeBlock,
    styles: InlineStyles,
    highlight: String,
) {
    val spacing = LerdrTheme.spacing
    val colors = LerdrTheme.extendedColors
    val clipboard = LocalClipboard.current
    val scope = rememberCoroutineScope()
    Surface(
        color = colors.terminalSurface,
        contentColor = colors.terminalText,
        shape = MaterialTheme.shapes.medium,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(start = spacing.small),
            ) {
                Text(
                    block.language.ifEmpty { "code" },
                    style = MaterialTheme.typography.labelSmall,
                    color = colors.terminalAccent,
                    modifier = Modifier.weight(1f),
                )
                IconButton(onClick = {
                    scope.launch {
                        clipboard.setClipEntry(
                            ClipEntry(
                                android.content.ClipData.newPlainText("code", block.code),
                            ),
                        )
                    }
                }) {
                    Icon(
                        Icons.Default.ContentCopy,
                        contentDescription = "Copy code",
                        tint = colors.terminalAccent,
                    )
                }
            }
            SelectionContainer {
                Text(
                    inlineAnnotated(block.code, styles, highlight),
                    style = LerdrTextStyles.code,
                    modifier = Modifier
                        .fillMaxWidth()
                        .horizontalScroll(rememberScrollState())
                        .padding(
                            start = spacing.small,
                            end = spacing.small,
                            bottom = spacing.small,
                        ),
                )
            }
        }
    }
}
