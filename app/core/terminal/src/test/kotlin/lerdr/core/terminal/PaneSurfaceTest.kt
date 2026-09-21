package lerdr.core.terminal

import com.google.common.truth.Truth.assertThat
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonObjectBuilder
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import org.junit.Test

/**
 * [PaneSurface] protocol behavior — the pane frame handlers ported from
 * `store.ts`, driven over raw wire `JsonObject`s. Covers the resync paths
 * the 21-vector suite cannot reach (chain breaks, missing fingerprints,
 * hash corruption, metadata-only deltas) plus snapshot semantics.
 */
class PaneSurfaceTest {

    private var now = 0L
    private fun surface(verifyHash: Boolean = true) =
        PaneSurface("pane1", verifyContentHash = verifyHash) { now }

    private fun fp(content: String) = FingerprintChain.fingerprint(content)

    private fun contentMessage(
        content: String,
        fingerprint: String? = fp(content),
        ackRequired: Boolean = true,
        extra: JsonObjectBuilder.() -> Unit = {},
    ): JsonObject = buildJsonObject {
        put("type", "pane_content")
        put("pane_id", "pane1")
        put("content", content)
        if (fingerprint != null) put("content_fingerprint", fingerprint)
        if (ackRequired) put("ack_required", true)
        extra()
    }

    private fun deltaMessage(
        baseFingerprint: JsonElementOrAbsent,
        contentFingerprint: JsonElementOrAbsent,
        segments: JsonElement,
        extra: JsonObjectBuilder.() -> Unit = {},
    ): JsonObject = buildJsonObject {
        put("type", "pane_delta")
        put("pane_id", "pane1")
        when (baseFingerprint) {
            is JsonElementOrAbsent.Value -> put("base_fingerprint", baseFingerprint.element)
            JsonElementOrAbsent.Null -> put("base_fingerprint", JsonNull)
            JsonElementOrAbsent.Absent -> Unit
        }
        when (contentFingerprint) {
            is JsonElementOrAbsent.Value -> put("content_fingerprint", contentFingerprint.element)
            JsonElementOrAbsent.Null -> put("content_fingerprint", JsonNull)
            JsonElementOrAbsent.Absent -> Unit
        }
        put("segments", segments)
        extra()
    }

    /** Tri-state wire value: absent key / explicit null / real value. */
    private sealed interface JsonElementOrAbsent {
        data class Value(val element: JsonElement) : JsonElementOrAbsent
        data object Null : JsonElementOrAbsent
        data object Absent : JsonElementOrAbsent
    }

    private fun wire(value: String) = JsonElementOrAbsent.Value(JsonPrimitive(value))

    private fun segmentsOf(vararg segments: JsonElement) = JsonArray(segments.toList())

    private fun copySegment(copyLines: Int, copyStart: Int? = null) = buildJsonObject {
        put("copy_lines", copyLines)
        if (copyStart != null) put("copy_start", copyStart)
    }

    private fun textSegment(text: String) = buildJsonObject { put("text", text) }

    @Test
    fun baseMismatchResyncsAndKeepsStaleFrame() {
        val surface = surface()
        surface.watch()
        surface.applyContent(contentMessage("one\ntwo\n"))

        val result = surface.applyDelta(
            deltaMessage(wire("wrong-base"), wire("f-next"), segmentsOf(textSegment("x"))),
        )
        val resync = result as PaneSurface.Result.ResyncRequired
        assertThat(resync.reason).isEqualTo(ResyncReason.BASE_MISMATCH)
        assertThat(resync.intents)
            .containsExactly(AckGate.Intent.ReadPane(force = true))
        // The stale frame stays displayed; the chain head is untouched.
        assertThat(surface.snapshot?.content).isEqualTo("one\ntwo\n")
        assertThat(surface.fingerprint).isEqualTo(fp("one\ntwo\n"))
    }

    @Test
    fun deltaWithoutFrameResyncs() {
        val surface = surface()
        val result = surface.applyDelta(
            deltaMessage(wire(fp("a")), wire("f1"), segmentsOf(textSegment("a"))),
        )
        assertThat((result as PaneSurface.Result.ResyncRequired).reason)
            .isEqualTo(ResyncReason.NO_BASE_FRAME)
        // Not watching → no forced read goes out.
        assertThat(result.intents).isEmpty()
        assertThat(surface.snapshot).isNull()
    }

    @Test
    fun deltaWithoutContentFingerprintResyncs() {
        val surface = surface()
        surface.applyContent(contentMessage("a\nb\n"))
        val result = surface.applyDelta(
            deltaMessage(wire(fp("a\nb\n")), JsonElementOrAbsent.Absent, segmentsOf()),
        )
        assertThat((result as PaneSurface.Result.ResyncRequired).reason)
            .isEqualTo(ResyncReason.MISSING_FINGERPRINT)
    }

    @Test
    fun malformedSegmentsResync() {
        val surface = surface()
        surface.watch()
        surface.applyContent(contentMessage("a\nb\n"))
        // "copy_lines": 0 decodes fine as a Segment but the strict client
        // apply rejects it (present copy_lines must be an integer > 0).
        val result = surface.applyDelta(
            deltaMessage(wire(fp("a\nb\n")), wire("f-next"), segmentsOf(copySegment(0))),
        )
        val resync = result as PaneSurface.Result.ResyncRequired
        assertThat(resync.reason).isEqualTo(ResyncReason.APPLY_REJECTED)
        assertThat(resync.intents)
            .containsExactly(AckGate.Intent.ReadPane(force = true))
    }

    @Test
    fun hashMismatchDetectedWhenVerifying() {
        val surface = surface()
        surface.applyContent(contentMessage("a\nb\n"))
        // The delta applies cleanly but the claimed fingerprint is wrong —
        // corrupted bytes must never silently diverge.
        val result = surface.applyDelta(
            deltaMessage(
                wire(fp("a\nb\n")),
                wire("0000000000000000"),
                segmentsOf(copySegment(3)),
            ),
        )
        val resync = result as PaneSurface.Result.ResyncRequired
        assertThat(resync.reason).isEqualTo(ResyncReason.HASH_MISMATCH)
        assertThat(surface.snapshot?.content).isEqualTo("a\nb\n")
    }

    @Test
    fun oracleModeTrustsWireFingerprint() {
        // verifyContentHash=false reproduces the released client exactly:
        // the claimed fingerprint is stored without recomputing.
        val surface = surface(verifyHash = false)
        surface.applyContent(contentMessage("a\nb\n"))
        val result = surface.applyDelta(
            deltaMessage(wire(fp("a\nb\n")), wire("0".repeat(16)), segmentsOf(copySegment(3))),
        )
        assertThat(result).isInstanceOf(PaneSurface.Result.Committed::class.java)
        assertThat(surface.fingerprint).isEqualTo("0".repeat(16))
    }

    @Test
    fun metadataOnlyNullSegmentsKeepContent() {
        // docs/specs/pane-delta.md §6.1 — released relays encode
        // metadata-only deltas as segments:null.
        val surface = surface()
        surface.watch()
        surface.applyContent(contentMessage("a\nb\n"))
        val fingerprint = fp("a\nb\n")

        val result = surface.applyDelta(
            deltaMessage(wire(fingerprint), wire(fingerprint), JsonNull) {
                put("attention_kind", "question")
            },
        )
        val committed = result as PaneSurface.Result.Committed
        assertThat(committed.snapshot.content).isEqualTo("a\nb\n")
        assertThat(committed.snapshot.fingerprint).isEqualTo(fingerprint)
        // Metadata-only deltas ack like any committed delta.
        assertThat(committed.intents)
            .containsExactly(AckGate.Intent.Applied(fingerprint))
    }

    @Test
    fun metadataOnlyBoundaryCopyAccepted() {
        // The Go relay's actual metadata-only shape: copy_lines = count+1,
        // legal only under the boundary-table apply (§6 / OPEN QUESTION-1).
        val surface = surface()
        surface.applyContent(contentMessage("a\nb\n"))
        val fingerprint = fp("a\nb\n")

        val result = surface.applyDelta(
            deltaMessage(wire(fingerprint), wire(fingerprint), segmentsOf(copySegment(3))),
        )
        assertThat(result).isInstanceOf(PaneSurface.Result.Committed::class.java)
        assertThat(surface.snapshot?.content).isEqualTo("a\nb\n")
    }

    @Test
    fun unwatchedDeltaAppliesWithoutAck() {
        // store.ts:1586 — a stray delta still applies and stores the frame.
        val surface = surface()
        surface.applyContent(contentMessage("a\nb\n", ackRequired = false))
        val result = surface.applyDelta(
            deltaMessage(wire(fp("a\nb\n")), wire(fp("a\nb\nc\n")),
                segmentsOf(copySegment(3), textSegment("c\n"))),
        )
        assertThat(result).isInstanceOf(PaneSurface.Result.Committed::class.java)
        assertThat(result.intents).isEmpty()
        assertThat(surface.fingerprint).isEqualTo(fp("a\nb\nc\n"))
    }

    @Test
    fun contentWithoutFingerprintNeitherAcksNorWatches() {
        val surface = surface()
        surface.watch()
        val result = surface.applyContent(
            contentMessage("a\nb\n", fingerprint = null, ackRequired = true),
        )
        assertThat(result).isInstanceOf(PaneSurface.Result.Committed::class.java)
        // No fingerprint → no ack and no watch (startPaneWatch early-return).
        assertThat(result.intents).isEmpty()
        assertThat(surface.snapshot?.content).isEqualTo("a\nb\n")
        assertThat(surface.fingerprint).isNull()
        assertThat(surface.watchStarted).isFalse()
    }

    @Test
    fun deltaInheritsMetadataExceptResizeSettling() {
        val surface = surface()
        surface.applyContent(
            contentMessage("a\nb\nc\nd\ne\nf\n") {
                put("viewport_rows", 24)
                put("truncated", true)
                put("no_echo", true)
                put("no_echo_prompt", "Password:")
                put("resize_settling", true)
            },
        )
        val first = surface.snapshot!!
        assertThat(first.resizeSettling).isTrue()
        assertThat(first.noEcho).isTrue()
        assertThat(first.noEchoPrompt).isEqualTo("Password:")

        // A delta specifying nothing inherits the frame's metadata but
        // recomputes resize_settling (per-frame flag, never inherited).
        val next = "a\nb\nc\nd\ne\nf\ng\n"
        surface.applyDelta(
            deltaMessage(wire(fp("a\nb\nc\nd\ne\nf\n")), wire(fp(next)),
                segmentsOf(copySegment(6), textSegment("g\n"))),
        )
        val second = surface.snapshot!!
        assertThat(second.viewportRows).isEqualTo(24)
        assertThat(second.truncated).isTrue()
        assertThat(second.noEcho).isTrue()
        assertThat(second.noEchoPrompt).isEqualTo("Password:")
        assertThat(second.resizeSettling).isFalse()
    }

    @Test
    fun deltaMetadataOverride() {
        val surface = surface()
        surface.applyContent(contentMessage("a\nb\nc\nd\n") { put("viewport_rows", 24) })
        val next = "a\nb\nc\nd\ne\n"
        surface.applyDelta(
            deltaMessage(wire(fp("a\nb\nc\nd\n")), wire(fp(next)),
                segmentsOf(copySegment(4), textSegment("e\n"))) {
                put("viewport_rows", 12)
                put("format", "ansi")
                put("resize_settling", true)
            },
        )
        val snapshot = surface.snapshot!!
        assertThat(snapshot.viewportRows).isEqualTo(12)
        assertThat(snapshot.format).isEqualTo("ansi")
        assertThat(snapshot.resizeSettling).isTrue()
    }

    @Test
    fun resyncNudgeForcesReadWhenWatching() {
        val surface = surface()
        surface.watch()
        surface.applyContent(contentMessage("a\nb\n"))
        val result = surface.onResync()
        val resync = result as PaneSurface.Result.ResyncRequired
        assertThat(resync.reason).isEqualTo(ResyncReason.SERVER_NUDGE)
        assertThat(resync.intents)
            .containsExactly(AckGate.Intent.ReadPane(force = true))
    }

    @Test
    fun unchangedAdoptsFingerprintAndRewatches() {
        val surface = surface()
        surface.watch()
        // read_pane answered pane_unchanged: fingerprint lands, watch re-issues.
        val result = surface.applyUnchanged(
            buildJsonObject {
                put("type", "pane_unchanged")
                put("pane_id", "pane1")
                put("content_fingerprint", fp("a\nb\n"))
            },
        )
        assertThat(result).isInstanceOf(PaneSurface.Result.Unchanged::class.java)
        assertThat(result.intents).containsExactly(AckGate.Intent.Watch)
        assertThat(surface.fingerprint).isEqualTo(fp("a\nb\n"))
    }

    @Test
    fun reconnectCycleResyncsThroughStoredFingerprint() {
        val surface = surface()
        surface.watch()
        surface.applyContent(contentMessage("a\nb\n"))
        surface.onDisconnect()
        assertThat(surface.watchStarted).isFalse()

        // Post-reconnect: non-forced read carries the stored fingerprint;
        // pane_unchanged re-issues the watch.
        assertThat(surface.requestRead())
            .containsExactly(AckGate.Intent.ReadPane(force = false))
        surface.applyUnchanged(
            buildJsonObject {
                put("content_fingerprint", fp("a\nb\n"))
            },
        )
        assertThat(surface.watchStarted).isTrue()
    }

    @Test
    fun snapshotSplitsRowsAndTracksRevision() {
        val surface = surface()
        surface.applyContent(contentMessage("a\nb\n"))
        val first = surface.snapshot!!
        assertThat(first.lines).containsExactly("a", "b", "").inOrder()
        assertThat(first.revision).isEqualTo(1)

        // Metadata-only delta: same content + fingerprint, higher revision.
        val fingerprint = fp("a\nb\n")
        surface.applyDelta(
            deltaMessage(wire(fingerprint), wire(fingerprint), segmentsOf(copySegment(3))),
        )
        val second = surface.snapshot!!
        assertThat(second.content).isEqualTo("a\nb\n")
        assertThat(second.fingerprint).isEqualTo(fingerprint)
        assertThat(second.revision).isEqualTo(2)
    }

    @Test
    fun resizeUpdatesGridGeometry() {
        val surface = surface()
        surface.applyContent(contentMessage("a\n"))
        val snapshot = surface.resize(120, 30)!!
        assertThat(snapshot.columns).isEqualTo(120)
        assertThat(snapshot.rows).isEqualTo(30)
    }

    @Test
    fun emptyStringFingerprintOnDeltaIsStored() {
        // typeof "" === 'string' — the delta path accepts and stores it;
        // the empty fingerprint then suppresses watch_pane. Run under
        // oracle semantics: a "" claim fails content verification.
        val surface = surface(verifyHash = false)
        surface.applyContent(contentMessage("a\nb\n"))
        val result = surface.applyDelta(
            deltaMessage(wire(fp("a\nb\n")), wire(""), segmentsOf(copySegment(3))),
        )
        assertThat(result).isInstanceOf(PaneSurface.Result.Committed::class.java)
        assertThat(surface.fingerprint).isEqualTo("")
        surface.unwatch()
        assertThat(surface.watch()).isEmpty()
    }
}
