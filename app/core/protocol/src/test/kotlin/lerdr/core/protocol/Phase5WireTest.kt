package lerdr.core.protocol

import com.google.common.truth.Truth.assertThat
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonObject
import lerdr.core.model.CapsUpdateMessage
import lerdr.core.model.ClientCapabilities
import lerdr.core.model.Inbound
import lerdr.core.model.PaneLinkActivatedResult
import lerdr.core.model.PaneLinkResolvedResult
import lerdr.core.model.PaneSearchResult
import lerdr.core.model.PaneSelectionResult
import lerdr.core.model.PaneTextPoint
import lerdr.core.model.PaneTextRange
import lerdr.core.model.TargetRef
import lerdr.core.model.UnknownServerMessage
import org.junit.Test

/**
 * Phase-5 Track-A wire conformance (`docs/13-phase5-wire-spec.md`) —
 * the app-side request encodings and `command_result.data` decodings the
 * Rust relay landed in `8334689`/`ae7ca0f`/`53e4148`.
 */
class Phase5WireTest {

    private fun encode(message: Inbound): JsonObject =
        LerdrJson.parseToJsonElement(InboundCodec.encode(message)).jsonObject

    // ── §0 capability negotiation ─────────────────────────────────────

    @Test
    fun clientCapsEncodesSpecShape() {
        val frame = encode(
            Inbound(
                type = "client_caps",
                protocol = Protocol.VERSION,
                capabilities = ClientCapabilities.ANNOUNCED,
                preferredInnerCodec = ClientCapabilities.PREFERRED_INNER_CODEC,
            ),
        )
        assertThat(frame["type"]!!.jsonPrimitive.content).isEqualTo("client_caps")
        assertThat(frame["protocol"]!!.jsonPrimitive.int).isEqualTo(3)
        assertThat(frame["capabilities"]!!.jsonArray.map { it.jsonPrimitive.content })
            .containsExactly(
                "focus", "pane_search", "pane_links", "layout",
                "convo_sub", "frame_zstd", "upload_binary",
            )
            .inOrder()
        assertThat(frame["preferred_inner_codec"]!!.jsonPrimitive.content).isEqualTo("json")
    }

    @Test
    fun capsUpdateDecodesAsTypedMessage() {
        val message = ServerMessageCodec.decode(
            """{"type":"caps_update","capabilities":["focus","layout"]}""",
        )
        assertThat(message).isInstanceOf(CapsUpdateMessage::class.java)
        assertThat((message as CapsUpdateMessage).capabilities)
            .containsExactly("focus", "layout").inOrder()
    }

    @Test
    fun capsUpdateReEncodesVerbatim() {
        val raw = """{"type":"caps_update","capabilities":["focus","pane_links"]}"""
        val encoded = ServerMessageCodec.encode(ServerMessageCodec.decode(raw))
        assertThat(LerdrJson.parseToJsonElement(encoded).jsonObject)
            .isEqualTo(LerdrJson.parseToJsonElement(raw).jsonObject)
    }

    @Test
    fun oldRelayUnknownActionReplyIsHarmless() {
        // A pre-Phase-5 relay answers `client_caps` with `unknown_action` —
        // it must keep decoding to the generic envelope, not crash.
        val message = ServerMessageCodec.decode(
            """{"type":"unknown_action","action":"client_caps"}""",
        )
        assertThat(message).isInstanceOf(UnknownServerMessage::class.java)
    }

    // ── TargetRef §1.1 additions ──────────────────────────────────────

    @Test
    fun targetRefEmitsWorkspaceAndTabIdsOnlyWhenSet() {
        val bare = encode(Inbound(type = "focus_pane", target = TargetRef(paneId = "wE:p1")))
            .getValue("target").jsonObject
        assertThat(bare).doesNotContainKey("workspace_id")
        assertThat(bare).doesNotContainKey("tab_id")

        val full = encode(
            Inbound(
                type = "focus_workspace",
                target = TargetRef(workspaceId = "wE", tabId = "wE:p1:t2"),
            ),
        ).getValue("target").jsonObject
        assertThat(full["workspace_id"]!!.jsonPrimitive.content).isEqualTo("wE")
        assertThat(full["tab_id"]!!.jsonPrimitive.content).isEqualTo("wE:p1:t2")
    }

    // ── §1.1 focus family ─────────────────────────────────────────────

    @Test
    fun focusActionsEncodeTheirTargets() {
        val pane = encode(
            Inbound(type = "focus_pane", target = TargetRef(paneId = "wE:p1")),
        )
        assertThat(pane["type"]!!.jsonPrimitive.content).isEqualTo("focus_pane")
        assertThat(pane.getValue("target").jsonObject["pane_id"]!!.jsonPrimitive.content)
            .isEqualTo("wE:p1")

        val tab = encode(
            Inbound(
                type = "focus_tab",
                target = TargetRef(paneId = "wE:p1", tabId = "wE:p1:t2"),
            ),
        ).getValue("target").jsonObject
        assertThat(tab["tab_id"]!!.jsonPrimitive.content).isEqualTo("wE:p1:t2")

        val workspace = encode(
            Inbound(type = "focus_workspace", target = TargetRef(workspaceId = "wE")),
        ).getValue("target").jsonObject
        assertThat(workspace["workspace_id"]!!.jsonPrimitive.content).isEqualTo("wE")

        val agent = encode(
            Inbound(type = "focus_agent", target = TargetRef(agentSessionId = "wE:a3")),
        ).getValue("target").jsonObject
        assertThat(agent["agent_session_id"]!!.jsonPrimitive.content).isEqualTo("wE:a3")
    }

    // ── §1.2 pane_search ──────────────────────────────────────────────

    @Test
    fun paneSearchEncodesStructuredCursorAndPrevious() {
        val frame = encode(
            Inbound(
                type = "pane_search",
                target = TargetRef(paneId = "wE:p1"),
                query = "panic",
                direction = "backward",
                cursor = buildJsonObject { put("row", 3); put("col", 4) },
                previous = buildJsonObject {
                    putJsonObject("start") { put("row", 1); put("col", 0) }
                    putJsonObject("end") { put("row", 2); put("col", 5) }
                },
            ),
        )
        // The structured point rides as an OBJECT — never a quoted string.
        assertThat(frame["query"]!!.jsonPrimitive.content).isEqualTo("panic")
        assertThat(frame["direction"]!!.jsonPrimitive.content).isEqualTo("backward")
        assertThat(frame["cursor"]!!.jsonObject["row"]!!.jsonPrimitive.int).isEqualTo(3)
        assertThat(frame["cursor"]!!.jsonObject["col"]!!.jsonPrimitive.int).isEqualTo(4)
        assertThat(
            frame["previous"]!!.jsonObject.getValue("start").jsonObject["row"]!!
                .jsonPrimitive.int,
        ).isEqualTo(1)
    }

    @Test
    fun paneSearchDecodesMatchPositionMetadata() {
        val result = LerdrJson.decodeFromString(
            PaneSearchResult.serializer(),
            """{"matches":[{"start":{"row":1,"col":2},"end":{"row":1,"col":10}}],
                "content_revision":42,"total":7,"current":3,"current_global":8}""",
        )
        assertThat(result.matches).hasSize(1)
        assertThat(result.matches[0].end.col).isEqualTo(10)
        assertThat(result.contentRevision).isEqualTo(42)
        assertThat(result.total).isEqualTo(7)
        assertThat(result.current).isEqualTo(3)
        assertThat(result.currentGlobal).isEqualTo(8)
    }

    @Test
    fun paneSearchDecodesNullMatchPositions() {
        // `current`/`current_global` are null on herdr builds that do not
        // track them — additive fields must tolerate that.
        val result = LerdrJson.decodeFromString(
            PaneSearchResult.serializer(),
            """{"matches":[],"content_revision":1,"total":0,
                "current":null,"current_global":null}""",
        )
        assertThat(result.matches).isEmpty()
        assertThat(result.current).isNull()
        assertThat(result.currentGlobal).isNull()
    }

    // ── §1.3 pane_selection_read ──────────────────────────────────────

    @Test
    fun paneSelectionReadEncodesAnchorAndCursor() {
        val frame = encode(
            Inbound(
                type = "pane_selection_read",
                target = TargetRef(paneId = "wE:p1"),
                anchor = buildJsonObject { put("row", 0); put("col", 0) },
                cursor = buildJsonObject { put("row", 9); put("col", 20) },
            ),
        )
        assertThat(frame["anchor"]!!.jsonObject["row"]!!.jsonPrimitive.int).isEqualTo(0)
        assertThat(frame["cursor"]!!.jsonObject["col"]!!.jsonPrimitive.int).isEqualTo(20)
    }

    @Test
    fun paneSelectionResultDecodes() {
        val result = LerdrJson.decodeFromString(
            PaneSelectionResult.serializer(),
            """{"text":"the quick brown fox","content_revision":11}""",
        )
        assertThat(result.text).isEqualTo("the quick brown fox")
        assertThat(result.contentRevision).isEqualTo(11)
    }

    // ── §1.4 links — regions on resolve, url on activate ──────────────

    @Test
    fun paneLinkActionsEncodeViewportRowAndCol() {
        for (type in listOf("pane_link_resolve", "pane_link_activate")) {
            val frame = encode(
                Inbound(type = type, target = TargetRef(paneId = "wE:p1"), row = 5, col = 12),
            )
            assertThat(frame["row"]!!.jsonPrimitive.int).isEqualTo(5)
            assertThat(frame["col"]!!.jsonPrimitive.int).isEqualTo(12)
        }
    }

    @Test
    fun paneLinkResolveDecodesRegionsNeverUrl() {
        // herdr 0.9.1 exposes {row,start_col,end_col} cell bounds only —
        // the URL surfaces on activate (docs/13 §1.4 correction).
        val result = LerdrJson.decodeFromString(
            PaneLinkResolvedResult.serializer(),
            """{"regions":[{"row":3,"start_col":4,"end_col":17}]}""",
        )
        assertThat(result.regions).hasSize(1)
        assertThat(result.regions[0].row).isEqualTo(3)
        assertThat(result.regions[0].startCol).isEqualTo(4)
        assertThat(result.regions[0].endCol).isEqualTo(17)
    }

    @Test
    fun paneLinkActivateDecodesHandledAndUrl() {
        val handled = LerdrJson.decodeFromString(
            PaneLinkActivatedResult.serializer(),
            """{"handled":true,"url":"https://example.com"}""",
        )
        assertThat(handled.handled).isTrue()
        assertThat(handled.url).isEqualTo("https://example.com")

        // `url` is present even when no handler took the link.
        val unhandled = LerdrJson.decodeFromString(
            PaneLinkActivatedResult.serializer(),
            """{"handled":false,"url":"https://example.com/x"}""",
        )
        assertThat(unhandled.handled).isFalse()
        assertThat(unhandled.url).isEqualTo("https://example.com/x")
    }

    // ── §1.5 layout ───────────────────────────────────────────────────

    @Test
    fun layoutApplyEncodesRootAsObjectWithOptions() {
        val root = buildJsonObject {
            put("kind", "tab")
            putJsonObject("child") { put("kind", "pane"); put("pane_id", "wE:p1") }
        }
        val frame = encode(
            Inbound(
                type = "layout_apply",
                target = TargetRef(workspaceId = "wE", tabId = "wE:t9"),
                root = root,
                tabLabel = "restored",
                focus = true,
            ),
        )
        assertThat(frame["root"]!!.jsonObject["kind"]!!.jsonPrimitive.content).isEqualTo("tab")
        assertThat(frame["tab_label"]!!.jsonPrimitive.content).isEqualTo("restored")
        assertThat(frame["focus"]!!.jsonPrimitive.boolean).isTrue()
        val target = frame.getValue("target").jsonObject
        assertThat(target["workspace_id"]!!.jsonPrimitive.content).isEqualTo("wE")
        assertThat(target["tab_id"]!!.jsonPrimitive.content).isEqualTo("wE:t9")
    }

    @Test
    fun layoutApplyOmitsOptionsAtDefaults() {
        val frame = encode(
            Inbound(type = "layout_apply", root = buildJsonObject { put("kind", "tab") }),
        )
        assertThat(frame).doesNotContainKey("tab_label")
        assertThat(frame).doesNotContainKey("focus")
        assertThat(frame).doesNotContainKey("target")
    }

    // ── legacy cursor stays a string ──────────────────────────────────

    @Test
    fun legacyPaginationCursorStillEmitsString() {
        val frame = encode(
            Inbound(type = "get_conversation_history", cursor = JsonPrimitive("page-2")),
        )
        assertThat(frame["cursor"]!!.jsonPrimitive.isString).isTrue()
        assertThat(frame["cursor"]!!.jsonPrimitive.content).isEqualTo("page-2")
    }

    @Test
    fun absentCursorEmitsNothing() {
        val frame = encode(Inbound(type = "get_conversation_history"))
        assertThat(frame).doesNotContainKey("cursor")
    }

    // ── catalog classification matches the relay ──────────────────────

    @Test
    fun trackAClassificationsMatchRelayCatalog() {
        fun meta(op: String) = ActionCatalog.classify(op)

        // Reads.
        for (op in listOf(
            "pane_search", "pane_selection_read", "pane_link_resolve", "layout_export",
            "client_caps", "caps_update",
        )) {
            assertThat(meta(op)?.actionClass).isEqualTo(ActionClass.READ_ONLY)
        }
        // Coordinated mutations — layout_apply is the audited one.
        for (op in listOf(
            "focus_pane", "focus_tab", "focus_workspace", "focus_agent",
            "pane_link_activate",
        )) {
            assertThat(meta(op)?.actionClass).isEqualTo(ActionClass.MUTATING)
            assertThat(meta(op)?.coordinated).isTrue()
            assertThat(meta(op)?.audited).isFalse()
        }
        assertThat(meta("layout_apply")?.actionClass).isEqualTo(ActionClass.MUTATING)
        assertThat(meta("layout_apply")?.coordinated).isTrue()
        assertThat(meta("layout_apply")?.audited).isTrue()
    }

    // ── DTO sanity ────────────────────────────────────────────────────

    @Test
    fun paneTextPointAndRangeRoundTrip() {
        val point = LerdrJson.decodeFromString(
            PaneTextPoint.serializer(), """{"row":3,"col":4}""",
        )
        assertThat(point.row).isEqualTo(3)
        assertThat(point.col).isEqualTo(4)
        val range = LerdrJson.decodeFromString(
            PaneTextRange.serializer(),
            """{"start":{"row":0,"col":1},"end":{"row":2,"col":3}}""",
        )
        assertThat(range.end.row).isEqualTo(2)
    }
}
