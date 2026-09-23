package com.lerdr.app.home

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import com.google.common.truth.Truth.assertThat
import com.lerdr.app.session.FakeCredentialStore
import com.lerdr.app.session.FakeRelaySessionFactory
import com.lerdr.app.session.FakeRelaySessionHandle
import com.lerdr.app.session.SessionRepository
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.toList
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonPrimitive
import lerdr.core.data.DeviceRole
import lerdr.core.data.RelayDeviceCredential
import lerdr.core.data.RelayEndpoint
import lerdr.core.data.RelayRegistry
import lerdr.core.data.RelayTransport
import lerdr.core.model.Interaction
import lerdr.core.model.Option
import lerdr.core.model.Other
import lerdr.core.protocol.LerdrJson
import lerdr.core.store.AgentStore
import lerdr.core.store.ConnectionStore
import lerdr.core.store.WorkspaceStore
import lerdr.core.store.clientPaneId
import org.junit.After
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

private fun json(raw: String): JsonObject =
    LerdrJson.parseToJsonElement(raw) as JsonObject

/**
 * HomeViewModel → SessionRepository wire delegation: inline `respond`,
 * `answer_question` chips, confirmed `agent_stop`, and the reader gate
 * that suppresses all of them.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class HomeViewModelTest {

    @get:Rule
    val tmp = TemporaryFolder()

    private val mainDispatcher = UnconfinedTestDispatcher()

    @Before
    fun setMain() = Dispatchers.setMain(mainDispatcher)

    @After
    fun resetMain() = Dispatchers.resetMain()

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
        val viewModel = HomeViewModel(FakeHomeRepository(), repository)
        val messages = mutableListOf<String>()

        init {
            scope.launch { viewModel.messages.toList(messages) }
        }

        val endpoint = RelayEndpoint(
            id = "r1",
            label = "workstation",
            host = "192.168.1.5",
            port = 7474,
            transport = RelayTransport.WEBSOCKET,
        )
        private val origin = "ws://192.168.1.5:7474"

        fun pump() = testScope.runCurrent()

        /** Poll on a real clock — registry reconcile rides DataStore IO. */
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

        fun sentFrames(): List<JsonObject> = handle().sentRaw.map {
            LerdrJson.parseToJsonElement(it) as JsonObject
        }

        /**
         * Enroll, upsert the registry (the reconcile loop owns session
         * membership — a direct `connect()` would be torn down), then drive
         * the fake handle through Connected + the ready push_config.
         */
        suspend fun connectReady(role: DeviceRole = DeviceRole.CONTROLLER) {
            credentials.seed("r1", credential(role))
            repository.start()
            pump()
            registry.upsert(endpoint)
            val handle = awaitHandle()
            handle.connect()
            handle.emit(
                json(
                    """{"type":"push_config","capabilities":["attention_classification"],"inventory":{"state":"ready"}}""",
                ),
            )
            handle.emit(
                json(
                    """{"type":"agents","agents":[{"pane_id":"%1","raw_pane_id":"%1","terminal_id":"t1","server_session_id":"ss1","generation":3,"agent":"claude","name":"claude","status":"blocked","attention_kind":"approval","prompt":"Run tests?","options":["Allow","Deny"],"event_id":"ev1","approval_fingerprint":"afp","updated_at":100}]}""",
                ),
            )
            handle.emit(
                json(
                    """{"type":"blocked","pane_id":"%2","raw_pane_id":"%2","terminal_id":"t2","server_session_id":"ss2","generation":1,"attention_kind":"question","prompt":"","interaction":{"id":"q1","kind":"single_select","question":"Pick a module","options":[{"index":0,"label":"store"},{"index":1,"label":"session"}],"other":{"hidden":true},"question_total":1}}""",
                ),
            )
            pump()
        }

        /** Resolve the pending raw command with an `ok` command_result. */
        suspend fun answer(action: String) {
            val sent = sentFrames().single {
                it["type"]?.jsonPrimitive?.content == action
            }
            handle().emit(
                json(
                    """{"type":"command_result","request_id":"${sent["request_id"]!!.jsonPrimitive.content}","action":"$action","ok":true,"phase":"completed"}""",
                ),
            )
            pump()
        }

        private fun credential(role: DeviceRole) = RelayDeviceCredential(
            id = "cred-1",
            version = 1,
            secret = java.util.Base64.getUrlEncoder().withoutPadding()
                .encodeToString(ByteArray(32) { it.toByte() }),
            deviceId = "dev-1",
            role = role,
            locale = "en",
            issuedAtEpochMs = 1_000L,
        )
    }

    private fun approvalCard(
        paneId: String = clientPaneId("r1", "%1"),
        controllable: Boolean = true,
        responding: Boolean = false,
    ) = AttentionCardUi(
        paneId = paneId,
        relayId = "r1",
        agentLabel = "claude · lerdr",
        kind = AttentionKind.APPROVAL,
        metaLabel = "approval · 40s",
        prompt = "Run tests?",
        options = listOf("Allow", "Deny"),
        responding = responding,
        controllable = controllable,
    )

    @Test
    fun `respond sends the wire respond frame with the picked choice`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        h.viewModel.respond(approvalCard(), 0)
        h.pump()
        val sent = h.sentFrames().single { it["type"]?.jsonPrimitive?.content == "respond" }
        assertThat(sent["index"]?.jsonPrimitive?.content).isEqualTo("0")
        assertThat(sent["choice"]?.jsonPrimitive?.content).isEqualTo("Allow")
        assertThat(sent["approval_fingerprint"]?.jsonPrimitive?.content).isEqualTo("afp")
        h.answer("respond")
        assertThat(h.messages).contains("Confirmed: Allow")
    }

    @Test
    fun `answerQuestion sends selected option index`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val card = AttentionCardUi(
            paneId = clientPaneId("r1", "%2"),
            relayId = "r1",
            agentLabel = "claude · lerdr",
            kind = AttentionKind.QUESTION,
            metaLabel = "question · 2 options",
            prompt = "Pick a module",
            interaction = Interaction(
                id = "q1",
                kind = "single_select",
                question = "Pick a module",
                options = listOf(
                    Option(index = 0, label = "store"),
                    Option(index = 1, label = "session"),
                ),
                other = Other(hidden = true),
                questionTotal = 1,
            ),
            controllable = true,
        )
        h.viewModel.answerQuestion(card, 1)
        h.pump()
        val sent = h.sentFrames().single {
            it["type"]?.jsonPrimitive?.content == "answer_question"
        }
        assertThat(sent["interaction_id"]?.jsonPrimitive?.content).isEqualTo("q1")
        h.answer("answer_question")
    }

    @Test
    fun `stopAgent sends agent_stop and confirms via message`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val row = AgentListItemUi(
            paneId = clientPaneId("r1", "%1"),
            relayId = "r1",
            title = "claude · lerdr",
            statusLine = "blocked",
            activityLabel = null,
            elapsedLabel = "idle",
            working = false,
            controllable = true,
        )
        h.viewModel.stopAgent(row)
        h.pump()
        // `agent_stop` rides the typed request path — `handle().requests`,
        // not the raw-frame channel.
        assertThat(h.handle().requests.map { it.type }).contains("agent_stop")
        assertThat(h.messages).contains("Agent stopped.")
    }

    @Test
    fun `reader cards never emit mutations`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady(role = DeviceRole.READER)
        h.viewModel.respond(approvalCard(controllable = false), 0)
        h.viewModel.stopAgent(
            AgentListItemUi(
                paneId = clientPaneId("r1", "%1"),
                relayId = "r1",
                title = "claude · lerdr",
                statusLine = "blocked",
                activityLabel = null,
                elapsedLabel = "idle",
                working = false,
                controllable = false,
            ),
        )
        h.pump()
        assertThat(
            h.sentFrames().map { it["type"]?.jsonPrimitive?.content },
        ).doesNotContain("respond")
        assertThat(h.handle().requests.map { it.type }).doesNotContain("agent_stop")
    }

    @Test
    fun `an in-flight card ignores a second tap`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        h.viewModel.respond(approvalCard(responding = true), 0)
        h.pump()
        assertThat(
            h.sentFrames().map { it["type"]?.jsonPrimitive?.content },
        ).doesNotContain("respond")
    }
}
