package lerdr.core.transport

import com.lerdr.core.e2ee.E2EEServerFinish
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.concurrent.withLock
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.receiveAsFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import lerdr.core.model.ActionReceiptMessage
import lerdr.core.model.ActionReceiptPhase
import lerdr.core.model.ClientCapabilities
import lerdr.core.model.CommandResultMessage
import lerdr.core.model.ErrorMessage
import lerdr.core.model.Inbound
import lerdr.core.protocol.InboundCodec
import lerdr.core.protocol.LerdrJson
import lerdr.core.protocol.Protocol
import okhttp3.OkHttpClient

/**
 * The supervised relay session — a port of the oracle's per-relay connection
 * logic in `frontend/src/lib/store.ts` (`connectRelay`, `scheduleReconnect`,
 * `sendKeepalive`, `revalidateConnections`, `retireConnection`,
 * `rejectPendingOperations`):
 *
 * - Dials immediately, then retries on unexpected close with exponential
 *   backoff + jitter ([ReconnectPolicy.Backoff]); a full E2EE re-handshake
 *   runs on every new socket.
 * - Backoff resets on the **first inbound message** of a session
 *   (`reconnectAttempts.delete` in `onMessage`), not on socket open.
 * - Fatal closes redial on the 60 s floor, except `unknown_relay` (a
 *   restarting relay — normal cadence) and `device_unauthorized` (terminal:
 *   the credential itself was refused; only new enrollment unlatches).
 * - Keepalive = `refresh_agents` every 120 s; an unanswered ping aborts the
 *   socket — visible sessions redial, hidden sessions retire until the next
 *   [revalidate].
 * - Sends while disconnected queue in [sendBuffer] (64 items / 4 MiB);
 *   `request` rejects immediately like the oracle's `sendCommand`.
 */
class RelaySession(
    private val url: String,
    private val scope: CoroutineScope,
    private val getAuthentication: () -> DeviceAuthentication?,
    private val onEnrolled: suspend (DeviceAuthentication, E2EEServerFinish) -> Unit = { _, _ -> },
    private val client: OkHttpClient = OkHttpClient(),
    val sendBuffer: ClientSendBuffer = ClientSendBuffer(),
    private val backoff: ReconnectPolicy.Backoff = ReconnectPolicy.Backoff(),
    private val now: () -> Long = System::currentTimeMillis,
    /** `pushClientId()` — injected as `client_id` on commands (lease actions excepted). */
    private val clientId: String = "",
    private val handshakeTimeoutMs: Long = ReconnectPolicy.HANDSHAKE_TIMEOUT_MS,
    private val keepaliveIntervalMs: Long = ReconnectPolicy.KEEPALIVE_INTERVAL_MS,
    private val freshProofMs: Long = ReconnectPolicy.FRESH_PROOF_MS,
    private val staleConnectingMs: Long = ReconnectPolicy.STALE_CONNECTING_MS,
    private val foregroundHealthTimeoutMs: Long = ReconnectPolicy.FOREGROUND_HEALTH_TIMEOUT_MS,
    private val backgroundHealthTimeoutMs: Long = ReconnectPolicy.BACKGROUND_HEALTH_TIMEOUT_MS,
    /** Test hook: replace the one-shot connection (fixture-bound handshakes). */
    private val connectionFactory: (DeviceAuthentication) -> RelayConnection = { auth ->
        RelayConnection(
            url = url,
            authentication = auth,
            client = client,
            handshakeTimeoutMs = handshakeTimeoutMs,
            now = now,
        )
    },
) {
    /** Per-relay session status — the oracle's `connection.status` + latches. */
    sealed interface SessionState {
        /** No dial yet — waiting for [start] or authentication material. */
        data object Idle : SessionState

        /** Dialing or handshaking. */
        data object Connecting : SessionState

        /** Handshake accepted; the framed session is live. */
        data class Connected(val finish: E2EEServerFinish) : SessionState

        /** Down; a redial is scheduled or parked. [reason] is the last close detail. */
        data class Disconnected(val reason: DisconnectReason? = null) : SessionState

        /** `authRejected` — the relay refused this credential (4401). Terminal until re-paired. */
        data class AuthRejected(val reason: DisconnectReason) : SessionState

        /** [close] ran — the session is done. */
        data object Closed : SessionState
    }

    private val _state = MutableStateFlow<SessionState>(SessionState.Idle)
    val state: StateFlow<SessionState> = _state

    /**
     * Decrypted server→client messages across connections. Single-consumer
     * (the oracle has exactly one handler) and lossless within the bound:
     * frames buffer until collected, so early `push_config` traffic survives
     * subscription order. Overflow aborts the connection — frames can't be
     * skipped mid-stream, so the next dial's resync replays a consistent
     * snapshot instead of growing the queue without limit.
     */
    private val incomingChannel =
        Channel<JsonObject>(ReconnectPolicy.INCOMING_BUFFER_CAPACITY)
    val incoming: kotlinx.coroutines.flow.Flow<JsonObject> = incomingChannel.receiveAsFlow()

    // The oracle's store holds exactly one message consumer; transport
    // callers needing fan-out layer a broadcast above this stream.

    /** Wake signal for the dial loop — conflated, like `connectRelay` calls. */
    private val dialNow = Channel<Unit>(Channel.CONFLATED)

    private val sendLock = java.util.concurrent.locks.ReentrantLock()
    private val pendingRequests = ConcurrentHashMap<String, PendingRequest>()
    private val started = AtomicBoolean(false)

    @Volatile
    private var supervisor: Job? = null

    @Volatile
    private var closed = false

    @Volatile
    private var hidden = false

    @Volatile
    private var hiddenSince = 0L

    @Volatile
    private var connectingSince = 0L

    @Volatile
    private var retiredHidden = false

    @Volatile
    private var healthJob: Job? = null

    @Volatile
    private var currentConnection: RelayConnection? = null

    /** Keepalive ping send time — the next inbound frame completes the RTT. */
    @Volatile
    private var keepaliveSentAt = 0L

    private val _rttMs = MutableStateFlow(-1L)

    /**
     * Last measured keepalive round-trip in ms, `-1` until the first reply
     * lands or after a disconnect. The mockup's `relay · transport · 12ms`.
     */
    val rttMs: StateFlow<Long> = _rttMs.asStateFlow()

    private class PendingRequest(
        val deferred: CompletableDeferred<CommandResultMessage>,
        val action: String,
        val actionId: String?,
        @Volatile var timeoutJob: Job?,
    )

    /** The identity the live connection was authenticated as, when connected. */
    val finish: E2EEServerFinish? get() = (state.value as? SessionState.Connected)?.finish

    fun start() {
        if (!started.compareAndSet(false, true)) return
        supervisor = scope.launch { run() }
    }

    /** `disconnectRelay` + stop: terminal. Queued messages stay in [sendBuffer]. */
    fun close() {
        if (closed) return
        closed = true
        dialNow.trySend(Unit)
        currentConnection?.abort()
        supervisor?.cancel()
        failPending("Relay disconnected")
        sendBuffer.close()
        incomingChannel.close()
        _state.value = SessionState.Closed
    }

    /** Hard redial now — drop any live socket, clear backoff, connect immediately. */
    fun reconnect() {
        if (closed) return
        backoff.reset()
        currentConnection?.abort()
        retiredHidden = false
        dialNow.trySend(Unit)
    }

    /**
     * `revalidateConnections` — called on wake/focus/network-restore:
     *
     * - `authRejected`/closed → no-op.
     * - `connecting` ≥ [ReconnectPolicy.STALE_CONNECTING_MS] → the dial is
     *   probably blackholed; replace it.
     * - disconnected → dial now.
     * - connected + silence > [freshProofMs] → the socket is a corpse; redial.
     * - otherwise arm the health timer and ping with `refresh_agents`.
     */
    fun revalidate(timeoutMs: Long = healthTimeoutMs()) {
        if (closed) return
        when (val state = _state.value) {
            is SessionState.AuthRejected, SessionState.Closed -> return
            SessionState.Connecting -> {
                if (now() - connectingSince >= staleConnectingMs) reconnect()
            }
            is SessionState.Connected -> {
                val connection = currentConnection ?: return
                if (now() - connection.lastMessageAt > freshProofMs) {
                    reconnect()
                    return
                }
                if (healthJob?.isActive == true) return
                armHealth(connection, timeoutMs)
                keepaliveSentAt = now()
                if (!connection.sendRaw(KEEPALIVE_JSON)) {
                    keepaliveSentAt = 0
                    clearHealth()
                    reconnect()
                }
            }
            else -> dialNow.trySend(Unit)
        }
    }

    /** `setHidden` — hidden apps stop keepalives after [ReconnectPolicy.HIDDEN_KEEPALIVE_MAX_MS]. */
    fun setHidden(hidden: Boolean) {
        if (this.hidden == hidden) return
        this.hidden = hidden
        this.hiddenSince = if (hidden) now() else 0L
    }

    /**
     * `sendRaw`/`transport.send` with the offline queue in front: while a
     * connection is live and the buffer is empty the frame writes directly;
     * otherwise it queues in [sendBuffer] (bounded — see [PushResult]).
     * A failed direct write falls back to the buffer instead of dropping.
     */
    fun send(message: Inbound): Boolean =
        sendSerialized(InboundCodec.encode(message), message.type)

    fun sendRaw(json: String): Boolean =
        sendSerialized(json, ClientSendBuffer.sniffMessageType(json.toByteArray(Charsets.UTF_8)) ?: "")

    private fun sendSerialized(json: String, kind: String): Boolean = sendLock.withLock {
        val connection = currentConnection
        if (connection != null && sendBuffer.isEmpty()) {
            if (connection.sendRaw(json)) return true
        }
        val data = json.toByteArray(Charsets.UTF_8)
        sendBuffer.pushTyped(data, kind, ClientSendBuffer.isReplaceable(kind)).admitted()
    }

    /**
     * `sendCommand` — write a request and await its `command_result`/
     * `action_receipt`/`error` resolution. The request frame gets
     * `request_id`, `protocol: 3`, and `client_id` like the oracle.
     * Rejects immediately while disconnected — a queued command is answered
     * by nobody. [CommandException.dispatchedUnknown] marks results that may
     * still have landed on the relay.
     */
    suspend fun request(
        message: Inbound,
        timeoutMs: Long = ReconnectPolicy.COMMAND_TIMEOUT_MS,
    ): CommandResultMessage {
        val connection = currentConnection?.takeIf { _state.value is SessionState.Connected }
            ?: throw TransportException.NotConnected()
        val requestId = message.requestId.ifEmpty { UUID.randomUUID().toString() }
        val pending = PendingRequest(
            deferred = CompletableDeferred(),
            action = message.type,
            actionId = message.actionId.ifEmpty { null },
            timeoutJob = null,
        )
        pending.rearm(requestId, timeoutMs)
        pendingRequests[requestId] = pending
        val command = message.copy(
            requestId = requestId,
            protocol = Protocol.VERSION,
            clientId = if (message.type == "lease_pane_size" || message.type == "release_pane_size") {
                message.clientId
            } else {
                clientId
            },
        )
        if (!connection.send(command)) {
            pendingRequests.remove(requestId)
            pending.timeoutJob?.cancel()
            throw TransportException.WriteRejected("Could not send command to relay")
        }
        return pending.deferred.await()
    }

    // ── supervisor loop ─────────────────────────────────────────────────

    private suspend fun run() {
        var floorMs = 0L
        var immediate = true
        while (true) {
            if (closed) return
            if (!immediate) {
                // scheduleReconnect: wait the computed delay; a wake signal
                // (resetReconnectBackoff) clears attempts and dials at once.
                val delayMs = backoff.nextDelay(floorMs)
                if (withTimeoutOrNull(delayMs) { dialNow.receive() } != null) backoff.reset()
            }
            immediate = false
            floorMs = 0L
            if (closed) return
            val authentication = getAuthentication()
            if (authentication == null) {
                _state.value = SessionState.Disconnected()
                parkUntilDialed()
                immediate = true
                continue
            }
            _state.value = SessionState.Connecting
            connectingSince = now()
            val connection = connectionFactory(authentication)
            currentConnection = connection
            val finish = try {
                connection.connect()
            } catch (failed: Exception) {
                when (failed) {
                    is CancellationException -> throw failed
                    else -> {
                        val reason = closeReasonFor(failed)
                        currentConnection = null
                        if (reason.isAuthRejection) {
                            _state.value = SessionState.AuthRejected(reason)
                            return
                        }
                        _state.value = SessionState.Disconnected(reason)
                        // Fatal dial/handshake failures retry at the slowest
                        // cadence — the oracle floors fatal closes at 60 s.
                        floorMs = if (reason.fatal && reason.code != ReconnectPolicy.UNKNOWN_RELAY_CODE) {
                            ReconnectPolicy.MAX_DELAY_MS
                        } else {
                            0L
                        }
                        continue
                    }
                }
            }
            // Enrollment persists before the connection becomes visible
            // (the oracle's onAuthenticated → commitDeviceEnrollment).
            onEnrolled(authentication, finish)
            _state.value = SessionState.Connected(finish)
            // Phase-5 §0 — `client_caps` is the first post-handshake frame;
            // old relays answer `unknown_action`, which the app ignores.
            connection.send(clientCapsFrame())
            connection.sendRaw(KEEPALIVE_JSON)
            drainBuffer(connection)
            val reason = serve(connection)
            if (currentConnection === connection) currentConnection = null
            _state.value = SessionState.Disconnected(reason)
            failPending(reason.reason)
            floorMs = if (reason.fatal && reason.code != ReconnectPolicy.UNKNOWN_RELAY_CODE) {
                ReconnectPolicy.MAX_DELAY_MS
            } else {
                0L
            }
            if (reason.isAuthRejection) {
                _state.value = SessionState.AuthRejected(reason)
                return
            }
            if (retiredHidden) {
                retiredHidden = false
                parkUntilDialed()
                immediate = true
            }
        }
    }

    /**
     * Leaves the dial loop parked until [reconnect]/[revalidate]/[close]
     * signals — a hidden-retired or unpaired session does not churn sockets.
     */
    private suspend fun parkUntilDialed() {
        dialNow.receive()
    }

    /**
     * One live connection's lifetime: forward inbound frames, run the
     * keepalive cadence, return the close reason when the socket ends.
     */
    private suspend fun serve(connection: RelayConnection): DisconnectReason = coroutineScope {
        val forwarder = launch {
            try {
                connection.incoming.collect { message ->
                    // Any inbound frame is proof of life: it answers the
                    // pending keepalive ping (RTT), clears the health probe,
                    // and drops reconnect backoff (reconnectAttempts.delete).
                    val sentAt = keepaliveSentAt
                    if (sentAt > 0L) {
                        _rttMs.value = now() - sentAt
                        keepaliveSentAt = 0L
                    }
                    backoff.reset()
                    clearHealth()
                    dispatch(message)
                }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failed: Exception) {
                // A decrypt/parse failure already terminated the socket —
                // make sure the close state is what the supervisor sees.
                connection.abort()
            }
        }
        val keepalive = launch {
            while (true) {
                delay(keepaliveIntervalMs)
                if (!keepaliveExhausted()) sendKeepalive(connection)
            }
        }
        val closed = connection.state.first { it is RelayConnection.State.Closed }
            as RelayConnection.State.Closed
        forwarder.cancel()
        keepalive.cancel()
        clearHealth()
        keepaliveSentAt = 0L
        _rttMs.value = -1L
        closed.reason
    }

    private suspend fun drainBuffer(connection: RelayConnection) {
        sendLock.withLock {
            while (true) {
                val head = sendBuffer.peek() ?: return
                if (!connection.sendRaw(String(head, Charsets.UTF_8))) return
                sendBuffer.tryPop()
            }
        }
    }

    // ── keepalive / health ──────────────────────────────────────────────

    /** `healthTimeoutMs()` — visible pages probe at 2 s, hidden at 10 s. */
    private fun healthTimeoutMs(): Long =
        if (hidden) backgroundHealthTimeoutMs else foregroundHealthTimeoutMs

    private fun keepaliveExhausted(): Boolean =
        hidden && now() - hiddenSince >= ReconnectPolicy.HIDDEN_KEEPALIVE_MAX_MS

    /**
     * `sendKeepalive` — one `refresh_agents` per live connection with the
     * health timer armed for the reply. The ping never dials; it only proves
     * or kills an existing socket.
     */
    private fun sendKeepalive(connection: RelayConnection) {
        if (connection !== currentConnection || healthJob?.isActive == true) return
        armHealth(connection, backgroundHealthTimeoutMs)
        keepaliveSentAt = now()
        if (!connection.sendRaw(KEEPALIVE_JSON)) {
            keepaliveSentAt = 0
            clearHealth()
            failKeepalive(connection)
        }
    }

    private fun armHealth(connection: RelayConnection, timeoutMs: Long) {
        clearHealth()
        healthJob = scope.launch {
            delay(timeoutMs)
            if (connection === currentConnection) failKeepalive(connection)
        }
    }

    private fun clearHealth() {
        healthJob?.cancel()
        healthJob = null
    }

    /**
     * `failKeepalive` — an unanswered ping means the socket is gone. A
     * visible session redials at once; a hidden one retires the connection
     * and leaves the relay parked for the next revalidation.
     */
    private fun failKeepalive(connection: RelayConnection) {
        if (hidden) {
            retiredHidden = true
        }
        connection.abort()
    }

    // ── inbound dispatch ────────────────────────────────────────────────

    private fun dispatch(message: JsonObject) {
        val type = message.stringField("type")
        val requestId = message.stringField("request_id")
        when (type) {
            "command_result" -> handleCommandResult(requestId, message)
            "action_receipt" -> handleActionReceipt(requestId, message)
            "error" -> handleApiError(requestId, message)
        }
        if (incomingChannel.trySend(message).isFailure) {
            // Demux stalled past the bound — the stream can't skip frames,
            // so kill the socket; redial + resync replays consistent state.
            currentConnection?.abort()
        }
    }

    private fun handleCommandResult(requestId: String?, raw: JsonObject) {
        val pending = requestId?.let { pendingRequests[it] } ?: return
        val result = try {
            LerdrJson.decodeFromJsonElement(CommandResultMessage.serializer(), raw)
        } catch (invalid: IllegalArgumentException) {
            return
        }
        if (result.phase == CommandResultMessage.PHASE_ACCEPTED) {
            pending.rearm(requestId, ReconnectPolicy.ACCEPTED_COMMAND_TIMEOUT_MS)
            return
        }
        pendingRequests.remove(requestId)
        pending.timeoutJob?.cancel()
        if (result.ok == true) {
            pending.deferred.complete(result)
        } else {
            pending.deferred.completeExceptionally(
                CommandException(
                    message = result.error ?: "Command failed",
                    phase = result.phase,
                    dispatchedUnknown = result.phase == "dispatched_unknown",
                ),
            )
        }
    }

    private fun handleActionReceipt(requestId: String?, raw: JsonObject) {
        val receiptMessage = try {
            LerdrJson.decodeFromJsonElement(ActionReceiptMessage.serializer(), raw)
        } catch (invalid: IllegalArgumentException) {
            return
        }
        val receipt = receiptMessage.receipt ?: return
        var key = requestId
        var pending = key?.let { pendingRequests[it] }
        if (pending == null) {
            // Receipts correlate by request_id, falling back to the action_id
            // the caller embedded in the command's intent.
            for ((candidateId, candidate) in pendingRequests) {
                if (candidate.actionId != null && candidate.actionId == receipt.actionId) {
                    key = candidateId
                    pending = candidate
                    break
                }
            }
        }
        if (pending == null || key == null) return
        when (receipt.phase) {
            ActionReceiptPhase.PREPARED, ActionReceiptPhase.AWAITING_EVIDENCE ->
                pending.rearm(key, ReconnectPolicy.ACCEPTED_COMMAND_TIMEOUT_MS)
            ActionReceiptPhase.CONFIRMED -> {
                pendingRequests.remove(key)
                pending.timeoutJob?.cancel()
                pending.deferred.complete(
                    CommandResultMessage(
                        action = pending.action,
                        ok = true,
                        phase = ActionReceiptPhase.CONFIRMED.wireName(),
                        requestId = key,
                        data = buildJsonObject {
                            raw["receipt"]?.let { put("receipt", it) }
                        },
                    ),
                )
            }
            else -> {
                pendingRequests.remove(key)
                pending.timeoutJob?.cancel()
                val detail = raw.stringField("detail")?.takeIf {
                    it.toByteArray(Charsets.UTF_8).size <= 512
                }
                pending.deferred.completeExceptionally(
                    CommandException(
                        message = detail ?: receipt.error?.code ?: "Command failed",
                        phase = receipt.phase.wireName(),
                        apiError = receipt.error,
                        dispatchedUnknown = receipt.phase == ActionReceiptPhase.DISPATCHED_UNKNOWN,
                    ),
                )
            }
        }
    }

    private fun handleApiError(requestId: String?, raw: JsonObject) {
        val pending = requestId?.let { pendingRequests[it] } ?: return
        val error = try {
            LerdrJson.decodeFromJsonElement(ErrorMessage.serializer(), raw)
        } catch (invalid: IllegalArgumentException) {
            return
        }
        val apiError = error.error ?: return
        pendingRequests.remove(requestId)
        pending.timeoutJob?.cancel()
        val detail = raw.stringField("detail")?.takeIf {
            it.toByteArray(Charsets.UTF_8).size <= 512
        }
        pending.deferred.completeExceptionally(
            CommandException(
                message = detail ?: apiError.code,
                code = apiError.code,
                phase = "failed_before_dispatch",
                apiError = apiError,
            ),
        )
    }

    /** `rejectPendingOperations` — writes that landed may still be acted on. */
    private fun failPending(reason: String) {
        for ((requestId, pending) in pendingRequests) {
            if (!pendingRequests.remove(requestId, pending)) continue
            pending.timeoutJob?.cancel()
            pending.deferred.completeExceptionally(
                CommandException(
                    message = reason,
                    phase = "dispatched_unknown",
                    dispatchedUnknown = true,
                ),
            )
        }
    }

    private fun PendingRequest.rearm(requestId: String, timeoutMs: Long) {
        timeoutJob?.cancel()
        timeoutJob = scope.launch {
            delay(timeoutMs)
            if (pendingRequests.remove(requestId, this@rearm)) {
                deferred.completeExceptionally(
                    CommandException(
                        message = "Relay confirmation timed out",
                        phase = "dispatched_unknown",
                        dispatchedUnknown = true,
                    ),
                )
            }
        }
    }

    private fun closeReasonFor(failure: Exception): DisconnectReason = when (failure) {
        is TransportException.ConnectionClosed -> failure.detail
        is TransportException.EncryptionRequired ->
            DisconnectReason(failure.message ?: "Relay did not negotiate encrypted transport")
        else -> DisconnectReason(failure.message ?: "Relay connection failed", cause = failure)
    }

    private fun JsonObject.stringField(key: String): String? =
        (this[key] as? JsonPrimitive)?.takeIf { it.isString }?.content

    private fun ActionReceiptPhase.wireName(): String = when (this) {
        ActionReceiptPhase.PREPARED -> "prepared"
        ActionReceiptPhase.FAILED_BEFORE_DISPATCH -> "failed_before_dispatch"
        ActionReceiptPhase.AWAITING_EVIDENCE -> "awaiting_evidence"
        ActionReceiptPhase.CONFIRMED -> "confirmed"
        ActionReceiptPhase.DISPATCHED_UNKNOWN -> "dispatched_unknown"
    }

    companion object {
        /** `{"type":"refresh_agents"}` — the health ping every deployed relay answers. */
        const val KEEPALIVE_JSON = "{\"type\":\"refresh_agents\"}"

        /**
         * Phase-5 §0 `client_caps` — announces this app's capability set
         * and the preferred inner codec. Sent unconditionally; the live
         * set is `server-advertised ∩ this` (docs/13).
         */
        private fun clientCapsFrame(): Inbound = Inbound(
            type = "client_caps",
            protocol = Protocol.VERSION,
            capabilities = ClientCapabilities.ANNOUNCED,
            preferredInnerCodec = ClientCapabilities.PREFERRED_INNER_CODEC,
        )
    }
}
