package com.lerdr.app.session

import com.lerdr.core.e2ee.E2EEServerFinish
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.receiveAsFlow
import kotlinx.serialization.json.JsonObject
import lerdr.core.data.CredentialEnrollment
import lerdr.core.data.CredentialStore
import lerdr.core.data.RelayDeviceAuth
import lerdr.core.data.RelayDeviceCredential
import lerdr.core.data.RelayInvitation
import lerdr.core.model.CommandResultMessage
import lerdr.core.model.Inbound
import lerdr.core.transport.DeviceAuthentication
import lerdr.core.transport.ReconnectPolicy
import lerdr.core.transport.RelaySession

/**
 * In-memory [RelaySessionHandle] — the test double the factory injects.
 * `incoming` is a channel the test feeds; `sentRaw`/`requests` record every
 * outbound frame for assertions.
 */
class FakeRelaySessionHandle(
    private val scope: CoroutineScope,
) : RelaySessionHandle {

    private val _state = MutableStateFlow<RelaySession.SessionState>(RelaySession.SessionState.Idle)
    override val state: StateFlow<RelaySession.SessionState> = _state

    private val _rttMs = MutableStateFlow(-1L)
    override val rttMs: StateFlow<Long> = _rttMs

    private val incomingChannel = Channel<JsonObject>(Channel.UNLIMITED)
    override val incoming: Flow<JsonObject> = incomingChannel.receiveAsFlow()

    /** Every raw frame the repository wrote, in order. */
    val sentRaw = mutableListOf<String>()

    /** Typed messages sent via [send]. */
    val sentTyped = mutableListOf<Inbound>()

    /** `request` invocations — the test assigns [responder] to complete them. */
    val requests = mutableListOf<Inbound>()

    /** The auth material the factory captured at create() time. */
    var lastAuthLookup: (() -> DeviceAuthentication?)? = null
    var enrollmentHandler: (suspend (DeviceAuthentication, E2EEServerFinish) -> Unit)? = null

    var reconnectCount = 0
        private set
    var revalidateCount = 0
        private set
    var hidden = false
        private set
    var closed = false
        private set

    /** Responder for [request] — default completes `ok`. */
    var responder: suspend (Inbound) -> CommandResultMessage = { message ->
        CommandResultMessage(
            action = message.type,
            ok = true,
            phase = CommandResultMessage.PHASE_COMPLETED,
            requestId = message.requestId,
        )
    }

    override fun sendRaw(json: String): Boolean {
        sentRaw += json
        return true
    }

    /** `0x03` binary upload chunks sent through [sendBytes]. */
    val sentBytes = mutableListOf<ByteArray>()

    override fun sendBytes(payload: ByteArray): Boolean {
        sentBytes += payload
        return true
    }

    override fun send(message: Inbound): Boolean {
        sentTyped += message
        return true
    }

    override suspend fun request(
        message: Inbound,
        timeoutMs: Long,
    ): CommandResultMessage {
        requests += message
        return responder(message)
    }

    override fun reconnect() {
        reconnectCount++
    }

    override fun revalidate() {
        revalidateCount++
    }

    override fun setHidden(hidden: Boolean) {
        this.hidden = hidden
    }

    override fun close() {
        closed = true
        _state.value = RelaySession.SessionState.Closed
    }

    // ── test drivers ──────────────────────────────────────────────────

    suspend fun emit(raw: JsonObject) = incomingChannel.send(raw)

    fun connect(finish: E2EEServerFinish = testFinish()) {
        _state.value = RelaySession.SessionState.Connected(finish)
    }

    fun disconnect() {
        _state.value = RelaySession.SessionState.Disconnected(
            lerdr.core.transport.DisconnectReason(reason = "test disconnect"),
        )
    }

    /** `Disconnected()` with no reason — the real session's no-auth park. */
    fun statePark() {
        _state.value = RelaySession.SessionState.Disconnected()
    }

    fun rejectAuth() {
        _state.value = RelaySession.SessionState.AuthRejected(
            lerdr.core.transport.DisconnectReason(
                reason = "device unauthorized",
                code = ReconnectPolicy.DEVICE_UNAUTHORIZED_CODE,
                fatal = true,
            ),
        )
    }

    suspend fun enroll(
        auth: DeviceAuthentication = DeviceAuthentication.invitation(
            "inv1", ByteArray(32),
        ),
        finish: E2EEServerFinish = testFinish(withSecret = true),
    ) {
        enrollmentHandler?.invoke(auth, finish)
    }

    companion object {
        /**
         * `E2EEServerFinish`'s constructor is `internal` to core:e2ee — the
         * fake builds it reflectively. Field order: plaintext, deviceId,
         * credentialId, role(String), locale, credentialVersion,
         * credentialSecret(ByteArray?).
         */
        fun testFinish(withSecret: Boolean = false): E2EEServerFinish {
            val type = Class.forName("com.lerdr.core.e2ee.E2EEServerFinish")
            val ctor = type.declaredConstructors.single {
                it.parameterCount == 7
            }
            ctor.isAccessible = true
            @Suppress("UNCHECKED_CAST")
            return ctor.newInstance(
                byteArrayOf(0),
                "dev-1",
                "cred-1",
                // DeviceRole wire name — "owner" is not a valid role.
                "controller",
                "en",
                1L,
                if (withSecret) ByteArray(32) { it.toByte() } else null,
            ) as E2EEServerFinish
        }
    }
}

/** Recording factory — hands out [FakeRelaySessionHandle] per relay. */
class FakeRelaySessionFactory(
    private val scope: CoroutineScope,
) : RelaySessionFactory {
    val created = linkedMapOf<String, FakeRelaySessionHandle>()
    val urls = mutableMapOf<String, String>()

    override fun create(
        url: String,
        scope: CoroutineScope,
        getAuthentication: () -> DeviceAuthentication?,
        onEnrolled: suspend (DeviceAuthentication, E2EEServerFinish) -> Unit,
    ): RelaySessionHandle {
        val handle = FakeRelaySessionHandle(scope).apply {
            lastAuthLookup = getAuthentication
            enrollmentHandler = onEnrolled
        }
        created[url] = handle
        urls[url] = url
        return handle
    }

    /** The handle for a relay endpoint's socket url (`origin + "/ws"`). */
    fun handleFor(origin: String): FakeRelaySessionHandle? = created["$origin/ws"]
}

/** In-memory [CredentialStore] — the JVM twin of KeystoreCredentialStore. */
class FakeCredentialStore : CredentialStore {
    private val _records = MutableStateFlow<Map<String, RelayDeviceAuth>>(emptyMap())
    override val records: StateFlow<Map<String, RelayDeviceAuth>> = _records

    suspend fun seed(relayId: String, auth: RelayDeviceAuth) {
        _records.value += (relayId to auth)
    }

    override suspend fun get(relayId: String): RelayDeviceAuth? = _records.value[relayId]

    override suspend fun saveInvitation(
        relayId: String,
        invitation: RelayInvitation,
    ): RelayInvitation {
        require(!invitation.isExpired(System.currentTimeMillis())) {
            "The device invitation has expired."
        }
        _records.value += (relayId to invitation)
        return invitation
    }

    override suspend fun redeemInvitation(
        relayId: String,
        invitationId: String,
        enrollment: CredentialEnrollment,
    ): RelayDeviceCredential {
        val secret = enrollment.credentialSecret
            ?: throw IllegalArgumentException("Relay did not issue a device credential.")
        // KeystoreCredentialStore's private toCredential, inlined.
        val credential = RelayDeviceCredential(
            id = enrollment.credentialId,
            version = enrollment.credentialVersion,
            secret = java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(secret),
            deviceId = enrollment.deviceId,
            role = enrollment.role,
            locale = enrollment.locale,
            issuedAtEpochMs = System.currentTimeMillis(),
            invitationId = invitationId,
        )
        _records.value += (relayId to credential)
        return credential
    }

    override suspend fun updateCredential(
        relayId: String,
        enrollment: CredentialEnrollment,
    ): RelayDeviceCredential {
        val current = _records.value[relayId]
        check(current is RelayDeviceCredential) { "No enrolled credential for $relayId" }
        val updated = current.copy(version = enrollment.credentialVersion)
        _records.value += (relayId to updated)
        return updated
    }

    override suspend fun remove(relayId: String): Boolean {
        val existed = _records.value.containsKey(relayId)
        _records.value -= relayId
        return existed
    }

    override suspend fun clear() {
        _records.value = emptyMap()
    }
}

/**
 * In-memory [AttachmentSource] — `content://…` uris resolve to fixed bytes;
 * `opens` records each fresh stream (hashing/upload passes re-open).
 */
class FakeAttachmentSource(
    private val files: Map<String, ByteArray>,
    private val names: Map<String, String> = emptyMap(),
    private val mimes: Map<String, String?> = emptyMap(),
) : AttachmentSource {
    val opens = mutableListOf<String>()

    override fun probe(uri: String): AttachmentProbe? {
        val bytes = files[uri] ?: return null
        return AttachmentProbe(
            name = names[uri] ?: uri.substringAfterLast('/'),
            mediaType = mimes[uri] ?: "text/plain",
            bytes = bytes.size.toLong(),
        )
    }

    override fun open(uri: String): java.io.InputStream? =
        files[uri]?.let { opens += uri; java.io.ByteArrayInputStream(it) }
}
