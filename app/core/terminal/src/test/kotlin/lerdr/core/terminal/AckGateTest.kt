package lerdr.core.terminal

import com.google.common.truth.Truth.assertThat
import org.junit.Test

/**
 * [AckGate] state machine — the client's side of the pane-watch ack
 * contract ported from `store.ts`. Every test asserts both the emitted
 * intents (in wire order) and the gate's observable state.
 */
class AckGateTest {

    private var now = 0L
    private var storedFingerprint: String? = null
    private val gate = AckGate({ storedFingerprint }, { now })

    @Test
    fun watchWaitsForStoredFingerprint() {
        assertThat(gate.watch()).isEmpty()
        assertThat(gate.watching).isTrue()
        assertThat(gate.watchStarted).isFalse()

        storedFingerprint = "f0"
        assertThat(gate.watch()).containsExactly(AckGate.Intent.Watch)
        assertThat(gate.watchStarted).isTrue()
    }

    @Test
    fun deltaAcksUnconditionallyWhileWatching() {
        storedFingerprint = "f0"
        gate.watch()

        assertThat(gate.onDeltaCommitted("f1"))
            .containsExactly(AckGate.Intent.Applied("f1"))
        assertThat(gate.onDeltaCommitted("f2"))
            .containsExactly(AckGate.Intent.Applied("f2"))
    }

    @Test
    fun deltaAppliesSilentlyWhenNotWatching() {
        // store.ts:1586-1608 — an unwatched pane still stores the frame,
        // it just never acks.
        assertThat(gate.onDeltaCommitted("f1")).isEmpty()
        assertThat(gate.watchStarted).isFalse()
    }

    @Test
    fun contentAcksOnlyWhenRequired() {
        // Watched before any fingerprint existed — no watch was issued.
        gate.watch()
        storedFingerprint = "f0"

        // pane_content without ack_required: watch issued, no ack.
        assertThat(gate.onContent("f1", ackRequired = false))
            .containsExactly(AckGate.Intent.Watch)

        // Watch now live — an ack_required pane_content acks but does not
        // re-issue watch_pane (paneWatchesStarted already holds the pane).
        assertThat(gate.onContent("f2", ackRequired = true))
            .containsExactly(AckGate.Intent.Applied("f2"))
        assertThat(gate.watchStarted).isTrue()
    }

    @Test
    fun contentAckNeedsNonEmptyFingerprint() {
        storedFingerprint = "f0"
        gate.watch()
        gate.onSendFailed(AckGate.Intent.Watch) // drop the live watch mark

        // ack_required without a usable fingerprint: no ack — but the
        // stored fingerprint still lets the watch re-issue.
        assertThat(gate.onContent(null, ackRequired = true))
            .containsExactly(AckGate.Intent.Watch)
        // Watch now live: no intents at all.
        assertThat(gate.onContent("", ackRequired = true)).isEmpty()
        assertThat(gate.onContent("f1", ackRequired = true))
            .containsExactly(AckGate.Intent.Applied("f1"))
    }

    @Test
    fun rejectedDeltaForcesReadOnlyWhileWatching() {
        assertThat(gate.onDeltaRejected()).isEmpty()

        storedFingerprint = "f0"
        gate.watch()
        val intents = gate.onDeltaRejected()
        assertThat(intents).containsExactly(AckGate.Intent.ReadPane(force = true))
        // read_pane cancels the watch server-side — the started mark drops.
        assertThat(gate.watchStarted).isFalse()
        assertThat(gate.readPending).isTrue()
    }

    @Test
    fun resyncNudgeForcesRead() {
        assertThat(gate.onResync()).isEmpty()

        storedFingerprint = "f0"
        gate.watch()
        assertThat(gate.onResync())
            .containsExactly(AckGate.Intent.ReadPane(force = true))
    }

    @Test
    fun pendingReadSuppressesWatchUntilContentLands() {
        gate.watch() // no fingerprint yet — nothing emitted
        storedFingerprint = "f0"

        assertThat(gate.requestRead())
            .containsExactly(AckGate.Intent.ReadPane(force = false))
        // A second watch() while the read is in flight stays quiet —
        // startPaneWatch early-returns on pendingPaneReads.
        assertThat(gate.watch()).isEmpty()
        // pane_content clears the pending read, then re-issues the watch.
        assertThat(gate.onContent("f1", ackRequired = false))
            .containsExactly(AckGate.Intent.Watch)
        assertThat(gate.readPending).isFalse()
        assertThat(gate.watchStarted).isTrue()
    }

    @Test
    fun unchangedClearsReadAndRewatches() {
        gate.watch()
        storedFingerprint = "f0"
        gate.requestRead()

        assertThat(gate.onUnchanged()).containsExactly(AckGate.Intent.Watch)
        assertThat(gate.readPending).isFalse()
    }

    @Test
    fun nonForcedReadsCoalesceUnderRetryWindow() {
        assertThat(gate.requestRead())
            .containsExactly(AckGate.Intent.ReadPane(force = false))

        now += AckGate.READ_RETRY_MS - 1
        assertThat(gate.requestRead()).isEmpty()

        now += 1
        assertThat(gate.requestRead())
            .containsExactly(AckGate.Intent.ReadPane(force = false))
    }

    @Test
    fun forcedReadBypassesThrottle() {
        storedFingerprint = "f0"
        gate.watch()
        gate.requestRead()
        // A forced read inside the retry window still goes out.
        assertThat(gate.onDeltaRejected())
            .containsExactly(AckGate.Intent.ReadPane(force = true))
    }

    @Test
    fun disconnectDropsProtocolStateButKeepsWatching() {
        storedFingerprint = "f0"
        gate.watch()
        gate.requestRead()

        gate.onDisconnect()
        assertThat(gate.watchStarted).isFalse()
        assertThat(gate.readPending).isFalse()
        assertThat(gate.watching).isTrue()

        // Post-reconnect resync: not throttled — the pending read died
        // with the connection.
        assertThat(gate.requestRead())
            .containsExactly(AckGate.Intent.ReadPane(force = false))
    }

    @Test
    fun sendFailureRollsBackOptimisticMarks() {
        storedFingerprint = "f0"
        gate.watch()
        gate.onSendFailed(AckGate.Intent.Watch)
        assertThat(gate.watchStarted).isFalse()

        gate.requestRead()
        gate.onSendFailed(AckGate.Intent.ReadPane(force = false))
        assertThat(gate.readPending).isFalse()
    }

    @Test
    fun unwatchEmitsOnceAndClears() {
        storedFingerprint = "f0"
        gate.watch()

        assertThat(gate.unwatch()).containsExactly(AckGate.Intent.Unwatch)
        assertThat(gate.watching).isFalse()
        assertThat(gate.watchStarted).isFalse()
        // Nothing left to tear down — a second unwatch is silent.
        assertThat(gate.unwatch()).isEmpty()
        // And acks stop flowing.
        assertThat(gate.onDeltaCommitted("f9")).isEmpty()
    }

    @Test
    fun restartWatchReissuesWhenLive() {
        storedFingerprint = "f0"
        gate.watch()
        assertThat(gate.restartWatch()).containsExactly(AckGate.Intent.Watch)

        gate.unwatch()
        assertThat(gate.restartWatch()).isEmpty()
    }
}
