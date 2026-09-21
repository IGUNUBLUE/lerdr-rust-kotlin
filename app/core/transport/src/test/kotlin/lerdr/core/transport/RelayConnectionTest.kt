package lerdr.core.transport

import com.google.common.truth.Truth.assertThat
import com.lerdr.core.e2ee.E2EECodec
import com.lerdr.core.e2ee.E2EEClientHandshake
import com.lerdr.core.e2ee.E2EEException
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
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import org.junit.After
import org.junit.Before
import org.junit.Test

/**
 * Offline wire tests for [RelayConnection] over MockWebServer. The server
 * side replays `crypto.handshake.credential` verbatim — client hello bytes,
 * server hello, and both finish frames are pinned to the golden vector, and
 * the post-handshake session uses the fixture's derived keys, so the sealed
 * frames on the wire are exactly what the Go relay would produce.
 */
class RelayConnectionTest {

    private val vector = Fixtures.named("crypto.handshake.credential")
        .vector("credential-basic")

    private val server = MockWebServer()
    private val client = okhttp3.OkHttpClient()

    private val authentication = DeviceAuthentication.credential(
        id = vector.string("auth_id"),
        version = vector.long("auth_version"),
        secret = vector.b64("secret_b64"),
        locale = vector.string("locale"),
    )

    private val serverSession = E2EESession.server(
        vector.b64("session_key_c2s_b64"),
        vector.b64("session_key_s2c_b64"),
        E2EECodec.JSON,
    )

    private val finishFrames = vector.array("finish_frames")
        .map(JsonElement::jsonObject)
        .associateBy { it.string("direction") }

    private val received = ConcurrentLinkedQueue<String>()
    private val connections = ConcurrentLinkedQueue<RelayConnection>()

    @Before
    fun setUp() {
        server.start()
    }

    @After
    fun tearDown() {
        // Kill client sockets first — a live socket keeps the server's task
        // queue busy and `server.close()` gives up waiting on it.
        connections.forEach { it.abort() }
        server.close()
        client.dispatcher.executorService.shutdown()
        client.connectionPool.evictAll()
    }

    // ── fixture plumbing ────────────────────────────────────────────────

    private fun fixtureHandshakeFactory(): (DeviceAuthentication) -> E2EEClientHandshake = { auth ->
        E2EEClientHandshake(
            selector = auth.selector,
            secret = vector.b64("secret_b64"),
            nonce = vector.b64("client_nonce_b64"),
            ephemeral = p256KeyPair(
                vector.b64("client_ephemeral_priv_b64"),
                vector.b64("client_ephemeral_pub_b64"),
            ),
        )
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
        val publicKey = keyFactory.generatePublic(ECPublicKeySpec(point, params))
        return KeyPair(publicKey, privateKey)
    }

    private fun wsUrl(): String =
        server.url("/ws").toString().replaceFirst("http:", "ws:")

    private fun enqueueUpgrade(listener: WebSocketListener, subprotocol: Boolean = true) {
        val builder = MockResponse.Builder().webSocketUpgrade(listener)
        if (subprotocol) {
            builder.setHeader("Sec-WebSocket-Protocol", "herdr-e2ee-v2")
        }
        server.enqueue(builder.build())
    }

    private fun connection(
        handshakeTimeoutMs: Long = ReconnectPolicy.HANDSHAKE_TIMEOUT_MS,
    ) = RelayConnection(
        url = wsUrl(),
        authentication = authentication,
        client = client,
        handshakeTimeoutMs = handshakeTimeoutMs,
        handshakeFactory = fixtureHandshakeFactory(),
    ).also(connections::add)

    /**
     * The relay side of the fixture handshake: verify the hello byte-for-byte,
     * send the vector's server hello, verify the sealed client finish, then
     * seal the server finish plaintext and check it equals the golden frame.
     */
    private fun fixtureHandshakeListener(
        onEstablished: (WebSocket) -> Unit = {},
    ): WebSocketListener = object : WebSocketListener() {
        private var step = 0
        override fun onMessage(webSocket: WebSocket, text: String) {
            received.add(text)
            when (step++) {
                0 -> {
                    assertThat(text).isEqualTo(vector.string("client_hello_json"))
                    webSocket.send(vector.string("server_hello_json"))
                }
                1 -> {
                    assertThat(text).isEqualTo(String(finishFrames.getValue("c2s").b64("frame_b64")))
                    // Consume the client finish so the session's c2s counter
                    // tracks later client frames.
                    val clientFinish = serverSession.open(text.toByteArray(Charsets.UTF_8))
                    assertThat(clientFinish)
                        .isEqualTo(finishFrames.getValue("c2s").b64("plaintext_b64"))
                    val finishPlaintext = finishFrames.getValue("s2c").b64("plaintext_b64")
                    val finishFrame = serverSession.seal(finishPlaintext)
                    assertThat(finishFrame).isEqualTo(finishFrames.getValue("s2c").b64("frame_b64"))
                    webSocket.send(String(finishFrame))
                    onEstablished(webSocket)
                }
            }
        }
    }

    // ── tests ───────────────────────────────────────────────────────────

    @Test
    fun completesFixtureHandshakeAndDecryptsInbound() = runBlocking {
        val pushConfig = """{"type":"push_config","enabled":true}"""
        enqueueUpgrade(fixtureHandshakeListener { ws ->
            ws.send(String(serverSession.seal(pushConfig.toByteArray())))
        })

        val connection = connection()
        val finish = connection.connect()

        assertThat(connection.state.value).isInstanceOf(RelayConnection.State.Connected::class.java)
        assertThat(finish.deviceId).isNotEmpty()
        assertThat(finish.credentialId).isNotEmpty()
        val message = withTimeout(5_000) { connection.incoming.first() }
        assertThat(message["type"]?.jsonPrimitive?.content).isEqualTo("push_config")
        assertThat(connection.lastMessageAt).isGreaterThan(0L)
    }

    @Test
    fun refusesSocketWithoutSubprotocolEcho() {
        enqueueUpgrade(fixtureHandshakeListener(), subprotocol = false)
        val connection = connection()
        try {
            runBlocking { connection.connect() }
            error("connect should have failed")
        } catch (expected: TransportException.EncryptionRequired) {
            // oracle: 'Relay did not negotiate encrypted transport'
        }
        assertThat(connection.state.value).isInstanceOf(RelayConnection.State.Closed::class.java)
    }

    @Test
    fun handshakeTimesOut() {
        enqueueUpgrade(object : WebSocketListener() {
            // The upgrade completes but the server never sends its hello.
        })
        val connection = connection(handshakeTimeoutMs = 400)
        try {
            runBlocking { connection.connect() }
            error("connect should have failed")
        } catch (expected: TransportException.HandshakeTimeout) {
            // expected — 400 ms test timeout stands in for the 10 s contract
        }
        assertThat(connection.state.value).isInstanceOf(RelayConnection.State.Closed::class.java)
    }

    @Test
    fun serverHelloProofFailureKillsHandshake() {
        enqueueUpgrade(object : WebSocketListener() {
            override fun onMessage(webSocket: WebSocket, text: String) {
                // A syntactically valid hello with a bogus proof.
                webSocket.send(
                    """{"type":"e2ee_server_hello","version":2,"nonce":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","public_key":"BHQd1b2oF9leRiZTcyDl1VF5mDAosvgsmdUAxe6GJOPEB3C0apw4X9xWc4NVSIexVI7rkSw1ulynGZX_Is1EgdM","proof":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}""",
                )
            }
        })
        val connection = connection()
        try {
            runBlocking { connection.connect() }
            error("connect should have failed")
        } catch (expected: E2EEException.Auth) {
            // expected — the server proof did not authenticate
        }
    }

    @Test
    fun close4401SurfacesAuthRejection() {
        enqueueUpgrade(object : WebSocketListener() {
            // The real relay refuses after reading the client hello — closing
            // inside onOpen would race the hello write.
            override fun onMessage(webSocket: WebSocket, text: String) {
                webSocket.close(ReconnectPolicy.UNAUTHORIZED_CLOSE_CODE, "device_unauthorized")
            }
        })
        val connection = connection()
        try {
            runBlocking { connection.connect() }
            error("connect should have failed")
        } catch (expected: TransportException.ConnectionClosed) {
            assertThat(expected.detail.isAuthRejection).isTrue()
            assertThat(expected.detail.code).isEqualTo("device_unauthorized")
            assertThat(expected.detail.fatal).isTrue()
        }
    }

    @Test
    fun sealsOutboundFrames() = runBlocking {
        val outboundDone = CompletableDeferred<String>()
        val postHandshake = object : WebSocketListener() {
            private var step = 0
            override fun onMessage(webSocket: WebSocket, text: String) {
                received.add(text)
                when (step++) {
                    0 -> webSocket.send(vector.string("server_hello_json"))
                    1 -> {
                        serverSession.open(text.toByteArray(Charsets.UTF_8))
                        val finishPlaintext = finishFrames.getValue("s2c").b64("plaintext_b64")
                        webSocket.send(String(serverSession.seal(finishPlaintext)))
                    }
                    else -> {
                        val plaintext = serverSession.open(text.toByteArray(Charsets.UTF_8))
                        outboundDone.complete(String(plaintext))
                    }
                }
            }
        }
        server.enqueue(
            MockResponse.Builder()
                .setHeader("Sec-WebSocket-Protocol", "herdr-e2ee-v2")
                .webSocketUpgrade(postHandshake)
                .build(),
        )

        val connection = connection()
        connection.connect()
        assertThat(connection.send(lerdr.core.model.Inbound(type = "refresh_agents"))).isTrue()
        val plaintext = withTimeout(5_000) { outboundDone.await() }
        assertThat(plaintext).isEqualTo("""{"type":"refresh_agents","protocol":0}""")
    }

    @Test
    fun serverCloseEndsIncomingAndMarksClosed() = runBlocking {
        enqueueUpgrade(fixtureHandshakeListener { ws ->
            ws.close(1000, "bye")
        })
        val connection = connection()
        connection.connect()
        // The incoming flow terminates when the socket closes.
        val collectJob = launch {
            connection.incoming.collect { }
        }
        withTimeout(5_000) { collectJob.join() }
        val state = connection.state.value
        assertThat(state).isInstanceOf(RelayConnection.State.Closed::class.java)
        assertThat((state as RelayConnection.State.Closed).reason.isAuthRejection).isFalse()
    }

    @Test
    fun corruptInboundFrameFailsConnection() = runBlocking {
        enqueueUpgrade(object : WebSocketListener() {
            private var step = 0
            override fun onMessage(webSocket: WebSocket, text: String) {
                when (step++) {
                    0 -> webSocket.send(vector.string("server_hello_json"))
                    1 -> {
                        serverSession.open(text.toByteArray(Charsets.UTF_8))
                        val finishPlaintext = finishFrames.getValue("s2c").b64("plaintext_b64")
                        webSocket.send(String(serverSession.seal(finishPlaintext)))
                        // A frame whose GCM tag fails verification kills
                        // the socket (Go's decodeWebSocketMessage parity).
                        webSocket.send("""{"type":"e2ee","version":2,"sequence":1,"ciphertext":"AAAA"}""")
                    }
                }
            }
        })
        val connection = connection()
        connection.connect()
        try {
            withTimeout(5_000) { connection.incoming.first() }
            error("incoming should have failed")
        } catch (expected: E2EEException) {
            // Replay/sequence violation — the frame skipped seq 1.
        }
        assertThat(connection.state.value).isInstanceOf(RelayConnection.State.Closed::class.java)
    }

    @Test
    fun sendBeforeHandshakeIsRefused() = runBlocking {
        enqueueUpgrade(fixtureHandshakeListener())
        val connection = connection()
        assertThat(connection.send(lerdr.core.model.Inbound(type = "refresh_agents"))).isFalse()
        connection.connect()
        connection.abort()
    }
}
