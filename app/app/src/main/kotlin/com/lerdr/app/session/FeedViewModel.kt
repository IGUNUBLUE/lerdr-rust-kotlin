package com.lerdr.app.session

import androidx.compose.runtime.Immutable
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.job
import kotlinx.coroutines.launch
import com.lerdr.app.session.feed.HISTORY_MAX_PREPARATION_POLLS
import com.lerdr.app.session.feed.HISTORY_PREPARATION_INTERVAL_MS
import com.lerdr.app.session.feed.HISTORY_WIRE_PAGE_SIZE
import com.lerdr.app.session.feed.QuestionDraft
import com.lerdr.app.session.feed.SlashCommand
import com.lerdr.app.session.feed.SlashCommandCatalog
import com.lerdr.app.session.feed.createQuestionDraft
import com.lerdr.app.session.feed.mergeHistoryDiagnostics
import com.lerdr.app.session.feed.parseSlashCatalog
import com.lerdr.app.session.feed.questionDraftKey
import com.lerdr.app.session.feed.questionSubmitAllowed
import com.lerdr.app.session.feed.shouldRestoreQuestionDraft
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import lerdr.core.conversation.ConversationBrowseError
import lerdr.core.conversation.ConversationBrowseProgress
import lerdr.core.conversation.ConversationBrowseState
import lerdr.core.conversation.ConversationDiagnostics
import lerdr.core.conversation.ConversationEntry
import lerdr.core.conversation.ConversationPage
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
    /** Oracle `error.message` — the error row text (null when healthy). */
    val historyError: String? = null,
    /** Oracle `errorCode` — drives the Reload-vs-Retry/Continue affordance. */
    val historyErrorCode: String = "",
    /** Oracle `errorRetryable` — whether the error row offers recovery. */
    val historyErrorRetryable: Boolean = false,
    /** Oracle `available` — false when the relay cannot serve this conversation. */
    val historyPageAvailable: Boolean = true,
    /** Oracle `reason` — the unavailable explanation for the empty state. */
    val historyUnavailableReason: String = "",
    val hasMoreHistory: Boolean = false,
    /** Oracle `state` — the browse lifecycle of the loaded window. */
    val browseState: ConversationBrowseState = ConversationBrowseState.READY,
    /** Oracle `progress` — snapshot preparation progress while preparing. */
    val browseProgress: ConversationBrowseProgress? = null,
    /** Oracle `diagnostics` — reader/browser self-report merged across pages. */
    val historyDiagnostics: ConversationDiagnostics = ConversationDiagnostics(),
    /**
     * Oracle `preparationPolls >= HISTORY_MAX_PREPARATION_POLLS` — the
     * preparation poll loop is paused (Cancel) or stalled; the warning row
     * switches from "Preparing history…" + Cancel to "Preparation is
     * paused." + Continue.
     */
    val preparationPaused: Boolean = false,
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

    /** In-flight history demand — the oracle's `demandRunning`/`activeAbort`. */
    private var historyJob: Job? = null

    /** Oracle `manualPreparationPause` — Cancel stops polling without a wire call. */
    private var manualPreparationPause = false

    /** Oracle `preparationProgressKey` — identical progress snapshots stall the loop. */
    private var preparationProgressKey = ""

    private data class FeedLocal(
        val entries: List<ConversationEntry> = emptyList(),
        val loading: Boolean = false,
        val error: String? = null,
        val errorCode: String = "",
        val errorRetryable: Boolean = false,
        /** Page-level `available` — the oracle's `available`/`reason` empty state. */
        val pageAvailable: Boolean = true,
        val pageReason: String = "",
        val hasMore: Boolean = false,
        val nextCursor: String = "",
        /** Oracle `state`/`progress`/`diagnostics` — the browse status surface. */
        val browseState: ConversationBrowseState = ConversationBrowseState.READY,
        val browseProgress: ConversationBrowseProgress? = null,
        val diagnostics: ConversationDiagnostics = ConversationDiagnostics(),
        /** Oracle `preparationPolls` — identical-progress polls; >= max is paused. */
        val preparationPolls: Int = 0,
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

    /**
     * Which lane a history demand serves — the oracle's `initial`/`refresh`
     * (fresh head, replaces the window) vs `older`/`full` (cursorful,
     * prepend-merges). `retry`/`continuePreparation` pick by cursor.
     */
    private enum class HistoryLane { LATEST, OLDER }

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
            historyErrorCode = local.errorCode,
            historyErrorRetryable = local.errorRetryable,
            historyPageAvailable = local.pageAvailable,
            historyUnavailableReason = local.pageReason,
            hasMoreHistory = local.hasMore,
            browseState = local.browseState,
            browseProgress = local.browseProgress,
            historyDiagnostics = local.diagnostics,
            preparationPaused =
                local.preparationPolls >= HISTORY_MAX_PREPARATION_POLLS,
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

    /**
     * First page — newest entries — or a PREPARING/FAILED status surface.
     * Also the oracle's `returnToLatest` ("Reload history"): a cursorless
     * fresh browse that replaces the window, aborting any demand in flight.
     */
    fun loadHistory() {
        historyJob?.cancel()
        manualPreparationPause = false
        preparationProgressKey = ""
        local.value = local.value.copy(
            pageAvailable = true,
            pageReason = "",
            browseState = ConversationBrowseState.READY,
            browseProgress = null,
            diagnostics = ConversationDiagnostics(),
            preparationPolls = 0,
            error = null,
            errorCode = "",
            errorRetryable = false,
            nextCursor = "",
            hasMore = false,
        )
        launchHistoryDemand(HistoryLane.LATEST, cursor = "")
    }

    /**
     * Older page — prepends entries, cursor advances toward the past. The
     * oracle's `demandOlder` refuses while an error or a pause is set; the
     * recover affordances own those states.
     */
    fun loadOlderHistory() {
        val cursor = local.value.nextCursor
        if (local.value.error != null || manualPreparationPause) return
        if (cursor.isEmpty() || !local.value.hasMore) return
        if (historyJob?.isActive == true) return
        launchHistoryDemand(HistoryLane.OLDER, cursor)
    }

    /**
     * Oracle `pausePreparation` (the Cancel action) — purely client-side:
     * no wire call; the in-flight poll is aborted and `preparationPolls`
     * jumps to the cap so the row flips to "Preparation is paused.".
     */
    fun cancelPreparation() {
        if (local.value.browseState != ConversationBrowseState.PREPARING) return
        manualPreparationPause = true
        historyJob?.cancel()
        local.value = local.value.copy(
            loading = false,
            preparationPolls = HISTORY_MAX_PREPARATION_POLLS,
        )
    }

    /**
     * Oracle `continuePreparation` (the Continue action) — re-issues
     * `get_conversation_history` with the cursor the preparation was
     * polling; a fresh stall window starts (polls reset).
     */
    fun continuePreparation() {
        val cursor = local.value.nextCursor
        val stalled = local.value.errorCode == PREPARATION_STALLED_CODE
        if (cursor.isEmpty() || (!manualPreparationPause && !stalled)) return
        local.value = local.value.copy(preparationPolls = 0)
        launchHistoryDemand(HistoryLane.OLDER, cursor)
    }

    /**
     * Oracle `recoverHistory` — the error row's affordance: a
     * `preparation_stalled` error continues the poll loop; every other
     * retryable error re-issues the failed request with `retry: true`
     * (the oracle's `retry()`).
     */
    fun recoverHistory() {
        if (local.value.errorCode == PREPARATION_STALLED_CODE) {
            continuePreparation()
            return
        }
        if (!local.value.errorRetryable) return
        // After a failure `nextCursor` holds the failed request's cursor —
        // empty means the cursorless head failed and the retry re-browses.
        val cursor = local.value.nextCursor
        launchHistoryDemand(
            if (cursor.isEmpty()) HistoryLane.LATEST else HistoryLane.OLDER,
            cursor,
            retry = true,
        )
    }

    // ── history demand loop ───────────────────────────────────────────

    /**
     * The oracle's `runDemand`, reduced to our single-page contract: one
     * `get_conversation_history` request per iteration, where `preparing`
     * pages are status (not content) and re-polled after
     * [HISTORY_PREPARATION_INTERVAL_MS] with the page's `next_cursor` until
     * the read resolves, fails, or stalls at [HISTORY_MAX_PREPARATION_POLLS]
     * identical progress snapshots.
     */
    private fun launchHistoryDemand(
        lane: HistoryLane,
        cursor: String,
        retry: Boolean = false,
    ) {
        historyJob?.cancel()
        manualPreparationPause = false
        preparationProgressKey = ""
        historyJob = viewModelScope.launch {
            val thisJob = coroutineContext.job
            var requestedCursor = cursor
            var firstPage = true
            try {
                while (true) {
                    if (firstPage) local.value = local.value.copy(loading = true)
                    val page = try {
                        sessions.conversationPage(
                            paneId,
                            ConversationPageRequest(
                                cursor = requestedCursor,
                                limit = HISTORY_WIRE_PAGE_SIZE,
                                retry = retry && firstPage,
                            ),
                        )
                    } catch (cancelled: CancellationException) {
                        throw cancelled
                    } catch (failure: Exception) {
                        failHistoryRequest(failure, requestedCursor)
                        return@launch
                    }
                    firstPage = false
                    if (page.state == ConversationBrowseState.PREPARING) {
                        if (!acceptPreparingPage(page, requestedCursor)) return@launch
                        requestedCursor = local.value.nextCursor
                        delay(HISTORY_PREPARATION_INTERVAL_MS)
                    } else {
                        acceptHistoryPage(page, lane, requestedCursor)
                        return@launch
                    }
                }
            } finally {
                // A superseding demand owns `loading` now — don't stomp it.
                if (historyJob == thisJob) {
                    historyJob = null
                    local.value = local.value.copy(loading = false)
                }
            }
        }
    }

    /**
     * The oracle's `acceptPage` preparing branch + `advancePreparation`:
     * preparation pages carry no entries — only the cursor/progress snapshot
     * updates. Polls reset on progress changes; [HISTORY_MAX_PREPARATION_POLLS]
     * identical snapshots pause with the retryable `preparation_stalled`
     * error. Returns false when the loop must stop.
     */
    private fun acceptPreparingPage(
        page: ConversationPage,
        requestedCursor: String,
    ): Boolean {
        // `page.nextCursor || requestedCursor || this.cursor` — the poll lane.
        val resolvedCursor = page.nextCursor
            .ifEmpty { requestedCursor }
            .ifEmpty { local.value.nextCursor }
        // JSON.stringify(progress || {}) — a null progress is one stable key.
        val progressKey = page.progress?.let {
            "${it.phase}|${it.scannedBytes}|${it.sourceBytes}"
        } ?: ""
        val polls = if (progressKey == preparationProgressKey) {
            local.value.preparationPolls + 1
        } else {
            1
        }
        preparationProgressKey = progressKey
        local.value = local.value.copy(
            loading = false,
            browseState = ConversationBrowseState.PREPARING,
            browseProgress = page.progress,
            nextCursor = resolvedCursor,
            hasMore = page.hasMore || resolvedCursor.isNotEmpty(),
            diagnostics = if (requestedCursor.isEmpty()) {
                page.diagnostics
            } else {
                mergeHistoryDiagnostics(local.value.diagnostics, page.diagnostics)
            },
            error = null,
            errorCode = "",
            errorRetryable = false,
            preparationPolls = polls,
        )
        if (polls >= HISTORY_MAX_PREPARATION_POLLS) {
            local.value = local.value.copy(
                error = "Preparation is paused because progress has not changed. Continue to retry.",
                errorCode = PREPARATION_STALLED_CODE,
                errorRetryable = true,
            )
            return false
        }
        return true
    }

    /**
     * The oracle's `acceptPage` resolution branch: `failed`/error pages keep
     * the failed cursor for the recover affordance; a cursorful unavailable
     * page becomes a retryable `history_unavailable` error; a cursorless
     * unavailable page is the authoritative empty state; a ready page applies
     * its window (fresh head replaces, older pages prepend-merge by id).
     */
    private fun acceptHistoryPage(
        page: ConversationPage,
        lane: HistoryLane,
        requestedCursor: String,
    ) {
        val freshHead = lane == HistoryLane.LATEST && requestedCursor.isEmpty()
        val pageError = page.error
        if (pageError != null || page.state == ConversationBrowseState.FAILED) {
            val error = pageError ?: ConversationBrowseError(
                code = "history_failed",
                message = page.reason.ifEmpty {
                    "Conversation history could not be loaded."
                },
                retryable = false,
            )
            val resolvedCursor = page.nextCursor
                .ifEmpty { requestedCursor }
                .ifEmpty { local.value.nextCursor }
            local.value = local.value.copy(
                loading = false,
                // `stateValue.state = pageState` — the wire state stands even
                // when the page carries `error` without `failed`.
                browseState = page.state,
                browseProgress = page.progress,
                diagnostics = if (freshHead) {
                    page.diagnostics
                } else {
                    mergeHistoryDiagnostics(local.value.diagnostics, page.diagnostics)
                },
                error = error.message,
                errorCode = error.code,
                errorRetryable = error.retryable,
                nextCursor = resolvedCursor,
                hasMore = resolvedCursor.isNotEmpty() || page.hasMore,
            )
            return
        }
        if (!page.available) {
            if (!freshHead) {
                // A cursorful unavailable page keeps the loaded window and
                // surfaces a recoverable error instead (oracle acceptPage).
                val resolvedCursor = page.nextCursor
                    .ifEmpty { requestedCursor }
                    .ifEmpty { local.value.nextCursor }
                local.value = local.value.copy(
                    loading = false,
                    browseState = page.state,
                    browseProgress = page.progress,
                    error = page.reason.ifEmpty {
                        "Older conversation history is unavailable."
                    },
                    errorCode = page.reasonCode.ifEmpty { "history_unavailable" },
                    errorRetryable = true,
                    nextCursor = resolvedCursor,
                    hasMore = resolvedCursor.isNotEmpty() || page.hasMore,
                )
            } else {
                // The oracle's authoritative unavailable — the cursorless
                // result replaces the window and clears the browse lane.
                local.value = local.value.copy(
                    loading = false,
                    entries = emptyList(),
                    hasMore = false,
                    nextCursor = "",
                    browseState = page.state,
                    browseProgress = page.progress,
                    diagnostics = page.diagnostics,
                    pageAvailable = false,
                    pageReason = page.reason,
                    error = null,
                    errorCode = "",
                    errorRetryable = false,
                    preparationPolls = 0,
                )
            }
            return
        }
        val entries = if (freshHead) {
            page.entries
        } else {
            mergeOlderEntries(local.value.entries, page.entries)
        }
        local.value = local.value.copy(
            loading = false,
            entries = entries,
            hasMore = page.hasMore || page.nextCursor.isNotEmpty(),
            nextCursor = page.nextCursor,
            browseState = page.state,
            browseProgress = page.progress,
            diagnostics = if (freshHead) {
                page.diagnostics
            } else {
                mergeHistoryDiagnostics(local.value.diagnostics, page.diagnostics)
            },
            pageAvailable = true,
            pageReason = if (freshHead) page.reason else local.value.pageReason,
            error = null,
            errorCode = "",
            errorRetryable = false,
            preparationPolls = 0,
        )
    }

    /**
     * The oracle's `conversationError` — a transport failure becomes a
     * retryable `history_failed` error; the request's cursor is retained so
     * [recoverHistory] can re-issue it.
     */
    private fun failHistoryRequest(failure: Exception, requestedCursor: String) {
        val resolvedCursor = requestedCursor.ifEmpty { local.value.nextCursor }
        local.value = local.value.copy(
            loading = false,
            error = failure.message ?: "Conversation history could not be loaded.",
            errorCode = "history_failed",
            errorRetryable = true,
            nextCursor = resolvedCursor,
            hasMore = resolvedCursor.isNotEmpty() || local.value.hasMore,
        )
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
        /** The controller's client-side stall code — `recoverHistory` routes it to Continue. */
        const val PREPARATION_STALLED_CODE = "preparation_stalled"
    }
}

/** Oracle `mergeOlderEntries` — older pages prepend, deduplicated by id. */
private fun mergeOlderEntries(
    existing: List<ConversationEntry>,
    older: List<ConversationEntry>,
): List<ConversationEntry> {
    val existingIds = existing.mapTo(HashSet()) { it.id }
    return older.filter { it.id !in existingIds } + existing
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
