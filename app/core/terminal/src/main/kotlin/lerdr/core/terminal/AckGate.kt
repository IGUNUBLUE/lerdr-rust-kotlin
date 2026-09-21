package lerdr.core.terminal

/**
 * Client side of the pane-watch ack gate — the sender protocol the released
 * client runs after applying pane frames (`store.ts` `pane_*` handlers,
 * `acknowledgePaneFrame`, `readPane`, `startPaneWatch`). Pure state machine:
 * it never touches the socket. The session layer encodes the returned
 * [Intent]s and owns the `pane_realtime_delta` capability check and the
 * target identity payload.
 *
 * Oracle contract:
 * - every `pane_delta` that commits is acked while watching — deltas carry
 *   no `ack_required` flag, they are implicitly gated;
 * - `pane_content` is acked only when `ack_required` is true and the
 *   message carried a non-empty `content_fingerprint`;
 * - a rejected delta or a `pane_resync` nudge triggers a forced
 *   `read_pane` (`content_fingerprint: ""`), which also drops the live
 *   watch server-side;
 * - `pane_content` / `pane_unchanged` (re)issue `watch_pane` — suppressed
 *   while a read is in flight, while a watch is already live, or while no
 *   fingerprint is stored;
 * - non-forced `read_pane` requests coalesce under [READ_RETRY_MS].
 *
 * Acks are emitted synchronously per committed frame, in arrival order;
 * the server never pipelines (one unacked frame max), so at most one ack
 * is ever owed.
 */
class AckGate(
    private val storedFingerprint: () -> String?,
    private val clockMillis: () -> Long = System::currentTimeMillis,
) {

    /** Outbound protocol actions, in emission order. */
    sealed interface Intent {
        /** `pane_applied {content_fingerprint}` — the frame just committed. */
        data class Applied(val contentFingerprint: String) : Intent

        /** `read_pane`; [force] sends `content_fingerprint: ""` (resync path). */
        data class ReadPane(val force: Boolean) : Intent

        /** `watch_pane` (re)issue — first frame, post-read, interval change. */
        data object Watch : Intent

        /** `unwatch_pane` — the pane is no longer watched. */
        data object Unwatch : Intent
    }

    /** UI intent to stream this pane — `watchedPanes` membership. */
    var watching: Boolean = false
        private set

    /** A `watch_pane` request is live server-side — `paneWatchesStarted`. */
    var watchStarted: Boolean = false
        private set

    /** Epoch millis of the in-flight `read_pane` — `pendingPaneReads`. */
    var readRequestedAt: Long? = null
        private set

    val readPending: Boolean get() = readRequestedAt != null

    private val hasFingerprint: Boolean get() = !storedFingerprint().isNullOrEmpty()

    /** `watchPane` — mark watched, then try to open the watch. */
    fun watch(): List<Intent> {
        watching = true
        return startWatch()
    }

    /** `unwatchPane` — drop all watch state; emit [Intent.Unwatch] if any existed. */
    fun unwatch(): List<Intent> {
        val hadWatch = watching || watchStarted
        watching = false
        watchStarted = false
        return if (hadWatch) listOf(Intent.Unwatch) else emptyList()
    }

    /** A `pane_delta` committed (applied or metadata-only): ack unconditionally. */
    fun onDeltaCommitted(contentFingerprint: String): List<Intent> =
        if (watching) listOf(Intent.Applied(contentFingerprint)) else emptyList()

    /** Base mismatch, failed apply, bad fingerprint → forced `read_pane`. */
    fun onDeltaRejected(): List<Intent> = forceRead()

    /** `pane_resync` nudge → forced `read_pane`. */
    fun onResync(): List<Intent> = forceRead()

    /**
     * A `pane_content` frame committed. JS order: clear the pending read,
     * ack only `ack_required` frames carrying a non-empty fingerprint,
     * then re-issue the watch (the server cancelled it on `read_pane`, or
     * this is the first frame of a watch chain).
     */
    fun onContent(contentFingerprint: String?, ackRequired: Boolean): List<Intent> {
        readRequestedAt = null
        return buildList {
            if (watching && ackRequired && !contentFingerprint.isNullOrEmpty()) {
                add(Intent.Applied(contentFingerprint))
            }
            addAll(startWatch())
        }
    }

    /** `pane_unchanged` — the read answered with the stored fingerprint; re-watch. */
    fun onUnchanged(): List<Intent> {
        readRequestedAt = null
        return startWatch()
    }

    /**
     * `readPane(agent, force=false)` — refresh and post-reconnect resync.
     * Throttled: a second request inside [READ_RETRY_MS] is dropped; forced
     * reads (delta rejection, `pane_resync`) are never throttled.
     */
    fun requestRead(): List<Intent> {
        val requestedAt = readRequestedAt
        if (requestedAt != null && clockMillis() - requestedAt < READ_RETRY_MS) {
            return emptyList()
        }
        return issueRead(force = false)
    }

    /** `restartPaneWatches` — settings/interval change re-issues the watch. */
    fun restartWatch(): List<Intent> {
        watchStarted = false
        return startWatch()
    }

    /**
     * Watches die with the connection; `watching` (UI intent) survives so
     * the post-reconnect `requestRead` + re-watch can run.
     */
    fun onDisconnect() {
        watchStarted = false
        readRequestedAt = null
    }

    /**
     * Roll back the optimistic mark when the transport refused to send
     * [intent] (`sendRaw` returned false — JS only marks on success).
     */
    fun onSendFailed(intent: Intent) {
        when (intent) {
            is Intent.ReadPane -> readRequestedAt = null
            Intent.Watch -> watchStarted = false
            else -> Unit
        }
    }

    /** Full teardown — sign-out clears watching too. */
    fun reset() {
        watching = false
        watchStarted = false
        readRequestedAt = null
    }

    private fun forceRead(): List<Intent> =
        if (!watching) emptyList() else issueRead(force = true)

    private fun issueRead(force: Boolean): List<Intent> {
        // `read_pane` cancels the watch server-side; the started mark drops
        // before the request goes out.
        watchStarted = false
        readRequestedAt = clockMillis()
        return listOf(Intent.ReadPane(force))
    }

    private fun startWatch(): List<Intent> =
        if (watching && !watchStarted && hasFingerprint && readRequestedAt == null) {
            watchStarted = true
            listOf(Intent.Watch)
        } else {
            emptyList()
        }

    companion object {
        /** `PANE_READ_RETRY_MS` — non-forced `read_pane` coalescing window. */
        const val READ_RETRY_MS = 35_000L
    }
}
