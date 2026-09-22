package com.lerdr.app.session

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Keyboard
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.tooling.preview.PreviewLightDark
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lerdr.app.di.AppEntryPoint
import com.lerdr.app.ui.terminal.TerminalInputBar
import com.lerdr.app.ui.terminal.TerminalSurface
import com.lerdr.app.ui.terminal.TERMINAL_FORMAT_ANSI
import com.lerdr.app.ui.terminal.parseTerminalRows
import com.lerdr.core.designsystem.theme.LerdrTextStyles
import com.lerdr.core.designsystem.theme.LerdrTheme
import dagger.hilt.android.EntryPointAccessors

/**
 * Terminal mode — the machine itself (docs/04 §Terminal mode). The pane
 * watch lives on [TerminalViewModel] for the screen's lifetime; this layer
 * renders the committed snapshot and forwards key chords.
 */
@Composable
fun TerminalScreen(
    paneId: String,
    onOpenFeed: () -> Unit,
    onOpenFiles: () -> Unit,
    onBack: () -> Unit,
) {
    val appContext = LocalContext.current.applicationContext
    val viewModel: TerminalViewModel = viewModel(key = "terminal:$paneId") {
        val entryPoint = EntryPointAccessors.fromApplication(appContext, AppEntryPoint::class.java)
        TerminalViewModel(paneId, entryPoint.sessionRepository(), entryPoint.appScope())
    }
    val uiState by viewModel.uiState.collectAsStateWithLifecycle()
    TerminalContent(
        uiState = uiState,
        onOpenFeed = onOpenFeed,
        onOpenFiles = onOpenFiles,
        onBack = onBack,
        onSendKeys = viewModel::sendKeys,
        onSendText = viewModel::sendLiteralText,
        onViewportMeasured = viewModel::onViewportMeasured,
        onRefresh = viewModel::refresh,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun TerminalContent(
    uiState: TerminalUiState,
    onOpenFeed: () -> Unit,
    onOpenFiles: () -> Unit,
    onBack: () -> Unit,
    onSendKeys: (List<String>) -> Unit,
    onSendText: (String) -> Unit,
    onViewportMeasured: (columns: Int, rows: Int) -> Unit,
    onRefresh: () -> Unit,
) {
    val spacing = LerdrTheme.spacing
    val colors = LerdrTheme.extendedColors
    Scaffold(
        topBar = {
            SessionTopBar(
                title = uiState.title.ifEmpty { uiState.paneId.substringAfter("::") },
                breadcrumb = uiState.breadcrumb,
                statusLabel = uiState.statusLabel.ifEmpty {
                    if (uiState.connected) "live" else "offline"
                },
                statusColor = if (uiState.connected) colors.live else colors.idle,
                mode = SessionMode.TERMINAL,
                onSelectMode = { mode ->
                    when (mode) {
                        SessionMode.FEED -> onOpenFeed()
                        SessionMode.FILES -> onOpenFiles()
                        SessionMode.TERMINAL -> Unit
                    }
                },
                onBack = onBack,
                trailing = {
                    TextButton(onClick = onRefresh) {
                        Text("Refresh", style = MaterialTheme.typography.labelMedium)
                    }
                },
            )
        },
        bottomBar = {
            Column {
                SpecialKeysBar(onSendKeys = onSendKeys)
                TerminalInputBar(
                    onSendText = onSendText,
                    onSendKeys = onSendKeys,
                )
            }
        },
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
                if (uiState.waitingForContent) {
                    Column(modifier = Modifier.padding(spacing.medium)) {
                        Text(
                            if (uiState.connected) {
                                "Watching pane…"
                            } else {
                                "Waiting for relay…"
                            },
                            style = LerdrTextStyles.terminal,
                            color = colors.terminalAccent,
                        )
                    }
                } else {
                    TerminalSurface(
                        rows = uiState.rows,
                        cursor = uiState.cursor,
                        revision = uiState.revision,
                        contentPadding = PaddingValues(spacing.medium),
                        onViewportMeasured = onViewportMeasured,
                    )
                }
            }
            if (uiState.truncated) {
                Surface(
                    color = MaterialTheme.colorScheme.surfaceContainerHigh,
                    contentColor = MaterialTheme.colorScheme.onSurface,
                    shape = CircleShape,
                    modifier = Modifier
                        .align(Alignment.TopCenter)
                        .padding(top = spacing.small),
                ) {
                    Text(
                        "pane truncated — full view on the computer",
                        style = MaterialTheme.typography.labelSmall,
                        modifier = Modifier.padding(
                            horizontal = spacing.small,
                            vertical = spacing.extraSmall,
                        ),
                    )
                }
            }
        }
    }
}

/** Esc Tab arrows Ctrl — the fixed special-keys bar. */
@Composable
private fun SpecialKeysBar(onSendKeys: (List<String>) -> Unit) {
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
                SPECIAL_KEYS.forEach { (label, key) ->
                    Surface(
                        color = MaterialTheme.colorScheme.surfaceContainerHighest,
                        contentColor = MaterialTheme.colorScheme.onSurface,
                        shape = MaterialTheme.shapes.small,
                        onClick = { onSendKeys(listOf(key)) },
                    ) {
                        Text(
                            label,
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

/** label → wire key name the relay understands. */
private val SPECIAL_KEYS = listOf(
    "Esc" to "Escape",
    "Tab" to "Tab",
    "←" to "ArrowLeft",
    "↓" to "ArrowDown",
    "↑" to "ArrowUp",
    "→" to "ArrowRight",
    "Ctrl-C" to "Ctrl+C",
)

@PreviewLightDark
@Composable
private fun TerminalContentPreview() {
    LerdrTheme {
        TerminalContent(
            uiState = TerminalUiState(
                paneId = "sd::%1",
                title = "claude",
                breadcrumb = "lerdr · main · sd",
                statusLabel = "lease 92×42",
                connected = true,
                waitingForContent = false,
                rows = parseTerminalRows(
                    listOf(
                        "lerdr git:(main) [32mcargo test[0m -p lerdr-e2ee",
                        "running 14 tests  test handshake_credential … [32mok[0m",
                        "[1mtest result: ok.[0m 14 passed; 0 failed",
                    ),
                    TERMINAL_FORMAT_ANSI,
                ),
            ),
            onOpenFeed = {},
            onOpenFiles = {},
            onBack = {},
            onSendKeys = {},
            onSendText = {},
            onViewportMeasured = { _, _ -> },
            onRefresh = {},
        )
    }
}
