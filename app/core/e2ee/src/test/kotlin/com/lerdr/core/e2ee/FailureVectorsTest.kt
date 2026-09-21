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
 * Replays `crypto.failures`: each mutated frame must fail with the pinned
 * error class (format / replay / seq / auth), and a failed open must never
 * advance the receive sequence.
 */
@RunWith(Parameterized::class)
class FailureVectorsTest(
    private val vectorName: String,
    private val vector: JsonObject,
) {
    companion object {
        @JvmStatic
        @Parameterized.Parameters(name = "{0}")
        fun vectors(): Collection<Array<Any>> =
            Fixtures.named("crypto.failures").vectors.map { vector ->
                arrayOf("crypto.failures#${vector.string("name")}", vector)
            }
    }

    @Test
    fun rejectsWithExpectedError() {
        val codec = when (val wire = vector.string("codec")) {
            "json" -> E2EECodec.JSON
            "binary" -> E2EECodec.BINARY
            else -> error("unknown codec '$wire' in $vectorName")
        }
        val direction = vector.string("direction")
        val c2sKey = vector.b64("session_key_c2s_b64")
        val s2cKey = vector.b64("session_key_s2c_b64")
        // Receiver oriented on the vector's direction: c2s frames are opened by
        // the server side, s2c frames by the client side.
        val session = when (direction) {
            "c2s" -> E2EESession.server(c2sKey, s2cKey, codec)
            "s2c" -> E2EESession.client(c2sKey, s2cKey, codec)
            else -> error("unknown direction '$direction' in $vectorName")
        }
        val expectedSequence = vector.long("receiver_next_sequence")
        session.receiveSequence = expectedSequence

        val expected = when (val kind = vector.string("expected_error")) {
            "format" -> E2EEException.Format::class.java
            "replay" -> E2EEException.Replay::class.java
            "seq" -> E2EEException.Sequence::class.java
            "auth" -> E2EEException.Auth::class.java
            else -> error("unknown expected_error '$kind' in $vectorName")
        }
        assertThrows(expected) { session.open(vector.b64("frame_b64")) }
        assertThat(session.receiveSequence).isEqualTo(expectedSequence)
    }
}
