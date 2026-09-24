package com.lerdr.app.session

import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.WindowInsetsSides
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.ime
import androidx.compose.foundation.layout.only
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.systemBars
import androidx.compose.foundation.layout.union
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Keyboard
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.KeyboardArrowUp
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Shield
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
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
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
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
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import lerdr.core.model.PaneLinkActivatedResult
import lerdr.core.model.PaneSearchResult

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
    onSelectTab: (String) -> Unit = {},
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
        onSelectTab = onSelectTab,
        tabsPaneId = paneId,
        onSendKeys = viewModel::sendKeys,
        onSendText = viewModel::sendText,
        onSendSecret = viewModel::sendSecret,
        onDismissError = viewModel::dismissError,
        onViewportMeasured = viewModel::onViewportMeasured,
        onRefresh = viewModel::refresh,
        onPaneLinkResolve = viewModel::paneLinkRegions,
        onPaneLinkActivate = viewModel::activatePaneLink,
        onPaneSearch = viewModel::paneSearch,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun TerminalContent(
    uiState: TerminalUiState,
    onOpenFeed: () -> Unit,
    onOpenFiles: () -> Unit,
    onBack: () -> Unit,
    onSelectTab: (String) -> Unit = {},
    tabsPaneId: String? = null,
    onSendKeys: (List<String>) -> Unit,
    onSendText: (String) -> Unit,
    onSendSecret: (String) -> Unit = {},
    onDismissError: () -> Unit = {},
    onViewportMeasured: (columns: Int, rows: Int) -> Unit,
    onRefresh: () -> Unit,
    /** `pane_link_resolve` — viewport cell → has server-side link regions. */
    onPaneLinkResolve: suspend (row: Int, col: Int) -> Boolean = { _, _ -> false },
    /** `pane_link_activate` — null when the action failed upstream. */
    onPaneLinkActivate: suspend (row: Int, col: Int) -> PaneLinkActivatedResult? = { _, _ -> null },
    /** `pane_search` — full-scrollback result; null when unsupported/failed. */
    onPaneSearch: suspend (query: String) -> PaneSearchResult? = { null },
) {
    val spacing = LerdrTheme.spacing
    val colors = LerdrTheme.extendedColors
    val scope = rememberCoroutineScope()
    val surfaceState = rememberTerminalSurfaceState()
    val context = LocalContext.current

    // Find-in-buffer — view-local like the oracle's TerminalView state:
    // the composition is per-pane, so the bar closes with the pane switch.
    var findOpen by rememberSaveable { mutableStateOf(false) }
    var findQuery by rememberSaveable { mutableStateOf("") }
    var activeFindIndex by rememberSaveable { mutableStateOf(-1) }
    var ctrlLatched by remember { mutableStateOf(false) }
    var combosOpen by remember { mutableStateOf(false) }
    val inputFocus = remember { FocusRequester() }
    val keyboardController = LocalSoftwareKeyboardController.current
    val snackbarHostState = remember { SnackbarHostState() }

    // The oracle's `noEchoActive` — the pane reports a hidden prompt. The
    // wire may omit the prompt text; the oracle defaults it to
    // "Password:". Readers never reach the field (the bar is disabled);
    // the banner still explains what the pane is asking.
    val secretActive = uiState.noEcho
    val secretPrompt = uiState.noEchoPrompt?.takeIf { it.isNotEmpty() }
        ?: "Password:"

    LaunchedEffect(uiState.lastError) {
        uiState.lastError?.let {
            snackbarHostState.showSnackbar(it)
            onDismissError()
        }
    }

    fun showKeyboard() {
        inputFocus.requestFocus()
        keyboardController?.show()
    }

    fun sendCtrlChord(letter: Char) {
        onSendKeys(listOf("Ctrl+${letter.uppercaseChar()}"))
        ctrlLatched = false
    }

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

    // `pane_search` — the server's full-scrollback count annotates the
    // local "n of m" when hits live beyond the rendered buffer. Debounced
    // so each keystroke doesn't pay a fenced upstream call.
    var scrollbackFindTotal by remember { mutableStateOf<Long?>(null) }
    LaunchedEffect(findQuery, findOpen, uiState.paneSearchSupported) {
        val query = findQuery.trim()
        scrollbackFindTotal = if (findOpen && query.isNotEmpty() && uiState.paneSearchSupported) {
            delay(SCROLLBACK_FIND_DEBOUNCE_MS)
            onPaneSearch(query)?.total
        } else {
            null
        }
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
                // Mockup: the "lease N×M" chip is amber while this view
                // holds the pane's size; otherwise the live/offline dot.
                statusColor = when {
                    uiState.leaseColumns > 0 -> colors.attention
                    uiState.connected -> colors.live
                    else -> colors.idle
                },
                mode = SessionMode.TERMINAL,
                onSelectMode = { mode ->
                    when (mode) {
                        SessionMode.FEED -> onOpenFeed()
                        SessionMode.FILES -> onOpenFiles()
                        SessionMode.TERMINAL -> Unit
                    }
                },
                onBack = onBack,
                provider = uiState.provider,
                active = uiState.connected,
                tabsPaneId = tabsPaneId,
                onSelectTab = { onSelectTab(it.paneId) },
                actions = listOf(
                    SessionBarAction(
                        label = "Find in terminal",
                        icon = Icons.Default.Search,
                        onClick = { findOpen = true },
                    ),
                    SessionBarAction(
                        label = "Refresh",
                        icon = Icons.Default.Refresh,
                        onClick = onRefresh,
                    ),
                ),
            )
        },
        bottomBar = {
            Column(
                modifier = Modifier.windowInsetsPadding(
                    WindowInsets.systemBars
                        .union(WindowInsets.ime)
                        .only(WindowInsetsSides.Horizontal + WindowInsetsSides.Bottom),
                ),
            ) {
                if (secretActive) {
                    SecretPromptBanner(
                        prompt = secretPrompt,
                        supported = uiState.secretInputSupported,
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
                SpecialKeysBar(
                    onSendKeys = onSendKeys,
                    ctrlLatched = ctrlLatched,
                    enabled = uiState.canControl,
                    onCtrlTap = {
                        ctrlLatched = !ctrlLatched
                        if (ctrlLatched) showKeyboard()
                    },
                    onCtrlLongPress = { combosOpen = true },
                    onShowKeyboard = ::showKeyboard,
                )
                TerminalInputBar(
                    onSendText = onSendText,
                    enabled = uiState.canControl,
                    hint = if (uiState.canControl) {
                        "Inject text…"
                    } else {
                        "Read-only session"
                    },
                    focusRequester = inputFocus,
                    ctrlLatched = ctrlLatched,
                    onCtrlChord = ::sendCtrlChord,
                    secretMode = secretActive && uiState.secretInputSupported,
                    onSendSecret = onSendSecret,
                )
            }
        },
        snackbarHost = { SnackbarHost(snackbarHostState) },
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
                    scrollbackTotal = scrollbackFindTotal,
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
                    Column {
                        // The mockup's pane meta row — the lease grid as a
                        // ── pane N×M ── divider inside the surface card,
                        // with the truncated / hidden-input markers.
                        val metaLabel = paneMetaLabel(uiState)
                        if (metaLabel != null) {
                            PaneMetaRow(
                                label = metaLabel,
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .padding(
                                        horizontal = spacing.small,
                                        vertical = spacing.extraSmall,
                                    ),
                            )
                        }
                        Box(modifier = Modifier.weight(1f).fillMaxWidth()) {
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
                                    onTapSurface = {
                                        // Readers have no composer — the
                                        // tap stays a scroll gesture.
                                        if (uiState.canControl) {
                                            inputFocus.requestFocus()
                                            keyboardController?.show()
                                        }
                                    },
                                    paneLinksSupported = uiState.paneLinksSupported,
                                    onResolveLink = onPaneLinkResolve,
                                    onActivateLink = activate@{ row, col ->
                                        val result = onPaneLinkActivate(row, col)
                                            ?: return@activate
                                        when {
                                            // The pane host's browser took
                                            // it — surface the target, which
                                            // OSC8 hides from the text.
                                            result.handled -> snackbarHostState.showSnackbar(
                                                result.url
                                                    ?.let { "Opened on desktop · $it" }
                                                    ?: "Opened on desktop",
                                            )
                                            // Resolved but not handled
                                            // upstream — open it here.
                                            result.url != null -> runCatching {
                                                context.startActivity(
                                                    Intent(Intent.ACTION_VIEW, Uri.parse(result.url)),
                                                )
                                            }.onFailure {
                                                snackbarHostState.showSnackbar(
                                                    "No app can open that link",
                                                )
                                            }
                                            else -> Unit
                                        }
                                    },
                                )
                            }
                        }
                    }
                }
                // Mockup's "scroll to live" — appears once the follow-live
                // pin is released by scrolling into history.
                if (!surfaceState.stickToBottom && !uiState.waitingForContent) {
                    Surface(
                        onClick = { scope.launch { surfaceState.scrollToLive() } },
                        color = MaterialTheme.colorScheme.surfaceContainerHigh,
                        contentColor = MaterialTheme.colorScheme.onSurface,
                        shape = CircleShape,
                        modifier = Modifier
                            .align(Alignment.BottomCenter)
                            .padding(bottom = spacing.small)
                            .defaultMinSize(minHeight = 48.dp),
                    ) {
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            modifier = Modifier.padding(
                                horizontal = spacing.small,
                                vertical = spacing.extraSmall,
                            ),
                        ) {
                            Icon(
                                Icons.Default.KeyboardArrowDown,
                                contentDescription = null,
                                modifier = Modifier.padding(end = spacing.extraSmall),
                            )
                            Text(
                                "scroll to live",
                                style = MaterialTheme.typography.labelMedium,
                            )
                        }
                    }
                }
            }
        }
    }

    if (combosOpen) {
        CtrlCombosSheet(
            onSendKeys = onSendKeys,
            onDismiss = { combosOpen = false },
        )
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
    /** `pane_search` full-scrollback hit count — beyond the rendered rows. */
    scrollbackTotal: Long? = null,
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
                // `pane_search` counts the full scrollback — mention hits
                // living beyond the rendered buffer instead of implying
                // "n of m" is the whole pane.
                val deeper = scrollbackTotal?.takeIf { it > matchCount }
                val count = if (matchCount == 0) {
                    "No matches"
                } else {
                    "${activeIndex + 1} of $matchCount${if (truncated) "+" else ""}"
                }
                Text(
                    text = if (deeper != null) "$count · $deeper in scrollback" else count,
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

/**
 * Esc Tab arrows Enter ⌫ Ctrl ⌨ — the mockup's single special-keys bar.
 * Every chip enforces the 48 dp touch target; [enabled] is the reader
 * gate (`readOnly` in the oracle — mutating affordances stay reachable
 * but inert so the bar's layout doesn't jump between roles).
 */
@Composable
private fun SpecialKeysBar(
    onSendKeys: (List<String>) -> Unit,
    ctrlLatched: Boolean,
    enabled: Boolean,
    onCtrlTap: () -> Unit,
    onCtrlLongPress: () -> Unit,
    onShowKeyboard: () -> Unit,
) {
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
                if (!enabled) {
                    // The oracle's readOnly gate — a persistent hint in
                    // place of usable keys.
                    Surface(
                        color = MaterialTheme.colorScheme.surfaceContainerHighest,
                        contentColor = MaterialTheme.colorScheme.onSurfaceVariant,
                        shape = MaterialTheme.shapes.small,
                    ) {
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            modifier = Modifier
                                .defaultMinSize(minHeight = 48.dp)
                                .padding(
                                    horizontal = spacing.small + spacing.extraSmall,
                                ),
                        ) {
                            Icon(
                                Icons.Default.Lock,
                                contentDescription = null,
                                modifier = Modifier.padding(end = spacing.extraSmall),
                            )
                            Text(
                                "read-only",
                                style = MaterialTheme.typography.labelLarge,
                            )
                        }
                    }
                }
                SPECIAL_KEYS.forEach { (label, key) ->
                    KeyButton(
                        label = label,
                        onClick = { onSendKeys(listOf(key)) },
                        enabled = enabled,
                    )
                }
                // Latching modifier — tap, then a letter on the keyboard
                // sends the chord; long-press opens the combos sheet. The
                // latched state is announced (selected + stateDescription)
                // and painted on the container (the oracle's aria-pressed
                // + `keyControlStatus`).
                val ctrlColors = when {
                    !enabled -> MaterialTheme.colorScheme.surfaceContainerHigh to
                        MaterialTheme.colorScheme.onSurfaceVariant
                    ctrlLatched -> MaterialTheme.colorScheme.primaryContainer to
                        MaterialTheme.colorScheme.onPrimaryContainer
                    else -> MaterialTheme.colorScheme.surfaceContainerHighest to
                        MaterialTheme.colorScheme.onSurface
                }
                Surface(
                    color = ctrlColors.first,
                    contentColor = ctrlColors.second,
                    shape = MaterialTheme.shapes.small,
                    modifier = Modifier.semantics {
                        selected = ctrlLatched
                        stateDescription = if (ctrlLatched) {
                            "Ctrl latched — the next letter sends a Ctrl chord"
                        } else {
                            "Ctrl not latched"
                        }
                    },
                ) {
                    Text(
                        "Ctrl",
                        style = MaterialTheme.typography.labelLarge,
                        modifier = Modifier
                            .combinedClickable(
                                enabled = enabled,
                                onClick = onCtrlTap,
                                onLongClick = onCtrlLongPress,
                            )
                            .defaultMinSize(minWidth = 48.dp, minHeight = 48.dp)
                            .padding(
                                horizontal = spacing.small + spacing.extraSmall,
                            ),
                    )
                }
                // Mockup's ⌨ tail chip — focuses the input field and
                // raises the soft keyboard.
                KeyButton(
                    label = null,
                    onClick = onShowKeyboard,
                    enabled = true,
                ) {
                    Icon(
                        Icons.Default.Keyboard,
                        contentDescription = "Show keyboard",
                        modifier = Modifier.padding(
                            horizontal = spacing.small + spacing.extraSmall,
                        ),
                    )
                }
            }
            if (ctrlLatched) {
                // The oracle's `keyControlStatus` — the latch is visible
                // in words, not only in the chip's container color.
                Text(
                    "Ctrl latched — type a letter for the chord",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.primary,
                    modifier = Modifier.padding(
                        horizontal = spacing.medium,
                        vertical = spacing.extraSmall,
                    ),
                )
            }
        }
    }
}

@Composable
private fun KeyButton(
    label: String?,
    onClick: () -> Unit,
    enabled: Boolean,
    content: (@Composable () -> Unit)? = null,
) {
    val spacing = LerdrTheme.spacing
    Surface(
        color = if (enabled) {
            MaterialTheme.colorScheme.surfaceContainerHighest
        } else {
            MaterialTheme.colorScheme.surfaceContainerHigh
        },
        contentColor = if (enabled) {
            MaterialTheme.colorScheme.onSurface
        } else {
            MaterialTheme.colorScheme.onSurfaceVariant
        },
        shape = MaterialTheme.shapes.small,
        onClick = onClick,
        enabled = enabled,
        modifier = Modifier.defaultMinSize(minWidth = 48.dp, minHeight = 48.dp),
    ) {
        Box(contentAlignment = Alignment.Center) {
            if (content != null) {
                content()
            } else {
                Text(
                    label.orEmpty(),
                    style = MaterialTheme.typography.labelLarge,
                    modifier = Modifier.padding(
                        horizontal = spacing.small + spacing.extraSmall,
                    ),
                )
            }
        }
    }
}

/** Long-press-Ctrl combos sheet — the doc's `C-c C-d C-z C-l C-r` set. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun CtrlCombosSheet(
    onSendKeys: (List<String>) -> Unit,
    onDismiss: () -> Unit,
) {
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(modifier = Modifier.padding(bottom = LerdrTheme.spacing.large)) {
            CTRL_COMBOS.forEach { combo ->
                Surface(
                    onClick = {
                        onSendKeys(listOf("Ctrl+${combo.last()}"))
                        onDismiss()
                    },
                    modifier = Modifier
                        .fillMaxWidth()
                        .defaultMinSize(minHeight = 48.dp),
                ) {
                    Text(
                        combo,
                        style = MaterialTheme.typography.titleMedium,
                        modifier = Modifier.padding(
                            horizontal = LerdrTheme.spacing.medium,
                            vertical = LerdrTheme.spacing.small,
                        ),
                    )
                }
            }
        }
    }
}

/**
 * label → wire key name (`send_keys` passes names through; Herdr's
 * vocabulary is Up/Down/Left/Right/Esc/Enter/Tab/Backspace — the oracle's
 * `sendTerminalKey` spellings). Ctrl is not here — it's the latching
 * modifier rendered after these keys.
 */
private val SPECIAL_KEYS = listOf(
    "Esc" to "Escape",
    "Tab" to "Tab",
    "←" to "Left",
    "↓" to "Down",
    "↑" to "Up",
    "→" to "Right",
    "Enter" to "Enter",
    "⌫" to "Backspace",
)

private val CTRL_COMBOS = listOf("C-c", "C-d", "C-z", "C-l", "C-r")

/**
 * The mockup's `─── pane 92×42 ───` divider row — session meta as a
 * terminal-styled caption flanked by rules inside the surface card.
 * Shows the leased grid, then the markers the frame flagged.
 */
@Composable
private fun PaneMetaRow(label: String, modifier: Modifier = Modifier) {
    val colors = LerdrTheme.extendedColors
    Row(verticalAlignment = Alignment.CenterVertically, modifier = modifier) {
        HorizontalDivider(
            modifier = Modifier.weight(1f),
            color = colors.terminalAccent.copy(alpha = 0.25f),
        )
        Text(
            label,
            style = MaterialTheme.typography.labelSmall,
            color = colors.terminalAccent.copy(alpha = 0.8f),
            maxLines = 1,
            modifier = Modifier.padding(horizontal = LerdrTheme.spacing.small),
        )
        HorizontalDivider(
            modifier = Modifier.weight(1f),
            color = colors.terminalAccent.copy(alpha = 0.25f),
        )
    }
}

/** `pane 92×42` + `truncated` + `hidden input` — null when nothing applies. */
private fun paneMetaLabel(uiState: TerminalUiState): String? {
    val parts = buildList {
        if (uiState.leaseColumns > 0) {
            add(
                if (uiState.leaseRows > 0) {
                    "pane ${uiState.leaseColumns}×${uiState.leaseRows}"
                } else {
                    "pane ${uiState.leaseColumns} cols"
                },
            )
        }
        if (uiState.truncated) add("truncated")
        if (uiState.noEcho) add("hidden input")
    }
    return parts.takeIf { it.isNotEmpty() }?.joinToString(" · ")
}

/**
 * The oracle's `.secret-prompt` section — explains that the pane is asking
 * for a hidden value (`no_echo`) before the password-mode input bar. When
 * the relay lacks `secret_input` the field stays inert and this carries
 * the oracle's too-old-relay hint as the inline error.
 */
@Composable
private fun SecretPromptBanner(
    prompt: String,
    supported: Boolean,
    modifier: Modifier = Modifier,
) {
    val spacing = LerdrTheme.spacing
    val colors = LerdrTheme.extendedColors
    Surface(color = MaterialTheme.colorScheme.surfaceContainerLow, modifier = modifier) {
        Column(
            modifier = Modifier.padding(
                horizontal = spacing.medium,
                vertical = spacing.extraSmall,
            ),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    Icons.Default.Shield,
                    contentDescription = null,
                    tint = colors.attention,
                )
                Text(
                    text = if (prompt.isNotEmpty()) {
                        "The terminal is asking for a hidden value: $prompt"
                    } else {
                        "The terminal is asking for a hidden value"
                    },
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurface,
                    modifier = Modifier.padding(start = spacing.small),
                )
            }
            if (!supported) {
                Text(
                    "This computer's relay is too old to accept a hidden value " +
                        "from the phone; answer it at the computer.",
                    style = MaterialTheme.typography.labelSmall,
                    color = colors.danger,
                    modifier = Modifier.padding(
                        start = spacing.medium + spacing.small,
                        top = spacing.extraSmall,
                    ),
                )
            }
        }
    }
}

@PreviewLightDark
@Composable
private fun TerminalContentPreview() {
    LerdrTheme {
        TerminalContent(
            uiState = TerminalUiState(
                paneId = "sd::%1",
                title = "claude",
                provider = "claude",
                breadcrumb = "lerdr · main · sd",
                statusLabel = "lease 92×42",
                connected = true,
                waitingForContent = false,
                leaseColumns = 92,
                leaseRows = 42,
                canControl = true,
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

/** Keystroke settle before the fenced `pane_search` call — search-as-you-type. */
private const val SCROLLBACK_FIND_DEBOUNCE_MS = 350L
