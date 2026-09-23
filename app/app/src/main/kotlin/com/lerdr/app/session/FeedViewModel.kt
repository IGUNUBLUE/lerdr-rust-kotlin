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
import com.lerdr.app.session.feed.QuestionDraft
import com.lerdr.app.session.feed.SlashCommand
import com.lerdr.app.session.feed.SlashCommandCatalog
import com.lerdr.app.session.feed.createQuestionDraft
import com.lerdr.app.session.feed.parseSlashCatalog
import com.lerdr.app.session.feed.questionDraftKey
import com.lerdr.app.session.feed.questionSubmitAllowed
import com.lerdr.app.session.feed.shouldRestoreQuestionDraft
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import lerdr.core.conversation.ConversationEntry
import lerdr.core.conversation.ConversationPageRequest
import lerdr.core.data.DraftStore
import lerdr.core.data.composerDraftIdentity
import lerdr.core.model.BlockedMessage
import lerdr.core.model.CommandResultMessage
import lerdr.core.model.Inbound
import lerdr.core.model.Interaction
import lerdr.core.model.UploadAttachment
import lerdr.core.protocol.LerdrJson
import lerdr.core.store.Agent
import lerdr.core.store.AgentStatusGroup
import lerdr.core.store.RelayConnection
import lerdr.core.store.RelayStatus
import lerdr.core.store.agentStatusGroup
import lerdr.core.store.attentionKind
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
    /**
     * The effective question interaction — a `command_result` override wins
     * over the store row while a pane frame is still catching up, and a
     * resolved id clears to null until the relay unblocks.
     */
    val blockedInteraction: Interaction? = null,
    /** The resolved draft for [blockedInteraction] (dirty drafts persist per interaction id). */
    val questionDraft: QuestionDraft = QuestionDraft(),
    /** Send/answer in flight — composer + card buttons disable. */
    val responding: Boolean = false,
    val composerDraft: String = "",
    /** Transient action failure — rendered as a snackbar/inline error. */
    val lastError: String? = null,
    /** Transient success/info line (oracle `showToast`) — snackbar too. */
    val notice: String? = null,
    /** Oracle `readOnly` — false for reader-role devices; mutes every mutating control. */
    val canControl: Boolean = false,
    /** `agent_response_copy` capability + supported agent profile. */
    val canCopyResponse: Boolean = false,
    /** `list_slash_commands` catalog — composer `/` suggestions. */
    val slashCommands: List<SlashCommand> = emptyList(),
    val slashLoading: Boolean = false,
    /** Fetch failed — the menu falls back to "you can still send this command". */
    val slashUnavailable: Boolean = false,
    val slashTruncated: Boolean = false,
    /** The attach affordance — connected, exact target, not blocked, controller. */
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
        val notice: String? = null,
        /**
         * Dirty question drafts by `paneId::interaction.id` — the oracle's
         * `drafts`/`dirtyDrafts` module maps. Presence marks the draft dirty.
         */
        val questionDrafts: Map<String, QuestionDraft> = emptyMap(),
        /**
         * `applyQuestionInteraction` — an `answer_question`/`navigate_question`
         * `command_result` carries the next interaction before any pane frame
         * lands. Applies only while the store still shows [baseId], so a
         * fresh pane frame wins automatically.
         */
        val interactionOverride: InteractionOverride? = null,
        /** Id of an interaction finished via `confirmed`/`clarify` — hidden till unblocked. */
        val clearedInteractionId: String? = null,
        /** `list_slash_commands` catalog state for the composer popover. */
        val slashCommands: List<SlashCommand> = emptyList(),
        val slashLoading: Boolean = false,
        val slashUnavailable: Boolean = false,
        val slashTruncated: Boolean = false,
        /** `uploadingAttachment` — an upload run is in flight. */
        val uploadingAttachments: Boolean = false,
        val uploadStatus: String = "",
        val uploadError: Boolean = false,
    )

    /** A deferred interaction — applies while the store row still shows [baseId]. */
    private data class InteractionOverride(
        val baseId: String,
        val interaction: Interaction,
    )

    private val local = MutableStateFlow(FeedLocal())

    // Slash catalog state — declared before `init`: the eager combine below
    // reaches `maybeLoadSlashCatalog` during construction, so these fields
    // must already be initialized when it first runs.
    private val slashCache = mutableMapOf<String, SlashCommandCatalog>()
    private var slashFetching: String? = null
    private val slashFailed = mutableSetOf<String>()

    val uiState: StateFlow<FeedUiState> = combine(
        sessions.agent(paneId),
        sessions.connection(relayId),
        sessions.responding,
        uploads.state(paneId),
        local,
    ) { agent, connection, responding, attachments, local ->
        val canControl = sessions.canControl(relayId)
        val blockedAgent = agent?.takeIf { rawBlocked(it) }
        val interaction = effectiveInteraction(blockedAgent, local)
        // The question card hides once its interaction resolves/clears —
        // the agent row stays `blocked` until the relay's next frame.
        val questionHidden = blockedAgent != null &&
            attentionKind(blockedAgent) == BlockedMessage.ATTENTION_QUESTION &&
            interaction == null
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
            blocked = blockedAgent?.takeIf { !questionHidden },
            blockedInteraction = interaction,
            questionDraft = interaction?.let {
                resolveQuestionDraft(it, local.questionDrafts)
            } ?: QuestionDraft(),
            responding = paneId in responding || local.sending,
            composerDraft = local.draft,
            lastError = local.lastError,
            notice = local.notice,
            canControl = canControl,
            canCopyResponse = canControl && agent != null &&
                connection?.capabilities?.contains(AGENT_RESPONSE_COPY_CAPABILITY) == true &&
                responseCopyProfileSupported(agent.agent),
            slashCommands = local.slashCommands,
            slashLoading = local.slashLoading,
            slashUnavailable = local.slashUnavailable,
            slashTruncated = local.slashTruncated,
            // `attachmentController(agent)` gate: exact target tuple + live
            // transport + the oracle's `inputLocked` (blocked) analogue.
            // The reader-role mute applies in the composer's controls lock —
            // `canAttach` itself stays the pure capability predicate the
            // upload tests exercise.
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
        // `loadSlashCommands` — one catalog fetch per agent identity
        // (`agent` + `cwd`), replayed from cache on change.
        viewModelScope.launch {
            combine(sessions.agent(paneId), sessions.connection(relayId)) { a, c -> a to c }
                .collect { (agent, connection) ->
                    maybeLoadSlashCatalog(agent, connection)
                }
        }
        // The oracle's `openAgent`: opening an agent acknowledges the pane
        // (optimistic done→idle + `acknowledge_pane`) — readers skip it.
        viewModelScope.launch {
            if (!sessions.canControl(relayId)) return@launch
            try {
                sessions.acknowledgePane(paneId)
            } catch (_: Exception) {
                // Fire-and-forget like the oracle's void-call — the relay's
                // next agents snapshot owns the truth either way.
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

    /**
     * User edit — writes the dirty draft into the `paneId::interaction.id`
     * slot the oracle's `save()` writes. Selected indices stay a `Set`;
     * [sessions.answerQuestion] sorts them on encode.
     */
    fun updateQuestionDraft(next: QuestionDraft) {
        val interaction = effectiveBlockedInteraction() ?: return
        local.value = local.value.copy(
            questionDrafts = local.value.questionDrafts +
                (questionDraftKey(paneId, interaction) to next),
        )
    }

    /**
     * `answer_question` submit — oracle `QuestionForm.submit`: guards the
     * draft, applies `data.interaction` on `advanced`, clears the card on a
     * final `confirmed`, and treats anything else as unexpected.
     */
    fun submitQuestion() {
        val agent = sessions.agents.value.firstOrNull { it.paneId == paneId }
        val interaction = effectiveInteraction(
            agent?.takeIf { rawBlocked(it) },
            local.value,
        ) ?: return
        val draft = resolveQuestionDraft(interaction, local.value.questionDrafts)
        if (!questionSubmitAllowed(interaction, draft)) {
            local.value = local.value.copy(lastError = "Complete the question first.")
            return
        }
        val submittedKey = questionDraftKey(paneId, interaction)
        val final = interaction.submitLabel != "Next"
        viewModelScope.launch {
            local.value = local.value.copy(lastError = null, notice = null)
            try {
                val result = sessions.answerQuestion(
                    paneId,
                    interaction,
                    draft.selected.sorted(),
                    draft.otherSelected,
                    draft.otherText,
                )
                val fresh = returnedInteraction(result)
                when {
                    result.phase == CommandResultMessage.PHASE_ADVANCED && fresh != null -> {
                        local.value = local.value.copy(
                            questionDrafts = local.value.questionDrafts - submittedKey,
                            interactionOverride = InteractionOverride(interaction.id, fresh),
                            notice = "Answer saved.",
                        )
                    }
                    result.phase == CommandResultMessage.PHASE_CONFIRMED && final -> {
                        local.value = local.value.copy(
                            questionDrafts = local.value.questionDrafts - submittedKey,
                            interactionOverride = null,
                            clearedInteractionId = interaction.id,
                            notice = "Answers submitted.",
                        )
                    }
                    else -> {
                        applyFreshInteraction(interaction.id, fresh)
                        local.value = local.value.copy(
                            lastError = "Unexpected question result.",
                        )
                    }
                }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: Exception) {
                handleQuestionFailure(interaction.id, failure)
            }
        }
    }

    /**
     * `navigate_question` — `"previous"` needs `can_go_back`, `"next"`
     * needs `0 < question_index < question_total`; the relay enforces both.
     */
    fun navigateQuestion(direction: String) {
        val interaction = effectiveBlockedInteraction() ?: return
        viewModelScope.launch {
            local.value = local.value.copy(lastError = null, notice = null)
            try {
                val result = sessions.navigateQuestion(paneId, interaction, direction)
                val fresh = returnedInteraction(result)
                if (result.phase == CommandResultMessage.PHASE_NAVIGATED && fresh != null) {
                    local.value = local.value.copy(
                        interactionOverride = InteractionOverride(interaction.id, fresh),
                        notice = "Opened $direction question.",
                    )
                } else {
                    applyFreshInteraction(interaction.id, fresh)
                    local.value = local.value.copy(
                        lastError = if (result.phase == CommandResultMessage.PHASE_UNCONFIRMED) {
                            "The agent still shows the same question; try again."
                        } else {
                            "No previous question returned."
                        },
                    )
                }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: Exception) {
                handleQuestionFailure(interaction.id, failure)
            }
        }
    }

    /** `clarify_question` — opens question chat; the card clears locally. */
    fun clarifyQuestion() {
        val interaction = effectiveBlockedInteraction() ?: return
        viewModelScope.launch {
            local.value = local.value.copy(lastError = null, notice = null)
            try {
                sessions.clarifyQuestion(paneId, interaction)
                local.value = local.value.copy(
                    interactionOverride = null,
                    clearedInteractionId = interaction.id,
                    notice = "Question chat opened.",
                )
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: Exception) {
                handleQuestionFailure(interaction.id, failure)
            }
        }
    }

    /** Legacy one-tap path — kept for callers that submit a bare index. */
    fun answerQuestion(
        selectedIndices: List<Int>,
        otherSelected: Boolean,
        otherText: String,
    ) {
        val interaction = effectiveBlockedInteraction() ?: return
        viewModelScope.launch {
            local.value = local.value.copy(lastError = null)
            try {
                sessions.answerQuestion(
                    paneId, interaction, selectedIndices, otherSelected, otherText,
                )
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: Exception) {
                handleQuestionFailure(interaction.id, failure)
            }
        }
    }

    /**
     * `copy_agent_response` — the relay's copy transaction returns the
     * rendered reply in `data.text` (controller + capable profile only —
     * it types into the agent's terminal, so readers never call it). When
     * the wire path is unavailable the caller's own entry text is the
     * history-fallback the oracle reads instead.
     */
    fun copyAgentResponse(entryText: String, onCopied: (String) -> Unit) {
        if (!sessions.canControl(relayId) ||
            !uiState.value.canCopyResponse
        ) {
            if (entryText.isNotBlank()) {
                onCopied(entryText)
                local.value = local.value.copy(notice = "Agent response copied.")
            } else {
                local.value = local.value.copy(
                    lastError = "No completed agent response is available to copy.",
                )
            }
            return
        }
        viewModelScope.launch {
            local.value = local.value.copy(lastError = null, notice = null)
            try {
                val result = sessions.copyAgentResponse(paneId)
                val text = ((result.data as? JsonObject)?.get("text") as? JsonPrimitive)
                    ?.contentOrNull.orEmpty()
                if (text.isNotBlank()) {
                    onCopied(text)
                    local.value = local.value.copy(notice = "Agent response copied.")
                } else {
                    local.value = local.value.copy(
                        lastError = "The agent returned no response text.",
                    )
                }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: Exception) {
                // `haltOnCopyFailure` — the relay's failure is the answer.
                local.value = local.value.copy(
                    lastError = failure.message ?: "Could not copy the agent response.",
                )
            }
        }
    }

    /** Snackbar consumed — clear the transient channels. */
    fun clearError() {
        if (local.value.lastError != null || local.value.notice != null) {
            local.value = local.value.copy(lastError = null, notice = null)
        }
    }

    // ── slash commands ────────────────────────────────────────────────

    /**
     * Oracle `loadSlashCommands` — gated on the `slash_commands`
     * capability AND the controller role (the fetch is a mutating-class
     * relay action; readers never open the menu anyway), cached by
     * `agent`+`cwd` identity, one flight at a time.
     */
    private fun maybeLoadSlashCatalog(agent: Agent?, connection: RelayConnection?) {
        if (agent == null || !sessions.canControl(relayId) ||
            connection?.capabilities?.contains(SLASH_COMMANDS_CAPABILITY) != true
        ) {
            return
        }
        val identity = "${agent.agent.orEmpty()}\u0000${agent.cwd.orEmpty()}"
        val cached = slashCache[identity]
        if (cached != null) {
            if (local.value.slashCommands != cached.commands ||
                local.value.slashTruncated != cached.truncated ||
                local.value.slashLoading || local.value.slashUnavailable
            ) {
                local.value = local.value.copy(
                    slashCommands = cached.commands,
                    slashTruncated = cached.truncated,
                    slashLoading = false,
                    slashUnavailable = false,
                )
            }
            return
        }
        if (slashFetching == identity || identity in slashFailed) return
        slashFetching = identity
        local.value = local.value.copy(slashLoading = true, slashUnavailable = false)
        viewModelScope.launch {
            try {
                val target = agent.wireTarget()
                    ?: throw IllegalStateException("This agent has no exact terminal identity.")
                val result = sessions.request(
                    agent.relayId,
                    Inbound(type = "list_slash_commands").withPaneTarget(agent, target),
                    timeoutMs = SLASH_TIMEOUT_MS,
                )
                val catalog = parseSlashCatalog(result.data)
                slashCache[identity] = catalog
                local.value = local.value.copy(
                    slashCommands = catalog.commands,
                    slashTruncated = catalog.truncated,
                    slashLoading = false,
                )
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: Exception) {
                slashFailed += identity
                local.value = local.value.copy(
                    slashLoading = false,
                    slashUnavailable = true,
                )
            } finally {
                slashFetching = null
            }
        }
    }

    // ── question plumbing ─────────────────────────────────────────────

    /** `effectiveInteraction` — the override wins while the store is stale. */
    private fun effectiveInteraction(agent: Agent?, local: FeedLocal): Interaction? {
        if (agent == null) return null
        val storeInteraction = agent.interaction
        if (local.clearedInteractionId != null &&
            storeInteraction?.id == local.clearedInteractionId
        ) {
            return null
        }
        val override = local.interactionOverride
        if (override != null && override.baseId == storeInteraction?.id) {
            return override.interaction
        }
        return storeInteraction
    }

    private fun effectiveBlockedInteraction(): Interaction? = effectiveInteraction(
        sessions.agents.value.firstOrNull { it.paneId == paneId }
            ?.takeIf { rawBlocked(it) },
        local.value,
    )

    /**
     * The oracle's dirty-restore — a stored draft wins when it still
     * submits or when the incoming baseline would not.
     */
    private fun resolveQuestionDraft(
        interaction: Interaction,
        drafts: Map<String, QuestionDraft>,
    ): QuestionDraft {
        val incoming = createQuestionDraft(interaction)
        val cached = drafts[questionDraftKey(paneId, interaction)] ?: return incoming
        return if (shouldRestoreQuestionDraft(interaction, cached, incoming)) {
            cached
        } else {
            incoming
        }
    }

    /** `returnedInteraction` — decode `result.data.interaction` when shaped right. */
    private fun returnedInteraction(result: CommandResultMessage): Interaction? {
        val element = (result.data as? JsonObject)?.get("interaction") ?: return null
        return try {
            LerdrJson.decodeFromJsonElement(Interaction.serializer(), element)
        } catch (_: Exception) {
            null
        }
    }

    /**
     * Oracle `handleQuestionError`. The oracle also applies
     * `error.data.interaction` — Kotlin's `CommandException` drops `data`
     * on failure, so only the message reaches the snackbar.
     */
    private fun handleQuestionFailure(interactionId: String, failure: Exception) {
        local.value = local.value.copy(
            lastError = failure.message ?: "Question failed",
        )
    }

    private fun applyFreshInteraction(baseId: String, fresh: Interaction?) {
        if (fresh == null) return
        local.value = local.value.copy(
            interactionOverride = InteractionOverride(baseId, fresh),
        )
    }

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

    private companion object {
        const val SLASH_COMMANDS_CAPABILITY = "slash_commands"
        const val AGENT_RESPONSE_COPY_CAPABILITY = "agent_response_copy"
        const val SLASH_TIMEOUT_MS = 10_000L
    }
}

/** Oracle `RESPONSE_COPY_AGENT_IDS` — profiles the copy transaction drives. */
private val RESPONSE_COPY_AGENT_IDS = setOf(
    "hermes", "hermesagent",
    "claude", "claudecode", "codex", "openaicodex", "kimi", "kimicode",
    "omp", "ohmypi", "pi", "picodingagent", "qoder", "qodercli",
)

/** Oracle `responseCopyProfileSupported` — normalized (lowercase, no spaces/dashes). */
private fun responseCopyProfileSupported(agentName: String?): Boolean {
    val normalized = agentName.orEmpty().trim()
        .lowercase().replace(Regex("\\s+"), "").replace("-", "")
    return normalized in RESPONSE_COPY_AGENT_IDS
}
