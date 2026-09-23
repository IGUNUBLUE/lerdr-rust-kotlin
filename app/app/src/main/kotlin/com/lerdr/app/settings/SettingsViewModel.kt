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
import lerdr.core.data.RelayEndpoint
import lerdr.core.store.RelayConnection
import lerdr.core.store.RelayStatus

/** One configured relay row — label, origin, live status, real actions. */
@Immutable
data class RelayRowUi(
    val relayId: String,
    val label: String,
    val origin: String,
    val statusLabel: String,
    /** Connected-only context: transport path, relay version, protocol. */
    val detailLabel: String,
    val connected: Boolean,
    /** The relay refused this device's credential — only re-pairing fixes it. */
    val authRejected: Boolean,
    /** Reconnect can actually do something (not a terminal pairing state). */
    val canReconnect: Boolean,
)

@Immutable
data class SettingsUiState(
    val relays: List<RelayRowUi> = emptyList(),
    val themeMode: ThemeMode = ThemeMode.SYSTEM,
    /**
     * App-lock toggle (docs/04 §App "biometric lock"). The gate itself
     * lives in `security.LockGate`; this flag is its durable switch.
     */
    val appLockEnabled: Boolean = false,
    /** Transient failure from an action — rendered once as a snackbar. */
    val lastError: String? = null,
)

/**
 * Settings (docs/04 §Settings) — relay rows with working lifecycle
 * actions plus the theme preference.
 *
 * Intents:
 * - [reconnectRelay] — no session yet → `connect(endpoint)` creates and
 *   dials it; a live session → `revalidateAll()` (per-transport
 *   `revalidate()` redials down sessions and health-pings live ones —
 *   SessionRepository exposes no narrower per-relay probe).
 * - [revalidateAll] — the section-level "probe every connection" action.
 * - [forgetRelay] — `removeRelay`: registry entry + credential drop; the
 *   registry diff tears the session down itself.
 * - [setThemeMode] — persists the picker selection (the screen applies
 *   the visual switch — that needs a `Context`, which a VM never holds).
 * - [setAppLockEnabled] — persists the app-lock toggle; `LockViewModel`
 *   observes the same preference and re-locks the moment it flips on.
 */
class SettingsViewModel(
    private val sessions: SessionRepository,
    private val preferences: AppPreferences,
) : ViewModel() {

    private val lastError = MutableStateFlow<String?>(null)

    val uiState: StateFlow<SettingsUiState> = combine(
        sessions.relays,
        sessions.connections,
        preferences.themeMode,
        preferences.appLockEnabled,
        lastError,
    ) { relays, connections, themeMode, appLockEnabled, error ->
        SettingsUiState(
            relays = relays.map { it.toRow(connections[it.id]) },
            themeMode = themeMode,
            appLockEnabled = appLockEnabled,
            lastError = error,
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), SettingsUiState())

    /** Per-row reconnect — see class doc for the two underlying paths. */
    fun reconnectRelay(relayId: String) {
        val endpoint = sessions.relays.value.firstOrNull { it.id == relayId } ?: return
        if (sessions.sessionState(relayId) == null) {
            sessions.connect(endpoint)
        } else {
            sessions.revalidateAll()
        }
    }

    /** `revalidateConnections` — foreground probe on every session. */
    fun revalidateAll() {
        sessions.revalidateAll()
    }

    /** Forget + unpair — drops the registry entry and the credential. */
    fun forgetRelay(relayId: String) {
        viewModelScope.launch {
            try {
                sessions.removeRelay(relayId)
            } catch (failure: Exception) {
                lastError.value = failure.message ?: "Could not forget the relay"
            }
        }
    }

    /** Theme picker intent — persists; the screen applies the switch. */
    fun setThemeMode(mode: ThemeMode) {
        viewModelScope.launch { preferences.setThemeMode(mode) }
    }

    /**
     * App-lock toggle intent — persists. Enabling locks the app right
     * away (the gate's session latch is off until the first successful
     * verification), which doubles as a "verify the prompt works" check.
     */
    fun setAppLockEnabled(enabled: Boolean) {
        viewModelScope.launch { preferences.setAppLockEnabled(enabled) }
    }

    /** Snackbar consumed the error. */
    fun dismissError() {
        lastError.value = null
    }

    private fun RelayEndpoint.toRow(connection: RelayConnection?): RelayRowUi {
        val pairing = connection != null &&
            (connection.pairingRequired || connection.pairingDeferred)
        return RelayRowUi(
            relayId = id,
            label = label,
            origin = socketOrigin,
            statusLabel = when {
                connection == null -> "offline"
                connection.authRejected -> "authorization rejected — re-pair"
                connection.pairingRequired -> "pairing required"
                connection.pairingDeferred -> "pairing deferred"
                connection.status == RelayStatus.CONNECTED -> "connected"
                connection.status == RelayStatus.CONNECTING -> "connecting…"
                else -> "offline"
            },
            detailLabel = if (connection?.status == RelayStatus.CONNECTED) {
                listOfNotNull(
                    connection.path?.wire,
                    connection.releaseVersion.ifEmpty { connection.version }
                        .takeIf { it.isNotEmpty() }
                        ?.let { "relay $it" },
                    connection.protocol.takeIf { it > 0 }?.let { "protocol $it" },
                    connection.rttMs.takeIf { it >= 0 }?.let { "${it}ms" },
                ).joinToString(" · ")
            } else {
                ""
            },
            connected = connection?.status == RelayStatus.CONNECTED,
            authRejected = connection?.authRejected == true,
            canReconnect = connection == null || (!connection.authRejected && !pairing),
        )
    }
}
