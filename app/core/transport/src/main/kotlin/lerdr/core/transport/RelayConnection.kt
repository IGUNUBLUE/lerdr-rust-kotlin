package lerdr.core.transport

import com.lerdr.core.e2ee.E2EEClientHandshake
import com.lerdr.core.e2ee.E2EEException
import com.lerdr.core.e2ee.E2EEServerFinish
import com.lerdr.core.e2ee.E2EESession
import java.nio.ByteBuffer
import java.nio.charset.CharacterCodingException
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.selects.select
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.JsonObject
import lerdr.core.model.Inbound
import lerdr.core.protocol.InboundCodec
import lerdr.core.protocol.LerdrJson
import lerdr.core.protocol.Protocol
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okio.ByteString

/**
 * A single `herdr-e2ee-v2` WebSocket session: one-shot connect → E2EE
 * handshake → framed JSON session. Mirrors the oracle's encrypted transport
 * (`frontend/src/lib/transports/encrypted.ts` + `websocket.ts`):
 *
 * ```
 * connect(): open socket → check subprotocol → client hello (plaintext)
 *          → server hello → e2ee_client_finish (sealed) → server finish (sealed)
 *          → Connected; inbound Flow<JsonObject> opens.
 * send*():  seal plaintext JSON → text frame; refused unless Connected.
 * close/abort: graceful (1000) or immediate teardown.
 * ```
 *
 * Single-use: one instance is one socket; the supervisor ([RelaySession])
 * builds a fresh one per reconnect. [incoming] is a single-consumer stream —
 * collect it once; [RelaySession] owns fan-out to multiple consumers.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class RelayConnection(
    private val url: String,
    private val authentication: DeviceAuthentication,
    private val client: OkHttpClient = OkHttpClient(),
    private val handshakeTimeoutMs: Long = ReconnectPolicy.HANDSHAKE_TIMEOUT_MS,
    private val now: () -> Long = System::currentTimeMillis,
    /** Fixture hook: tests inject a handshake with deterministic nonce/ephemeral. */
    private val handshakeFactory: (DeviceAuthentication) -> E2EEClientHandshake = { it.newHandshake() },
) {
    /** `TransportStatus` for one socket. */
    sealed interface State {
        /** Dialing or mid-handshake — the oracle's `connecting`. */
        data object Connecting : State

        /** Server finish accepted; the framed session is live. */
        data class Connected(val finish: E2EEServerFinish) : State

        /** Terminal — see [DisconnectReason.isAuthRejection] for the no-retry latch. */
        data class Closed(val reason: DisconnectReason) : State
    }

    private val started = AtomicBoolean(false)
    private val userClosed = AtomicBoolean(false)

    private val _state = MutableStateFlow<State>(State.Connecting)
    val state: StateFlow<State> = _state

    /**
     * Raw inbound text frames as delivered, before decryption. Bounded —
     * a stalled decrypt loop can't let this grow without limit; overflow
     * kills the socket so the next dial resyncs a consistent stream.
     */
    private val frames = Channel<ByteArray>(ReconnectPolicy.FRAME_BUFFER_CAPACITY)

    private val opened = CompletableDeferred<Response>()
    private val disconnect = CompletableDeferred<DisconnectReason>()
    private val ready = CompletableDeferred<Unit>()
    private val terminated = AtomicBoolean()

    @Volatile
    private var socketRef: WebSocket? = null

    @Volatile
    private var sessionRef: E2EESession? = null
    private val sealLock = Any()

    /** `connection.lastMessageAt` — set on handshake completion and every decoded message. */
    @Volatile
    var lastMessageAt: Long = 0L
        private set

    /**
     * Decrypted server→client messages as parsed JSON objects. Starts
     * emitting only after the handshake completes; ends when the socket
     * closes (the [State.Closed] reason says why). A decrypt or plaintext
     * parse failure terminates the socket — Go's `decodeWebSocketMessage`
     * parity — and rethrows into the collector.
     */
    val incoming: Flow<JsonObject> = flow {
        ready.await()
        val session = sessionRef ?: return@flow
        while (true) {
            val raw = try {
                awaitFrame()
            } catch (closed: TransportException.ConnectionClosed) {
                return@flow
            }
            val message = try {
                decodeMessage(session.open(raw))
            } catch (failure: Exception) {
                failIncoming(failure)
                throw failure
            }
            lastMessageAt = now()
            emit(message)
        }
    }

    /**
     * `transport.connect()` — drives the whole open→handshake→finish exchange
     * under the 10 s handshake timeout.
     *
     * @return the authenticated server finish (identity + issued credential
     *   secret for invitation handshakes) — persist it before declaring the
     *   connection usable, like the oracle's `onAuthenticated`.
     * @throws TransportException.HandshakeTimeout after 10 s
     * @throws TransportException.EncryptionRequired when the subprotocol was not negotiated
     * @throws TransportException.ConnectionClosed on socket close/failure mid-handshake
     *   (`detail.isAuthRejection` marks the 4401 refusal)
     * @throws E2EEException on crypto/format failure (see `crypto.failures` taxonomy)
     */
    suspend fun connect(): E2EEServerFinish {
        check(started.compareAndSet(false, true)) { "RelayConnection is single-use" }
        val request = Request.Builder()
            .url(url)
            .header("Sec-WebSocket-Protocol", Protocol.ENCRYPTED_WEBSOCKET_SUBPROTOCOL)
            .build()
        socketRef = client.newWebSocket(request, Listener())
        val finish = try {
            withTimeout(handshakeTimeoutMs) { driveHandshake() }
        } catch (timeout: TimeoutCancellationException) {
            socketRef?.cancel()
            terminate(DisconnectReason("Encrypted relay handshake timed out"))
            throw TransportException.HandshakeTimeout()
        } catch (cancelled: CancellationException) {
            socketRef?.cancel()
            terminate(DisconnectReason("Relay connection cancelled"))
            throw cancelled
        } catch (failure: TransportException) {
            socketRef?.cancel()
            throw failure
        } catch (failure: E2EEException) {
            socketRef?.cancel()
            terminate(DisconnectReason(failure.message ?: "Encrypted relay handshake failed"))
            throw failure
        }
        lastMessageAt = now()
        _state.value = State.Connected(finish)
        ready.complete(Unit)
        return finish
    }

    private suspend fun driveHandshake(): E2EEServerFinish {
        val response = awaitOpen()
        // A relay that ignores the encrypted subprotocol would otherwise get a
        // plaintext hello — refuse the socket before anything is sent
        // (websocket.ts: `socket.protocol !== E2EE_SUBPROTOCOL`).
        if (response.header("Sec-WebSocket-Protocol") != Protocol.ENCRYPTED_WEBSOCKET_SUBPROTOCOL) {
            socketRef?.cancel()
            terminate(DisconnectReason("Relay did not negotiate encrypted transport"))
            throw TransportException.EncryptionRequired()
        }
        val handshake = handshakeFactory(authentication)
        writeFrame(handshake.clientHello())
        val result = handshake.acceptServerHello(awaitFrame())
        sessionRef = result.session
        writeFrame(handshake.clientFinish(result.session))
        return handshake.acceptServerFinish(result.session, awaitFrame())
    }

    /** `send(payload)` — seal one message and write it as a text frame. */
    fun send(message: Inbound): Boolean = sendRaw(InboundCodec.encode(message))

    /** `send(payload)` for an already-decoded message shape. */
    fun send(payload: JsonObject): Boolean = sendRaw(payload.toString())

    /**
     * Oracle `sendRaw`/`transport.send`: false unless the session is ready,
     * the encrypt step and the socket write both succeed. Sealing is
     * serialized so frame sequences stay strictly ordered.
     */
    fun sendRaw(json: String): Boolean {
        if (!ready.isCompleted) return false
        val session = sessionRef ?: return false
        val socket = socketRef ?: return false
        val frame = try {
            synchronized(sealLock) { session.seal(json.toByteArray(Charsets.UTF_8)) }
        } catch (failure: E2EEException) {
            terminate(DisconnectReason("Could not encrypt relay message"))
            socket.cancel()
            return false
        }
        return socket.send(String(frame, Charsets.UTF_8))
    }

    /** Graceful close (1000); the peer's close handshake completes termination. */
    fun close() {
        userClosed.set(true)
        val socket = socketRef
        if (socket == null) {
            terminate(DisconnectReason("Relay connection closed"))
            return
        }
        socket.close(1000, "")
    }

    /** Immediate teardown — drops the socket without waiting for the close handshake. */
    fun abort() {
        userClosed.set(true)
        socketRef?.cancel()
        terminate(DisconnectReason("Relay disconnected"))
    }

    private suspend fun awaitOpen(): Response = select {
        opened.onAwait { it }
        disconnect.onAwait { throw TransportException.ConnectionClosed(it) }
    }

    private suspend fun awaitFrame(): ByteArray = select {
        frames.onReceiveCatching { result ->
            result.getOrNull() ?: throw TransportException.ConnectionClosed(
                if (disconnect.isCompleted) {
                    disconnect.getCompleted()
                } else {
                    DisconnectReason("Relay disconnected")
                },
            )
        }
        disconnect.onAwait { throw TransportException.ConnectionClosed(it) }
    }

    private fun writeFrame(frame: ByteArray) {
        val socket = socketRef ?: throw TransportException.WriteRejected()
        if (socket.send(String(frame, Charsets.UTF_8))) return
        // A refused write during handshake is almost always a lost race with
        // an inbound close — surface the close detail when it has landed.
        if (disconnect.isCompleted) {
            throw TransportException.ConnectionClosed(disconnect.getCompleted())
        }
        throw TransportException.WriteRejected()
    }

    private fun decodeMessage(plaintext: ByteArray): JsonObject {
        val text = try {
            Charsets.UTF_8.newDecoder().decode(ByteBuffer.wrap(plaintext)).toString()
        } catch (invalid: CharacterCodingException) {
            throw E2EEException.Format("relay message is not valid UTF-8", invalid)
        }
        val element = try {
            LerdrJson.parseToJsonElement(text)
        } catch (invalid: IllegalArgumentException) {
            throw E2EEException.Format("relay message is not valid JSON", invalid)
        }
        return element as? JsonObject
            ?: throw E2EEException.Format("relay message is not a JSON object")
    }

    private fun failIncoming(failure: Exception) {
        val reason = failure.message ?: "Encrypted relay connection failed"
        terminate(DisconnectReason(reason))
        socketRef?.cancel()
    }

    private fun terminate(reason: DisconnectReason) {
        if (!terminated.compareAndSet(false, true)) return
        // State first: completing `disconnect` resumes awaitFrame's onAwait
        // branch immediately, so collectors can observe the flow ending
        // before this call returns — Closed must already be visible.
        _state.value = State.Closed(reason)
        disconnect.complete(reason)
        frames.close()
        ready.completeExceptionally(TransportException.ConnectionClosed(reason))
    }

    private inner class Listener : WebSocketListener() {
        override fun onOpen(webSocket: WebSocket, response: Response) {
            opened.complete(response)
        }

        override fun onMessage(webSocket: WebSocket, text: String) {
            if (frames.trySend(text.toByteArray(Charsets.UTF_8)).isFailure) {
                // Pane/command frames can't be skipped mid-stream — overflow
                // means the consumer stalled, so drop the socket and let the
                // supervisor redial + resync instead of buffering forever.
                terminate(DisconnectReason("relay inbound queue overflow"))
                webSocket.cancel()
            }
        }

        override fun onMessage(webSocket: WebSocket, bytes: ByteString) {
            // The JSON codec speaks text frames only (Go's requireText path).
            terminate(DisconnectReason("unexpected binary relay frame"))
            webSocket.cancel()
        }

        override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
            webSocket.close(code, reason)
        }

        override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
            terminate(mapClose(code, reason))
        }

        override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
            terminate(DisconnectReason("Relay connection failed", cause = t))
        }

        private fun mapClose(code: Int, reason: String): DisconnectReason = when {
            // 4401: the relay refuses this device's credential. Retrying
            // replays the same rejected material — the supervisor must not.
            code == ReconnectPolicy.UNAUTHORIZED_CLOSE_CODE -> DisconnectReason(
                reason = "This relay no longer accepts this device",
                code = ReconnectPolicy.DEVICE_UNAUTHORIZED_CODE,
                wsCloseCode = code,
                wsReason = reason.ifEmpty { null },
                fatal = true,
            )
            userClosed.get() -> DisconnectReason(
                reason = "Relay connection closed",
                wsCloseCode = code,
                wsReason = reason.ifEmpty { null },
            )
            else -> DisconnectReason(
                reason = "Relay disconnected",
                wsCloseCode = code,
                wsReason = reason.ifEmpty { null },
            )
        }
    }
}
