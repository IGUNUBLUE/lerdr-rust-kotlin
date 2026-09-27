package com.lerdr.app.settings

import androidx.compose.runtime.Immutable
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import android.content.Intent
import com.lerdr.app.session.SessionRepository
import com.lerdr.app.update.AppUpdateManager
import com.lerdr.app.update.AppUpdateState
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
    /** GitHub self-update state — drives the About-section update row. */
    val update: AppUpdateState = AppUpdateState(),
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
    private val updates: AppUpdateManager,
) : ViewModel() {

    private val lastError = MutableStateFlow<String?>(null)

    // combine() tops out at five typed flows — pair the sparse ones first.
    private val extras = combine(lastError, updates.state, ::Pair)

    val uiState: StateFlow<SettingsUiState> = combine(
        sessions.relays,
        sessions.connections,
        preferences.themeMode,
        preferences.appLockEnabled,
        extras,
    ) { relays, connections, themeMode, appLockEnabled, (error, update) ->
        SettingsUiState(
            relays = relays.map { it.toRelayRowUi(connections[it.id]) },
            themeMode = themeMode,
            appLockEnabled = appLockEnabled,
            lastError = error,
            update = update,
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), SettingsUiState())

    /** `revalidateConnections` — foreground probe on every session. */
    fun revalidateAll() {
        sessions.revalidateAll()
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

    // ── app update row ────────────────────────────────────────────────

    /** "Check" button — manual GitHub latest-release probe. */
    fun checkForUpdate() = updates.checkNow()

    /**
     * "Update" / "Install" row action — starts the APK download or hands
     * the staged file to the installer; may flip the row to the
     * install-permission gate instead.
     */
    fun startUpdate() = updates.startUpdate()

    /** Post-resume hook — finishes the pending step after the grant. */
    fun resumeUpdateAfterPermission() = updates.resumeAfterPermission()

    /** System screen granting `REQUEST_INSTALL_PACKAGES` for Lerdr. */
    fun installPermissionIntent(): Intent = updates.installPermissionIntent()
}

/**
 * Shared relay-row projection — used by Settings' relay list and the
 * per-relay detail screen's header card.
 */
internal fun RelayEndpoint.toRelayRowUi(connection: RelayConnection?): RelayRowUi {
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
