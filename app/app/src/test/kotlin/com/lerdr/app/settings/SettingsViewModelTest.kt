package com.lerdr.app.settings

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import com.google.common.truth.Truth.assertThat
import com.lerdr.app.session.FakeCredentialStore
import com.lerdr.app.session.FakeRelaySessionFactory
import com.lerdr.app.session.FakeRelaySessionHandle
import com.lerdr.app.session.SessionRepository
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
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.JsonObject
import lerdr.core.data.RelayEndpoint
import lerdr.core.data.RelayInvitation
import lerdr.core.data.RelayRegistry
import lerdr.core.data.RelayTransport
import lerdr.core.store.AgentStore
import lerdr.core.store.ConnectionStore
import lerdr.core.store.WorkspaceStore
import lerdr.core.protocol.LerdrJson
import org.junit.After
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

private fun json(raw: String): JsonObject =
    LerdrJson.parseToJsonElement(raw) as JsonObject

@OptIn(ExperimentalCoroutinesApi::class)
class SettingsViewModelTest {

    @get:Rule
    val tmp = TemporaryFolder()

    private val mainDispatcher = UnconfinedTestDispatcher()

    /** viewModelScope rides Dispatchers.Main — redirect it into the test. */
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
            File(tmpDir, "app.preferences_pb")
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
        val preferences = AppPreferences(dataStore)

        val endpoint = RelayEndpoint(
            id = "r1",
            label = "workstation",
            host = "192.168.1.5",
            port = 7474,
            transport = RelayTransport.WEBSOCKET,
        )
        val origin = "ws://192.168.1.5:7474"

        fun pump() = testScope.runCurrent()

        fun handle(): FakeRelaySessionHandle =
            factory.handleFor(origin) ?: error("no session for $origin")

        fun viewModel() = SettingsViewModel(repository, preferences)

        /** Poll on a real clock — DataStore IO is off the test scheduler. */
        fun await(condition: () -> Boolean) {
            val deadline = System.currentTimeMillis() + 5_000
            while (!condition() && System.currentTimeMillis() < deadline) {
                testScope.runCurrent()
                Thread.sleep(5)
            }
            check(condition()) { "condition not met within deadline" }
        }
    }

    @Test
    fun `relay row reflects offline then connected state with detail`() = runTest {
        val h = Harness(this, tmp.root)
        val viewModel = h.viewModel()
        backgroundScope.launch { viewModel.uiState.collect { } }

        h.registry.upsert(h.endpoint)
        h.await { viewModel.uiState.value.relays.isNotEmpty() }
        var row = viewModel.uiState.value.relays.single()
        assertThat(row.statusLabel).isEqualTo("offline")
        assertThat(row.connected).isFalse()
        assertThat(row.canReconnect).isTrue()

        h.repository.connect(h.endpoint)
        h.pump()
        assertThat(viewModel.uiState.value.relays.single().statusLabel)
            .isEqualTo("connecting…")

        h.handle().connect()
        h.handle().emit(
            json(
                """{"type":"push_config","version":"0.4.2","protocol":3,"inventory":{"state":"ready"}}""",
            ),
        )
        h.pump()
        row = viewModel.uiState.value.relays.single()
        assertThat(row.statusLabel).isEqualTo("connected")
        assertThat(row.connected).isTrue()
        assertThat(row.detailLabel).contains("websocket")
        assertThat(row.detailLabel).contains("0.4.2")
        assertThat(row.detailLabel).contains("protocol 3")
    }

    @Test
    fun `auth-rejected row explains re-pairing and drops reconnect`() = runTest {
        val h = Harness(this, tmp.root)
        val viewModel = h.viewModel()
        backgroundScope.launch { viewModel.uiState.collect { } }

        h.registry.upsert(h.endpoint)
        h.await { viewModel.uiState.value.relays.isNotEmpty() }
        h.repository.connect(h.endpoint)
        h.handle().rejectAuth()
        h.pump()

        val row = viewModel.uiState.value.relays.single()
        assertThat(row.authRejected).isTrue()
        assertThat(row.canReconnect).isFalse()
        assertThat(row.statusLabel).contains("rejected")
    }

    @Test
    fun `reconnect dials a registry relay that has no session yet`() = runTest {
        val h = Harness(this, tmp.root)
        val viewModel = h.viewModel()
        backgroundScope.launch { viewModel.uiState.collect { } }

        // Repository not started — registry row with no session runtime.
        h.registry.upsert(h.endpoint)
        h.await { viewModel.uiState.value.relays.isNotEmpty() }
        assertThat(h.factory.created).isEmpty()

        viewModel.reconnectRelay("r1")
        h.pump()
        assertThat(h.factory.created).containsKey("ws://192.168.1.5:7474/ws")
        assertThat(viewModel.uiState.value.relays.single().statusLabel)
            .isEqualTo("connecting…")
    }

    @Test
    fun `reconnect on a live session runs the revalidate probe`() = runTest {
        val h = Harness(this, tmp.root)
        // reconnectRelay resolves the endpoint from the registry, like the UI.
        h.registry.upsert(h.endpoint)
        h.await { h.registry.relays.value.isNotEmpty() }
        h.repository.connect(h.endpoint)
        h.pump()

        val viewModel = h.viewModel()
        viewModel.reconnectRelay("r1")
        assertThat(h.handle().revalidateCount).isEqualTo(1)

        viewModel.revalidateAll()
        assertThat(h.handle().revalidateCount).isEqualTo(2)
    }

    @Test
    fun `forget drops the registry entry, credential, and live session`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.seed("r1", RelayInvitation(ByteArray(32)))
        h.repository.start()
        h.registry.upsert(h.endpoint)
        h.await { h.factory.handleFor(h.origin) != null }
        val handle = h.handle()

        val viewModel = h.viewModel()
        viewModel.forgetRelay("r1")
        h.await { handle.closed }

        assertThat(h.registry.snapshot()).isEmpty()
        assertThat(h.credentials.get("r1")).isNull()
    }

    @Test
    fun `reconnecting an unknown relay is a no-op`() = runTest {
        val h = Harness(this, tmp.root)
        val viewModel = h.viewModel()
        viewModel.reconnectRelay("ghost")
        h.pump()
        assertThat(h.factory.created).isEmpty()
    }

    @Test
    fun `theme intent persists and flows back into ui state`() = runTest {
        val h = Harness(this, tmp.root)
        val viewModel = h.viewModel()
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.await { viewModel.uiState.value.themeMode == ThemeMode.SYSTEM }

        viewModel.setThemeMode(ThemeMode.DARK)
        h.await { viewModel.uiState.value.themeMode == ThemeMode.DARK }
        assertThat(h.preferences.themeMode.first()).isEqualTo(ThemeMode.DARK)
    }

    @Test
    fun `app lock intent persists and flows back into ui state`() = runTest {
        val h = Harness(this, tmp.root)
        val viewModel = h.viewModel()
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.await { !viewModel.uiState.value.appLockEnabled }

        viewModel.setAppLockEnabled(true)
        h.await { viewModel.uiState.value.appLockEnabled }
        assertThat(h.preferences.appLockEnabled.first()).isTrue()
    }
}
