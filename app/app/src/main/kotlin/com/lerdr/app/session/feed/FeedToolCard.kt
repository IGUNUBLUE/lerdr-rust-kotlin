package com.lerdr.app.session.feed

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Article
import androidx.compose.material.icons.filled.AccountTree
import androidx.compose.material.icons.filled.Build
import androidx.compose.material.icons.filled.Checklist
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.Error
import androidx.compose.material.icons.filled.ExpandLess
import androidx.compose.material.icons.filled.ExpandMore
import androidx.compose.material.icons.filled.Language
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Terminal
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.unit.dp
import com.lerdr.core.designsystem.theme.LerdrTextStyles
import com.lerdr.core.designsystem.theme.LerdrTheme
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import lerdr.core.conversation.ConversationTool

/**
 * Tool-call card — the interactive port of the oracle's
 * `ConversationMessage.svelte` `<details>` blocks. Collapsed: icon tile +
 * tool name + status. Expanded: `Input`/`Output` payload sections with the
 * oracle's `formatToolPayload`/`clampPayload` rules. Failures get the
 * danger container + error icon, never a checkmark.
 */

/** Oracle `PAYLOAD_PREVIEW_LINES` — expanded payloads clamp past this. */
internal const val TOOL_PAYLOAD_PREVIEW_LINES = 8

private val prettyJson = Json { prettyPrint = true }

/**
 * Oracle `formatToolPayload` (frontend/src/lib/conversation.ts): a JSON
 * object renders as `key: value` lines — primitives inline, structured
 * values pretty-printed on the following line(s). Non-JSON and empty
 * objects pass through verbatim.
 */
internal fun formatToolPayload(raw: String): String {
    val trimmed = raw.trim()
    if (!trimmed.startsWith("{")) return raw
    val parsed = try {
        Json.parseToJsonElement(trimmed)
    } catch (_: Exception) {
        return raw
    }
    val obj = parsed as? JsonObject ?: return raw
    if (obj.isEmpty()) return raw
    return obj.entries.joinToString("\n") { (key, value) ->
        val rendered = when (value) {
            is JsonPrimitive -> value.content
            else -> prettyJson.encodeToString(
                kotlinx.serialization.json.JsonElement.serializer(),
                value,
            )
        }
        if (rendered.contains('\n')) "$key:\n$rendered" else "$key: $rendered"
    }
}

/** Oracle `clampPayload` — line-based preview, returns (visible, hidden). */
internal fun clampPayload(
    raw: String,
    previewLines: Int = TOOL_PAYLOAD_PREVIEW_LINES,
): Pair<String, Int> {
    val lines = raw.split('\n')
    if (lines.size <= previewLines) return raw to 0
    return lines.take(previewLines).joinToString("\n") to (lines.size - previewLines)
}

/** Per-tool-family icon — the "tool icon tile" in the feed mockup. */
internal fun toolIcon(name: String): ImageVector {
    val n = name.lowercase()
    return when {
        n.contains("bash") || n.contains("shell") || n.contains("terminal") ||
            n.contains("powershell") -> Icons.Filled.Terminal
        n.contains("edit") || n.contains("write") || n.contains("notebook") ||
            n.contains("patch") || n.contains("apply") -> Icons.Filled.Edit
        n.contains("read") || n.contains("view") || n.contains("open") ||
            n.contains("cat") -> Icons.AutoMirrored.Filled.Article
        n.contains("grep") || n.contains("glob") || n.contains("search") ||
            n.contains("find") || n.contains("list") || n == "ls" -> Icons.Filled.Search
        n.contains("web") || n.contains("fetch") || n.contains("browse") ||
            n.contains("http") -> Icons.Filled.Language
        n.contains("task") || n.contains("agent") || n.contains("todo") ->
            Icons.Filled.Checklist
        n.contains("git") -> Icons.Filled.AccountTree
        else -> Icons.Filled.Build
    }
}

/**
 * Collapsed/expanded tool row. Stable identity comes from the caller's
 * lazy-list key ([ConversationTool.id]).
 */
@Composable
internal fun FeedToolCard(
    tool: ConversationTool,
    modifier: Modifier = Modifier,
) {
    val spacing = LerdrTheme.spacing
    val colors = LerdrTheme.extendedColors
    var expanded by rememberSaveable(tool.id) { mutableStateOf(false) }
    val hasError = tool.error
    Card(
        onClick = { expanded = !expanded },
        colors = CardDefaults.cardColors(
            containerColor = if (hasError) {
                colors.dangerContainer
            } else {
                MaterialTheme.colorScheme.surfaceContainerLow
            },
            contentColor = if (hasError) {
                colors.onDangerContainer
            } else {
                MaterialTheme.colorScheme.onSurface
            },
        ),
        modifier = modifier.fillMaxWidth(),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = spacing.small, vertical = spacing.extraSmall),
        ) {
            Surface(
                color = if (hasError) {
                    colors.danger
                } else {
                    MaterialTheme.colorScheme.surfaceContainerHighest
                },
                contentColor = if (hasError) {
                    colors.onDanger
                } else {
                    MaterialTheme.colorScheme.onSurfaceVariant
                },
                shape = MaterialTheme.shapes.small,
            ) {
                Icon(
                    if (hasError) Icons.Filled.Error else toolIcon(tool.name),
                    contentDescription = null,
                    modifier = Modifier
                        .padding(6.dp)
                        .size(18.dp),
                )
            }
            Column(
                modifier = Modifier
                    .weight(1f)
                    .padding(horizontal = spacing.small),
            ) {
                Text(tool.name, style = MaterialTheme.typography.titleSmall)
                Text(
                    when {
                        hasError -> "failed"
                        tool.output.isNotEmpty() -> "completed"
                        else -> "called"
                    },
                    style = MaterialTheme.typography.labelSmall,
                    color = if (hasError) {
                        colors.danger
                    } else {
                        MaterialTheme.colorScheme.onSurfaceVariant
                    },
                )
            }
            Icon(
                if (expanded) Icons.Filled.ExpandLess else Icons.Filled.ExpandMore,
                contentDescription = if (expanded) "Collapse" else "Expand",
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        AnimatedVisibility(expanded) {
            Column(
                verticalArrangement = Arrangement.spacedBy(spacing.extraSmall),
                modifier = Modifier.padding(
                    start = spacing.small,
                    end = spacing.small,
                    bottom = spacing.small,
                ),
            ) {
                if (tool.input.isNotBlank()) {
                    ToolPayloadSection(label = "Input", raw = tool.input)
                }
                if (tool.output.isNotBlank()) {
                    ToolPayloadSection(label = "Output", raw = tool.output)
                } else {
                    Text(
                        "No output was captured in this session log.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                if (tool.truncated) {
                    Text(
                        "Tool detail was truncated by the relay.",
                        style = MaterialTheme.typography.labelSmall,
                        color = colors.attention,
                    )
                }
            }
        }
    }
}

/** `key: value` payload with the oracle's 8-line clamp + Show-all toggle. */
@Composable
private fun ToolPayloadSection(label: String, raw: String) {
    val spacing = LerdrTheme.spacing
    val colors = LerdrTheme.extendedColors
    val formatted = formatToolPayload(raw)
    val (preview, hidden) = clampPayload(formatted)
    var showAll by rememberSaveable(label, raw) { mutableStateOf(false) }
    Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
        Text(
            label,
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Surface(
            color = colors.terminalSurface,
            contentColor = colors.terminalText,
            shape = MaterialTheme.shapes.small,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text(
                if (showAll) formatted else preview,
                style = LerdrTextStyles.code,
                modifier = Modifier.padding(spacing.small),
            )
        }
        if (hidden > 0) {
            TextButton(onClick = { showAll = !showAll }) {
                Text(
                    if (showAll) "Show less" else "Show all ${hidden + TOOL_PAYLOAD_PREVIEW_LINES} lines",
                    style = MaterialTheme.typography.labelSmall,
                )
            }
        }
    }
}
