package lerdr.core.transport

import kotlin.random.Random

/**
 * Reconnect, keepalive, and staleness policy — a 1:1 port of the Go client's
 * scheduler in `frontend/src/lib/store.ts`. Every constant is the oracle's;
 * the wire-level ping is `refresh_agents`, never a dedicated ping frame.
 */
object ReconnectPolicy {
    /** `RECONNECT_BASE_DELAY_MS` — first retry waits exactly this long (jitter 1.0). */
    const val BASE_DELAY_MS = 1_000L

    /** `RECONNECT_MAX_DELAY_MS` — the clamp on the exponential base, also the floor for fatal redials. */
    const val MAX_DELAY_MS = 60_000L

    /**
     * Backoff exponent cap — `2 ** min(attempt - 1, 5)` in the oracle, so the
     * exponential saturates at 32 s rather than [MAX_DELAY_MS]; the 60 s
     * ceiling only binds via a fatal-close floor.
     */
    const val MAX_BACKOFF_EXPONENT = 5

    /** `STALE_CONNECTING_MS` — a dial this old is replaced on revalidation. */
    const val STALE_CONNECTING_MS = 5_000L

    /** `KEEPALIVE_INTERVAL_MS` — one `refresh_agents` per live connection this often. */
    const val KEEPALIVE_INTERVAL_MS = 120_000L

    /** `FOREGROUND_HEALTH_TIMEOUT_MS` — reply window for health pings while visible. */
    const val FOREGROUND_HEALTH_TIMEOUT_MS = 2_000L

    /** `BACKGROUND_HEALTH_TIMEOUT_MS` — reply window while hidden, and for keepalives. */
    const val BACKGROUND_HEALTH_TIMEOUT_MS = 10_000L

    /**
     * `FRESH_PROOF_MS` — silence longer than this means the socket is a
     * corpse: revalidation dials immediately instead of probing.
     */
    const val FRESH_PROOF_MS = 240_000L

    /** `HIDDEN_KEEPALIVE_MAX_MS` — a hidden app stops keepalives after this. */
    const val HIDDEN_KEEPALIVE_MAX_MS = 60L * 60_000L

    /** `COMMAND_TIMEOUT_MS` — default `request` deadline. */
    const val COMMAND_TIMEOUT_MS = 15_000L

    /** `ACCEPTED_COMMAND_TIMEOUT_MS` — deadline re-armed when a command is `accepted`/`prepared`/`awaiting_evidence`. */
    const val ACCEPTED_COMMAND_TIMEOUT_MS = 10_000L

    /** `E2EE_HANDSHAKE_TIMEOUT_MS` — open → server finish. */
    const val HANDSHAKE_TIMEOUT_MS = 10_000L

    /**
     * Inbound queue depth for one socket's raw frames — generous headroom
     * for bursts while the decrypt loop drains; on overflow the socket dies
     * and resync replays a consistent stream (frames can't be skipped).
     */
    const val FRAME_BUFFER_CAPACITY = 256

    /**
     * Decoded-message queue depth between `RelaySession` and its demux
     * consumer — same overflow policy as [FRAME_BUFFER_CAPACITY].
     */
    const val INCOMING_BUFFER_CAPACITY = 256

    /** The keepalive/revalidation ping action — the relay always answers it. */
    const val HEALTH_CHECK_ACTION = "refresh_agents"

    /**
     * `transport.UnauthorizedCloseCode` — the relay refuses this device's
     * credential; retrying replays rejected material, so the policy must not.
     */
    const val UNAUTHORIZED_CLOSE_CODE = 4401

    /** `transport.DeviceUnauthorizedReason` — close detail `code`. */
    const val DEVICE_UNAUTHORIZED_CODE = "device_unauthorized"

    /** `gatewaywire.CodeUnknownRelay` — fatal-looking but usually a restarting relay; normal cadence. */
    const val UNKNOWN_RELAY_CODE = "unknown_relay"

    /**
     * `scheduleReconnect`'s delay ladder:
     *
     * ```
     * attempt  = previous + 1
     * base     = max(floorMs, min(60_000, 1_000 * 2 ** min(attempt - 1, 5)))
     * jitter   = 1.0 (attempt 1) else random in [0.8, 1.2)
     * delay    = round(base * jitter)
     * ```
     *
     * A [floorMs] above the ladder forces slow retries — the oracle passes
     * [MAX_DELAY_MS] for fatal closes other than `unknown_relay`.
     */
    class Backoff(private val random: () -> Double = { Random.nextDouble() }) {
        /** Attempts consumed so far; reset on the first inbound frame. */
        var attempt = 0
            private set

        /** Next delay in milliseconds; increments [attempt]. */
        fun nextDelay(floorMs: Long = 0L): Long {
            attempt += 1
            val exponent = minOf(attempt - 1, MAX_BACKOFF_EXPONENT)
            val base = maxOf(floorMs, minOf(MAX_DELAY_MS, BASE_DELAY_MS shl exponent))
            val jitter = if (attempt == 1) 1.0 else 0.8 + random() * 0.4
            return Math.round(base * jitter)
        }

        /** `reconnectAttempts.delete` — the oracle resets on any inbound message. */
        fun reset() {
            attempt = 0
        }
    }
}
