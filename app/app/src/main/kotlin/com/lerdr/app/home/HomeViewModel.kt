package com.lerdr.app.home

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.lerdr.app.session.SessionRepository
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
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
}
