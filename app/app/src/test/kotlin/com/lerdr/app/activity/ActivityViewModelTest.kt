package com.lerdr.app.activity

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
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonPrimitive
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

private fun json(raw: String): JsonObject =
    LerdrJson.parseToJsonElement(raw) as JsonObject

@OptIn(ExperimentalCoroutinesApi::class)
class ActivityViewModelTest {

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
        var clock = 1_000_000L
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
        val journal = ActivityJournal(scope, repository, now = { clock })

        val endpoint = RelayEndpoint(
            id = "r1",
            label = "workstation",
            host = "192.168.1.5",
            port = 7474,
            transport = RelayTransport.WEBSOCKET,
        )
        val endpoint2 = RelayEndpoint(
            id = "r2",
            label = "lab",
            host = "192.168.1.9",
            port = 7474,
            transport = RelayTransport.WEBSOCKET,
        )
        val origin = "ws://192.168.1.5:7474"

        fun pump() = testScope.runCurrent()

        fun handle(): FakeRelaySessionHandle =
            factory.handleFor(origin) ?: error("no session for $origin")

        fun viewModel(): ActivityViewModel =
            ActivityViewModel(repository, journal, now = { clock })

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
    fun `journal lifecycle events and relay actions merge newest-first`() = runTest {
        val h = Harness(this, tmp.root)
        h.pump()
        // Registry membership is what resolves the relay's display label.
        h.registry.upsert(h.endpoint)
        h.await {
            h.journal.events.value.any { it.kind == ActivityJournal.Kind.RELAY_ADDED }
        }
        h.repository.connect(h.endpoint)
        h.pump()
        h.handle().connect()
        h.pump()

        val viewModel = h.viewModel()
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.pump()

        // Relay-reported action with an older timestamp lands below the
        // fresh local CONNECTED event.
        h.handle().emit(
            json(
                """{"type":"activity","activity":{"id":"a1","timestamp":1000,"kind":"send_input","summary":"Submitted text","pane_id":"%1"}}""",
            ),
        )
        h.pump()

        val items = viewModel.uiState.value.items
        assertThat(items.map { it.kind }).containsExactly(
            ActivityItemKind.CONNECTED,
            ActivityItemKind.CONNECTING,
            ActivityItemKind.RELAY_ADDED,
            ActivityItemKind.ACTION,
        ).inOrder()
        val action = items.last()
        assertThat(action.headline).isEqualTo("Submitted text")
        assertThat(action.relayLabel).isEqualTo("workstation")
        assertThat(action.detail).contains("send_input")
    }

    @Test
    fun `init fans get_activity out to live sessions`() = runTest {
        val h = Harness(this, tmp.root)
        h.pump()
        h.repository.connect(h.endpoint)
        h.pump()
        h.handle().connect()
        h.pump()

        h.viewModel()
        val sent = h.handle().sentRaw
            .map { json(it)["type"]?.jsonPrimitive?.content }
        assertThat(sent).contains("get_activity")
    }

    @Test
    fun `relay filter narrows the merged list and All restores it`() = runTest {
        val h = Harness(this, tmp.root)
        h.pump()
        h.registry.upsert(h.endpoint)
        h.registry.upsert(h.endpoint2)
        h.await { h.journal.events.value.size >= 2 }
        h.journal.record(
            ActivityJournal.Kind.CONNECTED,
            relayId = "r1",
            relayLabel = "workstation",
        )
        h.journal.record(
            ActivityJournal.Kind.CONNECTED,
            relayId = "r2",
            relayLabel = "lab",
        )

        val viewModel = h.viewModel()
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.pump()

        var state = viewModel.uiState.value
        assertThat(state.filters.map { it.relayId }).containsExactly("r1", "r2")

        viewModel.selectRelayFilter("r1")
        h.pump()
        state = viewModel.uiState.value
        assertThat(state.selectedFilter).isEqualTo("r1")
        assertThat(state.items.map { it.relayId }.distinct()).containsExactly("r1")

        viewModel.selectRelayFilter(null)
        h.pump()
        state = viewModel.uiState.value
        assertThat(state.selectedFilter).isNull()
        assertThat(state.items.map { it.relayId }.distinct())
            .containsExactly("r1", "r2")
    }

    @Test
    fun `a filter pointing at a forgotten relay falls back to All`() = runTest {
        val h = Harness(this, tmp.root)
        h.pump()
        h.registry.upsert(h.endpoint)
        h.await {
            h.journal.events.value.any { it.kind == ActivityJournal.Kind.RELAY_ADDED }
        }

        val viewModel = h.viewModel()
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.pump()
        viewModel.selectRelayFilter("r1")
        h.pump()
        assertThat(viewModel.uiState.value.selectedFilter).isEqualTo("r1")

        // Forget the relay — the dead filter must not blank the journal.
        h.repository.removeRelay("r1")
        h.await {
            viewModel.uiState.value.selectedFilter == null
        }
        assertThat(viewModel.uiState.value.items).isNotEmpty()
    }
}
