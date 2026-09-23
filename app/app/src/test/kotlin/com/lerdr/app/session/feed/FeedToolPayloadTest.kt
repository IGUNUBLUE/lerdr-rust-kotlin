package com.lerdr.app.session.feed

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Article
import androidx.compose.material.icons.filled.AccountTree
import androidx.compose.material.icons.filled.Build
import androidx.compose.material.icons.filled.Checklist
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.Language
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Terminal
import com.google.common.truth.Truth.assertThat
import org.junit.Test

/**
 * `ConversationMessage.svelte`'s `formatToolPayload`/`clampPayload` ports —
 * JSON objects render as `key: value` lines; the preview clamp is line-based.
 */
class FeedToolPayloadTest {

    @Test
    fun `blank payload passes through verbatim`() {
        // Non-JSON input is never rewritten — the composable gates on
        // `isNotBlank` before this is ever called.
        assertThat(formatToolPayload("")).isEmpty()
        assertThat(formatToolPayload("   \n ")).isEqualTo("   \n ")
    }

    @Test
    fun `raw text passes through unchanged`() {
        assertThat(formatToolPayload("plain output")).isEqualTo("plain output")
        assertThat(formatToolPayload("  spaced  ")).isEqualTo("  spaced  ")
    }

    @Test
    fun `json object renders key value lines`() {
        assertThat(formatToolPayload("""{"a":1,"b":"two"}"""))
            .isEqualTo("a: 1\nb: two")
    }

    @Test
    fun `structured values pretty-print under their key`() {
        val formatted = formatToolPayload("""{"cmd":"ls","opts":{"all":true}}""")
        assertThat(formatted).startsWith("cmd: ls\nopts:\n")
        assertThat(formatted).contains("\"all\": true")
    }

    @Test
    fun `empty object and non-object json pass through raw`() {
        assertThat(formatToolPayload("{}")).isEqualTo("{}")
        assertThat(formatToolPayload("[1,2]")).isEqualTo("[1,2]")
    }

    @Test
    fun `json-lookalike text that fails to parse stays raw`() {
        assertThat(formatToolPayload("{broken")).isEqualTo("{broken")
    }

    @Test
    fun `clampPayload keeps short payloads whole`() {
        val raw = (1..4).joinToString("\n") { "line $it" }
        val (preview, hidden) = clampPayload(raw)
        assertThat(preview).isEqualTo(raw)
        assertThat(hidden).isEqualTo(0)
    }

    @Test
    fun `clampPayload cuts at the preview line budget`() {
        val raw = (1..20).joinToString("\n") { "line $it" }
        val (preview, hidden) = clampPayload(raw)
        assertThat(preview).isEqualTo(
            (1..TOOL_PAYLOAD_PREVIEW_LINES).joinToString("\n") { "line $it" },
        )
        assertThat(hidden).isEqualTo(20 - TOOL_PAYLOAD_PREVIEW_LINES)
    }

    @Test
    fun `toolIcon maps the tool families`() {
        assertThat(toolIcon("read")).isEqualTo(Icons.AutoMirrored.Filled.Article)
        assertThat(toolIcon("grep")).isEqualTo(Icons.Filled.Search)
        assertThat(toolIcon("bash")).isEqualTo(Icons.Filled.Terminal)
        assertThat(toolIcon("edit")).isEqualTo(Icons.Filled.Edit)
        assertThat(toolIcon("write")).isEqualTo(Icons.Filled.Edit)
        assertThat(toolIcon("task")).isEqualTo(Icons.Filled.Checklist)
        assertThat(toolIcon("webfetch")).isEqualTo(Icons.Filled.Language)
        assertThat(toolIcon("git_status")).isEqualTo(Icons.Filled.AccountTree)
        assertThat(toolIcon("mystery")).isEqualTo(Icons.Filled.Build)
        // Case-insensitive.
        assertThat(toolIcon("Bash")).isEqualTo(Icons.Filled.Terminal)
    }
}
