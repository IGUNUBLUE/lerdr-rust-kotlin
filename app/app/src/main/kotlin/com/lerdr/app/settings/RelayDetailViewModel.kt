package com.lerdr.app.settings

import androidx.compose.runtime.Immutable
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.lerdr.app.session.SessionRepository
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch

@Immutable
data class RelayDetailUiState(
    /** Null while the relay is absent from the registry (e.g. forgotten). */
    val relay: RelayRowUi? = null,
    val lastError: String? = null,
)

/**
 * Per-relay management surface — one registry row's live status plus the
 * lifecycle actions that used to sit on the Settings list row.
 *
 * - [reconnect] — no session yet → `connect(endpoint)` creates and dials
 *   it; a live session → `revalidateAll()` redials/probes it (mirrors the
 *   oracle's per-relay probe, which has no narrower wire command).
 * - [forget] — `removeRelay`: registry entry + credential drop; the
 *   registry diff tears the session down itself, then the screen pops.
 */
class RelayDetailViewModel(
    private val relayId: String,
    private val sessions: SessionRepository,
) : ViewModel() {

    private val lastError = MutableStateFlow<String?>(null)

    val uiState: StateFlow<RelayDetailUiState> = combine(
        sessions.relays,
        sessions.connections,
        lastError,
    ) { relays, connections, error ->
        RelayDetailUiState(
            relay = relays.firstOrNull { it.id == relayId }
                ?.toRelayRowUi(connections[relayId]),
            lastError = error,
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), RelayDetailUiState())

    fun reconnect() {
        val endpoint = sessions.relays.value.firstOrNull { it.id == relayId } ?: return
        if (sessions.sessionState(relayId) == null) {
            sessions.connect(endpoint)
        } else {
            sessions.revalidateAll()
        }
    }

    /** Forget + unpair — drops the registry entry and the credential. */
    fun forget(onForgotten: () -> Unit) {
        viewModelScope.launch {
            try {
                sessions.removeRelay(relayId)
                onForgotten()
            } catch (failure: Exception) {
                lastError.value = failure.message ?: "Could not forget the relay"
            }
        }
    }

    fun dismissError() {
        lastError.value = null
    }
}
