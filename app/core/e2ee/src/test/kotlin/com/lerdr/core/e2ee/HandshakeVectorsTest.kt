package com.lerdr.core.e2ee

import com.google.common.truth.Truth.assertThat
import com.lerdr.core.testing.Fixtures
import com.lerdr.core.testing.array
import com.lerdr.core.testing.b64
import com.lerdr.core.testing.hexBytes
import com.lerdr.core.testing.long
import com.lerdr.core.testing.string
import java.nio.charset.StandardCharsets
import java.security.KeyPair
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import org.junit.Test
import org.junit.runner.RunWith
import org.junit.runners.Parameterized

/**
 * Replays `crypto.handshake.{credential,invitation}`: binding, transcript,
 * client/server proofs, ECDH shared secret, HKDF session keys, and both
 * finish frames must match the golden vectors byte-for-byte.
 */
@RunWith(Parameterized::class)
class HandshakeVectorsTest(
    private val vectorName: String,
    private val vector: JsonObject,
) {
    companion object {
        @JvmStatic
        @Parameterized.Parameters(name = "{0}")
        fun vectors(): Collection<Array<Any>> = listOf(
            "crypto.handshake.credential",
            "crypto.handshake.invitation",
        ).flatMap { suiteName ->
            Fixtures.named(suiteName).vectors.map { vector ->
                arrayOf("$suiteName#${vector.string("name")}", vector)
            }
        }
    }

    @Test
    fun replaysHandshake() {
        val selector = E2EEAuthSelector(
            kind = E2EEAuthKind.fromWireName(vector.string("auth_kind"))
                ?: error("unknown auth_kind in $vectorName"),
            id = vector.string("auth_id"),
            version = vector.long("auth_version"),
            locale = vector.string("locale"),
        )
        val handshake = E2EEClientHandshake(
            selector = selector,
            secret = vector.b64("secret_b64"),
            nonce = vector.b64("client_nonce_b64"),
            ephemeral = KeyPair(
                P256.publicKey(vector.b64("client_ephemeral_pub_b64")),
                P256.privateKey(vector.b64("client_ephemeral_priv_b64")),
            ),
        )

        // Binding is the HMAC domain separator; the client hello embeds the proof.
        assertThat(authBinding(selector)).isEqualTo(vector.hexBytes("binding_hex"))
        assertThat(handshake.clientHello())
            .isEqualTo(vector.string("client_hello_json").toByteArray(StandardCharsets.UTF_8))

        // Server hello: proof verified inside acceptServerHello; intermediates pinned.
        val result = handshake.acceptServerHello(
            vector.string("server_hello_json").toByteArray(StandardCharsets.UTF_8),
        )
        assertThat(result.serverNonce).isEqualTo(vector.b64("server_nonce_b64"))
        assertThat(result.serverPublicKey).isEqualTo(vector.b64("server_ephemeral_pub_b64"))
        assertThat(result.serverProof).isEqualTo(vector.b64("proof_server_b64"))
        assertThat(result.transcript).isEqualTo(vector.hexBytes("transcript_hex"))
        assertThat(result.sharedSecret).isEqualTo(vector.b64("shared_secret_b64"))
        assertThat(result.keySalt).isEqualTo(vector.b64("key_salt_b64"))
        assertThat(result.session.sendKeyBytes()).isEqualTo(vector.b64("session_key_c2s_b64"))
        assertThat(result.session.receiveKeyBytes()).isEqualTo(vector.b64("session_key_s2c_b64"))

        // Finish frames: c2s client finish seals byte-exactly; s2c opens + parses.
        val session = result.session
        for (element in vector.array("finish_frames")) {
            val finish = element.jsonObject
            assertThat(finish.long("sequence")).isEqualTo(0L)
            when (finish.string("direction")) {
                "c2s" -> assertThat(handshake.clientFinish(session))
                    .isEqualTo(finish.b64("frame_b64"))
                "s2c" -> {
                    val serverFinish = handshake.acceptServerFinish(session, finish.b64("frame_b64"))
                    assertThat(serverFinish.plaintext).isEqualTo(finish.b64("plaintext_b64"))
                }
                else -> error("unknown direction in $vectorName finish frame")
            }
        }
    }
}
