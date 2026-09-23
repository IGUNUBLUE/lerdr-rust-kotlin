package com.lerdr.app.home

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import com.google.common.truth.Truth.assertThat
import com.lerdr.app.session.FakeCredentialStore
import com.lerdr.app.session.FakeRelaySessionFactory
import com.lerdr.app.session.FakeRelaySessionHandle
import com.lerdr.app.session.SessionRepository
import java.io.File
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import lerdr.core.data.DeviceRole
import lerdr.core.data.RelayDeviceCredential
import lerdr.core.data.RelayEndpoint
import lerdr.core.data.RelayRegistry
import lerdr.core.data.RelayTransport
import lerdr.core.model.AgentState
import lerdr.core.model.AgentsMessage
import lerdr.core.model.Interaction
import lerdr.core.model.InventoryState
import lerdr.core.model.Option
import lerdr.core.model.Other
import lerdr.core.model.PushConfigMessage
import lerdr.core.model.WireField
import lerdr.core.model.WorkspaceInfo
import lerdr.core.model.WorkspacesMessage
import lerdr.core.store.AgentStore
import lerdr.core.store.ConnectionStore
import lerdr.core.store.StoreReducer
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
        private val testScope = testScope
        private val scope = testScope.backgroundScope
        private val credentials = FakeCredentialStore()
        private val dataStore = PreferenceDataStoreFactory.create(scope = scope) {
            File(tmpDir, "relays.preferences_pb")
        }
        val registry = RelayRegistry(dataStore, scope)
        val agents = AgentStore(scope)
        val connections = ConnectionStore(clock = { 1_000L })
        val workspaces = WorkspaceStore()
        val factory = FakeRelaySessionFactory(scope)
        val sessions = SessionRepository(
            scope = scope,
            credentialStore = credentials,
            relayRegistry = registry,
            agentStore = agents,
            workspaceStore = workspaces,
            connectionStore = connections,
            sessionFactory = factory,
        )
        // Same `relayLabel` lookup the repository's own reducer uses.
        val reducer = StoreReducer(
            agentStore = agents,
            workspaceStore = workspaces,
            connectionStore = connections,
            relayLabel = { relayId ->
                registry.relays.value.firstOrNull { it.id == relayId }?.label
                    ?: relayId
            },
        )
        val repository = RealHomeRepository(
            agentStore = agents,
            connectionStore = connections,
            relayRegistry = registry,
            sessions = sessions,
            workspaceStore = workspaces,
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
        private val origin = "ws://192.168.1.5:7474"

        /**
         * Poll on a real clock — the registry→reconcile→connect chain rides
         * a DataStore IO write that is off the test scheduler.
         */
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

        /**
         * Enroll this device, then bring the relay online through the real
         * reconcile path — the registry upsert creates the session and the
         * fake handle's `connect()` lands the CONNECTED transport status.
         * Store frames (`push_config`, `agents`, `workspaces`) still ride
         * the reducer seam for determinism.
         */
        suspend fun online(role: DeviceRole? = DeviceRole.CONTROLLER) {
            if (role != null) credentials.seed("r1", credential(role))
            sessions.start()
            testScope.runCurrent()
            registry.upsert(endpoint)
            val handle = awaitHandle()
            handle.connect()
            reducer.handle(
                "r1",
                PushConfigMessage(
                    capabilities = WireField.Present(listOf("attention_classification")),
                    inventory = WireField.Present(InventoryState(state = "ready")),
                ),
            )
            testScope.runCurrent()
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

        fun workspaces(vararg infos: WorkspaceInfo) {
            reducer.handle("r1", WorkspacesMessage(workspaces = infos.toList()))
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
        assertThat(state.relays.single().rttMs).isEqualTo(-1)
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
        val group = state.working.single()
        assertThat(group.agents.single().paneId).isEqualTo(clientPaneId("r1", "%1"))
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
        assertThat(card.controllable).isTrue()
        assertThat(card.responding).isFalse()
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
        assertThat(state.idle.single().agents.single().paneId)
            .isEqualTo(clientPaneId("r1", "%3"))
    }

    @Test
    fun `agents group by workspace id and prefer the snapshot label`() = runTest {
        val h = Harness(this, tmp.root)
        h.online()
        h.workspaces(
            WorkspaceInfo(workspaceId = "w1", label = "api-server"),
            WorkspaceInfo(workspaceId = "w2", label = "dotfiles"),
        )
        h.agent("%1", "working") { copy(workspaceId = "w1") }
        h.agent("%2", "working") { copy(workspaceId = "w2", project = "other") }
        val state = h.repository.uiState.first()
        assertThat(state.working).hasSize(2)
        assertThat(state.working.map { it.label })
            .containsExactly("api-server", "dotfiles")
        assertThat(state.working.map { it.relayLabel }.distinct())
            .containsExactly("workstation")
    }

    @Test
    fun `agents without workspace id group by cwd basename`() = runTest {
        val h = Harness(this, tmp.root)
        h.online()
        h.agent("%1", "working") { copy(cwd = "/home/u/lerdr", project = "") }
        h.agent("%2", "working") {
            copy(cwd = "/home/u/lerdr", project = "", name = "pi")
        }
        val state = h.repository.uiState.first()
        val group = state.working.single()
        assertThat(group.label).isEqualTo("lerdr")
        assertThat(group.agents).hasSize(2)
    }

    @Test
    fun `markResponding flips the card into the waiting state`() = runTest {
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
        h.agents.markResponding(clientPaneId("r1", "%2"))
        val state = h.repository.uiState.first { state ->
            state.needsYou.singleOrNull()?.responding == true
        }
        assertThat(state.needsYou.single().responding).isTrue()
    }

    @Test
    fun `reader credential hides mutating affordances`() = runTest {
        val h = Harness(this, tmp.root)
        h.online(role = DeviceRole.READER)
        h.agent("%2", "blocked") {
            copy(
                attentionKind = "approval",
                prompt = "Run tests?",
                options = listOf("Yes", "No"),
                blockedEventId = "ev1",
            )
        }
        h.agent("%3", "working")
        val state = h.repository.uiState.first()
        assertThat(state.needsYou.single().controllable).isFalse()
        assertThat(state.working.single().agents.single().controllable).isFalse()
    }

    @Test
    fun `single_select question projects a quick-answerable interaction`() = runTest {
        val h = Harness(this, tmp.root)
        h.online()
        h.agent("%4", "blocked") {
            copy(
                attentionKind = "question",
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
            )
        }
        val card = h.repository.uiState.first().needsYou.single()
        assertThat(card.kind).isEqualTo(AttentionKind.QUESTION)
        assertThat(card.quickOptions.map { it.label })
            .containsExactly("store", "session").inOrder()
        assertThat(card.chooseLabel).isNull()
    }

    @Test
    fun `multi-question interaction falls back to the full-form label`() = runTest {
        val h = Harness(this, tmp.root)
        h.online()
        h.agent("%4", "blocked") {
            copy(
                attentionKind = "question",
                interaction = Interaction(
                    id = "q1",
                    kind = "single_select",
                    question = "Pick a module",
                    options = listOf(Option(index = 0, label = "store")),
                    other = Other(hidden = true),
                    questionIndex = 0,
                    questionTotal = 3,
                ),
            )
        }
        val card = h.repository.uiState.first().needsYou.single()
        assertThat(card.quickOptions).isEmpty()
        assertThat(card.chooseLabel).isEqualTo("Choose answer (1)")
    }

    @Test
    fun `connected relay carries its measured rtt onto the card`() = runTest {
        val h = Harness(this, tmp.root)
        h.online()
        h.connections.noteRtt("r1", 87)
        val state = h.repository.uiState.first { it.relays.single().rttMs == 87L }
        assertThat(state.relays.single().statusLabel).isEqualTo("87ms")
    }
}
