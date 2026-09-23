package com.lerdr.app.ui.computers

import com.google.common.truth.Truth.assertThat
import com.lerdr.app.computers.LatencyBand
import com.lerdr.app.computers.latencyBand
import org.junit.Test

/**
 * `latencyBand` — connected RTT bands the status chip tints by; offline
 * and unmeasured relays return null so they can never read as fast.
 */
class LatencyBandTest {

    @Test
    fun `measured rtt maps to bands at the documented edges`() {
        assertThat(latencyBand(connected = true, rttMs = 0)).isEqualTo(LatencyBand.GOOD)
        assertThat(latencyBand(connected = true, rttMs = 49)).isEqualTo(LatencyBand.GOOD)
        assertThat(latencyBand(connected = true, rttMs = 50)).isEqualTo(LatencyBand.FAIR)
        assertThat(latencyBand(connected = true, rttMs = 149)).isEqualTo(LatencyBand.FAIR)
        assertThat(latencyBand(connected = true, rttMs = 150)).isEqualTo(LatencyBand.POOR)
        assertThat(latencyBand(connected = true, rttMs = 4_000)).isEqualTo(LatencyBand.POOR)
    }

    @Test
    fun `offline and unmeasured relays have no band`() {
        assertThat(latencyBand(connected = false, rttMs = 12)).isNull()
        assertThat(latencyBand(connected = false, rttMs = -1)).isNull()
        assertThat(latencyBand(connected = true, rttMs = -1)).isNull()
    }
}
