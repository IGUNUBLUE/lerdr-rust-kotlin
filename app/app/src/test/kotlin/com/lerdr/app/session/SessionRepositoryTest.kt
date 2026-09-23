package com.lerdr.app.session

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import com.google.common.truth.Truth.assertThat
import java.io.File
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.long
import lerdr.core.data.RelayEndpoint
import lerdr.core.data.RelayRegistry
import lerdr.core.protocol.LerdrJson
import lerdr.core.protocol.Protocol
import lerdr.core.store.AgentStore
import lerdr.core.store.ConnectionStore
import lerdr.core.store.RelayStatus
import lerdr.core.store.WorkspaceStore
import lerdr.core.store.clientPaneId
import lerdr.core.transport.CommandException
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

        suspend fun connectReady(
            caps: List<String> = listOf(
                "pane_realtime_delta",
                "pane_size_lease",
                "attention_classification",
            ),
        ) {
            repository.connect(endpoint)
            val handle = handle()
            handle.connect()
            val capList = caps.joinToString(",") { "\"$it\"" }
            handle.emit(json("""{"type":"push_config","capabilities":[$capList],"inventory":{"state":"ready"}}"""))
            handle.emit(json(agentRow("%1")))
            pump()
        }

        fun agentRow(
            rawPaneId: String,
            serverSessionId: String = "ss1",
            generation: Int = 3,
        ): String =
            """{"type":"agents","agents":[{"pane_id":"$rawPaneId","raw_pane_id":"$rawPaneId","terminal_id":"t1","server_session_id":"$serverSessionId","generation":$generation,"agent":"claude","name":"claude","status":"working","cwd":"/home/u/lerdr","project":"lerdr","workspace_id":"w1","updated_at":100}]}"""

        suspend fun connectPrimaryAgent() {
            connectReady()
            handle().emit(json(agentRow("%1", serverSessionId = "primary")))
            pump()
        }

        /** Seeds `connection.update` the way a `check_update` reply does. */
        suspend fun seedInstallableUpdate() {
            connectReady(caps = listOf("self_update"))
            val pending = scope.async { repository.checkUpdate("r1") }
            pump()
            val sent = sentFrames(handle())
                .single { it["type"]?.jsonPrimitive?.content == "check_update" }
            val requestId = sent["request_id"]!!.jsonPrimitive.content
            handle().emit(
                json(
                    """{"type":"command_result","request_id":"$requestId","action":"check_update","ok":true,"phase":"completed","data":{"update":{"state":"available","available_version":"1.4.0","available_revision":"abc123","target_version":"1.4.0","target_revision":"abc123","can_install":true}}}""",
                ),
            )
            pending.await()
        }
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

    // ── viewed pane (push_viewed_pane) ─────────────────────────────

    private fun viewedFrames(h: FakeRelaySessionHandle) = sentFrames(h)
        .filter { it["type"]?.jsonPrimitive?.content == "push_viewed_pane" }

    @Test
    fun `viewed pane pushes the exact target for a primary-session pane`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectPrimaryAgent()
        h.repository.setViewedPane(clientPaneId("r1", "%1"))
        val pushed = viewedFrames(h.handle())
        assertThat(pushed).hasSize(1)
        val set = pushed.single()
        assertThat(set["visible"]!!.jsonPrimitive.boolean).isTrue()
        assertThat(set["unlocked"]!!.jsonPrimitive.boolean).isTrue()
        assertThat(set["protocol"]!!.jsonPrimitive.int).isEqualTo(Protocol.VERSION)
        val target = set["target"] as JsonObject
        assertThat(target["pane_id"]!!.jsonPrimitive.content).isEqualTo("%1")
        assertThat(target["terminal_id"]!!.jsonPrimitive.content).isEqualTo("t1")
        assertThat(target["server_session_id"]!!.jsonPrimitive.content).isEqualTo("primary")
        assertThat(target["generation"]!!.jsonPrimitive.long).isEqualTo(3)
    }

    @Test
    fun `viewed pane dedupes an unchanged signature`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectPrimaryAgent()
        h.repository.setViewedPane(clientPaneId("r1", "%1"))
        h.repository.setViewedPane(clientPaneId("r1", "%1"))
        assertThat(viewedFrames(h.handle())).hasSize(1)
    }

    @Test
    fun `leaving the session clears the viewed relay`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectPrimaryAgent()
        h.repository.setViewedPane(clientPaneId("r1", "%1"))
        h.repository.setViewedPane(null)
        val pushed = viewedFrames(h.handle())
        assertThat(pushed).hasSize(2)
        val clear = pushed[1]
        assertThat(clear["visible"]!!.jsonPrimitive.boolean).isFalse()
        // The clear frame reports the real lock state; the app is unlocked.
        assertThat(clear["unlocked"]!!.jsonPrimitive.boolean).isTrue()
        assertThat(clear["target"]).isNull()
    }

    @Test
    fun `locking clears the viewed pane and unlocking republishes it`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectPrimaryAgent()
        h.repository.setViewedPane(clientPaneId("r1", "%1"))
        h.repository.setLocked(true)
        h.repository.setLocked(false)
        val pushed = viewedFrames(h.handle())
        assertThat(pushed).hasSize(3)
        val clear = pushed[1]
        assertThat(clear["visible"]!!.jsonPrimitive.boolean).isFalse()
        assertThat(clear["unlocked"]!!.jsonPrimitive.boolean).isFalse()
        val republished = pushed[2]
        assertThat(republished["visible"]!!.jsonPrimitive.boolean).isTrue()
        assertThat(republished["unlocked"]!!.jsonPrimitive.boolean).isTrue()
        assertThat(republished["target"]).isNotNull()
    }

    @Test
    fun `a non-primary agent never publishes a viewed pane`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady() // fixture agent rides server_session_id "ss1"
        h.repository.setViewedPane(clientPaneId("r1", "%1"))
        assertThat(viewedFrames(h.handle())).isEmpty()
    }

    @Test
    fun `hiding the app clears the viewed pane`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectPrimaryAgent()
        h.repository.setViewedPane(clientPaneId("r1", "%1"))
        h.repository.setHidden(true)
        val pushed = viewedFrames(h.handle())
        assertThat(pushed).hasSize(2)
        assertThat(pushed[1]["visible"]!!.jsonPrimitive.boolean).isFalse()
    }

    @Test
    fun `agent regeneration repushes the new signature`() = runTest {
        val h = Harness(this, tmp.root)
        // With start() the registry owns session membership — an unregistered
        // endpoint would be torn down by reconcile before the agent lands.
        h.registry.upsert(h.endpoint)
        h.repository.start()
        h.connectPrimaryAgent()
        h.repository.setViewedPane(clientPaneId("r1", "%1"))
        h.handle().emit(json(h.agentRow("%1", serverSessionId = "primary", generation = 4)))
        h.pump()
        val pushed = viewedFrames(h.handle())
        // set(gen 3) → clear → set(gen 4); the store may emit an
        // intermediate removal first, so assert the tail, not the count.
        val last = pushed.last()
        assertThat(last["visible"]!!.jsonPrimitive.boolean).isTrue()
        val target = last["target"] as JsonObject
        assertThat(target["generation"]!!.jsonPrimitive.long).isEqualTo(4)
        assertThat(pushed[pushed.size - 2]["visible"]!!.jsonPrimitive.boolean).isFalse()
    }

    // ── relay self-update ──────────────────────────────────────────

    @Test
    fun `checkUpdate refuses a relay without the self_update capability`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        try {
            h.repository.checkUpdate("r1")
            org.junit.Assert.fail("expected CommandException")
        } catch (expected: CommandException) {
            assertThat(expected).hasMessageThat().contains("does not support")
        }
    }

    @Test
    fun `checkUpdate sends check_update and folds the update payload`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady(caps = listOf("self_update"))
        val pending = backgroundScope.async { h.repository.checkUpdate("r1") }
        h.pump()
        val sent = sentFrames(h.handle())
            .single { it["type"]?.jsonPrimitive?.content == "check_update" }
        val requestId = sent["request_id"]!!.jsonPrimitive.content
        h.handle().emit(
            json(
                """{"type":"command_result","request_id":"$requestId","action":"check_update","ok":true,"phase":"completed","data":{"update":{"state":"available","available_version":"1.4.0","available_revision":"abc123","target_version":"1.4.0","target_revision":"abc123","can_install":true}}}""",
            ),
        )
        assertThat(pending.await().ok).isTrue()
        val update = h.connections.connectionNow("r1")?.update
        assertThat(update?.state).isEqualTo("available")
        assertThat(update?.availableVersion).isEqualTo("1.4.0")
        assertThat(update?.canInstall).isTrue()
    }

    @Test
    fun `installUpdate refuses when no installable update is checked`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady(caps = listOf("self_update"))
        try {
            h.repository.installUpdate("r1")
            org.junit.Assert.fail("expected CommandException")
        } catch (expected: CommandException) {
            assertThat(expected).hasMessageThat().contains("No installable update")
        }
    }

    @Test
    fun `installUpdate sends the checked target as expected version and revision`() = runTest {
        val h = Harness(this, tmp.root)
        h.seedInstallableUpdate()
        val pending = backgroundScope.async {
            h.repository.installUpdate("r1")
        }
        h.pump()
        val sent = sentFrames(h.handle())
            .single { it["type"]?.jsonPrimitive?.content == "install_update" }
        assertThat(sent["expected_version"]!!.jsonPrimitive.content).isEqualTo("1.4.0")
        assertThat(sent["expected_revision"]!!.jsonPrimitive.content).isEqualTo("abc123")
        val requestId = sent["request_id"]!!.jsonPrimitive.content
        h.handle().emit(
            json(
                """{"type":"command_result","request_id":"$requestId","action":"install_update","ok":true,"phase":"completed","data":{"update":{"state":"installing","target_version":"1.4.0","target_revision":"abc123"}}}""",
            ),
        )
        assertThat(pending.await().ok).isTrue()
        assertThat(h.connections.connectionNow("r1")?.update?.state).isEqualTo("installing")
    }

    @Test
    fun `installUpdate applies the update payload carried by a refusal`() = runTest {
        val h = Harness(this, tmp.root)
        h.seedInstallableUpdate()
        // async's failed deferred is also reported to backgroundScope — keep
        // the expected refusal inside a launch and hand it back explicitly.
        val caught = CompletableDeferred<CommandException>()
        backgroundScope.launch {
            try {
                h.repository.installUpdate("r1")
                caught.completeExceptionally(AssertionError("expected CommandException"))
            } catch (expected: CommandException) {
                caught.complete(expected)
            }
        }
        h.pump()
        val frame = sentFrames(h.handle())
            .single { it["type"]?.jsonPrimitive?.content == "install_update" }
        val requestId = frame["request_id"]!!.jsonPrimitive.content
        h.handle().emit(
            json(
                """{"type":"command_result","request_id":"$requestId","action":"install_update","ok":false,"error":"upstream moved","phase":"completed","data":{"update":{"state":"blocked","reason":"upstream moved"}}}""",
            ),
        )
        assertThat(caught.await()).hasMessageThat().contains("upstream moved")
        assertThat(h.connections.connectionNow("r1")?.update?.state).isEqualTo("blocked")
    }
}
