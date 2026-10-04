package com.lerdr.app.session

import androidx.compose.runtime.Immutable
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import lerdr.core.store.Agent
import lerdr.core.store.AgentInventoryState
import lerdr.core.store.RelayConnection
import lerdr.core.store.RelayStatus
import lerdr.core.store.RelayWorkspace
import lerdr.core.store.WorkspaceStore

@Immutable
data class TabOrderUiState(
    val available: Boolean = false,
    val position: Int = 0,
    val tabCount: Int = 0,
    val busy: Boolean = false,
    val waitingForOrder: Boolean = false,
    val error: String? = null,
    val message: String? = null,
) {
    val canMoveLeft: Boolean get() = available && !busy && position > 1
    val canMoveRight: Boolean get() = available && !busy && position < tabCount
}

/** Moves the selected tab, never an inferred list of agent cards or workspaces. */
class TabOrderViewModel(
    private val paneId: String,
    private val sessions: SessionRepository,
    private val workspaces: WorkspaceStore,
) : ViewModel() {
    private val relayId = paneId.substringBefore("::")

    private data class PendingOrder(val workspaceId: String, val tabId: String, val position: Int) {
        fun unchanged(agent: Agent?): Boolean = agent != null &&
            agent.workspaceId == workspaceId && agent.tabId == tabId && agent.tabOrder == position
    }

    private data class Local(
        val inFlight: Boolean = false,
        val awaitingOrder: PendingOrder? = null,
        val error: String? = null,
        val message: String? = null,
    )

    private val local = MutableStateFlow(Local())

    val uiState: StateFlow<TabOrderUiState> = combine(
        sessions.agent(paneId),
        sessions.connection(relayId),
        workspaces.workspaces,
        local,
    ) { agent, connection, rows, action ->
        project(agent, connection, rows, action)
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), TabOrderUiState())

    private fun project(
        agent: Agent?,
        connection: RelayConnection?,
        rows: List<RelayWorkspace>,
        action: Local,
    ): TabOrderUiState {
        val workspace = rows.firstOrNull {
            it.relayId == relayId && it.workspaceId == agent?.workspaceId
        }
        val position = agent?.tabOrder ?: 0
        val count = workspace?.tabCount ?: 0
        val available = agent != null && agent.tabId.isNotEmpty() &&
            agent.workspaceId.isNotEmpty() && position in 1..count &&
            sessions.canControl(relayId) &&
            connection?.status == RelayStatus.CONNECTED &&
            connection.inventory.state == AgentInventoryState.READY &&
            SessionRepository.TAB_REORDER_CAPABILITY in connection.capabilities
        val waiting = !action.inFlight && action.awaitingOrder?.unchanged(agent) == true
        return TabOrderUiState(
            available = available,
            position = position,
            tabCount = count,
            busy = action.inFlight || waiting,
            waitingForOrder = waiting,
            error = action.error,
            message = action.message,
        )
    }

    fun moveLeft() = move(right = false)
    fun moveRight() = move(right = true)

    private fun move(right: Boolean) {
        // Resolve the latest store values at dispatch, not the last rendered frame.
        val agent = sessions.agentNow(paneId) ?: return
        val state = project(agent, sessions.connectionNow(relayId), workspaces.workspaces.value, local.value)
        if (if (right) !state.canMoveRight else !state.canMoveLeft) return
        // Herdr resolves a zero-based boundary before removing the source tab.
        // Moving right must cross the next tab, not reinsert before it.
        val index = if (right) state.position + 1 else state.position - 2
        local.value = Local(inFlight = true)
        viewModelScope.launch {
            try {
                sessions.reorderTab(paneId, index)
                val pending = PendingOrder(agent.workspaceId, agent.tabId, state.position)
                local.value = Local(
                    awaitingOrder = pending,
                    message = "Tab move confirmed.",
                )
                sessions.agent(paneId).first { !pending.unchanged(it) }
                local.update {
                    if (it.awaitingOrder === pending) it.copy(awaitingOrder = null) else it
                }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: Exception) {
                val message = failure.message ?: "The tab could not be moved"
                local.value = Local(error = message, message = message)
            }
        }
    }

    fun consumeMessage() {
        local.update { it.copy(message = null) }
    }
}
