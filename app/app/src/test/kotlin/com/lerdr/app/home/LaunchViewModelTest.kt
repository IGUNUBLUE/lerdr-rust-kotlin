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
import lerdr.core.protocol.LerdrJson
import lerdr.core.store.AgentStore
import lerdr.core.store.ConnectionStore
import lerdr.core.store.WorkspaceStore
import org.junit.After
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

private fun launchJson(raw: String): JsonObject =
    LerdrJson.parseToJsonElement(raw) as JsonObject

/**
 * Launch-sheet logic: the oracle's `validAgentName`/`launchNamePart`/
 * `suggestedLaunchName` name rules, the `list_directories` browser flow,
 * and `workspace_create` submission including the reader gate.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class LaunchViewModelTest {

    @get:Rule
    val tmp = TemporaryFolder()

    private val mainDispatcher = UnconfinedTestDispatcher()

    @Before
    fun setMain() = Dispatchers.setMain(mainDispatcher)

    @After
    fun resetMain() = Dispatchers.resetMain()

    // ── pure naming rules ────────────────────────────────────────────

    @Test
    fun `validAgentName enforces the wire pattern`() {
        assertThat(validAgentName("lerdr-claude")).isTrue()
        assertThat(validAgentName("a")).isTrue()
        assertThat(validAgentName("a".repeat(32))).isTrue()
        assertThat(validAgentName("")).isFalse()
        assertThat(validAgentName("9lives")).isFalse()
        assertThat(validAgentName("Upper")).isFalse()
        assertThat(validAgentName("has space")).isFalse()
        assertThat(validAgentName("a".repeat(33))).isFalse()
    }

    @Test
    fun `launchNamePart normalizes diacritics and separators`() {
        assertThat(launchNamePart("Api Server", "x")).isEqualTo("api-server")
        assertThat(launchNamePart("études", "x")).isEqualTo("etudes")
        // Oracle parity: only edges are trimmed — interior separator runs
        // (`__`) survive unchanged.
        assertThat(launchNamePart("--weird__name--", "x")).isEqualTo("weird__name")
        assertThat(launchNamePart("", "fallback")).isEqualTo("fallback")
        // Leading non-letters are suffixed onto the fallback instead of
        // producing an invalid name.
        assertThat(launchNamePart("123abc", "agent")).isEqualTo("agent-123abc")
    }

    @Test
    fun `suggestedLaunchName is dir-basename plus profile`() {
        assertThat(suggestedLaunchName("/home/u/api-server", "claude"))
            .isEqualTo("api-server-claude")
        assertThat(suggestedLaunchName("/home/u/api-server", ""))
            .isEqualTo("api-server-agent")
        assertThat(suggestedLaunchName("", "claude")).isEqualTo("project-claude")
        assertThat(suggestedLaunchName("/home/u/a-very-long-directory-name", "claude"))
            .hasLength(32)
    }

    // ── submit paths through a real SessionRepository ────────────────

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
        val viewModel = LaunchViewModel(repository, workspaces)
        val events = mutableListOf<LaunchViewModel.LaunchEvent>()
        val messages = mutableListOf<String>()

        init {
            scope.launch { viewModel.events.toList(events) }
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
                launchJson(
                    """{"type":"push_config","capabilities":["directory_browser","workspace_management"],"inventory":{"state":"ready"},"agent_profiles":[{"id":"claude","label":"Claude"}]}""",
                ),
            )
            pump()
        }

        /** Answer the newest `list_directories` call with a root listing. */
        suspend fun answerDirectory() {
            val sent = sentFrames().last {
                it["type"]?.jsonPrimitive?.content == "list_directories"
            }
            handle().emit(
                launchJson(
                    """{"type":"command_result","request_id":"${sent["request_id"]!!.jsonPrimitive.content}","action":"list_directories","ok":true,"phase":"completed","data":{"current":{"path":"/home/u","label":"u"},"parent":"","directories":[{"name":"lerdr","path":"/home/u/lerdr"}]}}""",
                ),
            )
            pump()
        }

        suspend fun answer(action: String, data: String = "") {
            val sent = sentFrames().last {
                it["type"]?.jsonPrimitive?.content == action
            }
            val dataField = if (data.isEmpty()) "" else ""","data":$data"""
            handle().emit(
                launchJson(
                    """{"type":"command_result","request_id":"${sent["request_id"]!!.jsonPrimitive.content}","action":"$action","ok":true,"phase":"completed"$dataField}""",
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

    @Test
    fun `new workspace submit sends workspace_create and dismisses`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        h.viewModel.beginWorkspace()
        h.pump()
        // ensureRelay → selectRelay → loadDirectory("") — the browser warms
        // before the sheet even renders.
        h.answerDirectory()
        assertThat(h.viewModel.uiState.value.cwd).isEqualTo("/home/u")
        assertThat(h.viewModel.uiState.value.directoryReady).isTrue()

        h.viewModel.onWorkspaceLabelChange("lerdr")
        h.viewModel.submitWorkspace()
        h.pump()
        val sent = h.sentFrames().single {
            it["type"]?.jsonPrimitive?.content == "workspace_create"
        }
        assertThat(sent["cwd"]?.jsonPrimitive?.content).isEqualTo("/home/u")
        assertThat(sent["label"]?.jsonPrimitive?.content).isEqualTo("lerdr")
        h.answer("workspace_create")
        assertThat(h.messages).contains("Created workspace lerdr.")
        assertThat(h.events).contains(LaunchViewModel.LaunchEvent.Dismissed)
    }

    @Test
    fun `reader device sees readOnly and cannot submit`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady(role = DeviceRole.READER)
        h.viewModel.beginWorkspace()
        h.pump()
        h.answerDirectory()
        h.viewModel.onWorkspaceLabelChange("lerdr")
        h.viewModel.submitWorkspace()
        h.pump()
        assertThat(h.viewModel.uiState.value.readOnly).isTrue()
        assertThat(
            h.sentFrames().map { it["type"]?.jsonPrimitive?.content },
        ).doesNotContain("workspace_create")
    }

    @Test
    fun `new agent submit sends agent_start with the launch fields`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        h.viewModel.beginAgent()
        h.pump()
        h.answerDirectory()
        val state = h.viewModel.uiState.value
        assertThat(state.relayId).isEqualTo("r1")
        assertThat(state.profileId).isEqualTo("claude")
        assertThat(state.readOnly).isFalse()

        h.viewModel.onNameChange("u-claude")
        h.viewModel.onPromptChange("fix the flaky test")
        h.viewModel.submitAgent()
        h.pump()
        val sent = h.sentFrames().single {
            it["type"]?.jsonPrimitive?.content == "agent_start"
        }
        assertThat(sent["profile_id"]?.jsonPrimitive?.content).isEqualTo("claude")
        assertThat(sent["name"]?.jsonPrimitive?.content).isEqualTo("u-claude")
        assertThat(sent["cwd"]?.jsonPrimitive?.content).isEqualTo("/home/u")
        assertThat(sent["prompt"]?.jsonPrimitive?.content)
            .isEqualTo("fix the flaky test")
        h.answer("agent_start", """{"pane_id":"%9"}""")
        assertThat(h.messages).contains("Agent started.")
        assertThat(h.events).contains(LaunchViewModel.LaunchEvent.Dismissed)
    }

    @Test
    fun `directory browser reports capability gaps instead of loading`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        h.handle().emit(
            launchJson(
                """{"type":"push_config","capabilities":[],"inventory":{"state":"ready"}}""",
            ),
        )
        h.pump()
        h.viewModel.beginWorkspace()
        h.pump()
        h.viewModel.openDirectoryBrowser()
        val directory = h.viewModel.uiState.value.directory
        assertThat(directory.supported).isFalse()
        assertThat(directory.loading).isFalse()
        assertThat(
            h.sentFrames().map { it["type"]?.jsonPrimitive?.content },
        ).doesNotContain("list_directories")
    }
}
