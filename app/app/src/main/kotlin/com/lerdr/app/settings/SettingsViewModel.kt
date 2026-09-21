package com.lerdr.app.settings

import androidx.compose.runtime.Immutable
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.lerdr.app.session.SessionRepository
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import lerdr.core.store.RelayStatus

/** One configured relay row — label, origin, live status. */
@Immutable
data class RelayRowUi(
    val relayId: String,
    val label: String,
    val origin: String,
    val statusLabel: String,
    val connected: Boolean,
)

@Immutable
data class SettingsUiState(
    val relays: List<RelayRowUi> = emptyList(),
)

/** Settings reads the registry + live connection rows — no mutation yet. */
class SettingsViewModel(
    sessions: SessionRepository,
) : ViewModel() {

    val uiState: StateFlow<SettingsUiState> = combine(
        sessions.relays,
        sessions.connections,
    ) { relays, connections ->
        SettingsUiState(
            relays = relays.map { endpoint ->
                val connection = connections[endpoint.id]
                RelayRowUi(
                    relayId = endpoint.id,
                    label = endpoint.label,
                    origin = endpoint.socketOrigin,
                    statusLabel = when (connection?.status) {
                        RelayStatus.CONNECTED -> "connected"
                        RelayStatus.CONNECTING -> "connecting…"
                        else -> "offline"
                    },
                    connected = connection?.status == RelayStatus.CONNECTED,
                )
            },
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), SettingsUiState())
}
