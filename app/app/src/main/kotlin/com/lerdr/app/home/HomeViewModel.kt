package com.lerdr.app.home

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.lerdr.app.session.SessionRepository
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import lerdr.core.model.CommandResultMessage

/**
 * Plain ViewModel — `hilt-navigation-compose` is not on the classpath, so
 * constructor-injected `@HiltViewModel`s can't resolve inside Nav3 entry
 * scopes. Dependencies are supplied through a `viewModel { }` initializer
 * (see [HomeScreen]).
 *
 * Mutations delegate to [SessionRepository] — the wire actions are the
 * oracle's `respond`, `answer_question`, and `agent_stop`; failures surface
 * on [messages] like the oracle's `showToast`.
 */
class HomeViewModel(
    repository: HomeRepository,
    private val sessions: SessionRepository,
) : ViewModel() {

    val uiState: StateFlow<HomeUiState> = repository.uiState
        .stateIn(
            scope = viewModelScope,
            started = SharingStarted.WhileSubscribed(5_000),
            initialValue = HomeUiState(),
        )

    /** One-shot snackbar text — oracle `showToast` parity. */
    private val _messages = MutableSharedFlow<String>(extraBufferCapacity = 8)
    val messages: SharedFlow<String> = _messages.asSharedFlow()

    /**
     * The oracle's `pullRefreshing` — a fixed visual window that holds the
     * pull indicator up briefly after a trigger and refuses re-arming
     * while open (`touchStart` requires `!pullRefreshing`).
     */
    private val _inventoryRefreshing = MutableStateFlow(false)
    val inventoryRefreshing: StateFlow<Boolean> = _inventoryRefreshing.asStateFlow()

    /**
     * Pull-to-refresh on the agent list — the oracle's
     * `relayStore.requestInventoryRefresh()`: `refresh_agents` to every
     * connected relay plus a redial of disconnected registry endpoints.
     * A second trigger inside the window is a no-op.
     */
    fun refreshInventory() {
        if (_inventoryRefreshing.value) return
        _inventoryRefreshing.value = true
        sessions.inventoryRefresh()
        viewModelScope.launch {
            delay(INVENTORY_REFRESH_WINDOW_MS)
            _inventoryRefreshing.value = false
        }
    }

    /** `respond` — inline approval answer on a needs-you card. */
    fun respond(card: AttentionCardUi, index: Int) {
        if (!card.controllable || card.responding) return
        val choice = card.options.getOrNull(index) ?: return
        viewModelScope.launch {
            try {
                val result = sessions.respond(card.paneId, index, choice)
                _messages.emit(
                    if (result.phase == CommandResultMessage.PHASE_UNCONFIRMED) {
                        "Accepted; agent still appears blocked."
                    } else {
                        "Confirmed: $choice"
                    },
                )
            } catch (failure: Exception) {
                _messages.emit(failure.message ?: "Response failed")
            }
        }
    }

    /**
     * `answer_question` — a quick-answer chip on a single-question
     * `single_select` card; [optionIndex] is the wire `Option.index`.
     */
    fun answerQuestion(card: AttentionCardUi, optionIndex: Int) {
        val interaction = card.interaction ?: return
        if (!card.controllable || card.responding) return
        viewModelScope.launch {
            try {
                sessions.answerQuestion(
                    card.paneId,
                    interaction,
                    selectedIndices = listOf(optionIndex),
                    otherSelected = false,
                    otherText = "",
                )
            } catch (failure: Exception) {
                _messages.emit(failure.message ?: "Answer failed")
            }
        }
    }

    /**
     * `agent_stop` — the swipe-left action's confirmed leg; the confirmation
     * dialog is HomeContent's concern.
     */
    fun stopAgent(agent: AgentListItemUi) {
        if (!agent.controllable) return
        viewModelScope.launch {
            try {
                sessions.stopAgent(agent.paneId)
                _messages.emit("Agent stopped.")
            } catch (failure: Exception) {
                _messages.emit(failure.message ?: "Could not stop the agent")
            }
        }
    }

    companion object {
        /** Oracle `setTimeout(…, 900)` — the pull indicator's hold window. */
        const val INVENTORY_REFRESH_WINDOW_MS = 900L
    }
}
