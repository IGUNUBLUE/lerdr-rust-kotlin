package lerdr.core.protocol

import com.github.luben.zstd.Zstd
import com.google.common.truth.Truth.assertThat
import java.util.Base64
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import lerdr.core.model.ClientCapabilities
import lerdr.core.model.ConversationUpdateMessage
import lerdr.core.model.PaneContentMessage
import lerdr.core.model.UploadBeginResultMessage
import org.junit.Assert.assertThrows
import org.junit.Test

/**
 * Phase-5 Track-B wire conformance (`docs/13-phase5-wire-spec.md` §2) —
 * `convo_sub` frames, `frame_zstd` payload restoration, and the
 * `upload_binary` `0x03` carrier, matching the Rust relay's shapes.
 */
class Phase5TrackBWireTest {

    // ── §0 announced set ──────────────────────────────────────────────

    @Test
    fun announcedSetIncludesTrackBCapabilities() {
        assertThat(ClientCapabilities.ANNOUNCED).containsExactly(
            "focus", "pane_search", "pane_links", "layout",
            "convo_sub", "frame_zstd", "upload_binary",
        ).inOrder()
    }

    // ── §2.3 conversation_update ──────────────────────────────────────

    @Test
    fun conversationUpdateDecodesSpecShape() {
        // The exact outbound shape `ConversationUpdateMessage` serializes
        // relay-side (outbound.rs `conversation_update_round_trips`).
        val raw = LerdrJson.parseToJsonElement(
            """{"generation":2,"messages":[{"id":"e1","role":"user","text":"hi"}],
            "reset":true,"target":{"server_session_id":"primary","pane_id":"wE:p1",
            "terminal_id":"term-1","generation":2,"agent_session_id":"sess-1"},
            "type":"conversation_update"}""",
        ).jsonObject
        val message = ServerMessageCodec.decode(raw) as ConversationUpdateMessage
        assertThat(message.generation).isEqualTo(2L)
        assertThat(message.reset).isTrue()
        assertThat(message.messages).isNotNull()
        assertThat(message.target?.paneId).isEqualTo("wE:p1")
        assertThat(message.target?.terminalId).isEqualTo("term-1")
    }

    @Test
    fun conversationUpdateToleratesMissingAndNullMembers() {
        val bare = ServerMessageCodec.decode(
            buildJsonObject { put("type", "conversation_update") },
        ) as ConversationUpdateMessage
        assertThat(bare.reset).isNull()
        assertThat(bare.generation).isNull()
        assertThat(bare.messages).isNull()
        assertThat(bare.target).isNull()
    }

    // ── §2.2 frame_zstd ───────────────────────────────────────────────

    @Test
    fun paneContentDecodesCompressedEnvelope() {
        // `Outbound::decode` stays wire-faithful — the compressed members
        // surface as fields, never dropped or auto-inflated.
        val raw = buildJsonObject {
            put("type", "pane_content")
            put("pane_id", "wE:p1")
            put("encoding", "zstd")
            put("payload", "AAAA")
        }
        val message = ServerMessageCodec.decode(raw) as PaneContentMessage
        assertThat(message.encoding).isEqualTo("zstd")
        assertThat(message.payload).isEqualTo("AAAA")
        assertThat(message.content).isNull()
    }

    @Test
    fun decompressRestoresContentAndClearsNegotiatedMembers() {
        // The relay compresses `{"content":"…"}` at level 1 — the client
        // must restore exactly that member set, byte-identical.
        val content = "line one\nline two\u0000ansi"
        val payload = Base64.getEncoder().encodeToString(
            Zstd.compress("""{"content":${LerdrJson.encodeToString(JsonPrimitive(content))}}""".toByteArray()),
        )
        val raw = buildJsonObject {
            put("type", "pane_content")
            put("pane_id", "wE:p1")
            put("content_fingerprint", "fp")
            put("encoding", "zstd")
            put("payload", payload)
        }
        val restored = FrameZstd.decompressPaneContentPayload(raw)
        assertThat(restored["content"]?.jsonPrimitive?.content).isEqualTo(content)
        assertThat(restored).doesNotContainKey("encoding")
        assertThat(restored).doesNotContainKey("payload")
        assertThat(restored["pane_id"]).isEqualTo(JsonPrimitive("wE:p1"))
        assertThat(restored["content_fingerprint"]).isEqualTo(JsonPrimitive("fp"))
    }

    @Test
    fun decompressPassesPlaintextFramesThrough() {
        val raw = buildJsonObject {
            put("type", "pane_content")
            put("content", "plain")
        }
        assertThat(FrameZstd.decompressPaneContentPayload(raw)).isSameInstanceAs(raw)
    }

    @Test
    fun decompressRejectsMalformedPayloads() {
        // Unknown encoding names itself.
        val alien = buildJsonObject {
            put("type", "pane_content")
            put("encoding", "lz4")
        }
        assertThrows(IllegalArgumentException::class.java) {
            FrameZstd.decompressPaneContentPayload(alien)
        }
        // zstd declared without payload.
        val bare = buildJsonObject {
            put("type", "pane_content")
            put("encoding", "zstd")
        }
        assertThrows(IllegalArgumentException::class.java) {
            FrameZstd.decompressPaneContentPayload(bare)
        }
        // Not base64.
        val badBase64 = buildJsonObject {
            put("type", "pane_content")
            put("encoding", "zstd")
            put("payload", "!!!not-base64!!!")
        }
        assertThrows(IllegalArgumentException::class.java) {
            FrameZstd.decompressPaneContentPayload(badBase64)
        }
        // Valid base64, not a zstd frame.
        val notZstd = buildJsonObject {
            put("type", "pane_content")
            put("encoding", "zstd")
            put("payload", Base64.getEncoder().encodeToString("hello".toByteArray()))
        }
        assertThrows(IllegalArgumentException::class.java) {
            FrameZstd.decompressPaneContentPayload(notZstd)
        }
        // Inflates to a non-object payload (relay rejects it too).
        val wrongShape = buildJsonObject {
            put("type", "pane_content")
            put("encoding", "zstd")
            put("payload", Base64.getEncoder().encodeToString(Zstd.compress("\"just a string\"".toByteArray())))
        }
        assertThrows(IllegalArgumentException::class.java) {
            FrameZstd.decompressPaneContentPayload(wrongShape)
        }
    }

    // ── §2.4 upload_binary ────────────────────────────────────────────

    @Test
    fun encodeChunkBuildsExactHeader() {
        val id = "abcdefghijklmnopqrstuvwxyz123456" // 32 base64url chars
        val data = byteArrayOf(0x00, 0x01, 0x02, 0x7F)
        val frame = BinaryUploadChunk.encodeChunk(id, 41, data)!!
        assertThat(frame.size).isEqualTo(BinaryUploadChunk.HEADER_BYTES + data.size)
        assertThat(frame[0]).isEqualTo(0x03)
        assertThat(String(frame, 1, 32, Charsets.US_ASCII)).isEqualTo(id)
        // BE64 sequence at offset 33.
        val seq = java.nio.ByteBuffer.wrap(frame, 33, 8)
            .order(java.nio.ByteOrder.BIG_ENDIAN).long
        assertThat(seq).isEqualTo(41L)
        assertThat(frame.copyOfRange(41, frame.size)).isEqualTo(data)
    }

    @Test
    fun encodeChunkRejectsForeignIdShapes() {
        assertThat(BinaryUploadChunk.encodeChunk("", 0, byteArrayOf())).isNull()
        assertThat(BinaryUploadChunk.encodeChunk("short", 0, byteArrayOf())).isNull()
        assertThat(BinaryUploadChunk.encodeChunk("x".repeat(33), 0, byteArrayOf())).isNull()
        assertThat(
            BinaryUploadChunk.encodeChunk("+=+/+=+/+=+/+=+/+=+/+=+/+=+/+=+=", 0, byteArrayOf()),
        ).isNull()
    }

    @Test
    fun isUploadIdMatchesBase64urlAlphabet() {
        assertThat(BinaryUploadChunk.isUploadId("abcdefghijklmnopqrstuvwxyz123456")).isTrue()
        assertThat(BinaryUploadChunk.isUploadId("-_".repeat(16))).isTrue()
        assertThat(BinaryUploadChunk.isUploadId("")).isFalse()
        assertThat(BinaryUploadChunk.isUploadId("abc")).isFalse()
        assertThat(BinaryUploadChunk.isUploadId("a".repeat(33))).isFalse()
        assertThat(BinaryUploadChunk.isUploadId("!a".repeat(16))).isFalse()
    }

    @Test
    fun uploadBeginResultDecodesChunkEncoding() {
        val raw = LerdrJson.parseToJsonElement(
            """{"type":"upload_begin_result","request_id":"r1",
            "result":{"upload_id":"u","chunk_bytes":262144,"expires_at":"t",
            "limits":{},"chunk_encoding":"binary"}}""",
        ).jsonObject
        val message = ServerMessageCodec.decode(raw) as UploadBeginResultMessage
        assertThat(message.result?.chunkEncoding).isEqualTo("binary")
        // Absent member → null (unnegotiated relays).
        val plain = LerdrJson.decodeFromJsonElement(
            UploadBeginResultMessage.serializer(),
            LerdrJson.parseToJsonElement(
                """{"type":"upload_begin_result","result":{"upload_id":"u"}}""",
            ).jsonObject,
        )
        assertThat(plain.result?.chunkEncoding).isNull()
    }
}
