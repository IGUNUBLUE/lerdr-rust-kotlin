package com.lerdr.app.session

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
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
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.InputChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.PreviewLightDark
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lerdr.app.di.AppEntryPoint
import com.lerdr.core.designsystem.components.LerdrButtonGroup
import com.lerdr.core.designsystem.components.LerdrButtonGroupItem
import com.lerdr.core.designsystem.components.LerdrLoadingIndicator
import com.lerdr.core.designsystem.theme.LerdrTextStyles
import com.lerdr.core.designsystem.theme.LerdrTheme
import dagger.hilt.android.EntryPointAccessors
import lerdr.core.conversation.ConversationEntry
import lerdr.core.conversation.ConversationRole
import lerdr.core.model.BlockedMessage
import lerdr.core.store.Agent
import lerdr.core.store.attentionKind

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
        onAnswerOption = { index ->
            viewModel.answerQuestion(listOf(index), otherSelected = false, otherText = "")
        },
        onLoadOlder = viewModel::loadOlderHistory,
        onRetryHistory = viewModel::loadHistory,
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
    onAnswerOption: (Int) -> Unit,
    onLoadOlder: () -> Unit,
    onRetryHistory: () -> Unit,
    onPickAttachments: () -> Unit,
    onRemoveAttachment: (String) -> Unit,
    onClearAttachments: () -> Unit,
    onRestartAttachments: () -> Unit,
) {
    val spacing = LerdrTheme.spacing
    val listState = rememberLazyListState()
    LaunchedEffect(uiState.entries.size) {
        if (uiState.entries.isNotEmpty()) {
            listState.animateScrollToItem(uiState.entries.lastIndex)
        }
    }
    Scaffold(
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
                tabsPaneId = tabsPaneId,
                onSelectTab = { onSelectTab(it.paneId) },
            )
        },
        bottomBar = {
            Composer(
                modifier = Modifier.windowInsetsPadding(
                    WindowInsets.systemBars
                        .union(WindowInsets.ime)
                        .only(WindowInsetsSides.Horizontal + WindowInsetsSides.Bottom),
                ),
                agentLabel = uiState.title.ifEmpty { uiState.paneId.substringAfter("::") },
                draft = uiState.composerDraft,
                sending = uiState.responding,
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
        },
    ) { innerPadding ->
        LazyColumn(
            state = listState,
            modifier = Modifier.fillMaxSize(),
            contentPadding = PaddingValues(
                top = innerPadding.calculateTopPadding() + spacing.small,
                bottom = innerPadding.calculateBottomPadding() + spacing.small,
                start = spacing.medium,
                end = spacing.medium,
            ),
            verticalArrangement = Arrangement.spacedBy(spacing.small),
        ) {
            if (uiState.hasMoreHistory) {
                item(key = "load-older") {
                    TextButton(
                        onClick = onLoadOlder,
                        enabled = !uiState.historyLoading,
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Text(if (uiState.historyLoading) "Loading…" else "Load older")
                    }
                }
            }
            if (!uiState.historyAvailable && uiState.entries.isEmpty()) {
                item(key = "no-history") {
                    Text(
                        uiState.historyError
                            ?: "This agent does not report conversation history.",
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            items(uiState.entries, key = { it.id }) { entry ->
                ConversationEntryRow(entry)
            }
            uiState.blocked?.let { agent ->
                item(key = "blocker") {
                    BlockerCard(
                        agent = agent,
                        enabled = !uiState.responding,
                        onRespond = onRespond,
                        onAnswerOption = onAnswerOption,
                    )
                }
            }
            if (uiState.working) {
                item(key = "working") { WorkingRow() }
            }
        }
    }
}

@Composable
private fun ConversationEntryRow(entry: ConversationEntry) {
    when (entry.role) {
        ConversationRole.USER -> UserPromptBubble(entry.text)
        ConversationRole.ASSISTANT -> Column(
            verticalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.small),
        ) {
            if (entry.text.isNotEmpty()) {
                Text(entry.text, style = MaterialTheme.typography.bodyMedium)
            }
            entry.tools.forEach { tool ->
                ToolCallCard(ToolCallUi(tool.name, tool.input.ifEmpty { tool.output }, tool.error))
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

/**
 * The blocker card — pins above the composer until answered. Approvals get
 * the oracle's Allow/Deny-style option buttons (`respond`); questions get
 * single-tap option rows (`answer_question` with that index).
 */
@Composable
private fun BlockerCard(
    agent: Agent,
    enabled: Boolean,
    onRespond: (Int, String) -> Unit,
    onAnswerOption: (Int) -> Unit,
) {
    val colors = LerdrTheme.extendedColors
    val spacing = LerdrTheme.spacing
    val isApproval = attentionKind(agent) == BlockedMessage.ATTENTION_APPROVAL
    val prompt = agent.prompt ?: agent.interaction?.question ?: agent.command ?: ""
    val options = agent.options
        ?: agent.interaction?.options?.map { it.label }.orEmpty()

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
                if (isApproval) "APPROVAL NEEDED" else "QUESTION",
                style = MaterialTheme.typography.labelMedium,
                color = colors.attention,
            )
            if (prompt.isNotEmpty()) {
                Text(prompt, style = MaterialTheme.typography.titleSmall)
            }
            if (isApproval) {
                LerdrButtonGroup(
                    items = options.mapIndexed { index, label ->
                        LerdrButtonGroupItem(
                            label = label,
                            onClick = { onRespond(index, label) },
                            enabled = enabled,
                        )
                    },
                    modifier = Modifier.fillMaxWidth(),
                )
            } else {
                options.forEachIndexed { index, label ->
                    FilledTonalButton(
                        onClick = { onAnswerOption(index) },
                        enabled = enabled,
                        modifier = Modifier.fillMaxWidth(),
                    ) { Text(label) }
                }
            }
        }
    }
}

@Composable
private fun Composer(
    agentLabel: String,
    draft: String,
    sending: Boolean,
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
    val controlsLocked = sending || attachments.uploading
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
                        MaterialTheme.colorScheme.error
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
                    placeholder = { Text("Message $agentLabel…") },
                    enabled = !sending,
                    singleLine = false,
                    maxLines = 4,
                    keyboardOptions = KeyboardOptions(imeAction = ImeAction.Send),
                    keyboardActions = KeyboardActions(onSend = { onSend() }),
                    shape = MaterialTheme.shapes.extraLarge,
                    modifier = Modifier.weight(1f),
                )
                Spacer(Modifier.width(spacing.small))
                val sendable = draft.isNotBlank() ||
                    attachments.items.any { it.state == AttachmentItemState.SELECTED }
                IconButton(onClick = onSend, enabled = !controlsLocked && sendable) {
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
                color = MaterialTheme.colorScheme.error,
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
                    tint = MaterialTheme.colorScheme.error,
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
                historyAvailable = true,
            ),
            onOpenTerminal = {},
            onOpenFiles = {},
            onBack = {},
            onDraftChange = {},
            onSendPrompt = {},
            onRespond = { _, _ -> },
            onAnswerOption = {},
            onLoadOlder = {},
            onRetryHistory = {},
            onPickAttachments = {},
            onRemoveAttachment = {},
            onClearAttachments = {},
            onRestartAttachments = {},
        )
    }
}
