package com.lerdr.app.session.feed

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.font.FontWeight
import com.google.common.truth.Truth.assertThat
import org.junit.Test

/**
 * `safeMarkdownHtml` subset coverage — block parsing, fenced-code
 * extraction, and the inline emphasis/link pass.
 */
class FeedMarkdownTest {

    private val styles = InlineStyles(
        codeBackground = Color.Black,
        link = Color.Blue,
        highlightBackground = Color.Yellow,
        highlightColor = Color.Black,
        baseColor = Color.Unspecified,
    )

    @Test
    fun `headings paragraphs and rules split into blocks`() {
        val blocks = parseFeedMarkdown(
            "# Title\n\nSome prose\nwith a second line\n\n---\n\nAfter the rule",
        )
        assertThat(blocks).containsExactly(
            FeedBlock.Heading(1, "Title"),
            FeedBlock.Paragraph("Some prose\nwith a second line"),
            FeedBlock.Rule,
            FeedBlock.Paragraph("After the rule"),
        ).inOrder()
    }

    @Test
    fun `fenced code keeps its language and swallows inner markdown`() {
        val blocks = parseFeedMarkdown(
            "Intro\n```kotlin\nval **notBold** = 1\n```\n\nOutro",
        )
        assertThat(blocks).containsExactly(
            FeedBlock.Paragraph("Intro"),
            FeedBlock.CodeBlock("kotlin", "val **notBold** = 1"),
            FeedBlock.Paragraph("Outro"),
        ).inOrder()
    }

    @Test
    fun `an unterminated fence still yields its code block`() {
        val blocks = parseFeedMarkdown("Before\n```\nunclosed")
        assertThat(blocks).containsExactly(
            FeedBlock.Paragraph("Before"),
            FeedBlock.CodeBlock("", "unclosed"),
        ).inOrder()
    }

    @Test
    fun `bullet and numbered lists group consecutive items`() {
        val blocks = parseFeedMarkdown("- a\n- b\n\n3. three\n4. four\n\ntail")
        assertThat(blocks).containsExactly(
            FeedBlock.BulletList(listOf("a", "b")),
            FeedBlock.NumberedList(
                listOf(NumberedItem(3, "three"), NumberedItem(4, "four")),
            ),
            FeedBlock.Paragraph("tail"),
        ).inOrder()
    }

    @Test
    fun `consecutive quotes merge into one block`() {
        val blocks = parseFeedMarkdown("> one\n> two\n\nplain")
        assertThat(blocks).containsExactly(
            FeedBlock.Quote("one\ntwo"),
            FeedBlock.Paragraph("plain"),
        ).inOrder()
    }

    @Test
    fun `fencedCodeText joins blocks and reports null when absent`() {
        assertThat(
            fencedCodeText("```\nfirst\n```\nmiddle\n```sh\nsecond\n```"),
        ).isEqualTo("first\n\nsecond")
        assertThat(fencedCodeText("no code here")).isNull()
    }

    @Test
    fun `inline bold italic code and strike annotate their spans`() {
        val annotated = inlineAnnotated("a **b** and `c` and *d* and ~~e~~", styles)
        // Markers are consumed — the visible text keeps only their content.
        assertThat(annotated.text).isEqualTo("a b and c and d and e")
        val bold = annotated.spanStyles.single {
            it.item.fontWeight == FontWeight.Bold
        }
        assertThat(annotated.text.substring(bold.start, bold.end)).isEqualTo("b")
    }

    @Test
    fun `inline code span carries the code background`() {
        val annotated = inlineAnnotated("run `gradle test` now", styles)
        val code = annotated.spanStyles.single { it.item.background == Color.Black }
        assertThat(annotated.text.substring(code.start, code.end)).isEqualTo("gradle test")
    }

    @Test
    fun `markdown links and bare urls produce url annotations`() {
        val linked = inlineAnnotated("[docs](https://example.com/x)", styles)
        val link = linked.getLinkAnnotations(0, linked.length)
            .map { it.item }
            .filterIsInstance<LinkAnnotation.Url>()
            .single()
        assertThat(link.url).isEqualTo("https://example.com/x")

        val bare = inlineAnnotated("see https://example.com/a).", styles)
        val bareLink = bare.getLinkAnnotations(0, bare.length)
            .map { it.item }
            .filterIsInstance<LinkAnnotation.Url>()
            .single()
        // `trimUrlPunctuation` — the trailing `).` stays out of the link.
        assertThat(bareLink.url).isEqualTo("https://example.com/a")
    }

    @Test
    fun `highlight paints case-insensitive literal matches`() {
        val annotated = inlineAnnotated("Alpha ALPHA beta", styles, highlight = "alpha")
        val marks = annotated.spanStyles.filter {
            it.item.background == Color.Yellow
        }
        assertThat(marks).hasSize(2)
    }
}
