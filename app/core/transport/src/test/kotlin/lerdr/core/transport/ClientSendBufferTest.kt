package lerdr.core.transport

import com.google.common.truth.Truth.assertThat
import com.lerdr.core.testing.Fixtures
import com.lerdr.core.testing.array
import com.lerdr.core.testing.int
import com.lerdr.core.testing.long
import com.lerdr.core.testing.obj
import com.lerdr.core.testing.string
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import lerdr.core.transport.ClientSendBuffer.Companion.isReplaceable
import lerdr.core.transport.ClientSendBuffer.PushResult
import lerdr.core.transport.ClientSendBuffer.RejectReason
import org.junit.Test
import org.junit.runner.RunWith
import org.junit.runners.Parameterized

/**
 * Replays `pane.sendbuffer` — the Go `sendBuffer` op scripts exported from
 * `internal/transport/sendbuffer_export_test.go`. Each vector runs pushes,
 * drains, and pops and asserts the op results, eviction reasons, pending
 * types, and byte accounting byte-for-byte.
 */
@RunWith(Parameterized::class)
class SendBufferVectorsTest(
    private val vectorName: String,
    private val vector: JsonObject,
) {
    companion object {
        @JvmStatic
        @Parameterized.Parameters(name = "{0}")
        fun vectors(): Collection<Array<Any>> =
            Fixtures.named("pane.sendbuffer").vectors.map { vector ->
                arrayOf(vector.string("name"), vector)
            }

        /** A synthetic message of exactly `size` bytes carrying `kind`. */
        private fun syntheticMessage(kind: String, size: Int): ByteArray {
            val overhead = "{\"type\":\"$kind\",\"p\":\"\"}".length
            val pad = (size - overhead).coerceAtLeast(0)
            return "{\"type\":\"$kind\",\"p\":\"${"x".repeat(pad)}\"}".toByteArray(Charsets.UTF_8)
        }

        private fun sniffKind(data: ByteArray): String =
            ClientSendBuffer.sniffMessageType(data) ?: error("untyped message")

        private fun rejectWireName(reason: RejectReason): String = reason.wireName
    }

    @Test
    fun replaysOpScript() {
        val buffer = ClientSendBuffer(
            maxItems = vector.int("capacity_items"),
            maxBytes = vector.long("capacity_bytes"),
        )
        val results = mutableListOf<JsonElement>()
        val evicted = mutableListOf<Triple<Int, String, String>>()

        for ((index, op) in vector.array("ops").map(JsonElement::jsonObject).withIndex()) {
            when (op.string("op")) {
                "push" -> {
                    val kind = op.string("msg_type")
                    val data = syntheticMessage(kind, op.int("size"))
                    assertThat(data.size).isEqualTo(op.int("size"))
                    when (val result = buffer.pushTyped(data, kind, isReplaceable(kind))) {
                        PushResult.Queued -> results.add(jsonPrimitive("queued"))
                        PushResult.Coalesced -> results.add(jsonPrimitive("coalesced"))
                        is PushResult.Rejected -> {
                            evicted.add(Triple(index, kind, rejectWireName(result.reason)))
                            results.add(jsonPrimitive("rejected"))
                        }
                    }
                }
                "drain" -> {
                    val drained = mutableListOf<String>()
                    for (i in 0 until op.int("count")) {
                        val data = buffer.tryPop() ?: break
                        drained.add(sniffKind(data))
                    }
                    results.add(jsonArrayOf(drained))
                }
                "pop" -> results.add(
                    buffer.tryPop()?.let { jsonPrimitive(sniffKind(it)) } ?: kotlinx.serialization.json.JsonNull,
                )
                else -> error("unknown op in $vectorName")
            }
        }

        assertThat(results).isEqualTo(vector.array("op_results").toList())
        val expectedEvicted = vector.obj("expected").array("evicted").map(JsonElement::jsonObject)
            .map { Triple(it.int("op_index"), it.string("msg_type"), it.string("reason")) }
        assertThat(evicted).isEqualTo(expectedEvicted)
        assertThat(buffer.pendingTypes()).isEqualTo(
            vector.obj("expected").array("pending_types").map { it.jsonPrimitive.content },
        )
        assertThat(buffer.bytes()).isEqualTo(vector.obj("expected").long("bytes"))
    }

    private fun jsonPrimitive(value: String): JsonElement =
        kotlinx.serialization.json.JsonPrimitive(value)

    private fun jsonArrayOf(values: List<String>): JsonElement =
        kotlinx.serialization.json.JsonArray(values.map { kotlinx.serialization.json.JsonPrimitive(it) })
}

/** Unit tests for the parts the fixture scripts can't express. */
class ClientSendBufferTest {

    @Test
    fun untypedPushNeverCoalesces() {
        val buffer = ClientSendBuffer()
        buffer.push("""{"type":"pane_content","p":"a"}""".toByteArray())
        buffer.push("""{"type":"pane_content","p":"b"}""".toByteArray())
        // Go's Push passes replaceable=false — typed pushes coalesce, untyped don't.
        assertThat(buffer.len()).isEqualTo(2)
    }

    @Test
    fun closeRejectsPushesButDrains() {
        val buffer = ClientSendBuffer()
        buffer.pushTyped("first".toByteArray(), "a", replaceable = false)
        buffer.pushTyped("second".toByteArray(), "b", replaceable = false)
        buffer.close()
        assertThat(buffer.pushTyped("third".toByteArray(), "c", false))
            .isEqualTo(PushResult.Rejected(RejectReason.Closed))
        assertThat(buffer.tryPop()).isEqualTo("first".toByteArray())
        assertThat(buffer.tryPop()).isEqualTo("second".toByteArray())
        assertThat(buffer.tryPop()).isNull()
    }

    @Test
    fun popSuspendsUntilPushOrClose() = runTest {
        val buffer = ClientSendBuffer()
        var received: ByteArray? = null
        val waiter = launch { received = buffer.pop() }
        assertThat(waiter.isCompleted).isFalse()
        buffer.pushTyped("hello".toByteArray(), "x", false)
        kotlinx.coroutines.yield()
        assertThat(received).isEqualTo("hello".toByteArray())
        waiter.join()

        val closedWaiter = launch { received = buffer.pop() }
        buffer.close()
        kotlinx.coroutines.yield()
        assertThat(closedWaiter.isCompleted).isTrue()
        assertThat(received).isNull()
    }

    @Test
    fun peekDoesNotConsume() {
        val buffer = ClientSendBuffer()
        buffer.pushTyped("only".toByteArray(), "k", false)
        assertThat(buffer.peek()).isEqualTo("only".toByteArray())
        assertThat(buffer.peekType()).isEqualTo("k")
        assertThat(buffer.len()).isEqualTo(1)
        assertThat(buffer.tryPop()).isEqualTo("only".toByteArray())
    }

    @Test
    fun coalesceReplacesTailBytes() {
        val buffer = ClientSendBuffer(maxItems = 64, maxBytes = 1000)
        buffer.pushTyped(ByteArray(300), "agents", true)
        buffer.pushTyped(ByteArray(400), "agents", true)
        assertThat(buffer.len()).isEqualTo(1)
        assertThat(buffer.bytes()).isEqualTo(400)
    }

    @Test
    fun productionDefaults() {
        assertThat(ClientSendBuffer.DEFAULT_MAX_ITEMS).isEqualTo(64)
        assertThat(ClientSendBuffer.MAX_OUTBOUND_MESSAGE_BYTES).isEqualTo(4 * 1024 * 1024)
        assertThat(ClientSendBuffer.DEFAULT_MAX_BYTES)
            .isEqualTo(ClientSendBuffer.MAX_OUTBOUND_MESSAGE_BYTES.toLong())
    }

    @Test
    fun replaceableSetMatchesOracle() {
        assertThat(ClientSendBuffer.REPLACEABLE_TYPES).containsExactly(
            "agents", "inventory_status", "update_status", "app_deploy_status",
            "herdr_status", "pane_content", "pane_unchanged", "pane_resync",
        )
        assertThat(isReplaceable("pane_delta")).isFalse()
        assertThat(isReplaceable("command_result")).isFalse()
        assertThat(isReplaceable("agent_update")).isFalse()
    }

    @Test
    fun sniffsTypeFromSerializedEnvelope() {
        assertThat(ClientSendBuffer.sniffMessageType("""{"type":"pane_delta","x":1}""".toByteArray()))
            .isEqualTo("pane_delta")
        assertThat(ClientSendBuffer.sniffMessageType("not json".toByteArray())).isNull()
        assertThat(ClientSendBuffer.sniffMessageType("""{"type":42}""".toByteArray())).isNull()
        assertThat(ClientSendBuffer.sniffMessageType("""["a"]""".toByteArray())).isNull()
        assertThat(ClientSendBuffer.sniffMessageType("""{"other":1}""".toByteArray())).isNull()
    }
}
