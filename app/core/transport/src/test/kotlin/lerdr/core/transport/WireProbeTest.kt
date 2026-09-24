package lerdr.core.transport

import java.util.concurrent.CopyOnWriteArrayList
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
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
        val collector = launch {
            session.incoming.collect { frame ->
                val type = frame["type"]?.jsonPrimitive?.content ?: "?"
                if (type == "agents") agentsFrames += frame
                val decoded = try {
                    ServerMessageCodec.decode(frame).let { "ok:${it::class.simpleName}" }
                } catch (invalid: Exception) {
                    "DECODE-FAIL:${invalid::class.simpleName}:${invalid.message?.take(160)}"
                }
                val detail = when (type) {
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

    private fun isEnabled(): Boolean = System.getenv("LERDR_PROBE") == "1"

    companion object {
        private const val RELAY_WS_URL = "ws://127.0.0.1:8377/ws"
        private const val COLLECT_MS = 6_000L
    }
}
