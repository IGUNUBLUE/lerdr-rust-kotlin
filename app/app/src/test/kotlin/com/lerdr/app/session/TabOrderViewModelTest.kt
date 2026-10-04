package com.lerdr.app.session

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import com.google.common.truth.Truth.assertThat
import java.io.File
import kotlinx.coroutines.CompletableDeferred
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
import lerdr.core.data.DeviceRole
import lerdr.core.data.RelayDeviceCredential
import lerdr.core.data.RelayEndpoint
import lerdr.core.data.RelayRegistry
import lerdr.core.model.CommandResultMessage
import lerdr.core.protocol.LerdrJson
import lerdr.core.store.AgentStore
import lerdr.core.store.ConnectionStore
import lerdr.core.store.WorkspaceStore
import lerdr.core.transport.CommandException
import org.junit.After
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

@OptIn(ExperimentalCoroutinesApi::class)
class TabOrderViewModelTest {
    @get:Rule val tmp = TemporaryFolder()
    private val mainDispatcher = UnconfinedTestDispatcher()
    @Before fun setMain() = Dispatchers.setMain(mainDispatcher)
    @After fun resetMain() = Dispatchers.resetMain()

    private class Harness(private val testScope: TestScope, folder: File) {
        private val scope = testScope.backgroundScope
        private val credentials = FakeCredentialStore()
        private val registry = RelayRegistry(
            PreferenceDataStoreFactory.create(scope = scope) { File(folder, "relays.preferences_pb") },
            scope,
        )
        private val workspaces = WorkspaceStore()
        private val factory = FakeRelaySessionFactory(scope)
        val repository = SessionRepository(
            scope, credentials, registry, AgentStore(scope), workspaces,
            ConnectionStore(clock = { 0L }), factory,
        )
        private val endpoint = RelayEndpoint(
            id = "r1", label = "test", host = "192.168.1.5", port = 7474,
            transport = lerdr.core.data.RelayTransport.WEBSOCKET,
        )
        val handle: FakeRelaySessionHandle get() = factory.handleFor("ws://192.168.1.5:7474")!!
        lateinit var vm: TabOrderViewModel

        private suspend fun emit(raw: String) {
            handle.emit(LerdrJson.parseToJsonElement(raw) as JsonObject)
            testScope.runCurrent()
        }

        suspend fun start(role: DeviceRole = DeviceRole.CONTROLLER, capability: Boolean = true) {
            registry.upsert(endpoint)
            credentials.seed("r1", RelayDeviceCredential(
                id = "cred", version = 1,
                secret = java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(ByteArray(32)),
                deviceId = "dev", role = role, locale = "en", issuedAtEpochMs = 0,
            ))
            repository.start()
            repository.connect(endpoint)
            handle.connect()
            capabilities(capability)
            emit("""{"type":"workspaces","workspaces":[{"workspace_id":"w1","tab_count":3},{"workspace_id":"other","tab_count":9}]}""")
            position(2)
            vm = TabOrderViewModel("r1::%1", repository, workspaces)
            testScope.backgroundScope.launch { vm.uiState.collect {} }
            testScope.runCurrent()
        }

        suspend fun capabilities(enabled: Boolean) = emit(
            """{"type":"push_config","capabilities":[${if (enabled) "\"tab_reorder\"" else ""}],"inventory":{"state":"ready"}}""",
        )

        // Only one agent represents a three-tab workspace: missing agent cards must
        // not invent a one-tab strip or prevent moving past a non-agent tab.
        suspend fun position(ordinal: Int) = emit(
            """{"type":"agents","agents":[{"pane_id":"%1","raw_pane_id":"%1","terminal_id":"terminal","server_session_id":"session","generation":1,"workspace_id":"w1","tab_id":"tab1","tab_order":$ordinal,"tab_number":42,"agent":"shell","status":"idle"}]}""",
        )
    }

    @Test
    fun `boundaries use workspace topology not agent count or tab number`() = runTest {
        val h = Harness(this, tmp.root)
        h.start()
        assertThat(h.vm.uiState.value.position).isEqualTo(2)
        assertThat(h.vm.uiState.value.tabCount).isEqualTo(3)
        h.position(1)
        h.vm.moveLeft()
        assertThat(h.handle.requests).isEmpty()
        h.vm.moveRight()
        runCurrent()
        h.position(3)
        h.vm.moveRight()
        assertThat(h.handle.requests).hasSize(1)
    }

    @Test
    fun `moves both directions across pre-removal insertion boundaries`() = runTest {
        val h = Harness(this, tmp.root)
        h.start()
        val tabs = mutableListOf("other-left", "tab1", "other-right")
        h.handle.responder = { command ->
            // Herdr resolves the insertion boundary in the original strip,
            // then removes the source tab before inserting it.
            val source = tabs.indexOf("tab1")
            val boundary = requireNotNull(command.insertIndex)
            tabs.removeAt(source)
            tabs.add(if (boundary > source) boundary - 1 else boundary, "tab1")
            h.position(tabs.indexOf("tab1") + 1)
            CommandResultMessage(ok = true, phase = "confirmed")
        }

        h.vm.moveLeft()
        runCurrent()
        assertThat(h.vm.uiState.value.position).isEqualTo(1)
        assertThat(h.vm.uiState.value.canMoveLeft).isFalse()
        assertThat(h.vm.uiState.value.busy).isFalse()

        h.vm.moveRight()
        runCurrent()
        assertThat(h.vm.uiState.value.position).isEqualTo(2)
        assertThat(h.vm.uiState.value.busy).isFalse()

        h.vm.moveRight()
        runCurrent()
        assertThat(h.vm.uiState.value.position).isEqualTo(3)
        assertThat(h.vm.uiState.value.canMoveRight).isFalse()
        assertThat(h.vm.uiState.value.busy).isFalse()

        h.vm.moveLeft()
        runCurrent()
        assertThat(h.vm.uiState.value.position).isEqualTo(2)
        assertThat(h.vm.uiState.value.busy).isFalse()
    }

    @Test
    fun `reader pairing prevents tab mutation dispatch`() = runTest {
        val h = Harness(this, tmp.root)
        h.start(role = DeviceRole.READER)
        assertThat(h.vm.uiState.value.available).isFalse()
        h.vm.moveLeft()
        h.vm.moveRight()
        assertThat(h.handle.requests).isEmpty()
    }

    @Test
    fun `capability revocation and unknown ordinal hide the action`() = runTest {
        val h = Harness(this, tmp.root)
        h.start(capability = false)
        assertThat(h.vm.uiState.value.available).isFalse()
        h.vm.moveLeft()
        h.capabilities(true)
        assertThat(h.vm.uiState.value.available).isTrue()
        h.capabilities(false)
        h.vm.moveRight()
        assertThat(h.handle.requests).isEmpty()
        h.capabilities(true)
        h.position(4)
        assertThat(h.vm.uiState.value.available).isFalse()
        h.vm.moveLeft()
        assertThat(h.handle.requests).isEmpty()
    }

    @Test
    fun `pending command and confirmed receipt wait for authoritative order without duplicate moves`() = runTest {
        val h = Harness(this, tmp.root)
        h.start()
        val receipt = CompletableDeferred<CommandResultMessage>()
        h.handle.responder = { receipt.await() }
        h.vm.moveRight()
        h.vm.moveRight()
        assertThat(h.handle.requests).hasSize(1)
        assertThat(h.vm.uiState.value.busy).isTrue()
        receipt.complete(CommandResultMessage(ok = true, phase = "confirmed"))
        runCurrent()
        assertThat(h.vm.uiState.value.position).isEqualTo(2)
        assertThat(h.vm.uiState.value.waitingForOrder).isTrue()
        h.vm.consumeMessage()
        h.vm.moveLeft()
        assertThat(h.handle.requests).hasSize(1)
        h.position(3)
        assertThat(h.vm.uiState.value.busy).isFalse()
        assertThat(h.vm.uiState.value.position).isEqualTo(3)
        assertThat(h.vm.uiState.value.canMoveRight).isFalse()
        assertThat(h.vm.uiState.value.message).isNull()
        // A later host/controller move back is not a new pending app request.
        h.position(2)
        assertThat(h.vm.uiState.value.waitingForOrder).isFalse()
        assertThat(h.vm.uiState.value.canMoveRight).isTrue()
        h.vm.moveRight()
        runCurrent()
        assertThat(h.handle.requests).hasSize(2)
    }

    @Test
    fun `refusal leaves authoritative position and truthful error with retry enabled`() = runTest {
        val h = Harness(this, tmp.root)
        h.start()
        h.handle.responder = { throw CommandException("Tab is unavailable") }
        h.vm.moveLeft()
        runCurrent()
        assertThat(h.vm.uiState.value.position).isEqualTo(2)
        assertThat(h.vm.uiState.value.busy).isFalse()
        assertThat(h.vm.uiState.value.canMoveLeft).isTrue()
    }
}
