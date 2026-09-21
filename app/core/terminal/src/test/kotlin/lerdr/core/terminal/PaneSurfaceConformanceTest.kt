package lerdr.core.terminal

import com.google.common.truth.Truth.assertThat
import com.google.common.truth.Truth.assertWithMessage
import com.lerdr.core.testing.Fixtures
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import org.junit.Test

/**
 * End-to-end conformance: the 21 `pane.delta` vectors replayed through
 * [PaneSurface] as a watched pane — a base `pane_content` followed by a
 * `pane_delta` carrying the vector's `expected_segments`.
 *
 * The suite carries no fingerprints (fingerprints cover content, not
 * segments), so the test recomputes them with [FingerprintChain.fingerprint]
 * — `hex(sha256(content_utf8))[0:16]` — which also exercises the surface's
 * post-apply hash verification: a committed frame proves the applied bytes
 * hash to the claimed fingerprint.
 *
 * Vectors whose `expected_segments` contain `{}` empty literals are the
 * known client rejection (`PaneDeltaConformanceTest.strictApplyMatches
 * ClientSemantics`): the surface must surface ResyncRequired, not diverge.
 */
class PaneSurfaceConformanceTest {

    private val vectors = Fixtures.load("pane/pane.delta.json").vectors

    private fun contentMessage(
        content: String,
        fingerprint: String,
        ackRequired: Boolean = true,
    ): JsonObject = buildJsonObject {
        put("type", "pane_content")
        put("pane_id", "pane1")
        put("content", content)
        put("content_fingerprint", fingerprint)
        if (ackRequired) put("ack_required", true)
    }

    private fun deltaMessage(
        baseFingerprint: String?,
        contentFingerprint: String?,
        segments: JsonElement,
    ): JsonObject = buildJsonObject {
        put("type", "pane_delta")
        put("pane_id", "pane1")
        if (baseFingerprint != null) put("base_fingerprint", baseFingerprint)
        if (contentFingerprint != null) put("content_fingerprint", contentFingerprint)
        put("segments", segments)
    }

    @Test
    fun deltaChainReproducesExpectedContent() {
        for (vector in vectors) {
            val name = vector["name"]!!.jsonPrimitive.content
            val previous = vector["previous"]!!.jsonPrimitive.content
            val expectedApplied = vector["expected_applied"]!!.jsonPrimitive.content
            val baseFingerprint = FingerprintChain.fingerprint(previous)
            val nextFingerprint = FingerprintChain.fingerprint(expectedApplied)

            val surface = PaneSurface("pane1")
            surface.watch()
            val base = surface.applyContent(contentMessage(previous, baseFingerprint))
            assertWithMessage("base frame commits for %s", name)
                .that(base).isInstanceOf(PaneSurface.Result.Committed::class.java)
            assertWithMessage("base frame acks then watches for %s", name)
                .that(base.intents)
                .containsExactly(
                    AckGate.Intent.Applied(baseFingerprint),
                    AckGate.Intent.Watch,
                ).inOrder()

            val result = surface.applyDelta(
                deltaMessage(baseFingerprint, nextFingerprint, vector["expected_segments"]!!),
            )

            // `{}` empty literals carry neither copy_lines nor string text —
            // the strict client apply rejects them where Go's Apply emits "".
            val clientRejects = vector["expected_segments"]!!.jsonArray.any { segment ->
                segment is JsonObject &&
                    "copy_lines" !in segment &&
                    (segment["text"] as? JsonPrimitive)?.isString != true
            }
            if (clientRejects) {
                assertWithMessage("strict apply for %s", name)
                    .that(result)
                    .isInstanceOf(PaneSurface.Result.ResyncRequired::class.java)
                assertWithMessage("resync reason for %s", name)
                    .that((result as PaneSurface.Result.ResyncRequired).reason)
                    .isEqualTo(ResyncReason.APPLY_REJECTED)
                assertWithMessage("resync forces a read for %s", name)
                    .that(result.intents)
                    .containsExactly(AckGate.Intent.ReadPane(force = true))
                continue
            }

            assertWithMessage("delta commits for %s", name)
                .that(result).isInstanceOf(PaneSurface.Result.Committed::class.java)
            val committed = result as PaneSurface.Result.Committed
            assertWithMessage("applied content for %s", name)
                .that(committed.snapshot.content).isEqualTo(expectedApplied)
            assertWithMessage("chain head for %s", name)
                .that(committed.snapshot.fingerprint).isEqualTo(nextFingerprint)
            assertWithMessage("delta ack for %s", name)
                .that(committed.intents)
                .containsExactly(AckGate.Intent.Applied(nextFingerprint))
        }
    }

    @Test
    fun fingerprintIsSha256Prefix() {
        // Known vectors: hex(sha256(content))[0:16] per server.go paneFingerprint.
        assertThat(FingerprintChain.fingerprint("")).isEqualTo("e3b0c44298fc1c14")
        assertThat(FingerprintChain.fingerprint("a\nb\n")).isEqualTo("911169ddaaf146af")
        assertThat(FingerprintChain.fingerprint("hello world\n"))
            .isEqualTo("a948904f2f0f479b")
    }
}
