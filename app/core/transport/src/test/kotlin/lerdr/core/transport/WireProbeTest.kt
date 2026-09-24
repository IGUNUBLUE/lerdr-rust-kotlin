package lerdr.core.transport

import java.util.concurrent.CopyOnWriteArrayList
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import lerdr.core.model.Inbound
import lerdr.core.model.TargetRef
import lerdr.core.protocol.ServerMessageCodec
import okhttp3.OkHttpClient
import org.junit.Assume.assumeTrue
import org.junit.Test

/**
 * Live wire probe against the running Rust relay (`lerdr-relay serve`,
 * `ws://127.0.0.1:8377`). Gated on `LERDR_PROBE=1` — absent, the test
 * skips. Unlike InteropTest this does not assert a flow; it pairs a
 * throwaway device and prints every inbound frame type (with the agents
 * row count) so relay↔app gaps are visible on the wire, not guessed.
 *
 * Requires the relay's bootstrap invitation to be armed (`kill -USR1`)
 * and `LERDR_PROBE_TOKEN` to carry the relay's 32-byte bootstrap secret.
 */
class WireProbeTest {

    @Test(timeout = 120_000)
    fun dumpsInboundFramesOverLiveRelay() = runBlocking {
        assumeTrue("LERDR_PROBE=1 not set — live probe skipped", isEnabled())
        val token = System.getenv("LERDR_PROBE_TOKEN")?.toByteArray(Charsets.US_ASCII)
        assumeTrue("LERDR_PROBE_TOKEN missing/not 32 bytes", token?.size == 32)

        val invitation = DeviceAuthentication.invitation(
            id = "bootstrap",
            secret = token!!,
            version = 1,
            locale = "en",
        )
        val enrolled = CompletableDeferred<DeviceAuthentication>()
        val session = RelaySession(
            url = RELAY_WS_URL,
            scope = this,
            getAuthentication = { invitation },
            onEnrolled = { presented, finish ->
                presented.issuedCredential(finish)?.let { enrolled.complete(it) }
            },
            client = OkHttpClient(),
        )
        val frames = CopyOnWriteArrayList<String>()
        val agentsFrames = CopyOnWriteArrayList<JsonObject>()
        val uploadFrames = CopyOnWriteArrayList<JsonObject>()
        val collector = launch {
            session.incoming.collect { frame ->
                val type = frame["type"]?.jsonPrimitive?.content ?: "?"
                if (type == "agents") agentsFrames += frame
                if (type == "upload_begin_result" || type == "upload_chunk_result") {
                    uploadFrames += frame
                }
                val decoded = try {
                    ServerMessageCodec.decode(frame).let { "ok:${it::class.simpleName}" }
                } catch (invalid: Exception) {
                    "DECODE-FAIL:${invalid::class.simpleName}:${invalid.message?.take(160)}"
                }
                val detail = when (type) {
                    "pane_content" -> {
                        // §2.2 — report negotiated compression and restore
                        // the payload through the Track-B decompressor.
                        val encoding = frame["encoding"]?.jsonPrimitive?.content
                        if (encoding == null) {
                            "plain"
                        } else {
                            val restored = try {
                                lerdr.core.protocol.FrameZstd
                                    .decompressPaneContentPayload(frame)
                            } catch (invalid: Exception) {
                                null
                            }
                            "enc=$encoding restored=${restored?.get("content")
                                ?.jsonPrimitive?.content?.length ?: -1}"
                        }
                    }
                    "conversation_update" -> "reset=" +
                        (frame["reset"]?.jsonPrimitive?.content ?: "-") +
                        " msgs=" + (frame["messages"]?.jsonArray?.size ?: -1)
                    "upload_begin_result", "upload_chunk_result" ->
                        "req=${frame["request_id"]?.jsonPrimitive?.content ?: "∅"} " +
                            (frame["result"]?.toString()?.take(160) ?: frame["error"].toString())
                    "agents" -> "rows=" +
                        (frame["agents"]?.jsonArray?.size ?: -1) + " " +
                        (frame["agents"]?.jsonArray?.joinToString(",") { row ->
                            val o = row.jsonObject
                            listOf("pane_id", "status", "attention_kind")
                                .joinToString("|") { k -> o[k]?.jsonPrimitive?.content ?: "-" }
                        } ?: "?")
                    "workspaces" -> "rows=" +
                        (frame["workspaces"]?.jsonArray?.size ?: -1)
                    "push_config" -> "keys=" +
                        frame.keys.sorted().joinToString(",")
                    "error" -> frame["error"]?.toString()?.take(200) ?: "?"
                    "caps_update" -> "caps=" +
                        (frame["capabilities"]?.jsonArray?.size ?: -1)
                    else -> frame.keys.sorted().joinToString(",")
                }
                frames += "$type{$detail} $decoded"
            }
        }
        try {
            session.start()
            val state = withTimeout(30_000) {
                session.state.first {
                    it is RelaySession.SessionState.Connected ||
                        it is RelaySession.SessionState.AuthRejected
                }
            }
            check(state is RelaySession.SessionState.Connected) {
                "probe session failed to connect: $state"
            }
            withTimeout(10_000) { enrolled.await() }
            delay(COLLECT_MS)
            probeTrackA(session, agentsFrames)
            probeTrackB(session, agentsFrames, uploadFrames)
        } finally {
            collector.cancel()
            session.close()
        }
        System.err.println("WIRE-PROBE frames (${frames.size}):")
        frames.forEach { System.err.println("  $it") }
    }

    /**
     * Phase-5 Track-A smoke — after the caps handshake, fire a read-only
     * `pane_search` against the first agents row carrying a complete
     * target tuple (admission rejects pane-directed actions without the
     * exact `target` identity) and print the correlated `command_result`.
     */
    private suspend fun probeTrackA(
        session: RelaySession,
        agentsFrames: CopyOnWriteArrayList<JsonObject>,
    ) {
        val row = agentsFrames.lastOrNull()
            ?.get("agents")?.jsonArray?.firstOrNull()?.jsonObject ?: return
        fun field(key: String) = row[key]?.jsonPrimitive?.content.orEmpty()
        val paneId = field("pane_id")
        if (paneId.isEmpty()) return
        val result = runCatching {
            withTimeout(15_000) {
                session.request(
                    Inbound(
                        type = "pane_search",
                        paneId = paneId,
                        target = TargetRef(
                            serverSessionId = field("server_session_id"),
                            paneId = paneId,
                            terminalId = field("terminal_id"),
                            generation = field("generation").toLongOrNull() ?: 0,
                            agentSessionId = field("agent_session_id"),
                        ),
                        query = "lerdr-probe-token-string",
                        direction = "forward",
                        cursor = buildJsonObject { put("row", 0); put("col", 0) },
                    ),
                )
            }
        }
        val line = result.fold(
            onSuccess = { "pane_search → ok=${it.ok} data=${it.data}" },
            onFailure = { "pane_search → ${it::class.simpleName}: ${it.message?.take(160)}" },
        )
        System.err.println("TRACK-A $line")
    }

    /**
     * Phase-5 Track-B smoke — `subscribe_conversation` round-trips an
     * `action_receipt` and spawns the per-pane `conversation_update` feed;
     * `upload_begin` reports `chunk_encoding:"binary"` while negotiated,
     * and one `0x03` chunk acks on the JSON channel with an empty
     * `request_id` (`upload_cancel` discards the staged session after).
     */
    private suspend fun probeTrackB(
        session: RelaySession,
        agentsFrames: CopyOnWriteArrayList<JsonObject>,
        uploadFrames: CopyOnWriteArrayList<JsonObject>,
    ) {
        val row = agentsFrames.lastOrNull()
            ?.get("agents")?.jsonArray?.firstOrNull()?.jsonObject ?: return
        fun field(key: String) = row[key]?.jsonPrimitive?.content.orEmpty()
        val paneId = field("pane_id")
        if (paneId.isEmpty()) return
        val target = TargetRef(
            serverSessionId = field("server_session_id"),
            paneId = paneId,
            terminalId = field("terminal_id"),
            generation = field("generation").toLongOrNull() ?: 0,
            agentSessionId = field("agent_session_id"),
        )

        // §2.2 — a watch makes pane_content flow; with `frame_zstd`
        // negotiated the relay compresses it (collector prints enc= and
        // the restored length through the decompressor).
        val targetJson = buildJsonObject {
            target.serverSessionId.let { put("server_session_id", it) }
            put("pane_id", target.paneId)
            target.terminalId.let { put("terminal_id", it) }
            put("generation", target.generation)
            target.agentSessionId.let { put("agent_session_id", it) }
        }
        session.sendRaw(
            buildJsonObject {
                put("type", "watch_pane")
                put("request_id", "probe-watch")
                put("protocol", 3)
                put("pane_id", paneId)
                put("target", targetJson)
            }.toString(),
        )

        // §2.3 — subscribe; the reset snapshot lands on `incoming`.
        val sub = runCatching {
            withTimeout(15_000) {
                session.request(
                    Inbound(type = "subscribe_conversation", paneId = paneId, target = target),
                )
            }
        }
        System.err.println(
            "TRACK-B subscribe → " + sub.fold(
                { "ok=${it.ok} phase=${it.phase}" },
                { "${it::class.simpleName}: ${it.message?.take(120)}" },
            ),
        )
        delay(2_500) // let the first conversation_update frames arrive

        // §2.4 — staged session + one binary chunk + cancel. `files`
        // rides as a raw extra; the begin result answers on its own type.
        val beginFramesBefore = uploadFrames.size
        val beginReq = "probe-upload-begin"
        session.sendRaw(
            buildJsonObject {
                put("type", "upload_begin")
                put("request_id", beginReq)
                put("protocol", 3)
                put("pane_id", paneId)
                put("target", targetJson)
                put(
                    "files",
                    kotlinx.serialization.json.buildJsonArray {
                        add(
                            buildJsonObject {
                                put("name", "probe.txt")
                                put("media_type", "text/plain")
                                put("bytes", 42)
                            },
                        )
                    },
                )
            }.toString(),
        )
        val beginFrame = awaitUploadFrame(uploadFrames, beginFramesBefore, "upload_begin_result")
        val uploadId = beginFrame?.get("result")?.jsonObject
            ?.get("upload_id")?.jsonPrimitive?.content
        System.err.println(
            "TRACK-B begin → " + (beginFrame?.toString()?.take(200) ?: "no frame"),
        )
        if (uploadId.isNullOrEmpty()) return

        // `0x03` carrier — raw bytes through the sealed channel.
        val chunkFramesBefore = uploadFrames.size
        val sent = session.sendBytes(
            lerdr.core.protocol.BinaryUploadChunk.encodeChunk(
                uploadId, 0, "probe-bytes for the staged upload carrier\n".encodeToByteArray(),
            )!!,
        )
        val chunkFrame = if (sent) {
            awaitUploadFrame(uploadFrames, chunkFramesBefore, "upload_chunk_result")
        } else {
            null
        }
        System.err.println(
            "TRACK-B chunk(sent=$sent) → " + (chunkFrame?.toString()?.take(200) ?: "no ack"),
        )

        // Discard the staged session.
        session.sendRaw(
            buildJsonObject {
                put("type", "upload_cancel")
                put("request_id", "probe-upload-cancel")
                put("protocol", 3)
                put("pane_id", paneId)
                put("target", targetJson)
                put("upload_id", uploadId)
            }.toString(),
        )
    }

    private suspend fun awaitUploadFrame(
        frames: CopyOnWriteArrayList<JsonObject>,
        from: Int,
        type: String,
    ): JsonObject? = withTimeoutOrNull(15_000) {
        while (true) {
            frames.subList(from, frames.size)
                .firstOrNull { it["type"]?.jsonPrimitive?.content == type }
                ?.let { return@withTimeoutOrNull it }
            delay(100)
        }
        @Suppress("UNREACHABLE_CODE")
        null
    }

    private fun isEnabled(): Boolean = System.getenv("LERDR_PROBE") == "1"

    companion object {
        private const val RELAY_WS_URL = "ws://127.0.0.1:8377/ws"
        private const val COLLECT_MS = 6_000L
    }
}
