package lerdr.core.terminal

import com.lerdr.core.testing.Fixtures
import com.google.common.truth.Truth.assertWithMessage
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Test

/**
 * Conformance over `fixtures/ansi/ansi.spans.json`: for every vector,
 * [AnsiSpans.parse] under the vector's scheme/options must produce
 * `expected_spans` field-for-field (`expected_html` is the upstream
 * reference; the span tree is the contract we verify).
 */
class AnsiSpansConformanceTest {

    private val json = Json { encodeDefaults = false; explicitNulls = false }

    private val vectors = Fixtures.load("ansi/ansi.spans.json")
        .vectors

    @Test
    fun everyVectorProducesExpectedSpans() {
        for (vector in vectors) {
            val v = vector
            val name = v["name"]!!.jsonPrimitive.content
            val scheme = when (v["scheme"]?.jsonPrimitive?.content) {
                "light" -> TerminalScheme.LIGHT
                else -> TerminalScheme.DARK
            }
            val options = v["options"]?.jsonObject
            fun flag(key: String) =
                options?.get(key)?.jsonPrimitive?.boolean ?: false
            val spans = AnsiSpans.parse(
                v["input"]!!.jsonPrimitive.content,
                scheme = scheme,
                normalizeNearWhiteBackground = flag("normalizeNearWhiteBackground"),
                normalizeNearBlackForeground = flag("normalizeNearBlackForeground"),
                preserveTerminalCells = flag("preserveTerminalCells"),
            )
            val encoded = JsonArray(spans.map { json.encodeToJsonElement(it) })
            assertWithMessage("ansi vector %s", name).that(encoded)
                .isEqualTo(v["expected_spans"]!!.jsonArray)
        }
    }
}
