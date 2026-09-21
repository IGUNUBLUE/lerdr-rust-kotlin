package lerdr.core.transport

import com.google.common.truth.Truth.assertThat
import com.lerdr.core.e2ee.E2EECodec
import com.lerdr.core.e2ee.E2EEClientHandshake
import com.lerdr.core.e2ee.E2EESession
import com.lerdr.core.testing.Fixtures
import com.lerdr.core.testing.array
import com.lerdr.core.testing.b64
import com.lerdr.core.testing.long
import com.lerdr.core.testing.string
import java.math.BigInteger
import java.security.AlgorithmParameters
import java.security.KeyFactory
import java.security.KeyPair
import java.security.spec.ECGenParameterSpec
import java.security.spec.ECParameterSpec
import java.security.spec.ECPoint
import java.security.spec.ECPrivateKeySpec
import java.security.spec.ECPublicKeySpec
import java.util.concurrent.ConcurrentLinkedQueue
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.filterIsInstance
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import lerdr.core.model.Inbound
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.OkHttpClient
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import org.junit.After
import org.junit.Before
import org.junit.Test

/**
 * Supervisor tests over MockWebServer. Each connection attempt replays the
 * `crypto.handshake.credential` fixture handshake (the client presents the
 * vector's deterministic nonce/ephemeral, so every dial produces identical
 * wire bytes), then the scripted server drives close/reconnect scenarios.
 */
class RelaySessionTest {

    private val vector = Fixtures.named("crypto.handshake.credential")
        .vector("credential-basic")

    private val server = MockWebServer()
    private val httpClient = OkHttpClient()

    private val authentication = DeviceAuthentication.credential(
        id = vector.string("auth_id"),
        version = vector.long("auth_version"),
        secret = vector.b64("secret_b64"),
        locale = vector.string("locale"),
    )

    private val serverFinishPlaintext = vector.array("finish_frames")
        .map(JsonElement::jsonObject)
        .first { it.string("direction") == "s2c" }
        .b64("plaintext_b64")

    private fun serverSession() = E2EESession.server(
        vector.b64("session_key_c2s_b64"),
        vector.b64("session_key_s2c_b64"),
        E2EECodec.JSON,
    )

    @Before
    fun setUp() {
        server.start()
    }

    @After
    fun tearDown() {
        server.close()
        httpClient.dispatcher.executorService.shutdown()
        httpClient.connectionPool.evictAll()
    }

    private fun p256KeyPair(privateScalar: ByteArray, publicUncompressed: ByteArray): KeyPair {
        val params = AlgorithmParameters.getInstance("EC")
            .apply { init(ECGenParameterSpec("secp256r1")) }
            .getParameterSpec(ECParameterSpec::class.java)
        val keyFactory = KeyFactory.getInstance("EC")
        val privateKey = keyFactory.generatePrivate(
            ECPrivateKeySpec(BigInteger(1, privateScalar), params),
        )
        val point = ECPoint(
            BigInteger(1, publicUncompressed, 1, 32),
            BigInteger(1, publicUncompressed, 33, 32),
        )
        return KeyPair(keyFactory.generatePublic(ECPublicKeySpec(point, params)), privateKey)
    }

    private fun wsUrl(): String =
        server.url("/ws").toString().replaceFirst("http:", "ws:")

    private fun fixtureConnectionFactory(): (DeviceAuthentication) -> RelayConnection = { auth ->
        RelayConnection(
            url = wsUrl(),
            authentication = auth,
            client = httpClient,
            handshakeFactory = { presented ->
                E2EEClientHandshake(
                    selector = presented.selector,
                    secret = vector.b64("secret_b64"),
                    nonce = vector.b64("client_nonce_b64"),
                    ephemeral = p256KeyPair(
                        vector.b64("client_ephemeral_priv_b64"),
                        vector.b64("client_ephemeral_pub_b64"),
                    ),
                )
            },
        )
    }

    private fun upgrade(listener: WebSocketListener) {
        server.enqueue(
            MockResponse.Builder()
                .setHeader("Sec-WebSocket-Protocol", "herdr-e2ee-v2")
                .webSocketUpgrade(listener)
                .build(),
        )
    }

    /**
     * Server-side handshake driver: answers the client hello, then the finish
     * exchange, then hands the live socket (with a seq-1-advanced session) to
     * [afterEstablished]. [onPostHandshake] sees every later client frame
     * decrypted.
     */
    private fun handshakeListener(
        afterEstablished: (WebSocket, E2EESession) -> Unit = { _, _ -> },
        onPostHandshake: (WebSocket, E2EESession, JsonObject) -> Unit = { _, _, _ -> },
    ): WebSocketListener = object : WebSocketListener() {
        private val session = serverSession()
        private var step = 0
        override fun onMessage(webSocket: WebSocket, text: String) {
            when (step++) {
                0 -> webSocket.send(vector.string("server_hello_json"))
                1 -> {
                    // Consume the client finish so the session's c2s counter
                    // tracks post-handshake client frames.
                    session.open(text.toByteArray(Charsets.UTF_8))
                    webSocket.send(String(session.seal(serverFinishPlaintext)))
                    afterEstablished(webSocket, session)
                }
                else -> {
                    val plaintext = session.open(text.toByteArray(Charsets.UTF_8))
                    val parsed = try {
                        lerdr.core.protocol.LerdrJson.parseToJsonElement(
                            String(plaintext),
                        ).jsonObject
                    } catch (invalid: IllegalArgumentException) {
                        return
                    }
                    onPostHandshake(webSocket, session, parsed)
                }
            }
        }
    }

    private fun newSession(
        scope: CoroutineScope,
        keepaliveIntervalMs: Long = ReconnectPolicy.KEEPALIVE_INTERVAL_MS,
        backgroundHealthTimeoutMs: Long = ReconnectPolicy.BACKGROUND_HEALTH_TIMEOUT_MS,
    ) = RelaySession(
        url = wsUrl(),
        scope = scope,
        getAuthentication = { authentication },
        client = httpClient,
        keepaliveIntervalMs = keepaliveIntervalMs,
        backgroundHealthTimeoutMs = backgroundHealthTimeoutMs,
        connectionFactory = fixtureConnectionFactory(),
    )

    // ── tests ───────────────────────────────────────────────────────────

    @Test
    fun connectsAndDeliversInbound() = runBlocking {
        upgrade(handshakeListener(afterEstablished = { ws, session ->
            ws.send(String(session.seal("""{"type":"push_config"}""".toByteArray())))
        }))
        val session = newSession(this)
        try {
            session.start()
            val state = withTimeout(5_000) {
                session.state.filterIsInstance<RelaySession.SessionState.Connected>().first()
            }
            assertThat(state.finish.deviceId).isNotEmpty()
            val first = withTimeout(5_000) { session.incoming.first() }
            assertThat(first["type"]?.jsonPrimitive?.content).isEqualTo("push_config")
        } finally {
            session.close()
        }
    }

    @Test
    fun authRejectionIsTerminal() = runBlocking {
        upgrade(object : WebSocketListener() {
            // The real relay refuses after reading the client hello — closing
            // inside onOpen would race the hello write.
            override fun onMessage(webSocket: WebSocket, text: String) {
                webSocket.close(ReconnectPolicy.UNAUTHORIZED_CLOSE_CODE, "device_unauthorized")
            }
        })
        val session = newSession(this)
        try {
            session.start()
            val state = withTimeout(5_000) {
                session.state.filterIsInstance<RelaySession.SessionState.AuthRejected>().first()
            }
            assertThat(state.reason.code).isEqualTo("device_unauthorized")
            // A refused credential must not be replayed: exactly one dial.
            assertThat(server.requestCount).isEqualTo(1)
        } finally {
            session.close()
        }
    }

    @Test
    fun reconnectsAfterDropAndDrainsBuffer() = runBlocking {
        val delivered = CompletableDeferred<String>()
        // Connection 1: completes the handshake, then dies.
        upgrade(handshakeListener(afterEstablished = { ws, _ -> ws.close(1000, "restart") }))
        // Connection 2: completes, then receives the buffered message.
        upgrade(handshakeListener(onPostHandshake = { _, _, parsed ->
            // The markReady refresh_agents lands first — wait for the drained
            // buffered message.
            if (parsed["type"]?.jsonPrimitive?.content == "pane_read") {
                delivered.complete(parsed.toString())
            }
        }))
        val session = newSession(this)
        try {
            session.start()
            withTimeout(5_000) {
                session.state.filterIsInstance<RelaySession.SessionState.Connected>().first()
            }
            // conn1 closes → Disconnected → the first retry waits ~1 s.
            withTimeout(5_000) {
                session.state.filterIsInstance<RelaySession.SessionState.Disconnected>().first()
            }
            // Buffered while down — drained after conn2's handshake.
            assertThat(session.send(Inbound(type = "pane_read", paneId = "p1"))).isTrue()
            assertThat(session.sendBuffer.len()).isEqualTo(1)
            withTimeout(10_000) {
                session.state.filterIsInstance<RelaySession.SessionState.Connected>()
                    .first()
            }
            val plaintext = withTimeout(5_000) { delivered.await() }
            assertThat(plaintext).contains("\"type\":\"pane_read\"")
            assertThat(session.sendBuffer.isEmpty()).isTrue()
        } finally {
            session.close()
        }
    }

    @Test
    fun requestResolvesOnCommandResult() = runBlocking {
        upgrade(handshakeListener(onPostHandshake = { ws, session, parsed ->
            // Skip the post-handshake refresh_agents ping; answer device_list.
            if (parsed["type"]?.jsonPrimitive?.content != "device_list") return@handshakeListener
            val requestId = parsed["request_id"]?.jsonPrimitive?.content.orEmpty()
            ws.send(
                String(
                    session.seal(
                        """{"type":"command_result","request_id":"$requestId","action":"device_list","ok":true,"phase":"completed","data":{"current_device_id":"dev-1"}}"""
                            .toByteArray(Charsets.UTF_8),
                    ),
                ),
            )
        }))
        val session = newSession(this)
        try {
            session.start()
            withTimeout(5_000) {
                session.state.filterIsInstance<RelaySession.SessionState.Connected>().first()
            }
            val result = session.request(Inbound(type = "device_list"))
            assertThat(result.ok).isTrue()
            assertThat(result.phase).isEqualTo("completed")
            assertThat(result.data?.jsonObject?.get("current_device_id")?.jsonPrimitive?.content)
                .isEqualTo("dev-1")
        } finally {
            session.close()
        }
    }

    @Test
    fun requestTimesOutAsDispatchedUnknown() = runBlocking {
        // The server handshakes but never answers commands.
        upgrade(handshakeListener())
        val session = newSession(this)
        try {
            session.start()
            withTimeout(5_000) {
                session.state.filterIsInstance<RelaySession.SessionState.Connected>().first()
            }
            try {
                session.request(Inbound(type = "device_list"), timeoutMs = 300)
                error("request should have timed out")
            } catch (expected: CommandException) {
                assertThat(expected.dispatchedUnknown).isTrue()
            }
        } finally {
            session.close()
        }
    }

    @Test
    fun unansweredKeepaliveRedials() = runBlocking {
        // Conn 1: handshake, then never answers the keepalive ping. Conn 2:
        // handshake and stay live.
        val keepalives = java.util.concurrent.atomic.AtomicInteger(0)
        upgrade(handshakeListener(onPostHandshake = { _, _, parsed ->
            if (parsed["type"]?.jsonPrimitive?.content == "refresh_agents") {
                keepalives.incrementAndGet()
            }
        }))
        upgrade(handshakeListener())
        val session = newSession(
            this,
            keepaliveIntervalMs = 150,
            backgroundHealthTimeoutMs = 400,
        )
        try {
            session.start()
            withTimeout(5_000) {
                session.state.filterIsInstance<RelaySession.SessionState.Connected>().first()
            }
            // First refresh_agents is the markReady ping; the 150 ms keepalive
            // sends another — unanswered, it kills conn1 → backoff → conn2.
            val secondConnect = withTimeout(10_000) {
                var connects = 0
                session.state.first {
                    if (it is RelaySession.SessionState.Connected) connects++
                    connects >= 2
                }
            }
            assertThat(secondConnect).isInstanceOf(RelaySession.SessionState.Connected::class.java)
            assertThat(keepalives.get()).isAtLeast(1)
            assertThat(server.requestCount).isEqualTo(2)
        } finally {
            session.close()
        }
    }
}
