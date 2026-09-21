package lerdr.core.model

import com.lerdr.core.testing.Fixtures
import com.google.common.truth.Truth.assertThat
import com.google.common.truth.Truth.assertWithMessage
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Test

/**
 * Conformance over `fixtures/questions/questions.interaction.json`:
 * every `expected_interaction` must decode into [Interaction] and
 * re-serialize field-for-field.
 */
class QuestionsConformanceTest {

    private val json = Json {
        ignoreUnknownKeys = true
        encodeDefaults = false
        explicitNulls = false
    }

    private val vectors = Fixtures.load("questions/questions.interaction.json")
        .vectors

    @Test
    fun everyExpectedInteractionRoundTrips() {
        var count = 0
        var nulls = 0
        for (vector in vectors) {
            val v = vector
            val name = v["name"]!!.jsonPrimitive.content
            val expected = v["expected_interaction"]
            // Approval/chat/unknown panes carry no question interaction —
            // the attention kind says which.
            if (expected == null || expected is kotlinx.serialization.json.JsonNull) {
                val kind = v["expected_attention"]!!.jsonObject["kind"]!!.jsonPrimitive.content
                assertWithMessage("non-interaction vector %s", name)
                    .that(kind).isNotEqualTo("question")
                nulls++
                continue
            }
            val interaction =
                json.decodeFromJsonElement(Interaction.serializer(), expected.jsonObject)
            val encoded = json.encodeToJsonElement(Interaction.serializer(), interaction).jsonObject
            assertWithMessage("interaction vector %s", name).that(encoded)
                .isEqualTo(expected.jsonObject)
            count++
        }
        assertThat(count + nulls).isEqualTo(vectors.size)
        assertThat(count).isGreaterThan(0)
    }

    @Test
    fun kindsAreOnlySpecValues() {
        for (vector in vectors) {
            val expected = vector["expected_interaction"]
            if (expected == null || expected is kotlinx.serialization.json.JsonNull) continue
            val kind = expected.jsonObject["kind"]!!.jsonPrimitive.content
            assertWithMessage("vector %s", vector["name"]!!.jsonPrimitive.content)
                .that(kind).isIn(setOf("single_select", "multi_select"))
        }
    }
}
