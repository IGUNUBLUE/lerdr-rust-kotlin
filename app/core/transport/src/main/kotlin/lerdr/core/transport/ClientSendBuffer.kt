package lerdr.core.transport

import java.util.ArrayDeque
import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.channels.ClosedReceiveChannelException
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import lerdr.core.protocol.LerdrJson

/**
 * Client outbound send buffer — a port of the relay's per-client `sendBuffer`
 * (`internal/transport/sendbuffer.go`, `lerdr-core/src/sendbuffer.rs`) for the
 * client→relay direction. Capacity is bounded in items AND serialized
 * plaintext bytes; overflow is **rejected**, never evicted — queued messages
 * survive untouched. The [REPLACEABLE_TYPES] set coalesces against an
 * identical-type tail only. Draining is FIFO; [close] keeps the queue
 * drainable, matching Go.
 */
class ClientSendBuffer(
    private val maxItems: Int = DEFAULT_MAX_ITEMS,
    private val maxBytes: Long = DEFAULT_MAX_BYTES,
) {
    /** `pushResult` — rejection reasons Go keeps internal are surfaced here. */
    sealed interface PushResult {
        /** Appended at the tail. */
        data object Queued : PushResult

        /** Replaced an identical-type replaceable tail in place. */
        data object Coalesced : PushResult

        /** Refused; see [RejectReason]. The queue is untouched. */
        data class Rejected(val reason: RejectReason) : PushResult

        /** `Push`/`PushTyped`'s bool: anything but [Rejected] admitted the data. */
        fun admitted(): Boolean = this !is Rejected
    }

    /** Why a push was refused. Ordering mirrors `pushTyped`: closed → coalesce → item cap → byte cap. */
    enum class RejectReason(val wireName: String) {
        Closed("closed"),
        ItemLimit("item_limit"),
        ByteLimit("byte_limit"),

        /** A same-type replaceable tail existed but the merged payload would exceed `maxBytes`. */
        CoalesceByteLimit("coalesce_byte_limit"),
    }

    private class BufferedMessage(
        var data: ByteArray,
        val kind: String,
        val replaceable: Boolean,
    )

    private val lock = ReentrantLock()
    private val items = ArrayDeque<BufferedMessage>()
    private var bytes = 0L
    private var closed = false

    // Conflated wake signal: a queued push wakes one suspended `pop`, like
    // Go's `ready.Signal()`. `close` closes it so every waiter unblocks.
    private val signal = Channel<Unit>(capacity = Channel.CONFLATED)

    /**
     * `Push` — the type is sniffed from the serialized envelope's `type`
     * field and the message is never treated as replaceable (Go passes
     * `false`; the hub path uses [pushTyped] with the `encodeMessage` verdict).
     */
    fun push(data: ByteArray): PushResult =
        pushTyped(data, sniffMessageType(data) ?: "", replaceable = false)

    /**
     * `pushTyped` — queue one serialized message.
     *
     * A replaceable incoming message merges with a replaceable tail **of the
     * same type**; the merged size still has to fit the byte budget. Any other
     * overflow rejects without touching the queue.
     */
    fun pushTyped(data: ByteArray, kind: String, replaceable: Boolean): PushResult = lock.withLock {
        if (closed) return PushResult.Rejected(RejectReason.Closed)
        if (replaceable && items.isNotEmpty()) {
            val tail = items.peekLast()
            if (tail != null && tail.replaceable && tail.kind == kind) {
                val nextBytes = bytes - tail.data.size + data.size
                if (nextBytes > maxBytes) {
                    return PushResult.Rejected(RejectReason.CoalesceByteLimit)
                }
                bytes = nextBytes
                tail.data = data
                return PushResult.Coalesced
            }
        }
        if (items.size >= maxItems) return PushResult.Rejected(RejectReason.ItemLimit)
        if (bytes + data.size > maxBytes) return PushResult.Rejected(RejectReason.ByteLimit)
        items.addLast(BufferedMessage(data, kind, replaceable))
        bytes += data.size
        signal.trySend(Unit)
        return PushResult.Queued
    }

    /**
     * Go's blocking `Pop`: suspends until an item is available or the buffer
     * is closed and drained. Returns `null` only when the buffer is closed
     * and empty.
     */
    suspend fun pop(): ByteArray? {
        while (true) {
            tryPop()?.let { return it }
            try {
                signal.receive()
            } catch (closedChannel: ClosedReceiveChannelException) {
                return tryPop()
            }
        }
    }

    /** Non-blocking `Pop` — the front item, FIFO. `null` when empty. */
    fun tryPop(): ByteArray? = lock.withLock {
        val item = items.pollFirst() ?: return@withLock null
        bytes -= item.data.size
        item.data
    }

    /** Front item without removing it — the writer peeks, sends, then pops. */
    fun peek(): ByteArray? = lock.withLock { items.peekFirst()?.data }

    /** Type of the front item without removing it. */
    fun peekType(): String? = lock.withLock { items.peekFirst()?.kind }

    /** `Close` — pushes reject; queued items remain drainable (Go parity). */
    fun close() {
        val changed = lock.withLock {
            if (closed) return@withLock false
            closed = true
            true
        }
        if (changed) signal.close()
    }

    val isClosed: Boolean get() = lock.withLock { closed }

    /** `Len` — queued item count. */
    fun len(): Int = lock.withLock { items.size }

    /** `Bytes` — queued serialized bytes. */
    fun bytes(): Long = lock.withLock { bytes }

    fun isEmpty(): Boolean = lock.withLock { items.isEmpty() }

    /** Queued message types, front to back — fixture `pending_types`. */
    fun pendingTypes(): List<String> = lock.withLock { items.map { it.kind } }

    companion object {
        /** `clientOutboundMaxItems`. */
        const val DEFAULT_MAX_ITEMS = 64

        /** `MaxOutboundMessageBytes` — the largest plaintext message the buffer admits. */
        const val MAX_OUTBOUND_MESSAGE_BYTES = 4 * 1024 * 1024

        /** `clientOutboundMaxBytes`. */
        const val DEFAULT_MAX_BYTES: Long = MAX_OUTBOUND_MESSAGE_BYTES.toLong()

        /**
         * Message types whose newest frame supersedes a queued same-type
         * tail — the exact list from `encodeMessage` in
         * `internal/transport/ws.go`. Snapshot/state streams collapse;
         * deltas, receipts, and per-event broadcasts never do.
         */
        val REPLACEABLE_TYPES: Set<String> = setOf(
            "agents",
            "inventory_status",
            "update_status",
            "app_deploy_status",
            "herdr_status",
            "pane_content",
            "pane_unchanged",
            "pane_resync",
        )

        /** `encodeMessage`'s replaceable verdict for [kind]. */
        fun isReplaceable(kind: String): Boolean = kind in REPLACEABLE_TYPES

        /** `messageType(data)` — sniff the envelope's `type` field without a typed decode. */
        fun sniffMessageType(data: ByteArray): String? = try {
            val element = LerdrJson.parseToJsonElement(data.decodeToString())
            val type = (element as? JsonObject)?.get("type")
            (type as? JsonPrimitive)?.takeIf { it.isString }?.content
        } catch (e: IllegalArgumentException) {
            null
        }
    }
}
