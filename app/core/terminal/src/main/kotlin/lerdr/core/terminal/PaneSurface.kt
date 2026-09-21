package lerdr.core.terminal

import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.doubleOrNull

/**
 * Per-pane terminal surface — the client-side frame model. Owns the frame
 * bytes, the [FingerprintChain] integrity state, and the [AckGate] protocol
 * state; the session layer feeds raw wire `JsonObject`s in and drains
 * [AckGate.Intent]s out. [snapshot] is the render-ready immutable seam the
 * Compose terminal view consumes.
 *
 * Consume the raw decoded message object, not a typed DTO: the strict
 * client apply needs `segments` presence semantics (`absent` / `null` /
 * array) that `Segment` decoding erases, and JS field reads degrade
 * per-field (`typeof` checks) instead of failing whole-message decode.
 *
 * Frame metadata mirrors `TerminalFrame` (`types.ts`): `pane_content`
 * rebuilds it from scratch, `pane_delta` inherits unspecified fields from
 * the previous frame — except `resize_settling`, which is per-frame.
 */
class PaneSurface(
    val paneId: String,
    /**
     * Re-hash committed content against the claimed `content_fingerprint`
     * ([FingerprintChain.verify]) — hardening beyond the oracle, which
     * stores the wire value without recomputing. A conforming relay never
     * trips it; a mismatch forces resync instead of silently diverging.
     */
    private val verifyContentHash: Boolean = true,
    clockMillis: () -> Long = System::currentTimeMillis,
) {
    private val chain = FingerprintChain()
    private val gate = AckGate(chain::fingerprint, clockMillis)

    private var content: String? = null
    private var format = "plain"
    private var truncated = false
    private var viewportOnly = false
    private var viewportRows: Int? = null
    private var noEcho = false
    private var noEchoPrompt: String? = null
    private var resizeSettling = false
    private var columns = 0
    private var rows = 0
    private var revision = 0L
    private var currentSnapshot: Snapshot? = null

    /** Last committed render state; null until the first `pane_content`/delta lands. */
    val snapshot: Snapshot? get() = currentSnapshot

    /** Chain head — the fingerprint `watch_pane`/`read_pane` echo back. */
    val fingerprint: String? get() = chain.fingerprint

    val watching: Boolean get() = gate.watching
    val watchStarted: Boolean get() = gate.watchStarted
    val readPending: Boolean get() = gate.readPending

    /**
     * `pane_delta` handler (`store.ts`): chain-check, metadata-only fast
     * path, strict boundary-table apply, commit + ack — or forced resync.
     * The stale frame stays displayed on failure; nothing is stored.
     */
    fun applyDelta(message: JsonObject): Result {
        val segments = message["segments"]
        val verdict = chain.evaluateDelta(
            hasFrame = content != null,
            baseFingerprint = message["base_fingerprint"],
            contentFingerprint = message["content_fingerprint"],
            segments = segments,
        )
        val claimedFingerprint: String
        val nextContent: String
        when (verdict) {
            is FingerprintChain.DeltaVerdict.ResyncRequired ->
                return Result.ResyncRequired(verdict.reason, gate.onDeltaRejected())
            is FingerprintChain.DeltaVerdict.MetadataOnly -> {
                claimedFingerprint = verdict.contentFingerprint
                nextContent = content!!
            }
            is FingerprintChain.DeltaVerdict.Apply -> {
                claimedFingerprint = verdict.contentFingerprint
                nextContent = PaneDelta.applyStrict(content!!, segments)
                    ?: return resync(ResyncReason.APPLY_REJECTED)
                if (verifyContentHash &&
                    !FingerprintChain.verify(nextContent, claimedFingerprint)
                ) {
                    return resync(ResyncReason.HASH_MISMATCH)
                }
            }
        }
        chain.commitDelta(claimedFingerprint)
        content = nextContent
        format = wireString(message["format"])?.takeIf { it.isNotEmpty() } ?: format
        truncated = wireBoolean(message["truncated"]) ?: truncated
        viewportOnly = wireBoolean(message["viewport_only"]) ?: viewportOnly
        viewportRows = wireInt(message["viewport_rows"]) ?: viewportRows
        noEcho = wireBoolean(message["no_echo"]) ?: noEcho
        noEchoPrompt = if (noEcho) {
            wireString(message["no_echo_prompt"]) ?: noEchoPrompt
        } else {
            null
        }
        resizeSettling = wireBoolean(message["resize_settling"]) == true
        return Result.Committed(commit(), gate.onDeltaCommitted(claimedFingerprint))
    }

    /**
     * `pane_content` handler (`store.ts`): clears the in-flight read,
     * adopts a non-empty fingerprint, rebuilds the frame with no field
     * inheritance, acks `ack_required` frames, then re-issues the watch.
     */
    fun applyContent(message: JsonObject): Result {
        val frameFingerprint = wireString(message["content_fingerprint"])
        chain.adopt(frameFingerprint)
        content = wireString(message["content"]) ?: "(empty)"
        format = wireString(message["format"])?.takeIf { it.isNotEmpty() } ?: "plain"
        truncated = wireBoolean(message["truncated"]) == true
        viewportOnly = wireBoolean(message["viewport_only"]) == true
        viewportRows = wireInt(message["viewport_rows"])
        noEcho = wireBoolean(message["no_echo"]) == true
        noEchoPrompt = if (noEcho) wireString(message["no_echo_prompt"]) else null
        resizeSettling = wireBoolean(message["resize_settling"]) == true
        return Result.Committed(
            commit(),
            gate.onContent(
                frameFingerprint,
                ackRequired = wireBoolean(message["ack_required"]) == true,
            ),
        )
    }

    /**
     * `pane_unchanged` handler (`store.ts`): adopt the echoed fingerprint,
     * keep the stored frame, re-issue the watch.
     */
    fun applyUnchanged(message: JsonObject): Result {
        chain.adopt(wireString(message["content_fingerprint"]))
        return Result.Unchanged(gate.onUnchanged())
    }

    /** `pane_resync` nudge — the server's ack gate saw a stale/foreign ack. */
    fun onResync(): Result =
        Result.ResyncRequired(ResyncReason.SERVER_NUDGE, gate.onResync())

    /** `watchPane` — begin streaming; emits [AckGate.Intent.Watch] when possible. */
    fun watch(): List<AckGate.Intent> = gate.watch()

    /** `unwatchPane` — emits [AckGate.Intent.Unwatch] when a watch existed. */
    fun unwatch(): List<AckGate.Intent> = gate.unwatch()

    /** Manual/reconnect `read_pane` (non-forced, 35 s coalescing window). */
    fun requestRead(): List<AckGate.Intent> = gate.requestRead()

    /** Watch-parameter change (interval/lines) — re-issue `watch_pane`. */
    fun restartWatch(): List<AckGate.Intent> = gate.restartWatch()

    /** Connection died — watch/read protocol state dies with it. */
    fun onDisconnect() = gate.onDisconnect()

    /** Roll back optimistic marks after a refused send ([AckGate.onSendFailed]). */
    fun onSendFailed(intent: AckGate.Intent) = gate.onSendFailed(intent)

    /**
     * Lease/grid geometry — the measured cell grid the Compose view leases
     * (`lease_pane_size` columns/rows). Carried into the snapshot so the
     * renderer sizes cells; content is unaffected. Returns the updated
     * snapshot when the geometry actually changed.
     */
    fun resize(columns: Int, rows: Int): Snapshot? {
        if (columns == this.columns && rows == this.rows) return currentSnapshot
        this.columns = columns
        this.rows = rows
        return if (content != null) commit() else currentSnapshot
    }

    /** Drop everything — sign-out or pane eviction. */
    fun reset() {
        chain.reset()
        gate.reset()
        content = null
        format = "plain"
        truncated = false
        viewportOnly = false
        viewportRows = null
        noEcho = false
        noEchoPrompt = null
        resizeSettling = false
        currentSnapshot = null
    }

    private fun commit(): Snapshot {
        revision++
        val body = content.orEmpty()
        val snapshot = Snapshot(
            paneId = paneId,
            content = body,
            lines = body.split('\n'),
            columns = columns,
            rows = rows,
            fingerprint = chain.fingerprint,
            revision = revision,
            format = format,
            truncated = truncated,
            viewportOnly = viewportOnly,
            viewportRows = viewportRows,
            noEcho = noEcho,
            noEchoPrompt = noEchoPrompt,
            resizeSettling = resizeSettling,
        )
        currentSnapshot = snapshot
        return snapshot
    }

    private fun resync(reason: ResyncReason) =
        Result.ResyncRequired(reason, gate.onDeltaRejected())

    /** `typeof x === 'string'` — JSON strings only; numbers/bools/null fail. */
    private fun wireString(element: JsonElement?): String? =
        (element as? JsonPrimitive)?.takeIf { it.isString }?.content

    /** `typeof x === 'boolean'` — string primitives do not coerce. */
    private fun wireBoolean(element: JsonElement?): Boolean? =
        (element as? JsonPrimitive)?.takeIf { !it.isString }?.booleanOrNull

    /** `typeof x === 'number'` — truncated to Int for `viewport_rows`. */
    private fun wireInt(element: JsonElement?): Int? =
        (element as? JsonPrimitive)?.takeIf { !it.isString }?.doubleOrNull?.toInt()

    /** The result of feeding one wire message to the surface. */
    sealed interface Result {
        /** Wire intents to send, in order — `pane_applied`/`read_pane`/`watch_pane`. */
        val intents: List<AckGate.Intent>

        /** A frame committed — [snapshot] is the new render state. */
        data class Committed(
            val snapshot: Snapshot,
            override val intents: List<AckGate.Intent>,
        ) : Result

        /**
         * No frame change — bookkeeping only (`pane_unchanged`): the
         * fingerprint was adopted and [intents] may carry a re-watch.
         */
        data class Unchanged(override val intents: List<AckGate.Intent>) : Result

        /**
         * The chain broke (or the server nudged a resync) — nothing was
         * committed, the stale frame stays displayed, and [intents]
         * carries the forced `read_pane` when the pane is watched.
         */
        data class ResyncRequired(
            val reason: ResyncReason,
            override val intents: List<AckGate.Intent>,
        ) : Result
    }

    /**
     * Immutable render state — the seam the Compose terminal view consumes.
     * [lines] is `content.split('\n')` (the JS row model — a trailing `\n`
     * leaves a final empty row). [fingerprint] is the content identity;
     * [revision] bumps on every commit — including metadata-only deltas
     * whose fingerprint is unchanged — so invalidate on revision, not on
     * fingerprint alone.
     */
    data class Snapshot(
        val paneId: String,
        val content: String,
        val lines: List<String>,
        /** Lease grid width in cells (0 = not leased). */
        val columns: Int,
        /** Lease grid height in rows (0 = not leased / width-only). */
        val rows: Int,
        val fingerprint: String?,
        val revision: Long,
        /** `"ansi" | "text" | "plain"` — the frame format the relay used. */
        val format: String,
        val truncated: Boolean,
        val viewportOnly: Boolean,
        val viewportRows: Int?,
        val noEcho: Boolean,
        val noEchoPrompt: String?,
        /** Inside the post-resize settle window — do not commit rows to history. */
        val resizeSettling: Boolean,
    )
}
