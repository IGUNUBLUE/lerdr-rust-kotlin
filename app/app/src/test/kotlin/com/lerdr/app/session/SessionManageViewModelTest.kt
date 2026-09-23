package com.lerdr.app.session

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import com.google.common.truth.Truth.assertThat
import com.lerdr.app.session.manage.ManageConfirm
import com.lerdr.app.session.manage.ManageViewModel
import com.lerdr.app.session.manage.sessionNameOf
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
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
import lerdr.core.model.CommandResultMessage
import lerdr.core.protocol.LerdrJson
import lerdr.core.store.AgentStore
import lerdr.core.store.ConnectionStore
import lerdr.core.store.WorkspaceStore
import org.junit.After
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

private fun json(raw: String): JsonObject =
    LerdrJson.parseToJsonElement(raw) as JsonObject

@OptIn(ExperimentalCoroutinesApi::class)
class SessionManageViewModelTest {

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

        val endpoint = RelayEndpoint(
            id = "r1",
            label = "workstation",
            host = "192.168.1.5",
            port = 7474,
            transport = lerdr.core.data.RelayTransport.WEBSOCKET,
        )
        val paneId = "r1::%1"

        fun pump() = testScope.runCurrent()

        fun handle(): FakeRelaySessionHandle =
            factory.handleFor("ws://192.168.1.5:7474") ?: error("no session")

        fun credential(role: DeviceRole) = RelayDeviceCredential(
            id = "cred-1",
            version = 1,
            secret = java.util.Base64.getUrlEncoder().withoutPadding()
                .encodeToString(ByteArray(32) { it.toByte() }),
            deviceId = "dev-1",
            role = role,
            locale = "en",
            issuedAtEpochMs = 1_000L,
        )

        /** Connected session, ready inventory, one agent + one workspace. */
        suspend fun connectReady(
            role: DeviceRole = DeviceRole.CONTROLLER,
            inventoryState: String = "ready",
        ) {
            // Registry first — `start()`'s reconcile tears down sessions
            // whose endpoint is not registered.
            registry.upsert(endpoint)
            credentials.seed("r1", credential(role))
            repository.start()
            repository.connect(endpoint)
            handle().connect()
            handle().emit(
                json(
                    """{"type":"push_config","capabilities":["workspace_management","directory_browser","worktree_management","tab_reorder"],"inventory":{"state":"$inventoryState"}}""",
                ),
            )
            handle().emit(
                json(
                    """{"type":"agents","agents":[{"pane_id":"%1","raw_pane_id":"%1","terminal_id":"t1","server_session_id":"ss1","generation":3,"agent":"claude","name":"claude","status":"working","cwd":"/home/u/lerdr","project":"lerdr","session_name":"Fix the login bug","workspace_id":"w1","tab_id":"tabA","updated_at":100}]}""",
                ),
            )
            handle().emit(
                json(
                    """{"type":"workspaces","workspaces":[{"workspace_id":"w1","number":1,"label":"lerdr","focused":true,"pane_count":1,"tab_count":1,"active_tab_id":"tabA","agent_status":"working","cwd":"/home/u/lerdr"}]}""",
                ),
            )
            pump()
        }

        fun viewModel(): ManageViewModel =
            ManageViewModel(paneId, repository, workspaces).also {
                testScope.backgroundScope.launch { it.uiState.collect { } }
            }
    }

    // ── state ─────────────────────────────────────────────────────────

    @Test
    fun `agent + workspace rows map into the metadata block`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        h.pump()

        val state = vm.uiState.value
        assertThat(state.title).isEqualTo("lerdr") // project wins
        assertThat(state.provider).isEqualTo("claude")
        assertThat(state.relayLabel).isEqualTo("workstation")
        assertThat(state.rawPaneId).isEqualTo("%1")
        assertThat(state.cwd).isEqualTo("/home/u/lerdr")
        assertThat(state.workspaceLabel).isEqualTo("lerdr")
        assertThat(state.sessionName).isEqualTo("Fix the login bug")
        assertThat(state.canControl).isTrue()
        assertThat(state.nameDraft).isEqualTo("lerdr")
        assertThat(state.nameDirty).isFalse()
    }

    @Test
    fun `a reader pairing gates every mutation`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady(role = DeviceRole.READER)
        val vm = h.viewModel()
        h.pump()

        assertThat(vm.uiState.value.canControl).isFalse()

        vm.onNameDraftChange("renamed")
        vm.saveRename()
        vm.restart()
        vm.beginConfirm(ManageConfirm.CLEAR)
        vm.confirmAction()
        vm.copyResponse()
        h.pump()

        assertThat(h.handle().requests).isEmpty()
        assertThat(vm.uiState.value.confirming).isNull()
    }

    // ── rename ────────────────────────────────────────────────────────

    @Test
    fun `saveRename sends agent_rename and dismisses`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        h.pump()

        vm.onNameDraftChange("  renamed agent  ")
        assertThat(vm.uiState.value.nameDirty).isTrue()
        vm.saveRename()
        h.pump()

        val sent = h.handle().requests.last()
        assertThat(sent.type).isEqualTo("agent_rename")
        assertThat(sent.name).isEqualTo("renamed agent")
        assertThat(vm.uiState.value.shouldDismiss).isTrue()
        assertThat(vm.uiState.value.busy).isFalse()
    }

    @Test
    fun `an unchanged or blank name sends nothing`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        h.pump()

        vm.onNameDraftChange("lerdr") // unchanged
        vm.saveRename()
        vm.onNameDraftChange("   ") // blank
        vm.saveRename()
        h.pump()

        assertThat(h.handle().requests).isEmpty()
        assertThat(vm.uiState.value.status).isEqualTo("Enter a new name.")
        assertThat(vm.uiState.value.statusError).isTrue()
    }

    // ── restart / clear / stop / copy ─────────────────────────────────

    @Test
    fun `restart sends agent_restart with a status`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        h.pump()

        vm.restart()
        h.pump()

        assertThat(h.handle().requests.last().type).isEqualTo("agent_restart")
        val state = vm.uiState.value
        assertThat(state.status).isEqualTo("Restart requested.")
        assertThat(state.statusError).isFalse()
        assertThat(state.shouldDismiss).isFalse()
    }

    @Test
    fun `clear runs behind the confirm panel then dismisses`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        h.pump()

        vm.beginConfirm(ManageConfirm.CLEAR)
        h.pump()
        assertThat(vm.uiState.value.confirming).isEqualTo(ManageConfirm.CLEAR)

        vm.confirmAction()
        h.pump()

        assertThat(h.handle().requests.last().type).isEqualTo("agent_clear")
        val state = vm.uiState.value
        assertThat(state.confirming).isNull()
        assertThat(state.shouldDismiss).isTrue()
    }

    @Test
    fun `stop sends agent_stop and dismisses`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        h.pump()

        vm.beginConfirm(ManageConfirm.STOP)
        vm.confirmAction()
        h.pump()

        assertThat(h.handle().requests.last().type).isEqualTo("agent_stop")
        assertThat(vm.uiState.value.shouldDismiss).isTrue()
    }

    @Test
    fun `cancelConfirm restores the menu without a send`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        h.pump()

        vm.beginConfirm(ManageConfirm.STOP)
        vm.cancelConfirm()
        h.pump()

        assertThat(vm.uiState.value.confirming).isNull()
        assertThat(h.handle().requests).isEmpty()
    }

    @Test
    fun `copyResponse lands data text on the clipboard channel`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        h.pump()

        // The typed request path answers via the fake's responder.
        h.handle().responder = { message ->
            CommandResultMessage(
                action = message.type,
                ok = true,
                phase = CommandResultMessage.PHASE_COMPLETED,
                requestId = message.requestId,
                data = json("""{"text":"the rendered reply"}"""),
            )
        }
        vm.copyResponse()
        h.pump()

        assertThat(h.handle().requests.last().type).isEqualTo("copy_agent_response")
        val state = vm.uiState.value
        assertThat(state.clipboardText).isEqualTo("the rendered reply")
        assertThat(state.status).isEqualTo("Last response copied to the clipboard.")
        vm.consumeClipboard()
        h.pump()
        assertThat(vm.uiState.value.clipboardText).isNull()
    }

    @Test
    fun `copyResponse with no reply surfaces the empty status`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        h.pump()

        vm.copyResponse()
        h.pump()

        val state = vm.uiState.value
        assertThat(state.clipboardText).isNull()
        assertThat(state.status).isEqualTo("The agent has no response to copy.")
        assertThat(state.statusError).isTrue()
    }

    // ── gates ─────────────────────────────────────────────────────────

    @Test
    fun `a non-ready inventory refuses before the frame leaves`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady(inventoryState = "starting")
        val vm = h.viewModel()
        h.pump()

        vm.restart()
        h.pump()

        assertThat(h.handle().requests).isEmpty()
        assertThat(vm.uiState.value.status)
            .isEqualTo("Herdr agent inventory is not ready on this computer")
        assertThat(vm.uiState.value.statusError).isTrue()
    }

    @Test
    fun `a failed mutation surfaces the relay's message`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        h.pump()

        h.handle().responder = { message ->
            throw lerdr.core.transport.CommandException("Agent is busy")
        }
        vm.restart()
        h.pump()

        val state = vm.uiState.value
        assertThat(state.busy).isFalse()
        assertThat(state.status).isEqualTo("Agent is busy")
        assertThat(state.statusError).isTrue()
    }

    @Test
    fun `the agent row vanishing dismisses the sheet`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        h.pump()
        assertThat(vm.uiState.value.shouldDismiss).isFalse()

        h.handle().emit(json("""{"type":"agents","agents":[]}"""))
        h.pump()

        assertThat(vm.uiState.value.shouldDismiss).isTrue()
    }

    // ── pure helpers ──────────────────────────────────────────────────

    @Test
    fun `sessionNameOf prefers session_name and filters uuid sessions`() {
        val named = agentOf(sessionName = "Fix the bug")
        assertThat(sessionNameOf(named)).isEqualTo("Fix the bug")
        val uuid = agentOf(session = "1e1c2d3e-4f5a-6b7c-8d9e-0f1a2b3c4d5e")
        assertThat(sessionNameOf(uuid)).isEmpty()
        val path = agentOf(session = "/home/u/.config/sessions/abc")
        assertThat(sessionNameOf(path)).isEmpty()
        val legacy = agentOf(session = "morning-run")
        assertThat(sessionNameOf(legacy)).isEqualTo("morning-run")
    }

    private fun agentOf(
        sessionName: String? = null,
        session: String? = null,
    ) = lerdr.core.store.Agent(
        relayId = "r1",
        relayLabel = "workstation",
        rawPaneId = "%1",
        paneId = "r1::%1",
        session = session,
        sessionName = sessionName,
    )
}
