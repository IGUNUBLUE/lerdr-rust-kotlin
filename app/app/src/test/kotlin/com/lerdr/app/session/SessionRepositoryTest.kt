package com.lerdr.app.session

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import com.google.common.truth.Truth.assertThat
import java.io.File
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonPrimitive
import lerdr.core.data.RelayEndpoint
import lerdr.core.data.RelayRegistry
import lerdr.core.protocol.LerdrJson
import lerdr.core.store.AgentStore
import lerdr.core.store.ConnectionStore
import lerdr.core.store.RelayStatus
import lerdr.core.store.WorkspaceStore
import lerdr.core.store.clientPaneId
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

private fun json(raw: String): JsonObject =
    LerdrJson.parseToJsonElement(raw) as JsonObject

private fun sentFrames(h: FakeRelaySessionHandle): List<JsonObject> =
    h.sentRaw.map { json(it) }

private fun sentTypes(h: FakeRelaySessionHandle) =
    sentFrames(h).map { it["type"]?.jsonPrimitive?.content }

@OptIn(ExperimentalCoroutinesApi::class)
class SessionRepositoryTest {

    @get:Rule
    val tmp = TemporaryFolder()

    private class Harness(
        private val testScope: TestScope,
        tmpDir: File,
    ) {
        private val scope = testScope.backgroundScope
        val credentials = FakeCredentialStore()
        private val dataStore = PreferenceDataStoreFactory.create(scope = scope) {
            File(tmpDir, "relays.preferences_pb")
        }
        val registry = RelayRegistry(dataStore, scope)
        val agents = AgentStore(scope)
        val workspaces = WorkspaceStore()
        val connections = ConnectionStore(clock = { 0L })
        val factory = FakeRelaySessionFactory(scope)
        val repository = SessionRepository(
            scope = scope,
            credentialStore = credentials,
            relayRegistry = registry,
            agentStore = agents,
            workspaceStore = workspaces,
            connectionStore = connections,
            sessionFactory = factory,
        )

        val endpoint = RelayEndpoint(
            id = "r1",
            label = "workstation",
            host = "192.168.1.5",
            port = 7474,
            transport = lerdr.core.data.RelayTransport.WEBSOCKET,
        )
        val origin = "ws://192.168.1.5:7474"

        /**
         * coroutines-test 1.11 treats backgroundScope as background work:
         * `advanceUntilIdle` skips it — `runCurrent` is the pump.
         */
        fun pump() = testScope.runCurrent()

        /** Poll on a real clock — DataStore IO is off-scheduler. */
        fun awaitHandle(origin: String = this.origin): FakeRelaySessionHandle {
            val deadline = System.currentTimeMillis() + 5_000
            var handle = factory.handleFor(origin)
            while (handle == null && System.currentTimeMillis() < deadline) {
                testScope.runCurrent()
                Thread.sleep(5)
                handle = factory.handleFor(origin)
            }
            return handle ?: error("no session for $origin")
        }

        fun handle(): FakeRelaySessionHandle =
            factory.handleFor(origin) ?: error("no session for $origin")

        suspend fun connectReady() {
            repository.connect(endpoint)
            val handle = handle()
            handle.connect()
            handle.emit(json("""{"type":"push_config","capabilities":["pane_realtime_delta","pane_size_lease","attention_classification"],"inventory":{"state":"ready"}}"""))
            handle.emit(json(agentRow("%1")))
            pump()
        }

        fun agentRow(rawPaneId: String): String =
            """{"type":"agents","agents":[{"pane_id":"$rawPaneId","raw_pane_id":"$rawPaneId","terminal_id":"t1","server_session_id":"ss1","generation":3,"agent":"claude","name":"claude","status":"working","cwd":"/home/u/lerdr","project":"lerdr","workspace_id":"w1","updated_at":100}]}"""
    }

    @Test
    fun `connect creates a session and reports CONNECTING`() = runTest {
        val h = Harness(this, tmp.root)
        h.repository.connect(h.endpoint)
        assertThat(h.factory.created).containsKey("ws://192.168.1.5:7474/ws")
        assertThat(h.connections.connectionNow("r1")?.status).isEqualTo(RelayStatus.CONNECTING)
    }

    @Test
    fun `registry upsert reconciles a session into existence`() = runTest {
        val h = Harness(this, tmp.root)
        h.repository.start()
        h.pump()
        assertThat(h.factory.created).isEmpty()
        h.registry.upsert(h.endpoint)
        h.awaitHandle()
    }

    @Test
    fun `missing auth parks the session until credentials land`() = runTest {
        val h = Harness(this, tmp.root)
        h.repository.start()
        // The registry owns session membership — reconcile creates it.
        h.registry.upsert(h.endpoint)
        val handle = h.awaitHandle()
        // No credential → the real session parks as Disconnected(no reason).
        handle.statePark()
        h.pump()
        h.credentials.seed("r1", lerdr.core.data.RelayInvitation(ByteArray(32)))
        h.pump()
        assertThat(handle.reconnectCount).isEqualTo(1)
    }

    @Test
    fun `agents snapshot lands in AgentStore after inventory ready`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        assertThat(h.agents.agents.value.map { it.paneId })
            .containsExactly(clientPaneId("r1", "%1"))
    }

    @Test
    fun `openPane reads then acks pane_content with the fingerprint`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        h.repository.openPane(clientPaneId("r1", "%1"))
        h.pump()
        // watch + read_pane go out — read carries empty fingerprint first time.
        assertThat(sentTypes(h.handle())).contains("read_pane")

        h.handle().emit(
            json(
                """{"type":"pane_content","pane_id":"%1","content":"hello pane","content_fingerprint":"fp-1","format":"ansi","ack_required":true}""",
            ),
        )
        h.pump()
        val applied = sentFrames(h.handle())
            .single { it["type"]?.jsonPrimitive?.content == "pane_applied" }
        assertThat(applied["content_fingerprint"]?.jsonPrimitive?.content).isEqualTo("fp-1")
        assertThat(applied["pane_id"]?.jsonPrimitive?.content).isEqualTo("%1")
        // Target identity rides every pane frame.
        assertThat(applied["server_session_id"]?.jsonPrimitive?.content).isEqualTo("ss1")

        val snapshot = h.repository.paneSnapshot(clientPaneId("r1", "%1")).first()
        assertThat(snapshot?.lines).containsExactly("hello pane")
    }

    @Test
    fun `watch_pane is emitted once content lands and relay supports deltas`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        h.repository.openPane(clientPaneId("r1", "%1"))
        h.handle().emit(
            json(
                """{"type":"pane_content","pane_id":"%1","content":"x","content_fingerprint":"fp-1","format":"ansi"}""",
            ),
        )
        h.pump()
        val watch = sentFrames(h.handle())
            .single { it["type"]?.jsonPrimitive?.content == "watch_pane" }
        assertThat(watch["content_fingerprint"]?.jsonPrimitive?.content).isEqualTo("fp-1")
        assertThat(watch["interval_ms"]).isNotNull()
        assertThat(watch["lines"]).isNotNull()
    }

    @Test
    fun `rejected pane_delta issues a forced read_pane`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        h.repository.openPane(clientPaneId("r1", "%1"))
        h.handle().emit(
            json(
                """{"type":"pane_content","pane_id":"%1","content":"x","content_fingerprint":"fp-1","format":"ansi"}""",
            ),
        )
        h.pump()
        h.handle().emit(
            json(
                """{"type":"pane_delta","pane_id":"%1","base_fingerprint":"WRONG","content_fingerprint":"fp-2","segments":[]}""",
            ),
        )
        h.pump()
        val reads = sentFrames(h.handle())
            .filter { it["type"]?.jsonPrimitive?.content == "read_pane" }
        assertThat(reads.size).isAtLeast(2)
        // The forced read clears the echoed fingerprint.
        assertThat(reads.last()["content_fingerprint"]?.jsonPrimitive?.content).isEqualTo("")
    }

    @Test
    fun `respond sends exact identity and resolves on command_result`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        h.handle().emit(
            json(
                """{"type":"blocked","pane_id":"%1","attention_kind":"approval","prompt":"Allow?","options":["Allow","Deny"],"event_id":"ev1","approval_fingerprint":"afp","server_session_id":"ss1","terminal_id":"t1","generation":3}""",
            ),
        )
        h.pump()

        // Capture the wire frame by resolving via incoming command_result.
        // The request coroutine lives on backgroundScope — pump with
        // runCurrent so its 12 s timeout never elapses.
        val pending = backgroundScope.async {
            h.repository.respond(clientPaneId("r1", "%1"), 0, "Allow")
        }
        h.pump()
        val sent = sentFrames(h.handle())
            .single { it["type"]?.jsonPrimitive?.content == "respond" }
        val requestId = sent["request_id"]!!.jsonPrimitive.content
        assertThat(sent["pane_id"]?.jsonPrimitive?.content).isEqualTo("%1")
        assertThat(sent["choice"]?.jsonPrimitive?.content).isEqualTo("Allow")
        assertThat(sent["approval_fingerprint"]?.jsonPrimitive?.content).isEqualTo("afp")
        assertThat(sent["source"]?.jsonPrimitive?.content).isEqualTo("App")
        assertThat(sent["target"]).isNotNull()

        h.handle().emit(
            json(
                """{"type":"command_result","request_id":"$requestId","action":"respond","ok":true,"phase":"completed"}""",
            ),
        )
        assertThat(pending.await().ok).isTrue()
    }

    @Test
    fun `enrollment redeems a stored invitation for the issued credential`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.saveInvitation(
            "r1",
            lerdr.core.data.RelayInvitation(
                id = "inv1",
                version = 1,
                secret = "a".repeat(43),
                expiresAtEpochMs = Long.MAX_VALUE,
            ),
        )
        h.repository.connect(h.endpoint)
        val handle = h.handle()
        handle.enroll(finish = FakeRelaySessionHandle.testFinish(withSecret = true))
        h.pump()
        val record = h.credentials.get("r1")
        assertThat(record).isInstanceOf(lerdr.core.data.RelayDeviceCredential::class.java)
        assertThat((record as lerdr.core.data.RelayDeviceCredential).invitationId)
            .isEqualTo("inv1")
    }

    @Test
    fun `disconnected session closes the store connection and pane watches reset`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        h.repository.openPane(clientPaneId("r1", "%1"))
        h.pump()
        h.handle().disconnect()
        h.pump()
        assertThat(h.connections.connectionNow("r1")?.status).isEqualTo(RelayStatus.DISCONNECTED)
    }

    @Test
    fun `activity journal collects activity frames`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        h.handle().emit(
            json(
                """{"type":"activity","activity":{"id":"a1","timestamp":1000,"kind":"send_input","summary":"Submitted text","pane_id":"%1"}}""",
            ),
        )
        h.pump()
        assertThat(h.repository.activities.value.map { it.key }).contains("r1:a1")
    }
}
