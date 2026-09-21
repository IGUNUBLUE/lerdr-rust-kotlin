package lerdr.core.terminal

import com.lerdr.core.testing.Fixtures
import com.google.common.truth.Truth.assertThat
import com.google.common.truth.Truth.assertWithMessage
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.decodeFromJsonElement
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.Json
import lerdr.core.model.Segment
import org.junit.Test

/**
 * Conformance over `fixtures/pane/pane.delta.json`:
 *
 * - [PaneDelta.build] must emit `expected_segments` field-for-field.
 * - [PaneDelta.apply] (Go `Apply` semantics) on `expected_segments` must
 *   produce `expected_applied`.
 * - [PaneDelta.efficient] must match the `efficient` flag.
 * - [PaneDelta.applyStrict] (the released client's apply) must produce
 *   `expected_applied` for every segment shape it accepts. It rejects
 *   `{}` segments — the literal-empty shape `Build` emits for empty
 *   frames — where Go's `Apply` returns `""`; that divergence is
 *   asserted, not silently accepted.
 */
class PaneDeltaConformanceTest {

    private val json = Json { encodeDefaults = false; explicitNulls = false }

    private val vectors = Fixtures.load("pane/pane.delta.json")
        .vectors

    private fun segmentsOf(v: JsonObject): List<Segment> =
        json.decodeFromJsonElement(v["expected_segments"]!!.jsonArray)

    @Test
    fun buildProducesExpectedSegments() {
        for (vector in vectors) {
            val v = vector
            val name = v["name"]!!.jsonPrimitive.content
            val built = PaneDelta.build(
                v["previous"]!!.jsonPrimitive.content,
                v["current"]!!.jsonPrimitive.content,
            )
            val encoded = JsonArray(built.map { json.encodeToJsonElement(it) })
            assertWithMessage("build for %s", name).that(encoded)
                .isEqualTo(v["expected_segments"]!!.jsonArray)
        }
    }

    @Test
    fun applyProducesExpectedContent() {
        for (vector in vectors) {
            val v = vector
            val name = v["name"]!!.jsonPrimitive.content
            val applied = PaneDelta.apply(
                v["previous"]!!.jsonPrimitive.content,
                segmentsOf(v),
            )
            assertWithMessage("apply for %s", name).that(applied)
                .isEqualTo(v["expected_applied"]!!.jsonPrimitive.content)
        }
    }

    @Test
    fun efficientMatchesFlag() {
        for (vector in vectors) {
            val v = vector
            val name = v["name"]!!.jsonPrimitive.content
            val efficient = PaneDelta.efficient(
                segmentsOf(v),
                v["current"]!!.jsonPrimitive.content,
            )
            assertWithMessage("efficient for %s", name).that(efficient)
                .isEqualTo(v["efficient"]!!.jsonPrimitive.boolean)
        }
    }

    @Test
    fun strictApplyMatchesClientSemantics() {
        for (vector in vectors) {
            val v = vector
            val name = v["name"]!!.jsonPrimitive.content
            val rawSegments = v["expected_segments"]!!
            val strict = PaneDelta.applyStrict(
                v["previous"]!!.jsonPrimitive.content,
                rawSegments,
            )
            // The released client rejects segments that carry neither
            // copy_lines nor a string text (e.g. the `{}` literal Build
            // emits for empty frames) — Go's Apply treats them as "".
            val clientRejects = rawSegments.jsonArray.any { segment ->
                segment is JsonObject &&
                    "copy_lines" !in segment &&
                    (segment["text"] as? JsonPrimitive)?.isString != true
            }
            if (clientRejects) {
                assertWithMessage("strict apply for %s", name).that(strict).isNull()
            } else {
                assertWithMessage("strict apply for %s", name).that(strict)
                    .isEqualTo(v["expected_applied"]!!.jsonPrimitive.content)
            }
        }
    }

    @Test
    fun strictApplyRejectsMalformedShapes() {
        // Non-array, non-object, and malformed copy/text members.
        assertThat(PaneDelta.applyStrict("a\nb\n", JsonPrimitive(3))).isNull()
        assertThat(PaneDelta.applyStrict("a\nb\n", JsonObject(emptyMap()))).isNull()
        assertThat(PaneDelta.applyStrict("a\nb\n", JsonArray(listOf(JsonPrimitive("x"))))).isNull()
        // copy_lines must be an integer > 0.
        assertThat(PaneDelta.applyStrict("a\nb\n", JsonArray(listOf(JsonObject(
            mapOf("copy_lines" to JsonPrimitive(0))))))).isNull()
        assertThat(PaneDelta.applyStrict("a\nb\n", JsonArray(listOf(JsonObject(
            mapOf("copy_lines" to JsonPrimitive(-1))))))).isNull()
        assertThat(PaneDelta.applyStrict("a\nb\n", JsonArray(listOf(JsonObject(
            mapOf("copy_lines" to JsonPrimitive(1.5))))))).isNull()
        assertThat(PaneDelta.applyStrict("a\nb\n", JsonArray(listOf(JsonObject(
            mapOf("copy_lines" to JsonPrimitive("1"))))))).isNull()
        // copy_start must be a non-negative integer when present.
        assertThat(PaneDelta.applyStrict("a\nb\n", JsonArray(listOf(JsonObject(
            mapOf("copy_lines" to JsonPrimitive(1), "copy_start" to JsonPrimitive(-1))))))).isNull()
        assertThat(PaneDelta.applyStrict("a\nb\n", JsonArray(listOf(JsonObject(
            mapOf("copy_lines" to JsonPrimitive(1), "copy_start" to JsonPrimitive(0.5))))))).isNull()
        // copy range must fit the newline boundary table.
        assertThat(PaneDelta.applyStrict("a\nb\n", JsonArray(listOf(JsonObject(
            mapOf("copy_lines" to JsonPrimitive(4))))))).isNull()
        // text must be a string when copy_lines is absent.
        assertThat(PaneDelta.applyStrict("a\nb\n", JsonArray(listOf(JsonObject(
            mapOf("text" to JsonPrimitive(7))))))).isNull()
        assertThat(PaneDelta.applyStrict("a\nb\n", JsonArray(listOf(JsonObject(
            emptyMap()))))).isNull()
    }

    @Test
    fun strictApplyAcceptsBoundaryTableTail() {
        // previous "a\n" has boundaries [0,2,2]: copy_lines 2 reaches the
        // trailing empty element the boundary table models.
        val result = PaneDelta.applyStrict(
            "a\n",
            JsonArray(listOf(JsonObject(mapOf("copy_lines" to JsonPrimitive(2))))),
        )
        assertThat(result).isEqualTo("a\n")
    }
}
