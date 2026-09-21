package com.lerdr.core.e2ee

import com.google.common.truth.Truth.assertThat
import com.lerdr.core.testing.Fixtures
import com.lerdr.core.testing.b64
import com.lerdr.core.testing.long
import com.lerdr.core.testing.string
import kotlinx.serialization.json.JsonObject
import org.junit.Assert.assertThrows
import org.junit.Test
import org.junit.runner.RunWith
import org.junit.runners.Parameterized

/**
 * Replays `crypto.frames.{json,binary}`: nonce, AAD, sealed envelope bytes and
 * round-trip open must match for every vector, both codecs, both directions.
 */
@RunWith(Parameterized::class)
class FrameVectorsTest(
    private val vectorName: String,
    private val codec: E2EECodec,
    private val vector: JsonObject,
) {
    companion object {
        @JvmStatic
        @Parameterized.Parameters(name = "{0}")
        fun vectors(): Collection<Array<Any>> = listOf(
            "crypto.frames.json" to E2EECodec.JSON,
            "crypto.frames.binary" to E2EECodec.BINARY,
        ).flatMap { (suiteName, codec) ->
            Fixtures.named(suiteName).vectors.map { vector ->
                arrayOf("$suiteName#${vector.string("name")}", codec, vector)
            }
        }
    }

    private val direction: E2EEDirection
        get() = when (val wire = vector.string("direction")) {
            "c2s" -> E2EEDirection.C2S
            "s2c" -> E2EEDirection.S2C
            else -> error("unknown direction '$wire' in $vectorName")
        }

    private val c2sKey: ByteArray get() = vector.b64("session_key_c2s_b64")
    private val s2cKey: ByteArray get() = vector.b64("session_key_s2c_b64")

    /** Session whose send side uses [E2EEDirection] of the vector. */
    private fun sendingSession(): E2EESession =
        if (direction == E2EEDirection.C2S) {
            E2EESession.client(c2sKey, s2cKey, codec)
        } else {
            E2EESession.server(c2sKey, s2cKey, codec)
        }

    /** Session whose receive side uses [E2EEDirection] of the vector. */
    private fun receivingSession(): E2EESession =
        if (direction == E2EEDirection.C2S) {
            E2EESession.server(c2sKey, s2cKey, codec)
        } else {
            E2EESession.client(c2sKey, s2cKey, codec)
        }

    @Test
    fun nonceAndAadMatchFixture() {
        val sequence = vector.long("seq")
        assertThat(frameNonce(sequence)).isEqualTo(vector.b64("nonce_b64"))
        assertThat(frameAad(direction, sequence)).isEqualTo(vector.b64("aad_b64"))
    }

    @Test
    fun sealMatchesFixture() {
        val sequence = vector.long("seq")
        val session = sendingSession()
        session.sendSequence = sequence
        assertThat(session.seal(vector.b64("plaintext_b64")))
            .isEqualTo(vector.b64("sealed_frame_b64"))
        if (sequence == MAX_SEQUENCE) {
            // Vectors at 2^53-1 pin the ceiling: the next seal must fail.
            assertThrows(E2EEException.Sequence::class.java) { session.seal(byteArrayOf()) }
        }
    }

    @Test
    fun openMatchesFixture() {
        val session = receivingSession()
        session.receiveSequence = vector.long("seq")
        assertThat(session.open(vector.b64("sealed_frame_b64")))
            .isEqualTo(vector.b64("plaintext_b64"))
    }
}
