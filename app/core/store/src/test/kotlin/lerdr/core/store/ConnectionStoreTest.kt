package lerdr.core.store

import app.cash.turbine.test
import com.google.common.truth.Truth.assertThat
import kotlinx.coroutines.test.runTest
import lerdr.core.model.HerdrStatus
import lerdr.core.model.HerdrStatusMessage
import lerdr.core.model.InventoryStatusMessage
import lerdr.core.model.InventoryState
import lerdr.core.model.PushConfigMessage
import lerdr.core.model.WireField
import org.junit.Test

class ConnectionStoreTest {

    private fun store() = ConnectionStore(clock = { 1_000L })

    private fun connect(store: ConnectionStore, relayId: String = "r1") {
        store.connect(relayId, "relay")
        store.onTransportStatus(
            relayId, TransportStatus.CONNECTED,
            TransportStatusDetail(path = TransportKind.WEBSOCKET),
        )
    }

    // ── state machine ────────────────────────────────────────────────

    @Test
    fun `connect registers a connecting row`() {
        val store = store()
        store.connect("r1", "relay")
        val conn = store.connectionNow("r1")!!
        assertThat(conn.status).isEqualTo(RelayStatus.CONNECTING)
        assertThat(conn.phase).isEqualTo(ConnectionPhase.CONNECTING)
        assertThat(conn.connectingSince).isEqualTo(1_000L)
    }

    @Test
    fun `connected status records the answering path`() {
        val store = store()
        connect(store)
        val conn = store.connectionNow("r1")!!
        assertThat(conn.status).isEqualTo(RelayStatus.CONNECTED)
        assertThat(conn.phase).isEqualTo(ConnectionPhase.DEGRADED) // inventory still starting
        assertThat(conn.path).isEqualTo(TransportKind.WEBSOCKET)
        assertThat(conn.activeGatewayUrl).isEmpty()
        assertThat(store.lastMessageAt("r1")).isEqualTo(1_000L)
    }

    @Test
    fun `gateway path keeps the answering gateway url`() {
        val store = store()
        store.connect("r1", "relay")
        store.onTransportStatus(
            "r1", TransportStatus.CONNECTED,
            TransportStatusDetail(path = TransportKind.GATEWAY, gatewayUrl = "wss://gw"),
        )
        assertThat(store.connectionNow("r1")!!.activeGatewayUrl).isEqualTo("wss://gw")
    }

    @Test
    fun `connected with ready inventory is fully connected, otherwise degraded`() {
        val store = store()
        connect(store)
        assertThat(store.connectionNow("r1")!!.phase).isEqualTo(ConnectionPhase.DEGRADED)
        store.applyInventoryStatus("r1", InventoryStatusMessage(state = "ready"))
        assertThat(store.connectionNow("r1")!!.phase).isEqualTo(ConnectionPhase.CONNECTED)
        store.applyInventoryStatus("r1", InventoryStatusMessage(state = "error", message = "x"))
        assertThat(store.connectionNow("r1")!!.phase).isEqualTo(ConnectionPhase.DEGRADED)
    }

    @Test
    fun `close lands at disconnected`() = runTest {
        val store = store()
        connect(store)
        store.connections.test {
            val before = awaitItem()
            assertThat(before["r1"]!!.status).isEqualTo(RelayStatus.CONNECTED)
            store.onTransportStatus("r1", TransportStatus.CLOSED)
            val closed = awaitItem()
            assertThat(closed["r1"]!!.status).isEqualTo(RelayStatus.DISCONNECTED)
            assertThat(closed["r1"]!!.phase).isEqualTo(ConnectionPhase.DISCONNECTED)
            cancelAndIgnoreRemainingEvents()
        }
    }

    @Test
    fun `device_unauthorized latches authRejected and closes the row`() {
        val store = store()
        connect(store)
        store.onTransportStatus(
            "r1", TransportStatus.CLOSED,
            TransportStatusDetail(code = TransportStatusDetail.DEVICE_UNAUTHORIZED),
        )
        val conn = store.connectionNow("r1")!!
        assertThat(conn.authRejected).isTrue()
        assertThat(conn.closed).isTrue()
        assertThat(conn.phase).isEqualTo(ConnectionPhase.PAIRING)
        // Late transport events are ignored once closed.
        store.onTransportStatus("r1", TransportStatus.CONNECTED)
        assertThat(store.connectionNow("r1")!!.status).isEqualTo(RelayStatus.DISCONNECTED)
    }

    @Test
    fun `pairingRequired shows a disconnected pairing row that never dials`() {
        val store = store()
        store.markPairingRequired("r1", "relay")
        val conn = store.connectionNow("r1")!!
        assertThat(conn.status).isEqualTo(RelayStatus.DISCONNECTED)
        assertThat(conn.pairingRequired).isTrue()
        assertThat(conn.phase).isEqualTo(ConnectionPhase.PAIRING)
        // Transport noise cannot resurrect it.
        store.onTransportStatus("r1", TransportStatus.CONNECTED)
        assertThat(store.connectionNow("r1")!!.phase).isEqualTo(ConnectionPhase.PAIRING)
    }

    @Test
    fun `connect resets pairing flags on a fresh attempt`() {
        val store = store()
        store.markPairingRequired("r1", "relay")
        store.connect("r1", "relay")
        val conn = store.connectionNow("r1")!!
        assertThat(conn.pairingRequired).isFalse()
        assertThat(conn.status).isEqualTo(RelayStatus.CONNECTING)
    }

    @Test
    fun `unknown relay ids are ignored`() {
        val store = store()
        store.onTransportStatus("nope", TransportStatus.CONNECTED)
        store.applyInventoryStatus("nope", InventoryStatusMessage(state = "ready"))
        assertThat(store.connections.value).isEmpty()
    }

    // ── push_config intake ───────────────────────────────────────────

    @Test
    fun `push_config populates the connection view`() {
        val store = store()
        connect(store)
        store.applyPushConfig(
            "r1",
            PushConfigMessage(
                host = "workstation",
                home = "/home/u",
                protocol = 3,
                version = "0.26.3",
                releaseVersion = "0.26.3",
                revision = "abc123",
                capabilities = WireField.Present(listOf("attention_classification", "push_policy")),
                inventory = WireField.Present(InventoryState(panes = 4, state = "ready")),
            ),
        )
        val conn = store.connectionNow("r1")!!
        assertThat(conn.host).isEqualTo("workstation")
        assertThat(conn.protocol).isEqualTo(3)
        assertThat(conn.capabilities).containsExactly("attention_classification", "push_policy")
        assertThat(conn.attentionCapable).isTrue()
        assertThat(conn.inventory.state).isEqualTo(AgentInventoryState.READY)
        assertThat(conn.phase).isEqualTo(ConnectionPhase.CONNECTED)
    }

    @Test
    fun `push_config absent inventory falls back to ready`() {
        val store = store()
        connect(store)
        store.applyPushConfig("r1", PushConfigMessage(protocol = 3))
        assertThat(store.connectionNow("r1")!!.inventory.state).isEqualTo(AgentInventoryState.READY)
    }

    @Test
    fun `herdr_status is generation gated`() {
        val store = store()
        connect(store)
        store.applyPushConfig("r1", PushConfigMessage(herdrStatus = HerdrStatus(generation = 5)))
        store.applyHerdrStatus(
            "r1", HerdrStatusMessage(status = HerdrStatus(generation = 4, serverVersion = "old")),
        )
        assertThat(store.connectionNow("r1")!!.herdrStatus!!.generation).isEqualTo(5)
        store.applyHerdrStatus(
            "r1", HerdrStatusMessage(status = HerdrStatus(generation = 6, serverVersion = "new")),
        )
        assertThat(store.connectionNow("r1")!!.herdrStatus!!.serverVersion).isEqualTo("new")
    }

    // ── snapshot gate ────────────────────────────────────────────────

    @Test
    fun `inventory gate drops snapshots while starting unless stale`() {
        val store = store()
        connect(store)
        assertThat(store.acceptsInventorySnapshots("r1")).isFalse()
        store.applyInventoryStatus("r1", InventoryStatusMessage(state = "starting", stale = true))
        assertThat(store.acceptsInventorySnapshots("r1")).isTrue()
        store.applyInventoryStatus("r1", InventoryStatusMessage(state = "ready"))
        assertThat(store.acceptsInventorySnapshots("r1")).isTrue()
        // No connection at all passes — matches the oracle's `connection &&`.
        assertThat(store.acceptsInventorySnapshots("ghost")).isTrue()
    }

    @Test
    fun `disconnect removes the row`() {
        val store = store()
        connect(store)
        store.disconnect("r1")
        assertThat(store.connections.value).isEmpty()
    }
}
