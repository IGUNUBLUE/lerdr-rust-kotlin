package lerdr.core.model

import com.google.common.truth.Truth.assertThat
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.jsonObject
import org.junit.Test

/**
 * Presence-triple semantics for Go `map[string]any` fields: absent, explicit
 * `null`, and a value are three distinct wire states and must survive a
 * decode→encode round-trip untouched.
 */
class WireFieldTest {

    @Serializable
    private data class Probe(
        @Serializable(with = WireFieldSerializer::class)
        val maybe: WireField<String> = WireField.Absent,
    )

    private val json = Json { encodeDefaults = false; explicitNulls = false }

    @Test
    fun absentDecodesToAbsentAndOmits() {
        val decoded = json.decodeFromString<Probe>("""{}""")
        assertThat(decoded.maybe).isSameInstanceAs(WireField.Absent)
        assertThat(json.encodeToJsonElement(decoded).jsonObject)
            .isEqualTo(JsonObject(emptyMap()))
    }

    @Test
    fun explicitNullDecodesToExplicitNullAndEmitsNull() {
        val decoded = json.decodeFromString<Probe>("""{"maybe":null}""")
        assertThat(decoded.maybe).isSameInstanceAs(WireField.ExplicitNull)
        assertThat(json.encodeToJsonElement(decoded).jsonObject)
            .isEqualTo(JsonObject(mapOf("maybe" to JsonNull)))
    }

    @Test
    fun valueDecodesToPresentAndEmitsValue() {
        val decoded = json.decodeFromString<Probe>("""{"maybe":"x"}""")
        assertThat(decoded.maybe).isEqualTo(WireField.Present("x"))
        assertThat(json.encodeToJsonElement(decoded).jsonObject["maybe"])
            .isEqualTo(kotlinx.serialization.json.JsonPrimitive("x"))
    }

    @Test
    fun orNullCollapsesAbsentAndNull() {
        val absent: WireField<String> = WireField.Absent
        val explicitNull: WireField<String> = WireField.ExplicitNull
        assertThat(absent.orNull).isNull()
        assertThat(explicitNull.orNull).isNull()
        assertThat(WireField.Present("v").orNull).isEqualTo("v")
    }

    @Test
    fun isPresentDistinguishesAbsent() {
        assertThat(WireField.Absent.isPresent).isFalse()
        assertThat(WireField.ExplicitNull.isPresent).isTrue()
        assertThat(WireField.Present("v").isPresent).isTrue()
    }
}
