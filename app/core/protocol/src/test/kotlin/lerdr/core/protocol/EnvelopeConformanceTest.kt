package lerdr.core.protocol

import com.lerdr.core.testing.Fixtures
import com.google.common.truth.Truth.assertThat
import com.google.common.truth.Truth.assertWithMessage
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import lerdr.core.model.PaneDeltaMessage
import lerdr.core.model.orNull
import lerdr.core.model.UnknownServerMessage
import org.junit.Test

/**
 * Conformance over `fixtures/protocol/protocol.envelope.json`.
 *
 * - c2s vectors: decode `json`, re-serialize, compare field-for-field
 *   against `decoded_json` (the Go flat-struct canonical view — unknown
 *   wire fields dropped, command envelopes normalized).
 * - s2c vectors: decode `json`, re-serialize, compare field-for-field
 *   against `json` itself (Go emits these via maps/structs; canonical
 *   comparison is order-insensitive JsonObject equality, which is what
 *   "field-for-field" means on this wire).
 */
class EnvelopeConformanceTest {

    private val vectors = Fixtures.load("protocol/protocol.envelope.json")
        .vectors

    @Test
    fun inboundVectorsDecodeToCanonicalView() {
        var count = 0
        for (vector in vectors) {
            val v = vector
            if (v["direction"]?.jsonPrimitive?.content != "c2s") continue
            val name = v["name"]!!.jsonPrimitive.content
            val decoded = InboundCodec.decode(v["json"]!!.jsonPrimitive.content)
            val encoded = Fixtures.parse(InboundCodec.encode(decoded)).jsonObject
            val expected = Fixtures.parse(v["decoded_json"]!!.jsonPrimitive.content).jsonObject
            assertWithMessage("inbound vector %s", name).that(encoded).isEqualTo(expected)
            count++
        }
        assertThat(count).isEqualTo(72)
    }

    @Test
    fun outboundVectorsRoundTripFieldForField() {
        var count = 0
        for (vector in vectors) {
            val v = vector
            if (v["direction"]?.jsonPrimitive?.content != "s2c") continue
            val name = v["name"]!!.jsonPrimitive.content
            val expected = Fixtures.parse(v["json"]!!.jsonPrimitive.content).jsonObject
            val message = ServerMessageCodec.decode(expected)
            val encoded = Fixtures.parse(ServerMessageCodec.encode(message)).jsonObject
            assertWithMessage("outbound vector %s", name).that(encoded).isEqualTo(expected)
            count++
        }
        assertThat(count).isEqualTo(51)
    }

    @Test
    fun commandEnvelopeActionOverridesType() {
        val raw = Fixtures.parse("""{"type":"command","action":"send_text","text":"hi"}""").jsonObject
        assertThat(InboundCodec.decode(raw).type).isEqualTo("send_text")
        val noType = Fixtures.parse("""{"action":"send_text","text":"hi"}""").jsonObject
        assertThat(InboundCodec.decode(noType).type).isEqualTo("send_text")
    }

    @Test
    fun commandEnvelopeActionDoesNotOverrideConcreteType() {
        val raw = Fixtures.parse("""{"type":"send_text","action":"other","text":"hi"}""").jsonObject
        assertThat(InboundCodec.decode(raw).type).isEqualTo("send_text")
    }

    @Test
    fun emptyTypeIsRejected() {
        val result = runCatching { InboundCodec.decode(JsonObject(emptyMap())) }
        assertThat(result.isFailure).isTrue()
    }

    @Test
    fun unknownServerMessagePreservesRawPayload() {
        val raw = Fixtures.parse("""{"type":"future_thing","x":{"y":[1,2]},"z":null}""").jsonObject
        val message = ServerMessageCodec.decode(raw)
        assertThat(message).isInstanceOf(UnknownServerMessage::class.java)
        val encoded = Fixtures.parse(ServerMessageCodec.encode(message)).jsonObject
        assertThat(encoded).isEqualTo(raw)
    }

    @Test
    fun paneDeltaMessageSegmentsAreTyped() {
        val raw = Fixtures.parse(
            """{"type":"pane_delta","pane_id":"p","segments":[{"copy_lines":2},{"text":"x"}]}"""
        ).jsonObject
        val message = ServerMessageCodec.decode(raw)
        assertThat(message).isInstanceOf(PaneDeltaMessage::class.java)
        val segments = (message as PaneDeltaMessage).segments.orNull!!
        assertThat(segments[0].copyLines).isEqualTo(2)
        assertThat(segments[1].text).isEqualTo("x")
    }
}
