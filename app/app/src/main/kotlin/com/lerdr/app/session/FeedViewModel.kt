package com.lerdr.app.session

import androidx.compose.runtime.Immutable
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import lerdr.core.conversation.ConversationEntry
import lerdr.core.conversation.ConversationPageRequest
import lerdr.core.data.DraftStore
import lerdr.core.data.composerDraftIdentity
import lerdr.core.model.UploadAttachment
import lerdr.core.store.Agent
import lerdr.core.store.AgentStatusGroup
import lerdr.core.store.RelayStatus
import lerdr.core.store.agentStatusGroup
import lerdr.core.store.rawBlocked

/** Everything Feed mode renders — conversation page, blocker card, composer. */
@Immutable
data class FeedUiState(
    val paneId: String,
    /** Agent display name for the top bar. */
    val title: String = "",
    /** "lerdr · main · sd" — workspace/context breadcrumb. */
    val breadcrumb: String = "",
    /** Normalized agent identity ("claude", "codex"…) — top-bar logo. */
    val provider: String? = null,
    val statusLabel: String = "",
    val working: Boolean = false,
    /** Transport liveness for this agent's relay. */
    val connected: Boolean = false,
    val historyAvailable: Boolean = false,
    /** Conversation entries, oldest → newest within the loaded window. */
    val entries: List<ConversationEntry> = emptyList(),
    val historyLoading: Boolean = false,
    val historyError: String? = null,
    val hasMoreHistory: Boolean = false,
    /** Non-null while this agent blocks — the blocker card renders it. */
    val blocked: Agent? = null,
    /** Send/answer in flight — composer + card buttons disable. */
    val responding: Boolean = false,
    val composerDraft: String = "",
    /** Transient action failure — rendered as a snackbar/inline error. */
    val lastError: String? = null,
    /** The attach affordance — connected, exact target, not blocked. */
    val canAttach: Boolean = false,
    /** Attachment tray snapshot — empty batch renders nothing. */
    val attachments: AttachmentBatch = AttachmentBatch(),
    /** Oracle `uploadStatus` — attachment progress/result line under the tray. */
    val uploadStatus: String = "",
    /** Oracle `uploadError` — renders [uploadStatus] in the error tone. */
    val uploadError: Boolean = false,
)

/**
 * Feed-mode mutation point — history pages, prompt submission, blocker
 * answers, composer drafts. All transport calls go through
 * [SessionRepository]; the screen stays a pure function of [FeedUiState].
 */
@OptIn(ExperimentalCoroutinesApi::class)
class FeedViewModel(
    private val paneId: String,
    private val sessions: SessionRepository,
    private val drafts: DraftStore,
    private val uploads: AttachmentUploads,
) : ViewModel() {

    private val relayId = paneId.substringBefore("::")

    /** Stale-run guard for picker coroutines — the oracle's controller swap. */
    private var attachmentGeneration = 0

    /** `attachmentCancelRequested` — a canceled upload reads as info, not error. */
    private var attachmentCancelRequested = false

    private data class FeedLocal(
        val entries: List<ConversationEntry> = emptyList(),
        val loading: Boolean = false,
        val error: String? = null,
        val hasMore: Boolean = false,
        val nextCursor: String = "",
        val draft: String = "",
        val sending: Boolean = false,
        val lastError: String? = null,
        /** `uploadingAttachment` — an upload run is in flight. */
        val uploadingAttachments: Boolean = false,
        val uploadStatus: String = "",
        val uploadError: Boolean = false,
    )

    private val local = MutableStateFlow(FeedLocal())

    val uiState: StateFlow<FeedUiState> = combine(
        sessions.agent(paneId),
        sessions.connection(relayId),
        sessions.responding,
        uploads.state(paneId),
        local,
    ) { agent, connection, responding, attachments, local ->
        FeedUiState(
            paneId = paneId,
            title = agent?.name ?: agent?.agent ?: paneId.substringAfter("::"),
            provider = agent?.agent?.takeIf { it.isNotEmpty() },
            breadcrumb = breadcrumbOf(agent),
            statusLabel = agent?.status ?: "",
            working = agentStatusGroup(agent) == AgentStatusGroup.WORKING,
            connected = connection?.status == RelayStatus.CONNECTED,
            historyAvailable = agent?.conversationHistoryAvailable == true,
            entries = local.entries,
            historyLoading = local.loading,
            historyError = local.error,
            hasMoreHistory = local.hasMore,
            blocked = agent?.takeIf { rawBlocked(it) },
            responding = paneId in responding || local.sending,
            composerDraft = local.draft,
            lastError = local.lastError,
            // `attachmentController(agent)` gate: exact target tuple + live
            // transport + the oracle's `inputLocked` (blocked) analogue.
            canAttach = connection?.status == RelayStatus.CONNECTED &&
                agent != null && !rawBlocked(agent) && agent.wireTarget() != null,
            attachments = attachments,
            uploadStatus = local.uploadStatus,
            uploadError = local.uploadError,
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), FeedUiState(paneId))

    init {
        // Composer drafts persist per pane identity — reload once the agent
        // row supplies the identity tuple.
        viewModelScope.launch {
            sessions.agent(paneId)
                .filterNotNull()
                .map { draftIdentity(it) }
                .distinctUntilChanged()
                .flatMapLatest { drafts.draft(it) }
                .collect { draft ->
                    local.value = local.value.copy(draft = draft?.text.orEmpty())
                }
        }
        loadHistory()
    }

    /** First page — newest entries — or a PREPARING/FAILED error row. */
    fun loadHistory() {
        viewModelScope.launch {
            local.value = local.value.copy(loading = true, error = null)
            try {
                val page = sessions.conversationPage(paneId)
                local.value = local.value.copy(
                    loading = false,
                    entries = page.entries,
                    hasMore = page.hasMore,
                    nextCursor = page.nextCursor,
                    error = page.error?.message
                        ?: page.reason.takeIf { !page.available },
                )
            } catch (failure: Exception) {
                local.value = local.value.copy(
                    loading = false,
                    error = failure.message ?: "History unavailable",
                )
            }
        }
    }

    /** Older page — prepends entries, cursor advances toward the past. */
    fun loadOlderHistory() {
        val cursor = local.value.nextCursor
        if (cursor.isEmpty() || local.value.loading) return
        viewModelScope.launch {
            local.value = local.value.copy(loading = true, error = null)
            try {
                val page = sessions.conversationPage(
                    paneId,
                    ConversationPageRequest(cursor = cursor),
                )
                local.value = local.value.copy(
                    loading = false,
                    entries = page.entries + local.value.entries,
                    hasMore = page.hasMore,
                    nextCursor = page.nextCursor,
                )
            } catch (failure: Exception) {
                local.value = local.value.copy(
                    loading = false,
                    error = failure.message ?: "Could not load older history",
                )
            }
        }
    }

    fun onDraftChange(text: String) {
        local.value = local.value.copy(draft = text)
        val agent = sessions.agents.value.firstOrNull { it.paneId == paneId } ?: return
        viewModelScope.launch { drafts.save(draftIdentity(agent), text) }
    }

    // ── attachments ───────────────────────────────────────────────────

    /**
     * `filesSelected` — the SAF picker callback: cancels a live/staged
     * batch (the oracle swaps controllers), seeds the new one, then
     * launches the upload run.
     */
    fun selectAttachments(uris: List<String>) {
        if (uris.isEmpty() || local.value.sending || local.value.uploadingAttachments) return
        val generation = ++attachmentGeneration
        attachmentCancelRequested = false
        viewModelScope.launch {
            try {
                if (uploads.itemsNow(paneId).isNotEmpty() ||
                    uploads.state(paneId).value.uploading
                ) {
                    runCatching { uploads.cancel(paneId) }
                }
                uploads.select(paneId, uris)
            } catch (failure: AttachmentIssueException) {
                setUploadStatus(attachmentIssueText(failure.issue), error = true)
                return@launch
            } catch (failure: Exception) {
                setUploadStatus(
                    failure.message ?: "Attachments could not be uploaded.",
                    error = true,
                )
                return@launch
            }
            if (generation != attachmentGeneration) return@launch
            runUpload(generation)
        }
    }

    /** `handleRemove` — one chip's remove action. */
    fun removeAttachment(clientId: String) {
        try {
            uploads.remove(paneId, clientId)
        } catch (failure: AttachmentIssueException) {
            setUploadStatus(attachmentIssueText(failure.issue), error = true)
        }
    }

    /**
     * `handleClearAttachments` — dismiss-all: kills any live upload,
     * sends `upload_cancel` for a staged session, empties the tray.
     */
    fun clearAttachments() {
        val generation = ++attachmentGeneration
        attachmentCancelRequested = true
        viewModelScope.launch {
            try {
                uploads.cancel(paneId)
                if (generation == attachmentGeneration) {
                    // The canceled run's `finally` skips its cleanup on the
                    // generation bump — clear the flag here instead.
                    local.value = local.value.copy(
                        uploadingAttachments = false,
                        uploadStatus = "Attachments canceled.",
                        uploadError = false,
                    )
                }
            } catch (failure: AttachmentIssueException) {
                if (generation == attachmentGeneration) {
                    local.value = local.value.copy(uploadingAttachments = false)
                    setUploadStatus(attachmentIssueText(failure.issue), error = true)
                }
            } catch (failure: Exception) {
                if (generation == attachmentGeneration) {
                    local.value = local.value.copy(uploadingAttachments = false)
                    setUploadStatus(
                        failure.message ?: "The attachment could not be uploaded.",
                        error = true,
                    )
                }
            }
        }
    }

    /** `restartAttachmentUpload` — restarts INTERRUPTED items from the beginning. */
    fun restartAttachments() {
        if (local.value.uploadingAttachments || local.value.sending) return
        val generation = ++attachmentGeneration
        attachmentCancelRequested = false
        viewModelScope.launch {
            local.value = local.value.copy(
                uploadingAttachments = true,
                uploadStatus = "Restarting interrupted files from the beginning…",
                uploadError = false,
            )
            try {
                val ready = uploads.restart(paneId)
                if (generation != attachmentGeneration) return@launch
                appendUploadedAttachments(ready)
            } catch (cancelled: CancellationException) {
                if (generation != attachmentGeneration) return@launch
                setUploadStatus("Attachments could not be restarted.", error = true)
            } catch (failure: AttachmentIssueException) {
                if (generation == attachmentGeneration) {
                    setUploadStatus(attachmentIssueText(failure.issue), error = true)
                }
            } catch (failure: Exception) {
                if (generation == attachmentGeneration) {
                    setUploadStatus(
                        failure.message ?: "Attachments could not be restarted.",
                        error = true,
                    )
                }
            } finally {
                if (generation == attachmentGeneration) {
                    local.value = local.value.copy(uploadingAttachments = false)
                }
            }
        }
    }

    /** The `upload` + `append` half of the oracle's `filesSelected`. */
    private suspend fun runUpload(generation: Int) {
        val count = uploads.itemsNow(paneId).count {
            it.state == AttachmentItemState.SELECTED
        }
        local.value = local.value.copy(
            uploadingAttachments = true,
            uploadStatus = "Uploading $count attachment${if (count == 1) "" else "s"}…",
            uploadError = false,
        )
        try {
            val ready = uploads.upload(paneId)
            if (generation != attachmentGeneration) return
            appendUploadedAttachments(ready)
        } catch (cancelled: CancellationException) {
            if (generation != attachmentGeneration) return
            if (attachmentCancelRequested) {
                setUploadStatus("Attachments canceled.", error = false)
            } else {
                setUploadStatus("The attachment could not be uploaded.", error = true)
            }
        } catch (failure: AttachmentIssueException) {
            if (generation == attachmentGeneration) {
                setUploadStatus(attachmentIssueText(failure.issue), error = true)
            }
        } catch (failure: Exception) {
            if (generation != attachmentGeneration) return
            setUploadStatus(
                failure.message ?: "The attachment could not be uploaded.",
                error = true,
            )
        } finally {
            if (generation == attachmentGeneration) {
                local.value = local.value.copy(uploadingAttachments = false)
            }
        }
    }

    /**
     * `appendUploadedAttachments` — `Attachment: <ref>` lines into the
     * composer draft: a leading `\n` only when the draft has content that
     * doesn't already end in one, and a trailing `\n` after the last ref.
     * Persisted like a manual edit.
     */
    private fun appendUploadedAttachments(ready: List<UploadAttachment>) {
        val rejected = uploads.itemsNow(paneId).count {
            it.state == AttachmentItemState.REJECTED
        }
        val refs = ready.mapNotNull { it.ref }.filter { it.isNotEmpty() }
        if (refs.isEmpty()) {
            setUploadStatus(
                when {
                    attachmentCancelRequested -> "Attachment upload canceled."
                    rejected > 0 -> "No selected attachments passed validation."
                    else -> "No attachments were uploaded."
                },
                error = !attachmentCancelRequested,
            )
            return
        }
        val draft = local.value.draft
        val prefix = if (draft.isNotEmpty() && !draft.endsWith("\n")) "\n" else ""
        val next = draft + prefix + refs.joinToString("\n") { "Attachment: $it" } + "\n"
        local.value = local.value.copy(draft = next)
        sessions.agents.value.firstOrNull { it.paneId == paneId }?.let { agent ->
            viewModelScope.launch { drafts.save(draftIdentity(agent), next) }
        }
        val names = ready.mapNotNull { it.name }.joinToString(", ")
        setUploadStatus(
            "Attached $names" + if (rejected > 0) "; $rejected rejected" else "",
            error = rejected > 0,
        )
        // All-rejected batches keep their rows for inspection; clean
        // batches clear like the oracle's `attachmentSnapshot = null`.
        if (rejected == 0) uploads.clear(paneId)
    }

    private fun setUploadStatus(text: String, error: Boolean) {
        local.value = local.value.copy(uploadStatus = text, uploadError = error)
    }

    // ── composer send ─────────────────────────────────────────────────

    /** `submit_prompt` — uploads selected attachments, then clears the draft on dispatch. */
    fun sendPrompt() {
        val pendingSelection = uploads.itemsNow(paneId).any {
            it.state == AttachmentItemState.SELECTED
        }
        val text = local.value.draft.trim()
        if ((text.isEmpty() && !pendingSelection) || local.value.sending) return
        val generation = ++attachmentGeneration
        attachmentCancelRequested = false
        viewModelScope.launch {
            local.value = local.value.copy(sending = true, lastError = null)
            try {
                if (pendingSelection) {
                    // `handleSubmit`: upload first, refs land in the draft,
                    // then the draft text (refs included) goes on the wire.
                    val count = uploads.itemsNow(paneId).count {
                        it.state == AttachmentItemState.SELECTED
                    }
                    local.value = local.value.copy(
                        uploadStatus = "Uploading $count attachment${if (count == 1) "" else "s"}…",
                        uploadError = false,
                    )
                    val ready = uploads.upload(paneId)
                    if (generation != attachmentGeneration) return@launch
                    appendUploadedAttachments(ready)
                }
                val outbound = local.value.draft.trim()
                if (outbound.isEmpty()) return@launch
                sessions.submitPrompt(paneId, outbound)
                local.value = local.value.copy(draft = "")
                sessions.agents.value.firstOrNull { it.paneId == paneId }?.let {
                    drafts.clear(draftIdentity(it))
                }
                runCatching { uploads.clear(paneId) }
            } catch (cancelled: CancellationException) {
                // A canceled upload reports through uploadStatus already.
            } catch (failure: AttachmentIssueException) {
                local.value = local.value.copy(
                    uploadStatus = attachmentIssueText(failure.issue),
                    uploadError = true,
                )
            } catch (failure: Exception) {
                local.value = local.value.copy(
                    lastError = failure.message ?: "Prompt failed",
                )
            } finally {
                local.value = local.value.copy(sending = false)
            }
        }
    }

    /** Approval button — index into the agent's options + the label sent. */
    fun respond(index: Int, choice: String) {
        viewModelScope.launch {
            local.value = local.value.copy(lastError = null)
            try {
                sessions.respond(paneId, index, choice)
            } catch (failure: Exception) {
                local.value = local.value.copy(
                    lastError = failure.message ?: "Response failed",
                )
            }
        }
    }

    fun answerQuestion(
        selectedIndices: List<Int>,
        otherSelected: Boolean,
        otherText: String,
    ) {
        val interaction = blockedInteraction() ?: return
        viewModelScope.launch {
            local.value = local.value.copy(lastError = null)
            try {
                sessions.answerQuestion(
                    paneId, interaction, selectedIndices, otherSelected, otherText,
                )
            } catch (failure: Exception) {
                local.value = local.value.copy(
                    lastError = failure.message ?: "Answer failed",
                )
            }
        }
    }

    fun navigateQuestion(direction: String) {
        val interaction = blockedInteraction() ?: return
        viewModelScope.launch {
            runCatching { sessions.navigateQuestion(paneId, interaction, direction) }
        }
    }

    fun clarifyQuestion() {
        val interaction = blockedInteraction() ?: return
        viewModelScope.launch {
            runCatching { sessions.clarifyQuestion(paneId, interaction) }
        }
    }

    private fun blockedInteraction() =
        sessions.agents.value.firstOrNull { it.paneId == paneId }
            ?.takeIf { rawBlocked(it) }?.interaction

    private fun draftIdentity(agent: Agent): String {
        val paneIdentity = agent.terminalId?.takeIf { it.isNotEmpty() }
            ?: listOf(agent.workspaceId, agent.tabId, agent.rawPaneId)
                .filter { it.isNotEmpty() }
                .joinToString(":")
        return composerDraftIdentity(
            relayId,
            paneIdentity,
            agent.agent.orEmpty(),
            agent.cwd.orEmpty(),
        )
    }

    private fun breadcrumbOf(agent: Agent?): String {
        if (agent == null) return ""
        val project = agent.project ?: agent.cwd?.substringAfterLast('/')
        return listOfNotNull(
            project?.takeIf { it.isNotEmpty() },
            agent.sessionName?.takeIf { it.isNotEmpty() },
            agent.relayLabel.takeIf { it.isNotEmpty() },
        ).joinToString(" · ")
    }

    /** `controller.onDestroy` — best-effort `upload_cancel`, then drop the batch. */
    override fun onCleared() {
        uploads.discard(paneId)
        super.onCleared()
    }
}
