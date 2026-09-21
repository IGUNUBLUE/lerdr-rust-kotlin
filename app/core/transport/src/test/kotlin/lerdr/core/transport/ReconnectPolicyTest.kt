package lerdr.core.transport

import com.google.common.truth.Truth.assertThat
import org.junit.Test

/**
 * `scheduleReconnect` parity: the ladder `1000 * 2 ** min(attempt-1, 5)`
 * clamped to 60 s, jitter 1.0 on attempt 1 and [0.8, 1.2) afterwards.
 */
class ReconnectPolicyTest {

    @Test
    fun firstRetryIsExactlyOneSecond() {
        val backoff = ReconnectPolicy.Backoff { 0.9 }
        assertThat(backoff.nextDelay()).isEqualTo(1_000L)
    }

    @Test
    fun exponentialLadderSaturatesAtExponentFive() {
        // Deterministic jitter: 0.8 + 0.5 * 0.4 = 1.0 → the exact base.
        val backoff = ReconnectPolicy.Backoff { 0.5 }
        val delays = (1..9).map { backoff.nextDelay() }
        assertThat(delays).containsExactly(
            1_000L, 2_000L, 4_000L, 8_000L, 16_000L, 32_000L, 32_000L, 32_000L, 32_000L,
        ).inOrder()
    }

    @Test
    fun jitterBoundsTheLaterAttempts() {
        val low = ReconnectPolicy.Backoff { 0.0 }
        val high = ReconnectPolicy.Backoff { 0.999_999 }
        low.nextDelay() // attempt 1: no jitter
        high.nextDelay()
        assertThat(low.nextDelay()).isEqualTo(1_600L) // 2000 * 0.8
        assertThat(high.nextDelay()).isEqualTo(2_400L) // 2000 * 1.1999996 → 2399.9992 → 2400
        assertThat(low.nextDelay()).isEqualTo(3_200L)
        assertThat(high.nextDelay()).isEqualTo(4_800L)
    }

    @Test
    fun floorBeatsTheLadderForFatalCloses() {
        val backoff = ReconnectPolicy.Backoff { 0.5 }
        // attempt 1 with the 60 s floor: exact floor, no jitter.
        assertThat(backoff.nextDelay(floorMs = ReconnectPolicy.MAX_DELAY_MS))
            .isEqualTo(60_000L)
        // Later attempts keep the floor and gain jitter.
        assertThat(backoff.nextDelay(floorMs = ReconnectPolicy.MAX_DELAY_MS))
            .isEqualTo(60_000L)
    }

    @Test
    fun resetRestartsAtAttemptOne() {
        val backoff = ReconnectPolicy.Backoff { 0.5 }
        backoff.nextDelay()
        backoff.nextDelay()
        assertThat(backoff.attempt).isEqualTo(2)
        backoff.reset()
        assertThat(backoff.attempt).isEqualTo(0)
        assertThat(backoff.nextDelay()).isEqualTo(1_000L)
    }

    @Test
    fun jitterActuallyVariesWithRandom() {
        var draw = 0.0
        val backoff = ReconnectPolicy.Backoff { draw }
        backoff.nextDelay() // attempt 1 — jitter is exactly 1.0 regardless
        draw = 0.25 // jitter 0.9 → 2000 * 0.9 = 1800
        assertThat(backoff.nextDelay()).isEqualTo(1_800L)
        draw = 0.75 // jitter 1.1 → 4000 * 1.1 = 4400
        assertThat(backoff.nextDelay()).isEqualTo(4_400L)
    }

    @Test
    fun oracleConstants() {
        assertThat(ReconnectPolicy.BASE_DELAY_MS).isEqualTo(1_000L)
        assertThat(ReconnectPolicy.MAX_DELAY_MS).isEqualTo(60_000L)
        assertThat(ReconnectPolicy.STALE_CONNECTING_MS).isEqualTo(5_000L)
        assertThat(ReconnectPolicy.KEEPALIVE_INTERVAL_MS).isEqualTo(120_000L)
        assertThat(ReconnectPolicy.FRESH_PROOF_MS).isEqualTo(240_000L)
        assertThat(ReconnectPolicy.FOREGROUND_HEALTH_TIMEOUT_MS).isEqualTo(2_000L)
        assertThat(ReconnectPolicy.BACKGROUND_HEALTH_TIMEOUT_MS).isEqualTo(10_000L)
        assertThat(ReconnectPolicy.HIDDEN_KEEPALIVE_MAX_MS).isEqualTo(3_600_000L)
        assertThat(ReconnectPolicy.COMMAND_TIMEOUT_MS).isEqualTo(15_000L)
        assertThat(ReconnectPolicy.ACCEPTED_COMMAND_TIMEOUT_MS).isEqualTo(10_000L)
        assertThat(ReconnectPolicy.HANDSHAKE_TIMEOUT_MS).isEqualTo(10_000L)
        assertThat(ReconnectPolicy.HEALTH_CHECK_ACTION).isEqualTo("refresh_agents")
        assertThat(ReconnectPolicy.UNAUTHORIZED_CLOSE_CODE).isEqualTo(4401)
    }
}
