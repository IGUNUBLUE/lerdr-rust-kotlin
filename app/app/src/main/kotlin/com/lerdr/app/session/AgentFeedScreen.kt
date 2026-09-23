package com.lerdr.app.session

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.core.Spring
import androidx.compose.animation.core.VisibilityThreshold
import androidx.compose.animation.core.spring
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.WindowInsetsSides
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.ime
import androidx.compose.foundation.layout.only
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.systemBars
import androidx.compose.foundation.layout.union
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.AttachFile
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.InputChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.platform.ClipEntry
import androidx.compose.ui.platform.LocalClipboard
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.PreviewLightDark
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lerdr.app.di.AppEntryPoint
import com.lerdr.app.session.feed.FeedBlockerCard
import com.lerdr.app.session.feed.FeedHistoryWarnings
import com.lerdr.app.session.feed.FeedMarkdown
import com.lerdr.app.session.feed.FeedToolCard
import com.lerdr.app.session.feed.MAX_VISIBLE_SLASH_COMMANDS
import com.lerdr.app.session.feed.QuestionDraft
import com.lerdr.app.session.feed.SlashCommandCatalog
import com.lerdr.app.session.feed.SlashCommandMenu
import com.lerdr.app.session.feed.effectiveSlashIndex
import com.lerdr.app.session.feed.feedMatchingEntryIndexes
import com.lerdr.app.session.feed.isHistorySourceChanged
import com.lerdr.app.session.feed.matchingSlashCommands
import com.lerdr.app.session.feed.slashQueryFor
import com.lerdr.app.session.feed.slashSelectionText
import com.lerdr.app.ui.terminal.wrapFindIndex
import com.lerdr.core.designsystem.components.LerdrLoadingIndicator
import com.lerdr.core.designsystem.theme.LerdrTheme
import dagger.hilt.android.EntryPointAccessors
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import lerdr.core.conversation.ConversationBrowseState
import lerdr.core.conversation.ConversationEntry
import lerdr.core.conversation.ConversationRole

/**
 * Feed mode — semantic timeline (docs/04 §Feed). [AgentFeedScreen] owns the
 * ViewModel seam; [AgentFeedContent] is pure state → previews stay honest.
 */
@Composable
fun AgentFeedScreen(
    paneId: String,
    onOpenTerminal: () -> Unit,
    onOpenFiles: () -> Unit,
    onBack: () -> Unit,
    onSelectTab: (String) -> Unit = {},
) {
    val appContext = LocalContext.current.applicationContext
    val viewModel: FeedViewModel = viewModel(key = "feed:$paneId") {
        val entryPoint = EntryPointAccessors.fromApplication(appContext, AppEntryPoint::class.java)
        FeedViewModel(
            paneId,
            entryPoint.sessionRepository(),
            entryPoint.draftStore(),
            AttachmentUploads(entryPoint.appScope(), entryPoint.sessionRepository(), appContext),
        )
    }
    val uiState by viewModel.uiState.collectAsStateWithLifecycle()
    // SAF picker — `*/*` so client-side validation reports the oracle's
    // per-file issue text instead of silently hiding unsupported types.
    val picker = rememberLauncherForActivityResult(
        ActivityResultContracts.OpenMultipleDocuments(),
    ) { uris ->
        if (uris.isNotEmpty()) {
            viewModel.selectAttachments(uris.map { it.toString() })
        }
    }
    AgentFeedContent(
        uiState = uiState,
        onOpenTerminal = onOpenTerminal,
        onOpenFiles = onOpenFiles,
        onBack = onBack,
        onSelectTab = onSelectTab,
        tabsPaneId = paneId,
        onDraftChange = viewModel::onDraftChange,
        onSendPrompt = viewModel::sendPrompt,
        onRespond = viewModel::respond,
        onQuestionDraftChange = viewModel::updateQuestionDraft,
        onSubmitQuestion = viewModel::submitQuestion,
        onNavigateQuestion = viewModel::navigateQuestion,
        onClarifyQuestion = viewModel::clarifyQuestion,
        onCopyResponse = viewModel::copyAgentResponse,
        onClearError = viewModel::clearError,
        onLoadOlder = viewModel::loadOlderHistory,
        onReloadHistory = viewModel::loadHistory,
        onRecoverHistory = viewModel::recoverHistory,
        onCancelPreparation = viewModel::cancelPreparation,
        onContinuePreparation = viewModel::continuePreparation,
        onPickAttachments = { picker.launch(arrayOf("*/*")) },
        onRemoveAttachment = viewModel::removeAttachment,
        onClearAttachments = viewModel::clearAttachments,
        onRestartAttachments = viewModel::restartAttachments,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AgentFeedContent(
    uiState: FeedUiState,
    onOpenTerminal: () -> Unit,
    onOpenFiles: () -> Unit,
    onBack: () -> Unit,
    onSelectTab: (String) -> Unit = {},
    tabsPaneId: String? = null,
    onDraftChange: (String) -> Unit,
    onSendPrompt: () -> Unit,
    onRespond: (Int, String) -> Unit,
    onQuestionDraftChange: (QuestionDraft) -> Unit,
    onSubmitQuestion: () -> Unit,
    onNavigateQuestion: (String) -> Unit,
    onClarifyQuestion: () -> Unit,
    onCopyResponse: (String, (String) -> Unit) -> Unit,
    onClearError: () -> Unit,
    onLoadOlder: () -> Unit,
    /** Oracle `reloadHistory`/`returnToLatest` — cursorless fresh browse. */
    onReloadHistory: () -> Unit,
    /** Oracle `recoverHistory` — the error row's Continue/Retry affordance. */
    onRecoverHistory: () -> Unit,
    /** Oracle `pausePreparation` — stops the client-side poll loop. */
    onCancelPreparation: () -> Unit,
    /** Oracle `continuePreparation` — resumes polling the stored cursor. */
    onContinuePreparation: () -> Unit,
    onPickAttachments: () -> Unit,
    onRemoveAttachment: (String) -> Unit,
    onClearAttachments: () -> Unit,
    onRestartAttachments: () -> Unit,
) {
    val spacing = LerdrTheme.spacing
    val scope = rememberCoroutineScope()
    val listState = rememberLazyListState()
    val clipboard = LocalClipboard.current
    val snackbarHostState = remember { SnackbarHostState() }

    // ── find-in-conversation — local state, `openTerminalFind` analogue ──
    var findOpen by rememberSaveable { mutableStateOf(false) }
    var findQuery by rememberSaveable { mutableStateOf("") }
    var searchQuery by rememberSaveable { mutableStateOf("") }
    var activeFindIndex by rememberSaveable { mutableIntStateOf(-1) }
    // The oracle debounces the transcript filter 250 ms.
    LaunchedEffect(findQuery) {
        delay(250)
        searchQuery = findQuery
    }
    val matchedIndexes = remember(uiState.entries, searchQuery) {
        feedMatchingEntryIndexes(uiState.entries, searchQuery)
    }
    val searching = searchQuery.trim().isNotEmpty()
    val visibleEntries = remember(uiState.entries, matchedIndexes) {
        matchedIndexes.map { uiState.entries[it] }
    }
    // One match = one entry row; the find bar counts rows like the oracle's
    // `n of m` over `visibleEntries`.
    val matchCount = if (searching) matchedIndexes.size else 0
    // ── history status surface — the oracle's `conversation-warning` block ──
    // `sourceChangedNotice` — cursor-invalidating failures reload cursorless.
    val historySourceChanged = isHistorySourceChanged(
        uiState.historyError,
        uiState.historyErrorCode,
    )
    // `hasMore && !sourceChangedNotice && polls < max && state !== 'preparing'`
    val loadOlderVisible = uiState.hasMoreHistory && !historySourceChanged &&
        !uiState.preparationPaused &&
        uiState.browseState != ConversationBrowseState.PREPARING
    // `loading && !entries.length` / `!available && !entries.length` — the
    // oracle's empty-state branches are exclusive (loading first) and
    // suppress the whole warning block.
    val historyLoadingEmpty = uiState.historyLoading && uiState.entries.isEmpty()
    val historyUnavailableEmpty = !historyLoadingEmpty &&
        uiState.entries.isEmpty() &&
        (!uiState.historyAvailable || !uiState.historyPageAvailable)
    // `load-older`/`no-history`/`history-loading` header items offset lazy indices.
    val headerOffset = (if (loadOlderVisible) 1 else 0) +
        (if (historyUnavailableEmpty) 1 else 0) +
        (if (historyLoadingEmpty) 1 else 0)

    fun revealFindMatch(index: Int) {
        val normalized = wrapFindIndex(index, matchCount)
        if (normalized < 0) return
        activeFindIndex = normalized
        scope.launch {
            listState.animateScrollToItem(headerOffset + normalized)
        }
    }

    fun closeFind() {
        findOpen = false
        findQuery = ""
        searchQuery = ""
        activeFindIndex = -1
    }

    // Oracle: a fresh result set auto-reveals the first match.
    LaunchedEffect(searchQuery, matchCount) {
        if (matchCount > 0) {
            revealFindMatch(0)
        } else {
            activeFindIndex = -1
        }
    }

    // ── slash commands — oracle `slashMenuOpen` + keyboard navigation ──
    var dismissedSlashQuery by rememberSaveable { mutableStateOf<String?>(null) }
    var activeSlashIndex by rememberSaveable { mutableIntStateOf(0) }
    val slashQuery = slashQueryFor(uiState.composerDraft)
    val slashMatches = remember(uiState.slashCommands, slashQuery) {
        matchingSlashCommands(
            SlashCommandCatalog(uiState.slashCommands, uiState.slashTruncated),
            slashQuery,
        )
    }
    val filteredSlash = remember(slashMatches) {
        slashMatches.take(MAX_VISIBLE_SLASH_COMMANDS)
    }
    val slashIndex = effectiveSlashIndex(activeSlashIndex, filteredSlash.size)
    // Oracle `slashMenuOpen` — pure draft-text drive; a blocked agent still
    // accepts commands (chat-while-blocked is the clarify path).
    val slashMenuOpen = uiState.canControl && slashQuery != null &&
        dismissedSlashQuery != uiState.composerDraft
    LaunchedEffect(uiState.composerDraft) {
        if (slashQuery == null) {
            dismissedSlashQuery = null
            activeSlashIndex = 0
        }
    }

    fun selectSlash(index: Int) {
        val entry = filteredSlash.getOrNull(index) ?: return
        val next = slashSelectionText(entry)
        dismissedSlashQuery = next
        activeSlashIndex = 0
        onDraftChange(next)
    }

    // ── transient status → snackbar (oracle `showToast`) ──
    LaunchedEffect(uiState.lastError) {
        uiState.lastError?.let {
            snackbarHostState.showSnackbar(it)
            onClearError()
        }
    }
    LaunchedEffect(uiState.notice) {
        uiState.notice?.let {
            snackbarHostState.showSnackbar(it)
            onClearError()
        }
    }
    LaunchedEffect(uiState.uploadStatus, uiState.uploadError) {
        if (uiState.uploadError && uiState.uploadStatus.isNotEmpty()) {
            snackbarHostState.showSnackbar(uiState.uploadStatus)
        }
    }

    // Follow the tail only while the user is pinned to the bottom — a
    // "Load older" prepend keeps its anchor via the stable item keys, and
    // manual scroll-back must not yank the viewport down on new output.
    val pinnedToBottom by remember {
        derivedStateOf {
            val info = listState.layoutInfo
            val lastVisible = info.visibleItemsInfo.lastOrNull()?.index ?: -1
            lastVisible >= info.totalItemsCount - 1
        }
    }
    // The tail key changes on a new last entry and on in-place text growth
    // (streaming replies); prepends leave it untouched so no scroll fires.
    val tailKey = visibleEntries.lastOrNull()?.let { "${it.id}:${it.text.length}" }
    LaunchedEffect(tailKey, uiState.working, uiState.blocked != null) {
        if (tailKey == null || !pinnedToBottom) return@LaunchedEffect
        val lastIndex = headerOffset +
            visibleEntries.size +
            (if (uiState.blocked != null) 1 else 0) +
            (if (uiState.working) 1 else 0) - 1
        listState.animateScrollToItem(lastIndex.coerceAtLeast(0))
    }
    Scaffold(
        snackbarHost = { SnackbarHost(snackbarHostState) },
        topBar = {
            SessionTopBar(
                title = uiState.title.ifEmpty { uiState.paneId.substringAfter("::") },
                breadcrumb = uiState.breadcrumb,
                statusLabel = uiState.statusLabel.ifEmpty {
                    if (uiState.connected) "connected" else "offline"
                },
                statusColor = if (uiState.working) {
                    LerdrTheme.extendedColors.working
                } else {
                    LerdrTheme.extendedColors.idle
                },
                mode = SessionMode.FEED,
                onSelectMode = { mode ->
                    when (mode) {
                        SessionMode.TERMINAL -> onOpenTerminal()
                        SessionMode.FILES -> onOpenFiles()
                        SessionMode.FEED -> Unit
                    }
                },
                onBack = onBack,
                provider = uiState.provider,
                active = uiState.working,
                actions = listOf(
                    SessionBarAction(
                        label = if (findOpen) "Close find" else "Find in conversation",
                        icon = Icons.Default.Search,
                        onClick = {
                            if (findOpen) closeFind() else findOpen = true
                        },
                    ),
                ),
                tabsPaneId = tabsPaneId,
                onSelectTab = { onSelectTab(it.paneId) },
            )
        },
        bottomBar = {
            Column {
                if (slashMenuOpen) {
                    SlashCommandMenu(
                        commands = filteredSlash,
                        matchCount = slashMatches.size,
                        loading = uiState.slashLoading,
                        unavailable = uiState.slashUnavailable,
                        truncated = uiState.slashTruncated,
                        activeIndex = slashIndex,
                        onSelect = { entry -> selectSlash(filteredSlash.indexOf(entry)) },
                        modifier = Modifier
                            .fillMaxWidth()
                            .padding(horizontal = spacing.medium),
                    )
                }
                Composer(
                    modifier = Modifier.windowInsetsPadding(
                        WindowInsets.systemBars
                            .union(WindowInsets.ime)
                            .only(WindowInsetsSides.Horizontal + WindowInsetsSides.Bottom),
                    ),
                    agentLabel = uiState.title.ifEmpty { uiState.paneId.substringAfter("::") },
                    draft = uiState.composerDraft,
                    sending = uiState.responding,
                    canControl = uiState.canControl,
                    slashMenuOpen = slashMenuOpen,
                    onSlashKey = { key ->
                        when (key) {
                            Key.Escape -> {
                                dismissedSlashQuery = uiState.composerDraft
                                true
                            }
                            Key.DirectionDown -> {
                                activeSlashIndex = if (slashIndex >= filteredSlash.size - 1) {
                                    0
                                } else {
                                    slashIndex + 1
                                }
                                true
                            }
                            Key.DirectionUp -> {
                                activeSlashIndex = if (slashIndex <= 0) {
                                    filteredSlash.size - 1
                                } else {
                                    slashIndex - 1
                                }
                                true
                            }
                            Key.Enter, Key.Tab -> {
                                if (slashIndex >= 0) {
                                    selectSlash(slashIndex)
                                    true
                                } else {
                                    false
                                }
                            }
                            else -> false
                        }
                    },
                    canAttach = uiState.canAttach,
                    attachments = uiState.attachments,
                    uploadStatus = uiState.uploadStatus,
                    uploadError = uiState.uploadError,
                    onDraftChange = onDraftChange,
                    onSend = onSendPrompt,
                    onPickAttachments = onPickAttachments,
                    onRemoveAttachment = onRemoveAttachment,
                    onClearAttachments = onClearAttachments,
                    onRestartAttachments = onRestartAttachments,
                )
            }
        },
    ) { innerPadding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(innerPadding),
        ) {
            if (findOpen) {
                TerminalFindBar(
                    query = findQuery,
                    onQueryChange = { findQuery = it },
                    matchCount = matchCount,
                    activeIndex = activeFindIndex,
                    truncated = false,
                    onStep = { delta -> revealFindMatch(activeFindIndex + delta) },
                    onClose = ::closeFind,
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = spacing.medium)
                        .padding(bottom = spacing.small),
                )
            }
            // The oracle renders its `conversation-warning` rows above the
            // scrollable transcript — fixed placement, same template order.
            if (!historyLoadingEmpty && !historyUnavailableEmpty) {
                FeedHistoryWarnings(
                    browseState = uiState.browseState,
                    browseProgress = uiState.browseProgress,
                    diagnostics = uiState.historyDiagnostics,
                    preparationPaused = uiState.preparationPaused,
                    hasMoreHistory = uiState.hasMoreHistory,
                    historyLoading = uiState.historyLoading,
                    entriesEmpty = uiState.entries.isEmpty(),
                    historyError = uiState.historyError,
                    historyErrorCode = uiState.historyErrorCode,
                    historyErrorRetryable = uiState.historyErrorRetryable,
                    onReloadHistory = onReloadHistory,
                    onRecoverHistory = onRecoverHistory,
                    onCancelPreparation = onCancelPreparation,
                    onContinuePreparation = onContinuePreparation,
                )
            }
            LazyColumn(
                state = listState,
                modifier = Modifier.weight(1f).fillMaxWidth(),
                contentPadding = PaddingValues(
                    top = spacing.small,
                    bottom = spacing.small,
                    start = spacing.medium,
                    end = spacing.medium,
                ),
                verticalArrangement = Arrangement.spacedBy(spacing.small),
            ) {
                if (historyLoadingEmpty) {
                    // `historyStatusText || 'Loading conversation…'`
                    item(key = "history-loading") {
                        Text(
                            "Loading conversation…",
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.fillMaxWidth(),
                            textAlign = TextAlign.Center,
                        )
                    }
                }
                if (loadOlderVisible) {
                    item(key = "load-older") {
                        TextButton(
                            onClick = onLoadOlder,
                            enabled = !uiState.historyLoading && !uiState.preparationPaused,
                            modifier = Modifier.fillMaxWidth(),
                        ) {
                            Text(
                                if (uiState.historyLoading) {
                                    "Loading…"
                                } else if (uiState.browseState == ConversationBrowseState.FAILED) {
                                    "Retry loading"
                                } else {
                                    "Load older turns"
                                },
                            )
                        }
                    }
                }
                if (historyUnavailableEmpty) {
                    item(key = "no-history") {
                        Text(
                            if (uiState.historyPageAvailable) {
                                "This agent does not report conversation history."
                            } else {
                                uiState.historyUnavailableReason.ifEmpty {
                                    "Conversation history is unavailable."
                                }
                            },
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            textAlign = TextAlign.Center,
                            modifier = Modifier.fillMaxWidth(),
                        )
                    }
                }
                items(visibleEntries, key = { it.id }) { entry ->
                    ConversationEntryRow(
                        entry = entry,
                        highlight = searchQuery.trim(),
                        onCopyResponse = {
                            onCopyResponse(entry.text) { text ->
                                scope.launch {
                                    clipboard.setClipEntry(
                                        ClipEntry(
                                            android.content.ClipData.newPlainText(
                                                "agent response",
                                                text,
                                            ),
                                        ),
                                    )
                                }
                            }
                        },
                        modifier = Modifier.animateItem(
                            placementSpec = spring(
                                stiffness = Spring.StiffnessMediumLow,
                                visibilityThreshold = IntOffset.VisibilityThreshold,
                            ),
                        ),
                    )
                }
                if (searching && visibleEntries.isEmpty() && uiState.entries.isNotEmpty()) {
                    item(key = "find-empty") {
                        Text(
                            "No loaded turns match “${searchQuery.trim()}”.",
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
                uiState.blocked?.let { agent ->
                    item(key = "blocker") {
                        FeedBlockerCard(
                            agent = agent,
                            interaction = uiState.blockedInteraction,
                            draft = uiState.questionDraft,
                            enabled = uiState.canControl && !uiState.responding,
                            onRespond = onRespond,
                            onDraftChange = onQuestionDraftChange,
                            onSubmitQuestion = onSubmitQuestion,
                            onPreviousQuestion = { onNavigateQuestion("previous") },
                            onClarifyQuestion = onClarifyQuestion,
                            onOpenTerminal = onOpenTerminal,
                            modifier = Modifier.animateItem(
                                placementSpec = spring(
                                    stiffness = Spring.StiffnessMediumLow,
                                    visibilityThreshold = IntOffset.VisibilityThreshold,
                                ),
                            ),
                        )
                    }
                }
                if (uiState.working) {
                    item(key = "working") { WorkingRow() }
                }
            }
        }
    }
}

@Composable
private fun ConversationEntryRow(
    entry: ConversationEntry,
    highlight: String,
    onCopyResponse: () -> Unit,
    modifier: Modifier = Modifier,
) {
    when (entry.role) {
        ConversationRole.USER -> UserPromptBubble(entry.text, modifier)
        ConversationRole.ASSISTANT -> Row(modifier = modifier.fillMaxWidth()) {
            Column(
                verticalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.small),
                modifier = Modifier.weight(1f),
            ) {
                if (entry.text.isNotEmpty()) {
                    FeedMarkdown(entry.text, highlight = highlight)
                }
                entry.tools.forEachIndexed { index, tool ->
                    // `${tool.id || tool.name}:${index}` — the oracle's key.
                    key(tool.id.ifEmpty { tool.name } + ":$index") {
                        FeedToolCard(tool)
                    }
                }
            }
            AssistantEntryActions(entry = entry, onCopyResponse = onCopyResponse)
        }
    }
}

/** Per-entry overflow — `copy_agent_response` with the entry as fallback. */
@Composable
private fun AssistantEntryActions(
    entry: ConversationEntry,
    onCopyResponse: () -> Unit,
) {
    var menuOpen by remember { mutableStateOf(false) }
    Box {
        IconButton(onClick = { menuOpen = true }) {
            Icon(
                Icons.Default.MoreVert,
                contentDescription = "Entry actions",
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.size(20.dp),
            )
        }
        DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
            DropdownMenuItem(
                text = { Text("Copy response") },
                enabled = entry.text.isNotBlank(),
                onClick = {
                    menuOpen = false
                    onCopyResponse()
                },
            )
        }
    }
}

/** User prompt — the mockup's brighter periwinkle bubble. */
@Composable
private fun UserPromptBubble(text: String, modifier: Modifier = Modifier) {
    val colors = LerdrTheme.extendedColors
    Surface(
        color = colors.chatContainer,
        contentColor = colors.onChatContainer,
        shape = MaterialTheme.shapes.large,
        modifier = modifier.fillMaxWidth(),
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

/** Live "thinking" row — morphing loader while the agent works. */
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
            "Working…",
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.weight(1f),
        )
        Text(
            "live",
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.primary,
        )
    }
}

@Composable
private fun Composer(
    agentLabel: String,
    draft: String,
    sending: Boolean,
    canControl: Boolean,
    slashMenuOpen: Boolean,
    onSlashKey: (Key) -> Boolean,
    canAttach: Boolean,
    attachments: AttachmentBatch,
    uploadStatus: String,
    uploadError: Boolean,
    onDraftChange: (String) -> Unit,
    onSend: () -> Unit,
    onPickAttachments: () -> Unit,
    onRemoveAttachment: (String) -> Unit,
    onClearAttachments: () -> Unit,
    onRestartAttachments: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val spacing = LerdrTheme.spacing
    val controlsLocked = sending || attachments.uploading || !canControl
    Surface(color = MaterialTheme.colorScheme.surface, modifier = modifier) {
        Column(modifier = Modifier.fillMaxWidth()) {
            if (attachments.items.isNotEmpty() || attachments.issue != null) {
                AttachmentTray(
                    attachments = attachments,
                    controlsLocked = controlsLocked,
                    onRemoveAttachment = onRemoveAttachment,
                    onClearAttachments = onClearAttachments,
                    onRestartAttachments = onRestartAttachments,
                )
            }
            if (uploadStatus.isNotEmpty()) {
                Text(
                    uploadStatus,
                    style = MaterialTheme.typography.labelMedium,
                    color = if (uploadError) {
                        LerdrTheme.extendedColors.danger
                    } else {
                        MaterialTheme.colorScheme.onSurfaceVariant
                    },
                    modifier = Modifier.padding(horizontal = spacing.medium),
                )
            }
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(spacing.medium),
            ) {
                if (canAttach) {
                    IconButton(onClick = onPickAttachments, enabled = !controlsLocked) {
                        Icon(
                            Icons.Default.AttachFile,
                            contentDescription = "Attach files",
                            tint = MaterialTheme.colorScheme.primary,
                        )
                    }
                    Spacer(Modifier.width(spacing.extraSmall))
                }
                OutlinedTextField(
                    value = draft,
                    onValueChange = onDraftChange,
                    placeholder = {
                        Text(
                            if (canControl) {
                                "Message $agentLabel…"
                            } else {
                                "Read-only — this device cannot reply"
                            },
                        )
                    },
                    enabled = canControl && !sending,
                    singleLine = false,
                    maxLines = 4,
                    keyboardOptions = KeyboardOptions(
                        // Prose input — suggestions/autocorrect stay on; the
                        // terminal bar disables them, the chat one keeps them.
                        keyboardType = KeyboardType.Text,
                        imeAction = ImeAction.Send,
                    ),
                    keyboardActions = KeyboardActions(onSend = { onSend() }),
                    shape = MaterialTheme.shapes.extraLarge,
                    modifier = Modifier
                        .weight(1f)
                        .onPreviewKeyEvent { event ->
                            // Oracle keydown — menu keys only while it is open.
                            if (!slashMenuOpen || event.type != KeyEventType.KeyDown) {
                                return@onPreviewKeyEvent false
                            }
                            onSlashKey(event.key)
                        },
                )
                Spacer(Modifier.width(spacing.small))
                val sendable = draft.isNotBlank() ||
                    attachments.items.any { it.state == AttachmentItemState.SELECTED }
                IconButton(
                    onClick = onSend,
                    enabled = canControl && !controlsLocked && sendable,
                ) {
                    Icon(
                        Icons.AutoMirrored.Filled.Send,
                        contentDescription = "Send",
                        tint = MaterialTheme.colorScheme.primary,
                    )
                }
            }
        }
    }
}

/**
 * The chip row — one [InputChip] per batch item (state icon + name + size),
 * plus batch actions: Restart for interrupted items, Clear for all.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun AttachmentTray(
    attachments: AttachmentBatch,
    controlsLocked: Boolean,
    onRemoveAttachment: (String) -> Unit,
    onClearAttachments: () -> Unit,
    onRestartAttachments: () -> Unit,
) {
    val spacing = LerdrTheme.spacing
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = spacing.medium),
        verticalArrangement = Arrangement.spacedBy(spacing.extraSmall),
    ) {
        FlowRow(
            horizontalArrangement = Arrangement.spacedBy(spacing.small),
            modifier = Modifier.fillMaxWidth(),
        ) {
            attachments.items.forEach { item ->
                AttachmentChip(
                    item = item,
                    controlsLocked = controlsLocked,
                    onRemove = { onRemoveAttachment(item.clientId) },
                )
            }
        }
        attachments.issue?.let { issue ->
            Text(
                attachmentIssueText(issue),
                style = MaterialTheme.typography.labelMedium,
                color = LerdrTheme.extendedColors.danger,
            )
        }
        Row(horizontalArrangement = Arrangement.spacedBy(spacing.small)) {
            if (attachments.canRestart) {
                TextButton(onClick = onRestartAttachments, enabled = !controlsLocked) {
                    Text("Restart upload")
                }
            }
            TextButton(onClick = onClearAttachments, enabled = !controlsLocked) {
                Text("Clear")
            }
        }
    }
}

@Composable
private fun AttachmentChip(
    item: AttachmentItem,
    controlsLocked: Boolean,
    onRemove: () -> Unit,
) {
    val label = buildString {
        append(item.name.ifEmpty { "attachment" })
        if (item.bytes >= 0) {
            append(" · ")
            append(formatBytes(item.bytes))
        }
    }
    InputChip(
        selected = item.state == AttachmentItemState.READY,
        onClick = onRemove,
        enabled = !controlsLocked,
        label = { Text(label, maxLines = 1, overflow = TextOverflow.Ellipsis) },
        leadingIcon = {
            when (item.state) {
                AttachmentItemState.UPLOADING -> CircularProgressIndicator(
                    progress = { item.progress },
                    modifier = Modifier.size(16.dp),
                    strokeWidth = 2.dp,
                )
                AttachmentItemState.READY -> Icon(
                    Icons.Default.CheckCircle,
                    contentDescription = "Uploaded",
                    tint = MaterialTheme.colorScheme.primary,
                    modifier = Modifier.size(16.dp),
                )
                AttachmentItemState.REJECTED,
                AttachmentItemState.INTERRUPTED,
                -> Icon(
                    Icons.Default.Warning,
                    contentDescription = item.issue?.let { attachmentIssueText(it) }
                        ?: "Attachment failed",
                    tint = LerdrTheme.extendedColors.danger,
                    modifier = Modifier.size(16.dp),
                )
                AttachmentItemState.SELECTED -> Icon(
                    Icons.Default.AttachFile,
                    contentDescription = null,
                    modifier = Modifier.size(16.dp),
                )
            }
        },
        trailingIcon = {
            Icon(
                Icons.Default.Close,
                contentDescription = "Remove ${item.name}",
                modifier = Modifier.size(16.dp),
            )
        },
    )
}

private fun formatBytes(bytes: Long): String = when {
    bytes >= 1L shl 20 -> "%.1f MiB".format(bytes.toDouble() / (1L shl 20))
    bytes >= 1L shl 10 -> "%.1f KiB".format(bytes.toDouble() / (1L shl 10))
    else -> "$bytes B"
}

@PreviewLightDark
@Composable
private fun AgentFeedContentPreview() {
    LerdrTheme {
        AgentFeedContent(
            uiState = FeedUiState(
                paneId = "sd::%1",
                title = "claude",
                provider = "claude",
                breadcrumb = "lerdr · main · sd",
                statusLabel = "working",
                working = true,
                connected = true,
                canControl = true,
                historyAvailable = true,
            ),
            onOpenTerminal = {},
            onOpenFiles = {},
            onBack = {},
            onDraftChange = {},
            onSendPrompt = {},
            onRespond = { _, _ -> },
            onQuestionDraftChange = {},
            onSubmitQuestion = {},
            onNavigateQuestion = {},
            onClarifyQuestion = {},
            onCopyResponse = { _, _ -> },
            onClearError = {},
            onLoadOlder = {},
            onReloadHistory = {},
            onRecoverHistory = {},
            onCancelPreparation = {},
            onContinuePreparation = {},
            onPickAttachments = {},
            onRemoveAttachment = {},
            onClearAttachments = {},
            onRestartAttachments = {},
        )
    }
}
