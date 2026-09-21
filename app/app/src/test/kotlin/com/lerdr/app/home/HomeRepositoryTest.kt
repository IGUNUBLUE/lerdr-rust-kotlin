package com.lerdr.app.home

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import com.google.common.truth.Truth.assertThat
import com.lerdr.app.session.FakeCredentialStore
import com.lerdr.app.session.FakeRelaySessionFactory
import com.lerdr.app.session.SessionRepository
import java.io.File
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runTest
import lerdr.core.data.RelayEndpoint
import lerdr.core.data.RelayRegistry
import lerdr.core.data.RelayTransport
import lerdr.core.model.AgentState
import lerdr.core.model.AgentsMessage
import lerdr.core.model.InventoryState
import lerdr.core.model.PushConfigMessage
import lerdr.core.model.WireField
import lerdr.core.store.AgentStore
import lerdr.core.store.ConnectionStore
import lerdr.core.store.RelayStatus
import lerdr.core.store.StoreReducer
import lerdr.core.store.TransportStatus
import lerdr.core.store.WorkspaceStore
import lerdr.core.store.clientPaneId
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

@OptIn(ExperimentalCoroutinesApi::class)
class HomeRepositoryTest {

    @get:Rule
    val tmp = TemporaryFolder()

    private class Harness(testScope: TestScope, tmpDir: File) {
        private val scope = testScope.backgroundScope
        private val credentials = FakeCredentialStore()
        private val dataStore = PreferenceDataStoreFactory.create(scope = scope) {
            File(tmpDir, "relays.preferences_pb")
        }
        val registry = RelayRegistry(dataStore, scope)
        val agents = AgentStore(scope)
        val connections = ConnectionStore(clock = { 1_000L })
        private val workspaces = WorkspaceStore()
        val sessions = SessionRepository(
            scope = scope,
            credentialStore = credentials,
            relayRegistry = registry,
            agentStore = agents,
            workspaceStore = workspaces,
            connectionStore = connections,
            sessionFactory = FakeRelaySessionFactory(scope),
        )
        val reducer = StoreReducer(agents, workspaces, connections)
        val repository = RealHomeRepository(
            agentStore = agents,
            connectionStore = connections,
            relayRegistry = registry,
            sessions = sessions,
            now = { 1_000_000L },
        )

        val endpoint = RelayEndpoint(
            id = "r1",
            label = "workstation",
            host = "192.168.1.5",
            port = 7474,
            transport = RelayTransport.WEBSOCKET,
        )

        private val agentRows = linkedMapOf<String, AgentState>()

        suspend fun online() {
            registry.upsert(endpoint)
            // relays rides a real DataStore write — await its emission.
            registry.relays.first { it.isNotEmpty() }
            connections.connect("r1", "workstation")
            connections.onTransportStatus("r1", TransportStatus.CONNECTED)
            reducer.handle(
                "r1",
                PushConfigMessage(
                    capabilities = WireField.Present(listOf("attention_classification")),
                    inventory = WireField.Present(InventoryState(state = "ready")),
                ),
            )
        }

        /** `agents` is a full-list snapshot — accumulate rows before emitting. */
        fun agent(
            rawPaneId: String,
            status: String,
            extra: AgentState.() -> AgentState = { this },
        ) {
            agentRows[rawPaneId] = AgentState(
                paneId = rawPaneId,
                rawPaneId = rawPaneId,
                agent = "claude",
                name = "claude",
                status = status,
                project = "lerdr",
                updatedAt = 900_000L,
                lastActiveAt = 900_000L,
            ).extra()
            reducer.handle("r1", AgentsMessage(agents = agentRows.values.toList()))
        }
    }

    @Test
    fun `empty state has no agents and an offline relay row`() = runTest {
        val h = Harness(this, tmp.root)
        h.registry.upsert(h.endpoint)
        h.registry.relays.first { it.isNotEmpty() }
        val state = h.repository.uiState.first()
        assertThat(state.live).isFalse()
        assertThat(state.needsYou).isEmpty()
        assertThat(state.working).isEmpty()
        assertThat(state.idle).isEmpty()
        assertThat(state.relays.single().relayId).isEqualTo("r1")
        assertThat(state.relays.single().statusLabel).isEqualTo("offline")
    }

    @Test
    fun `connected relay + working agent project live working row`() = runTest {
        val h = Harness(this, tmp.root)
        h.online()
        h.agent("%1", "working")
        val state = h.repository.uiState.first()
        assertThat(state.live).isTrue()
        assertThat(state.relays.single().connected).isTrue()
        assertThat(state.relays.single().statusLabel).isEqualTo("connected")
        assertThat(state.relays.single().agentCount).isEqualTo(1)
        assertThat(state.working.single().paneId).isEqualTo(clientPaneId("r1", "%1"))
        assertThat(state.idle).isEmpty()
        assertThat(state.needsYou).isEmpty()
    }

    @Test
    fun `blocked approval lands on the attention rail with options`() = runTest {
        val h = Harness(this, tmp.root)
        h.online()
        h.agent("%2", "blocked") {
            copy(
                attentionKind = "approval",
                prompt = "Run tests?",
                options = listOf("Yes", "No"),
                blockedEventId = "ev1",
            )
        }
        val state = h.repository.uiState.first()
        val card = state.needsYou.single()
        assertThat(card.kind).isEqualTo(AttentionKind.APPROVAL)
        assertThat(card.prompt).isEqualTo("Run tests?")
        assertThat(card.options).containsExactly("Yes", "No").inOrder()
        assertThat(state.working).isEmpty()
    }

    @Test
    fun `idle agents group separately from working`() = runTest {
        val h = Harness(this, tmp.root)
        h.online()
        h.agent("%1", "working")
        h.agent("%3", "idle")
        val state = h.repository.uiState.first()
        assertThat(state.working).hasSize(1)
        assertThat(state.idle.single().paneId).isEqualTo(clientPaneId("r1", "%3"))
    }
}
