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
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Keyboard
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.KeyboardArrowUp
import androidx.compose.material.icons.filled.Search
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.isAltPressed
import androidx.compose.ui.input.key.isCtrlPressed
import androidx.compose.ui.input.key.isMetaPressed
import androidx.compose.ui.input.key.isShiftPressed
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.tooling.preview.PreviewLightDark
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lerdr.app.di.AppEntryPoint
import com.lerdr.app.ui.terminal.TerminalInputBar
import com.lerdr.app.ui.terminal.TerminalSurface
import com.lerdr.app.ui.terminal.TERMINAL_FORMAT_ANSI
import com.lerdr.app.ui.terminal.findTerminalText
import com.lerdr.app.ui.terminal.parseTerminalRows
import com.lerdr.app.ui.terminal.rememberTerminalSurfaceState
import com.lerdr.app.ui.terminal.terminalFindRanges
import com.lerdr.app.ui.terminal.terminalFindRows
import com.lerdr.app.ui.terminal.terminalRowForOffset
import com.lerdr.app.ui.terminal.terminalRowOffsets
import com.lerdr.app.ui.terminal.terminalSearchText
import com.lerdr.app.ui.terminal.wrapFindIndex
import com.lerdr.core.designsystem.theme.LerdrTextStyles
import com.lerdr.core.designsystem.theme.LerdrTheme
import dagger.hilt.android.EntryPointAccessors
import kotlinx.coroutines.launch

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
    val scope = rememberCoroutineScope()
    val surfaceState = rememberTerminalSurfaceState()

    // Find-in-buffer — view-local like the oracle's TerminalView state:
    // the composition is per-pane, so the bar closes with the pane switch.
    var findOpen by rememberSaveable { mutableStateOf(false) }
    var findQuery by rememberSaveable { mutableStateOf("") }
    var activeFindIndex by rememberSaveable { mutableStateOf(-1) }

    // The corpus joins every rendered row — skipped entirely while find is
    // closed so each committed frame doesn't pay an O(text) rebuild for a
    // feature that isn't on screen (the oracle's terminalFindCorpus note).
    val findRows = remember(uiState.rows, findOpen) {
        if (findOpen) terminalFindRows(uiState.rows) else emptyList()
    }
    val findCorpus = remember(findRows) { terminalSearchText(findRows) }
    val findOffsets = remember(findRows) { terminalRowOffsets(findRows) }
    val findResult = remember(findCorpus, findQuery) {
        findTerminalText(findCorpus, findQuery.trim())
    }
    val findRanges = remember(findRows, findOffsets, findResult, activeFindIndex) {
        terminalFindRanges(findRows, findOffsets, findResult.matches, activeFindIndex)
    }

    fun revealFindMatch(index: Int) {
        val count = findResult.matches.size
        if (count == 0) return
        val normalized = wrapFindIndex(index, count)
        activeFindIndex = normalized
        val match = findResult.matches[normalized]
        val row = terminalRowForOffset(findRows, findOffsets, match.start)
        if (row < 0) return
        scope.launch { surfaceState.revealRow(row) }
    }

    fun closeFind() {
        findOpen = false
        findQuery = ""
        activeFindIndex = -1
    }

    // Typing re-anchors on the first match (the oracle's findInputChanged).
    LaunchedEffect(findQuery) {
        activeFindIndex = -1
        if (findResult.matches.isNotEmpty()) revealFindMatch(0)
    }

    // Commits move the corpus under a stable query — keep the index inside
    // the match list, dropping it when the query or matches go away.
    LaunchedEffect(findResult, findQuery) {
        if (findQuery.trim().isEmpty() || findResult.matches.isEmpty()) {
            activeFindIndex = -1
        } else if (activeFindIndex !in findResult.matches.indices) {
            activeFindIndex = 0
        }
    }

    Scaffold(
        modifier = Modifier.onPreviewKeyEvent { event ->
            // Ctrl/Cmd+F — the oracle's findShortcut on window keydown.
            if (event.type == KeyEventType.KeyDown &&
                (event.isCtrlPressed || event.isMetaPressed) &&
                !event.isAltPressed &&
                event.key == Key.F
            ) {
                findOpen = true
                true
            } else {
                false
            }
        },
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
                    IconButton(onClick = { findOpen = true }) {
                        Icon(
                            Icons.Default.Search,
                            contentDescription = "Find in terminal",
                        )
                    }
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
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(innerPadding)
                .padding(horizontal = spacing.medium),
        ) {
            if (findOpen) {
                TerminalFindBar(
                    query = findQuery,
                    onQueryChange = { findQuery = it },
                    matchCount = findResult.matches.size,
                    activeIndex = activeFindIndex,
                    truncated = findResult.truncated,
                    onStep = { delta -> revealFindMatch(activeFindIndex + delta) },
                    onClose = ::closeFind,
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(bottom = spacing.small),
                )
            }
            Box(modifier = Modifier.weight(1f).fillMaxWidth()) {
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
                            state = surfaceState,
                            findRanges = findRanges,
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
}

/**
 * The find bar — oracle `.terminal-find`: query field, `n of m` count,
 * previous/next, close. Enter steps forward, Shift+Enter back, Escape
 * closes; the soft keyboard's Search action is Enter. The bar focuses its
 * field when it enters composition — the oracle's focus+select on open.
 */
@Composable
internal fun TerminalFindBar(
    query: String,
    onQueryChange: (String) -> Unit,
    matchCount: Int,
    activeIndex: Int,
    truncated: Boolean,
    onStep: (Int) -> Unit,
    onClose: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val spacing = LerdrTheme.spacing
    val focusRequester = remember { FocusRequester() }
    LaunchedEffect(Unit) { focusRequester.requestFocus() }
    Surface(
        color = MaterialTheme.colorScheme.surfaceContainerLow,
        shape = MaterialTheme.shapes.medium,
        modifier = modifier,
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier
                .fillMaxWidth()
                .padding(
                    start = spacing.small,
                    top = spacing.extraSmall,
                    bottom = spacing.extraSmall,
                ),
        ) {
            OutlinedTextField(
                value = query,
                onValueChange = onQueryChange,
                placeholder = { Text("Find in terminal") },
                singleLine = true,
                textStyle = LerdrTheme.terminalStyle,
                keyboardOptions = KeyboardOptions(
                    // Terminal context — no autocorrect/suggestions; the
                    // Search action steps to the next match like Enter.
                    autoCorrectEnabled = false,
                    keyboardType = KeyboardType.Ascii,
                    imeAction = ImeAction.Search,
                ),
                keyboardActions = KeyboardActions(onSearch = { onStep(1) }),
                modifier = Modifier
                    .weight(1f)
                    .focusRequester(focusRequester)
                    .testTag("terminalFindField")
                    .onPreviewKeyEvent { event ->
                        if (event.type != KeyEventType.KeyDown) {
                            return@onPreviewKeyEvent false
                        }
                        when (event.key) {
                            Key.Escape -> {
                                onClose()
                                true
                            }
                            Key.Enter, Key.NumPadEnter -> {
                                onStep(if (event.isShiftPressed) -1 else 1)
                                true
                            }
                            else -> false
                        }
                    },
            )
            if (query.trim().isNotEmpty()) {
                Text(
                    text = if (matchCount == 0) {
                        "No matches"
                    } else {
                        "${activeIndex + 1} of $matchCount${if (truncated) "+" else ""}"
                    },
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    modifier = Modifier.padding(horizontal = spacing.extraSmall),
                )
            }
            IconButton(onClick = { onStep(-1) }, enabled = matchCount > 0) {
                Icon(
                    Icons.Default.KeyboardArrowUp,
                    contentDescription = "Previous match",
                )
            }
            IconButton(onClick = { onStep(1) }, enabled = matchCount > 0) {
                Icon(
                    Icons.Default.KeyboardArrowDown,
                    contentDescription = "Next match",
                )
            }
            IconButton(onClick = onClose) {
                Icon(Icons.Default.Close, contentDescription = "Close find")
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
