package com.lerdr.app.session

import com.lerdr.core.e2ee.E2EEServerFinish
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.serialization.json.JsonObject
import lerdr.core.model.CommandResultMessage
import lerdr.core.model.Inbound
import lerdr.core.transport.DeviceAuthentication
import lerdr.core.transport.ReconnectPolicy
import lerdr.core.transport.RelaySession
import okhttp3.OkHttpClient

/**
 * The repository-facing view of one supervised relay session. Exists so
 * JVM unit tests can drive [SessionRepository] with an in-memory fake —
 * `RelaySession` is a concrete final class whose socket lifecycle cannot be
 * stubbed, so the seam is declared here and implemented by delegation.
 */
interface RelaySessionHandle {
    /** Session lifecycle — mirrors [RelaySession.state]. */
    val state: StateFlow<RelaySession.SessionState>

    /** Last keepalive round-trip in ms; -1 while unmeasured/disconnected. */
    val rttMs: StateFlow<Long>

    /**
     * Decrypted server→client frames, raw JSON. Single-consumer — the
     * repository is the only collector ([RelaySession.incoming] parity).
     */
    val incoming: Flow<JsonObject>

    /** Fire-and-forget send; false when refused (queued frames report true). */
    fun sendRaw(json: String): Boolean

    /**
     * Phase-5 §2.4 `upload_binary` — send binary plaintext (`0x03` chunk
     * carrier) directly; never queued. False unless written now.
     */
    fun sendBytes(payload: ByteArray): Boolean

    /** Fire-and-forget send of a typed inbound message. */
    fun send(message: Inbound): Boolean

    /** Request/response command — resolves on the correlated `command_result`. */
    suspend fun request(
        message: Inbound,
        timeoutMs: Long = ReconnectPolicy.COMMAND_TIMEOUT_MS,
    ): CommandResultMessage

    /** Hard redial — clears backoff and dials now (also un-parks a no-auth dial). */
    fun reconnect()

    /** `revalidateConnections` — foreground/wake health probe. */
    fun revalidate()

    /** `setHidden` — hidden sessions stop keepalives after the oracle's cap. */
    fun setHidden(hidden: Boolean)

    /** Terminal teardown. */
    fun close()
}

/** Production adapter — [RelaySession] is already running when handed over. */
private class RelaySessionAdapter(
    private val session: RelaySession,
) : RelaySessionHandle {
    override val state: StateFlow<RelaySession.SessionState> get() = session.state
    override val rttMs: StateFlow<Long> get() = session.rttMs
    override val incoming: Flow<JsonObject> get() = session.incoming

    override fun sendRaw(json: String): Boolean = session.sendRaw(json)
    override fun sendBytes(payload: ByteArray): Boolean = session.sendBytes(payload)
    override fun send(message: Inbound): Boolean = session.send(message)
    override suspend fun request(message: Inbound, timeoutMs: Long): CommandResultMessage =
        session.request(message, timeoutMs)

    override fun reconnect() = session.reconnect()
    override fun revalidate() = session.revalidate()
    override fun setHidden(hidden: Boolean) = session.setHidden(hidden)
    override fun close() = session.close()
}

/**
 * Creates and starts one session handle per relay endpoint. The real factory
 * dials `ws(s)://host/ws`; tests substitute an in-memory handle.
 */
fun interface RelaySessionFactory {
    fun create(
        url: String,
        scope: CoroutineScope,
        getAuthentication: () -> DeviceAuthentication?,
        onEnrolled: suspend (DeviceAuthentication, E2EEServerFinish) -> Unit,
    ): RelaySessionHandle

    companion object {
        /** Default transport factory — the websocket path every relay speaks. */
        val websocket: RelaySessionFactory = RelaySessionFactory { url, scope, auth, onEnrolled ->
            RelaySession(
                url = url,
                scope = scope,
                getAuthentication = auth,
                onEnrolled = onEnrolled,
                client = sharedClient,
            ).also { it.start() }.let(::RelaySessionAdapter)
        }

        /** One OkHttpClient for the app — the oracle shares a connection pool too. */
        private val sharedClient: OkHttpClient by lazy { OkHttpClient() }
    }
}
