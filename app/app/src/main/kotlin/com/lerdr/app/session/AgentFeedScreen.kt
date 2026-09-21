package com.lerdr.app.session

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ArrowUpward
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.Terminal
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.PreviewLightDark
import androidx.compose.ui.unit.dp
import com.lerdr.core.designsystem.components.LerdrButtonGroup
import com.lerdr.core.designsystem.components.LerdrButtonGroupItem
import com.lerdr.core.designsystem.components.LerdrLoadingIndicator
import com.lerdr.core.designsystem.theme.LerdrTheme
import com.lerdr.core.designsystem.theme.LerdrTextStyles

/**
 * Feed mode — semantic timeline (docs/04 §Feed). Scaffold fidelity only:
 * static rows stand in for conversation pages; the blocker card and
 * composer render the real components they'll use.
 */
@Composable
fun AgentFeedScreen(
    paneId: String,
    onOpenTerminal: () -> Unit,
    onBack: () -> Unit,
) {
    AgentFeedContent(
        paneId = paneId,
        onOpenTerminal = onOpenTerminal,
        onBack = onBack,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AgentFeedContent(
    paneId: String,
    onOpenTerminal: () -> Unit,
    onBack: () -> Unit,
) {
    val spacing = LerdrTheme.spacing
    Scaffold(
        topBar = {
            SessionTopBar(
                title = paneId.substringAfter("::", paneId),
                breadcrumb = "lerdr · main · sd",
                statusLabel = "working",
                statusColor = LerdrTheme.extendedColors.working,
                mode = SessionMode.FEED,
                onSelectMode = { mode ->
                    if (mode == SessionMode.TERMINAL) onOpenTerminal()
                },
                onBack = onBack,
            )
        },
        bottomBar = { ComposerPlaceholder(agentLabel = paneId.substringAfter("::", paneId)) },
    ) { innerPadding ->
        LazyColumn(
            modifier = Modifier.fillMaxSize(),
            contentPadding = PaddingValues(
                top = innerPadding.calculateTopPadding() + spacing.small,
                bottom = innerPadding.calculateBottomPadding() + spacing.small,
                start = spacing.medium,
                end = spacing.medium,
            ),
            verticalArrangement = Arrangement.spacedBy(spacing.small),
        ) {
            item { UserPromptBubble("migrate the relay transport to rust, keep the wire protocol identical") }
            item {
                Text(
                    "I'll start with the E2EE handshake since it's the compatibility boundary…",
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
            items(previewToolCalls, key = { it.title }) { call ->
                ToolCallCard(call)
            }
            item { WorkingRow() }
            item {
                BlockerCard(
                    prompt = "Run cargo build --release in lerdr-rust/?",
                    onAnswer = {},
                )
            }
        }
    }
}

@Immutable
private data class ToolCallUi(
    val title: String,
    val detail: String,
    val isError: Boolean = false,
)

private val previewToolCalls = listOf(
    ToolCallUi("Edit", "crates/lerdr-e2ee/src/session.rs"),
    ToolCallUi("Bash", "cargo test --package lerdr-e2ee\nrunning 14 tests · test result: ok. 14 passed"),
    ToolCallUi("Read 3 files", "e2ee.go · sendbuffer.go · ws.go"),
)

@Composable
private fun UserPromptBubble(text: String) {
    Surface(
        color = MaterialTheme.colorScheme.secondaryContainer,
        contentColor = MaterialTheme.colorScheme.onSecondaryContainer,
        shape = MaterialTheme.shapes.large,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Text(
            text,
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.padding(
                horizontal = LerdrTheme.spacing.medium,
                vertical = LerdrTheme.spacing.small + LerdrTheme.spacing.extraSmall,
            ),
        )
    }
}

@Composable
private fun ToolCallCard(call: ToolCallUi) {
    val spacing = LerdrTheme.spacing
    Card(
        colors = CardDefaults.cardColors(
            containerColor = if (call.isError) {
                MaterialTheme.colorScheme.errorContainer
            } else {
                MaterialTheme.colorScheme.surfaceContainerLow
            },
        ),
        shape = MaterialTheme.shapes.medium,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.padding(spacing.small + spacing.extraSmall),
        ) {
            Icon(
                if (call.isError) Icons.Default.CheckCircle else Icons.Default.Edit,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.size(20.dp),
            )
            Spacer(Modifier.width(spacing.small))
            Column(Modifier.weight(1f)) {
                Text(call.title, style = MaterialTheme.typography.titleSmall)
                Text(
                    call.detail,
                    style = if (call.detail.contains('\n')) {
                        LerdrTextStyles.code
                    } else {
                        MaterialTheme.typography.bodySmall
                    },
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 3,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            Icon(
                Icons.Default.KeyboardArrowDown,
                contentDescription = "Expand tool call",
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

/** Live "thinking" row — morphing loader + latest tool + elapsed. */
@Composable
private fun WorkingRow() {
    val spacing = LerdrTheme.spacing
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = spacing.extraSmall),
    ) {
        LerdrLoadingIndicator(modifier = Modifier.size(28.dp))
        Spacer(Modifier.width(spacing.small))
        Text(
            "Running fixture vectors",
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.weight(1f),
        )
        Text(
            "live ↗",
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.primary,
        )
    }
}

/** The blocker card — pins above the composer until answered. */
@Composable
private fun BlockerCard(prompt: String, onAnswer: (String) -> Unit) {
    val colors = LerdrTheme.extendedColors
    val spacing = LerdrTheme.spacing
    Card(
        colors = CardDefaults.cardColors(containerColor = colors.attentionContainer),
        shape = MaterialTheme.shapes.large,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(
            modifier = Modifier.padding(spacing.medium),
            verticalArrangement = Arrangement.spacedBy(spacing.small),
        ) {
            Text(
                "⚠ APPROVAL NEEDED",
                style = MaterialTheme.typography.labelMedium,
                color = colors.attention,
            )
            Text(prompt, style = MaterialTheme.typography.titleSmall)
            LerdrButtonGroup(
                items = listOf(
                    LerdrButtonGroupItem(label = "Allow", onClick = { onAnswer("Allow") }),
                    LerdrButtonGroupItem(label = "Always", onClick = { onAnswer("Always") }),
                    LerdrButtonGroupItem(label = "Deny", onClick = { onAnswer("Deny") }),
                ),
                modifier = Modifier.fillMaxWidth(),
            )
        }
    }
}

@Composable
private fun ComposerPlaceholder(agentLabel: String) {
    val spacing = LerdrTheme.spacing
    Surface(color = MaterialTheme.colorScheme.surface) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier
                .fillMaxWidth()
                .padding(spacing.medium),
        ) {
            Surface(
                color = MaterialTheme.colorScheme.surfaceContainerHigh,
                shape = MaterialTheme.shapes.extraLarge,
                modifier = Modifier.weight(1f),
            ) {
                Text(
                    "Message $agentLabel…",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(
                        horizontal = spacing.medium,
                        vertical = spacing.small + spacing.extraSmall,
                    ),
                )
            }
            Spacer(Modifier.width(spacing.small))
            Surface(
                color = MaterialTheme.colorScheme.primary,
                contentColor = MaterialTheme.colorScheme.onPrimary,
                shape = MaterialTheme.shapes.extraLarge,
            ) {
                Icon(
                    Icons.Default.ArrowUpward,
                    contentDescription = "Send",
                    modifier = Modifier.padding(10.dp),
                )
            }
        }
    }
}

@PreviewLightDark
@Composable
private fun AgentFeedContentPreview() {
    LerdrTheme {
        AgentFeedContent(
            paneId = "sd::%1",
            onOpenTerminal = {},
            onBack = {},
        )
    }
}
