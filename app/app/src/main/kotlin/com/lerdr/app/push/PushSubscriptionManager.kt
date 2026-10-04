package com.lerdr.app.push

import android.content.Context
import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import com.lerdr.app.di.AppScope
import com.lerdr.app.notify.LerdrNotifier
import com.lerdr.app.notify.RelaySyncService
import com.lerdr.app.session.SessionRepository
import dagger.hilt.android.qualifiers.ApplicationContext
import java.util.UUID
import javax.inject.Inject
import javax.inject.Singleton
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.FlowPreview
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.debounce
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonObject
import lerdr.core.model.Inbound
import lerdr.core.store.RelayStatus
import org.unifiedpush.android.connector.UnifiedPush
import org.unifiedpush.android.connector.data.PushEndpoint
import org.unifiedpush.android.connector.data.PushMessage

/**
 * What the Settings row can truthfully claim — each stage is a distinct,
 * observed fact (never "push works" as one blob).
 */
enum class PushStage {
    /** No distributor app is installed (ntfy & co.). */
    NO_DISTRIBUTOR,

    /** Several distributors installed — the user must pick one. */
    NEEDS_PICK,

    /** Distributor selected; REGISTER broadcast sent, endpoint pending. */
    REGISTERING,

    /** Distributor issued an endpoint + keys; relay subscription pending. */
    ENDPOINT_READY,

    /** At least one relay acked `push_subscribed` for the live endpoint. */
    SUBSCRIBED,

    /** UP registration or relay subscribe failed — [PushUiState.error]. */
    FAILED,
}

/** Distributor-facing state for the Settings row — endpoint host only. */
data class PushUiState(
    val stage: PushStage = PushStage.REGISTERING,
    /** Distributor package (e.g. `io.heckel.ntfy`) once selected. */
    val distributor: String? = null,
    /** Endpoint host for display ("ntfy.sh") — never the full URL. */
    val endpointHost: String? = null,
    /** Relay ids that acked the current endpoint via `push_subscribed`. */
    val subscribedRelays: Set<String> = emptySet(),
    /** Available distributors while [PushStage.NEEDS_PICK]. */
    val distributors: List<String> = emptyList(),
    val error: String? = null,
) {
    /**
     * A relay acked the live endpoint — a push can reach this process
     * even while dead, so the socket keep-alive pin is dispensable.
     */
    val deliversWhileDead: Boolean
        get() = stage == PushStage.SUBSCRIBED && subscribedRelays.isNotEmpty()
}

/**
 * UnifiedPush ↔ relay bridge — the missing half of the push path.
 *
 * Lifecycle:
 * - [start] (Application.onCreate) restores the stored registration,
 *   re-registers with the saved distributor (token rotation lands as a
 *   fresh `onNewEndpoint`), and subscribes the endpoint on every relay
 *   CONNECTED edge. UP registration runs regardless of app lock — it opens
 *   no relay socket.
 * - `onNewEndpoint` persists `{endpoint, p256dh, auth}` and pushes
 *   `push_subscribe` to every connected relay; a rotated endpoint rides
 *   `replace_endpoints` so the relay prunes the stale record.
 * - `onMessage` binds the connector-decrypted record to exactly one stored
 *   enrolled relay before [PushPayload.toCommands] → [LerdrNotifier]. This
 *   works before sockets/snapshots load; pane slots match socket notifications.
 * - `onUnregistered` (distributor removed/reset) sends `push_unsubscribe`
 *   and clears the store; `push_viewed_pane` suppression stays on the
 *   socket path, untouched.
 * - Owns the [com.lerdr.app.notify.RelaySyncService] pin policy: the
 *   foreground-service pin is the *fallback* background channel, needed
 *   only while push cannot reach a dead process. Once a relay acks the
 *   live endpoint ([PushUiState.deliversWhileDead]) the pin stays off —
 *   no permanent FGS, no battery-warning quota burn; if the distributor
 *   or subscription drops, the pin returns.
 */
@Singleton
@OptIn(FlowPreview::class)
class PushSubscriptionManager @Inject constructor(
    @param:ApplicationContext private val context: Context,
    private val sessions: SessionRepository,
    private val notifier: LerdrNotifier,
    private val dataStore: DataStore<Preferences>,
    @param:AppScope private val scope: CoroutineScope,
) {
    private val _uiState = MutableStateFlow(PushUiState())
    val uiState: StateFlow<PushUiState> = _uiState

    /** Guards [sentSubscribe] + [inFlightSubscribe] — short sections only. */
    private val stateLock = Any()

    /** relayId → endpoint that `push_subscribe` was last sent for. */
    private val sentSubscribe = mutableMapOf<String, String>()
    private val inFlightSubscribe = mutableSetOf<String>()

    /** relayId → consecutive subscribe failures — drives the retry backoff. */
    private val subscribeFailures = mutableMapOf<String, Int>()

    /** relayId → pending job that re-arms the subscribe send. */
    private val subscribeRetries = mutableMapOf<String, kotlinx.coroutines.Job>()

    @Volatile
    private var started = false

    fun start() {
        if (started) return
        started = true
        // Let SessionRepository hand us the relay before teardown so the
        // subscription is withdrawn while the socket still lives.
        sessions.onRelayRemoving = ::unsubscribeRelay
        scope.launch { ensureRegistered() }
        scope.launch {
            // Installation happens outside Lerdr; returning must discover
            // the new distributor without replacing an active registration.
            sessions.hidden.drop(1).collect { hidden ->
                if (!hidden && _uiState.value.stage == PushStage.NO_DISTRIBUTOR) {
                    ensureRegistered()
                }
            }
        }
        scope.launch { observeConnections() }
        scope.launch { observePinPolicy() }
        scope.launch { loadStoredSubscription() }
    }

    /** UI action — the OS picker when several distributors are installed. */
    fun pickDistributor(packageName: String) {
        UnifiedPush.saveDistributor(context, packageName)
        _uiState.update {
            it.copy(
                stage = PushStage.REGISTERING,
                distributor = packageName,
                distributors = emptyList(),
                error = null,
            )
        }
        UnifiedPush.register(context)
    }

    // ── PushService callbacks (binder thread → scope) ─────────────────

    /** New/rotated endpoint — persist, then subscribe on every relay. */
    fun onNewEndpoint(endpoint: PushEndpoint, instance: String) {
        val keys = endpoint.pubKeySet
        if (keys == null) {
            _uiState.update {
                it.copy(
                    stage = PushStage.FAILED,
                    error = "distributor issued no web push keys",
                )
            }
            return
        }
        scope.launch {
            val previous = dataStore.data.first()[KEY_ENDPOINT]
            dataStore.edit {
                it[KEY_ENDPOINT] = endpoint.url
                it[KEY_P256DH] = keys.pubKey
                it[KEY_AUTH] = keys.auth
                if (previous != null && previous != endpoint.url) {
                    it[KEY_PREV_ENDPOINT] = previous
                }
            }
            _uiState.update {
                it.copy(
                    stage = PushStage.ENDPOINT_READY,
                    endpointHost = hostOf(endpoint.url),
                    subscribedRelays = emptySet(),
                    error = null,
                )
            }
            synchronized(stateLock) {
                sentSubscribe.clear()
                subscribeFailures.clear()
                subscribeRetries.values.forEach { it.cancel() }
                subscribeRetries.clear()
            }
            subscribeAllConnected()
        }
    }

    /** Connector already decrypted the record; enrollment still binds its owner. */
    fun onMessage(message: PushMessage, instance: String) {
        if (!message.decrypted) return
        val payload = PushPayload.parse(message.content) ?: return
        scope.launch {
            val relayId = sessions.enrolledRelayForPushDevice(payload.key.deviceId)
            notifier.execute(payload.toCommands(relayId))
        }
    }

    /** Distributor dropped the registration — tell relays, clear store. */
    fun onUnregistered(instance: String) {
        scope.launch {
            val old = dataStore.data.first()[KEY_ENDPOINT]
            dataStore.edit {
                it.remove(KEY_ENDPOINT)
                it.remove(KEY_P256DH)
                it.remove(KEY_AUTH)
                it.remove(KEY_PREV_ENDPOINT)
            }
            synchronized(stateLock) { sentSubscribe.clear() }
            _uiState.update {
                PushUiState(
                    stage = PushStage.REGISTERING,
                    distributor = it.distributor,
                )
            }
            if (old != null) unsubscribeAllConnected(old)
            synchronized(stateLock) {
                subscribeFailures.clear()
                subscribeRetries.values.forEach { it.cancel() }
                subscribeRetries.clear()
            }
            ensureRegistered()
        }
    }

    fun onRegistrationFailed(reason: org.unifiedpush.android.connector.FailedReason, instance: String) {
        _uiState.update {
            it.copy(stage = PushStage.FAILED, error = "registration failed ($reason)")
        }
        // Transient distributor hiccups resolve on the next register tick.
        scope.launch {
            delay(REGISTER_RETRY_MS)
            ensureRegistered()
        }
    }

    fun onTempUnavailable(instance: String) {
        _uiState.update {
            it.copy(error = "distributor temporarily unavailable")
        }
    }

    // ── registration ──────────────────────────────────────────────────

    /**
     * Pick-or-refresh the distributor and REGISTER. Idempotent — UP
     * re-acks into `onNewEndpoint` whether the endpoint rotated or not.
     */
    private suspend fun ensureRegistered() {
        val acked = UnifiedPush.getAckDistributor(context)
        if (acked != null) {
            _uiState.update { s ->
                s.copy(
                    stage = if (s.endpointHost != null) s.stage else PushStage.REGISTERING,
                    distributor = acked,
                    distributors = emptyList(),
                )
            }
            UnifiedPush.register(context)
            return
        }
        val distributors = UnifiedPush.getDistributors(context)
        when (distributors.size) {
            0 -> _uiState.update {
                PushUiState(
                    stage = PushStage.NO_DISTRIBUTOR,
                    distributors = emptyList(),
                )
            }
            1 -> pickDistributor(distributors.single())
            else -> _uiState.update {
                PushUiState(
                    stage = PushStage.NEEDS_PICK,
                    distributors = distributors,
                )
            }
        }
    }

    /** Restore persisted subscription state after a cold start. */
    private suspend fun loadStoredSubscription() {
        val prefs = dataStore.data.first()
        val endpoint = prefs[KEY_ENDPOINT] ?: return
        _uiState.update {
            it.copy(
                stage = PushStage.ENDPOINT_READY,
                endpointHost = hostOf(endpoint),
            )
        }
    }

    // ── relay subscription ────────────────────────────────────────────

    private suspend fun observeConnections() {
        sessions.connections.collect { connections ->
            val connected = connections.values
                .filter { it.status == RelayStatus.CONNECTED }
                .map { it.relayId }
                .toSet()
            synchronized(stateLock) { sentSubscribe.keys.retainAll(connected) }
            subscribeAllConnected()
        }
    }

    // ── keep-alive pin policy ─────────────────────────────────────────

    /**
     * Drives [RelaySyncService]: pin iff a relay is connected AND push
     * cannot reach a dead process yet.
     *
     * Arms settle briefly: `push_subscribed` typically lands ~a few
     * hundred ms after CONNECTED, so a short hold-off skips the
     * pin-then-release flicker on every (re)connect — including the
     * push-wake reconnects, where a foreground start would be refused
     * anyway. Releases settle longer so CONNECTED↔CLOSED flaps don't
     * bounce the pin. `pinned` stops a never-started service from being
     * "stopped" (which would boot it just to tear it down).
     */
    private suspend fun observePinPolicy() {
        var pinned = false
        combine(sessions.connections, uiState) { connections, push ->
            shouldPin(
                connected = connections.values.any { it.status == RelayStatus.CONNECTED },
                deliversWhileDead = push.deliversWhileDead,
            )
        }
            .distinctUntilChanged()
            .debounce { pin -> if (pin) PIN_ARM_DEBOUNCE_MS else PIN_RELEASE_DEBOUNCE_MS }
            .distinctUntilChanged()
            .collect { pin ->
                if (pin) {
                    pinned = true
                    RelaySyncService.start(context)
                } else if (pinned) {
                    pinned = false
                    RelaySyncService.stop(context)
                }
            }
    }

    private suspend fun subscribeAllConnected() {
        val prefs = dataStore.data.first()
        val endpoint = prefs[KEY_ENDPOINT] ?: return
        val keys = keysOf(prefs) ?: return
        val connected = sessions.connections.value
            .filterValues { it.status == RelayStatus.CONNECTED }
            .keys
        for (relayId in connected) {
            subscribe(relayId, endpoint, keys, prefs[KEY_PREV_ENDPOINT])
        }
    }

    private suspend fun subscribe(
        relayId: String,
        endpoint: String,
        keys: Pair<String, String>,
        replaceEndpoint: String?,
    ) {
        synchronized(stateLock) {
            if (sentSubscribe[relayId] == endpoint || !inFlightSubscribe.add(relayId)) return
        }
        try {
            val subscription: JsonObject = buildJsonObject {
                put("endpoint", endpoint)
                putJsonObject("keys") {
                    put("p256dh", keys.first)
                    put("auth", keys.second)
                }
            }
            sessions.request(
                relayId,
                Inbound(
                    type = ACTION_SUBSCRIBE,
                    subscription = subscription,
                    replaceEndpoints = listOfNotNull(replaceEndpoint?.takeIf { it != endpoint }),
                    clientId = pushClientId(),
                ),
            )
            synchronized(stateLock) {
                sentSubscribe[relayId] = endpoint
                subscribeFailures.remove(relayId)
                subscribeRetries.remove(relayId)?.cancel()
            }
            if (replaceEndpoint != null && replaceEndpoint != endpoint) {
                dataStore.edit { it.remove(KEY_PREV_ENDPOINT) }
            }
            _uiState.update {
                it.copy(
                    stage = PushStage.SUBSCRIBED,
                    subscribedRelays = it.subscribedRelays + relayId,
                    error = null,
                )
            }
        } catch (cancelled: kotlinx.coroutines.CancellationException) {
            throw cancelled
        } catch (failure: Exception) {
            // Mark as sent too — connections emits on every status/latency
            // tick, so an unmarked failure would spin into a retry storm.
            // The mark alone cannot be the only re-arm path, though: a
            // subscribe lost on a socket that dies during reconnect churn
            // leaves the next CONNECTED emission seeing a live relay with
            // the mark still set — nothing ever retries. Schedule one.
            synchronized(stateLock) { sentSubscribe[relayId] = endpoint }
            _uiState.update {
                it.copy(
                    stage = PushStage.FAILED,
                    error = "relay refused the subscription",
                )
            }
            scheduleSubscribeRetry(relayId, endpoint)
        } finally {
            inFlightSubscribe.remove(relayId)
        }
    }

    /**
     * Re-arms a refused/lost `push_subscribe` after a backoff. The send
     * stays marked (storm guard) until the delay elapses, then the mark
     * lifts and [subscribeAllConnected] drives the retry — a further
     * failure re-schedules with a longer delay. Stale jobs are inert:
     * a rotated endpoint or cleared mark makes the guard return early.
     */
    private fun scheduleSubscribeRetry(relayId: String, endpoint: String) {
        synchronized(stateLock) {
            val attempt = (subscribeFailures[relayId] ?: 0) + 1
            subscribeFailures[relayId] = attempt
            subscribeRetries[relayId]?.cancel()
            subscribeRetries[relayId] = scope.launch {
                delay(subscribeRetryDelayMs(attempt))
                synchronized(stateLock) {
                    if (sentSubscribe[relayId] != endpoint) return@launch
                    sentSubscribe.remove(relayId)
                }
                subscribeAllConnected()
            }
        }
    }

    private fun clearSubscribeRetry(relayId: String) {
        synchronized(stateLock) {
            subscribeFailures.remove(relayId)
            subscribeRetries.remove(relayId)?.cancel()
        }
    }

    /**
     * `removeRelay` hook — withdraw the subscription while the socket is
     * still up. Best-effort: a torn-down or wedged session must not block
     * the unpair.
     */
    private suspend fun unsubscribeRelay(relayId: String) {
        val endpoint = dataStore.data.first()[KEY_ENDPOINT] ?: return
        runCatching {
            sessions.request(
                relayId,
                Inbound(
                    type = ACTION_UNSUBSCRIBE,
                    endpoints = listOf(endpoint),
                    clientId = pushClientId(),
                ),
                timeoutMs = UNSUBSCRIBE_TIMEOUT_MS,
            )
        }
        synchronized(stateLock) { sentSubscribe.remove(relayId) }
        clearSubscribeRetry(relayId)
        _uiState.update { it.copy(subscribedRelays = it.subscribedRelays - relayId) }
    }

    private suspend fun unsubscribeAllConnected(endpoint: String) {
        sessions.connections.value
            .filterValues { it.status == RelayStatus.CONNECTED }
            .keys
            .forEach { relayId ->
                runCatching {
                    sessions.request(
                        relayId,
                        Inbound(
                            type = ACTION_UNSUBSCRIBE,
                            endpoints = listOf(endpoint),
                            clientId = pushClientId(),
                        ),
                        timeoutMs = UNSUBSCRIBE_TIMEOUT_MS,
                    )
                }
                synchronized(stateLock) { sentSubscribe.remove(relayId) }
                clearSubscribeRetry(relayId)
            }
        _uiState.update { it.copy(subscribedRelays = emptySet()) }
    }

    /** Stable per-install client id — the unsubscribe matcher. */
    private suspend fun pushClientId(): String {
        dataStore.data.first()[KEY_CLIENT_ID]?.let { return it }
        val generated = "android-${UUID.randomUUID()}"
        dataStore.edit { it[KEY_CLIENT_ID] = generated }
        return generated
    }

    private fun keysOf(prefs: Preferences): Pair<String, String>? {
        val p256dh = prefs[KEY_P256DH] ?: return null
        val auth = prefs[KEY_AUTH] ?: return null
        return p256dh to auth
    }

    private fun hostOf(endpoint: String): String = try {
        java.net.URI(endpoint).host ?: endpoint
    } catch (_: Exception) {
        endpoint
    }

    companion object {
        const val ACTION_SUBSCRIBE = "push_subscribe"
        const val ACTION_UNSUBSCRIBE = "push_unsubscribe"

        private const val UNSUBSCRIBE_TIMEOUT_MS = 3_000L
        private const val REGISTER_RETRY_MS = 30_000L
        private const val SUBSCRIBE_RETRY_BASE_MS = 5_000L
        private const val SUBSCRIBE_RETRY_MAX_MS = 300_000L

        /**
         * Hold-off before pinning — covers the connect→`push_subscribed`
         * round-trip so a subscribed client never arms the FGS at all.
         */
        private const val PIN_ARM_DEBOUNCE_MS = 1_500L

        /** Settle window before releasing the pin (matches the notifier's old flap window). */
        private const val PIN_RELEASE_DEBOUNCE_MS = 3_000L

        /**
         * The whole pin decision in one pure check — unit-testable, and
         * shared by the running service's own stop condition so a
         * system-restarted pin can't outlive push coverage.
         */
        internal fun shouldPin(connected: Boolean, deliversWhileDead: Boolean): Boolean =
            connected && !deliversWhileDead

        /**
         * Backoff for a refused/lost `push_subscribe`: 5 s doubling to a
         * 5 min ceiling — fast enough to cover reconnect-churn losses,
         * slow enough that a persistently refused endpoint costs a frame
         * every few minutes until a disconnect edge or endpoint rotation
         * clears it.
         */
        internal fun subscribeRetryDelayMs(attempt: Int): Long =
            (SUBSCRIBE_RETRY_BASE_MS shl (attempt - 1).coerceIn(0, 6))
                .coerceAtMost(SUBSCRIBE_RETRY_MAX_MS)

        private val KEY_ENDPOINT = stringPreferencesKey("push_up_endpoint")
        private val KEY_P256DH = stringPreferencesKey("push_up_p256dh")
        private val KEY_AUTH = stringPreferencesKey("push_up_auth")
        private val KEY_PREV_ENDPOINT = stringPreferencesKey("push_up_prev_endpoint")
        private val KEY_CLIENT_ID = stringPreferencesKey("push_client_id")
    }
}
