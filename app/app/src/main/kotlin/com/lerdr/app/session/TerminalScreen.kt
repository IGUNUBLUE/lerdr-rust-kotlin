package com.lerdr.app.session

import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Keyboard
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.tooling.preview.PreviewLightDark
import androidx.compose.ui.unit.dp
import com.lerdr.core.designsystem.theme.LerdrTextStyles
import com.lerdr.core.designsystem.theme.LerdrTheme

/**
 * Terminal mode — the machine itself (docs/04 §Terminal mode). Scaffold
 * fidelity: static ANSI-colored lines stand in for the delta-applied
 * renderer; the special-keys bar and scroll-to-live pill are the real
 * affordances.
 */
@Composable
fun TerminalScreen(
    paneId: String,
    onOpenFeed: () -> Unit,
    onBack: () -> Unit,
) {
    TerminalContent(
        paneId = paneId,
        onOpenFeed = onOpenFeed,
        onBack = onBack,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun TerminalContent(
    paneId: String,
    onOpenFeed: () -> Unit,
    onBack: () -> Unit,
) {
    val spacing = LerdrTheme.spacing
    val colors = LerdrTheme.extendedColors
    Scaffold(
        topBar = {
            SessionTopBar(
                title = paneId.substringAfter("::", paneId),
                breadcrumb = "lerdr · main · sd",
                statusLabel = "lease 92×42",
                statusColor = colors.attention,
                mode = SessionMode.TERMINAL,
                onSelectMode = { mode ->
                    if (mode == SessionMode.FEED) onOpenFeed()
                },
                onBack = onBack,
            )
        },
        bottomBar = { SpecialKeysBar() },
    ) { innerPadding ->
        Box(
            modifier = Modifier
                .fillMaxSize()
                .padding(innerPadding)
                .padding(horizontal = spacing.medium),
        ) {
            Surface(
                color = colors.terminalSurface,
                shape = MaterialTheme.shapes.medium,
                modifier = Modifier.fillMaxSize(),
            ) {
                Column(modifier = Modifier.padding(spacing.medium)) {
                    TerminalLine("lerdr git:(main) cargo test -p lerdr-e2ee", colors.terminalAccent)
                    TerminalLine("Compiling lerdr-e2ee v0.1.0  Compiling lerdr-core v0.1.0", colors.terminalText)
                    TerminalLine("Finished test [unoptimized] in 4.12s  Running unittests src/lib.rs", colors.terminalText)
                    TerminalLine("running 14 tests  test handshake_credential … ok", colors.terminalText)
                    TerminalLine("test handshake_invitation … ok  test golden_vectors … ok", colors.terminalText)
                    TerminalLine("test result: ok. 14 passed; 0 failed → lerdr git:(main) ▮", colors.terminalText)
                }
            }
            Surface(
                color = MaterialTheme.colorScheme.surfaceContainerHigh,
                contentColor = MaterialTheme.colorScheme.onSurface,
                shape = CircleShape,
                modifier = Modifier
                    .align(Alignment.BottomCenter)
                    .padding(bottom = spacing.medium),
            ) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier.padding(
                        horizontal = spacing.medium,
                        vertical = spacing.small,
                    ),
                ) {
                    Icon(
                        Icons.Default.KeyboardArrowDown,
                        contentDescription = null,
                        modifier = Modifier.padding(end = spacing.extraSmall),
                    )
                    Text("scroll to live", style = MaterialTheme.typography.labelMedium)
                }
            }
        }
    }
}

@Composable
private fun TerminalLine(text: String, color: androidx.compose.ui.graphics.Color) {
    Text(
        text,
        style = LerdrTextStyles.terminal,
        color = color,
        maxLines = 1,
        modifier = Modifier
            .fillMaxWidth()
            .horizontalScroll(rememberScrollState()),
    )
}

/** Esc Tab arrows Ctrl — the fixed special-keys bar (Ctrl latches). */
@Composable
private fun SpecialKeysBar() {
    val spacing = LerdrTheme.spacing
    Surface(color = MaterialTheme.colorScheme.surfaceContainerLow) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(bottom = spacing.small),
        ) {
            Row(
                horizontalArrangement = Arrangement.spacedBy(spacing.small),
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .fillMaxWidth()
                    .horizontalScroll(rememberScrollState())
                    .padding(horizontal = spacing.medium, vertical = spacing.small),
            ) {
                listOf("Esc", "Tab", "←", "↓", "↑", "→", "Ctrl").forEach { key ->
                    Surface(
                        color = MaterialTheme.colorScheme.surfaceContainerHighest,
                        contentColor = MaterialTheme.colorScheme.onSurface,
                        shape = MaterialTheme.shapes.small,
                    ) {
                        Text(
                            key,
                            style = MaterialTheme.typography.labelLarge,
                            modifier = Modifier.padding(
                                horizontal = spacing.small + spacing.extraSmall,
                                vertical = spacing.extraSmall,
                            ),
                        )
                    }
                }
                Spacer(Modifier.weight(1f))
            }
            Row(
                horizontalArrangement = Arrangement.Center,
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Surface(
                    color = MaterialTheme.colorScheme.surfaceContainerHigh,
                    contentColor = MaterialTheme.colorScheme.onSurfaceVariant,
                    shape = MaterialTheme.shapes.small,
                ) {
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier.padding(
                            horizontal = spacing.small,
                            vertical = spacing.extraSmall,
                        ),
                    ) {
                        Icon(
                            Icons.Default.Keyboard,
                            contentDescription = null,
                            modifier = Modifier.padding(end = spacing.extraSmall),
                        )
                        Text("show keyboard", style = MaterialTheme.typography.labelSmall)
                    }
                }
            }
        }
    }
}

@PreviewLightDark
@Composable
private fun TerminalContentPreview() {
    LerdrTheme {
        TerminalContent(
            paneId = "sd::%1",
            onOpenFeed = {},
            onBack = {},
        )
    }
}
