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
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
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
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.tooling.preview.PreviewLightDark
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lerdr.app.di.AppEntryPoint
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
        onBack = onBack,
        onSendKeys = viewModel::sendKeys,
        onRefresh = viewModel::refresh,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun TerminalContent(
    uiState: TerminalUiState,
    onOpenFeed: () -> Unit,
    onBack: () -> Unit,
    onSendKeys: (List<String>) -> Unit,
    onRefresh: () -> Unit,
) {
    val spacing = LerdrTheme.spacing
    val colors = LerdrTheme.extendedColors
    val listState = rememberLazyListState()
    LaunchedEffect(uiState.revision) {
        if (uiState.lines.isNotEmpty()) {
            listState.animateScrollToItem(uiState.lines.lastIndex)
        }
    }
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
                    if (mode == SessionMode.FEED) onOpenFeed()
                },
                onBack = onBack,
                trailing = {
                    TextButton(onClick = onRefresh) {
                        Text("Refresh", style = MaterialTheme.typography.labelMedium)
                    }
                },
            )
        },
        bottomBar = { SpecialKeysBar(onSendKeys = onSendKeys) },
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
                        TerminalLine(
                            if (uiState.connected) {
                                "Watching pane…"
                            } else {
                                "Waiting for relay…"
                            },
                            colors.terminalAccent,
                        )
                    }
                } else {
                    LazyColumn(
                        state = listState,
                        contentPadding = PaddingValues(spacing.medium),
                    ) {
                        itemsIndexed(uiState.lines) { _, line ->
                            TerminalLine(line.ifEmpty { " " }, colors.terminalText)
                        }
                    }
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

@Composable
private fun TerminalLine(text: String, color: androidx.compose.ui.graphics.Color) {
    Text(
        text,
        style = LerdrTextStyles.terminal,
        color = color,
        maxLines = 1,
        softWrap = false,
        modifier = Modifier
            .fillMaxWidth()
            .horizontalScroll(rememberScrollState()),
    )
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
                lines = listOf(
                    "lerdr git:(main) cargo test -p lerdr-e2ee",
                    "running 14 tests  test handshake_credential … ok",
                    "test result: ok. 14 passed; 0 failed",
                ),
            ),
            onOpenFeed = {},
            onBack = {},
            onSendKeys = {},
            onRefresh = {},
        )
    }
}
