package com.lerdr.app.session

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
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
import lerdr.core.store.Agent
import lerdr.core.store.AgentStatusGroup
import lerdr.core.store.RelayStatus
import lerdr.core.store.agentStatusGroup
import lerdr.core.store.rawBlocked

/** Everything Feed mode renders — conversation page, blocker card, composer. */
data class FeedUiState(
    val paneId: String,
    /** Agent display name for the top bar. */
    val title: String = "",
    /** "lerdr · main · sd" — workspace/context breadcrumb. */
    val breadcrumb: String = "",
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
) : ViewModel() {

    private val relayId = paneId.substringBefore("::")

    private data class FeedLocal(
        val entries: List<ConversationEntry> = emptyList(),
        val loading: Boolean = false,
        val error: String? = null,
        val hasMore: Boolean = false,
        val nextCursor: String = "",
        val draft: String = "",
        val sending: Boolean = false,
        val lastError: String? = null,
    )

    private val local = MutableStateFlow(FeedLocal())

    val uiState: StateFlow<FeedUiState> = combine(
        sessions.agent(paneId),
        sessions.connection(relayId),
        sessions.responding,
        local,
    ) { agent, connection, responding, local ->
        FeedUiState(
            paneId = paneId,
            title = agent?.name ?: agent?.agent ?: paneId.substringAfter("::"),
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

    /** `submit_prompt` — clears the draft on a confirmed dispatch. */
    fun sendPrompt() {
        val text = local.value.draft.trim()
        if (text.isEmpty() || local.value.sending) return
        viewModelScope.launch {
            local.value = local.value.copy(sending = true, lastError = null)
            try {
                sessions.submitPrompt(paneId, text)
                local.value = local.value.copy(draft = "")
                sessions.agents.value.firstOrNull { it.paneId == paneId }?.let {
                    drafts.clear(draftIdentity(it))
                }
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
}
