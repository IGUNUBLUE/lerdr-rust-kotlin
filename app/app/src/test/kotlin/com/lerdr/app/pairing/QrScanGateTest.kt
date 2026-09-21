package com.lerdr.app.pairing

import com.google.common.truth.Truth.assertThat
import org.junit.Test

/**
 * The analyzer's decode boundary, exercised without a camera: which raw
 * QR strings count as setup links, and the once-only latch that keeps a
 * held QR from re-firing.
 */
class QrScanGateTest {

    private fun invitationLink(
        relay: String = "ws://192.168.1.9:7474",
        expiresAt: Long = System.currentTimeMillis() + 600_000,
    ): String = "lerdr://pair#" +
        "setup=${"s".repeat(43)}" +
        "&invite=invitation_000001" +
        "&invite_version=1" +
        "&invite_expires=$expiresAt" +
        "&label=desk" +
        "&relay=$relay"

    @Test
    fun `blank and foreign decodes are ignored`() {
        val gate = QrScanGate()

        assertThat(gate.offer(null)).isNull()
        assertThat(gate.offer("")).isNull()
        assertThat(gate.offer("   ")).isNull()
        assertThat(gate.offer("hello world")).isNull()
        assertThat(gate.offer("https://example.com/page")).isNull()
        assertThat(gate.offer("WIFI:S:home;T:WPA;P:hunter2;;")).isNull()
        assertThat(gate.consumed).isFalse()
    }

    @Test
    fun `lerdr invitation link is emitted`() {
        val gate = QrScanGate()
        val link = invitationLink()

        assertThat(gate.offer(link)).isEqualTo(link)
        assertThat(gate.consumed).isTrue()
    }

    @Test
    fun `oracle page link is emitted`() {
        val gate = QrScanGate()
        val expires = System.currentTimeMillis() + 600_000
        val link = "https://relay.example.com/#setup=${"s".repeat(43)}" +
            "&invite=invitation_000001&invite_version=1&invite_expires=$expires"

        assertThat(gate.offer(link)).isEqualTo(link)
        assertThat(gate.consumed).isTrue()
    }

    @Test
    fun `bootstrap setup-only link is emitted`() {
        val gate = QrScanGate()
        val link = "lerdr://pair#setup=${"k".repeat(32)}&relay=ws://10.0.0.2:7474"

        assertThat(gate.offer(link)).isEqualTo(link)
    }

    @Test
    fun `first valid link wins — everything after is dropped`() {
        val gate = QrScanGate()
        val first = invitationLink()

        assertThat(gate.offer(first)).isEqualTo(first)
        assertThat(gate.offer(invitationLink(relay = "ws://10.9.9.9:1"))).isNull()
        assertThat(gate.offer(first)).isNull() // even the same link again
    }

    @Test
    fun `setup links failing strict validation are ignored`() {
        val gate = QrScanGate()

        // setup below the 16-char floor.
        assertThat(gate.offer("lerdr://pair#setup=short&relay=ws://x:1")).isNull()
        // Retired gateway transport is rejected outright.
        assertThat(
            gate.offer("lerdr://pair#setup=${"s".repeat(43)}&gateways=ws://g:1"),
        ).isNull()
        // lerdr:// links carry no page origin — relay= is mandatory.
        assertThat(gate.offer("lerdr://pair#setup=${"s".repeat(43)}")).isNull()
        // No fragment at all.
        assertThat(gate.offer("lerdr://pair")).isNull()
        assertThat(gate.consumed).isFalse()

        // …and a valid link afterwards still wins.
        assertThat(gate.offer(invitationLink())).isNotNull()
    }
}
