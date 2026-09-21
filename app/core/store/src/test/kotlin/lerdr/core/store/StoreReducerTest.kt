package lerdr.core.store

import com.google.common.truth.Truth.assertThat
import kotlinx.coroutines.test.runTest
import lerdr.core.model.AgentState
import lerdr.core.model.AgentUpdateMessage
import lerdr.core.model.AgentsMessage
import lerdr.core.model.BlockedMessage
import lerdr.core.model.CommandResultMessage
import lerdr.core.model.Interaction
import lerdr.core.model.InventoryStatusMessage
import lerdr.core.model.Option
import lerdr.core.model.Other
import lerdr.core.model.PaneContentMessage
import lerdr.core.model.PaneProbeMessage
import lerdr.core.model.PushConfigMessage
import lerdr.core.model.WireField
import lerdr.core.model.WorkspaceInfo
import lerdr.core.model.WorkspacesMessage
import org.junit.Test

class StoreReducerTest {

    private class Harness(scope: kotlinx.coroutines.CoroutineScope) {
        val agents = AgentStore(scope)
        val workspaces = WorkspaceStore()
        val connections = ConnectionStore(clock = { 0L })
        val labels = mutableMapOf("r1" to "workstation")
        val reducer = StoreReducer(agents, workspaces, connections) { id ->
            labels[id] ?: "relay"
        }

        fun connect(relayId: String = "r1") {
            connections.connect(relayId, labels[relayId] ?: "relay")
            connections.onTransportStatus(
                relayId, TransportStatus.CONNECTED,
                TransportStatusDetail(path = TransportKind.WEBSOCKET),
            )
        }

        fun ready(relayId: String = "r1") {
            reducer.handle(
                relayId,
                PushConfigMessage(
                    capabilities = WireField.Present(listOf("attention_classification")),
                    inventory = WireField.Present(
                        lerdr.core.model.InventoryState(state = "ready"),
                    ),
                ),
            )
        }
    }

    private fun state(paneId: String, status: String = "working") = AgentState(
        paneId = paneId, rawPaneId = paneId, agent = "claude", status = status,
    )

    // ── inventory gating ─────────────────────────────────────────────

    @Test
    fun `agents snapshot is dropped while inventory is starting`() = runTest {
        val h = Harness(backgroundScope)
        h.connect()
        assertThat(
            h.reducer.handle("r1", AgentsMessage(agents = listOf(state("p1")))),
        ).isTrue()
        assertThat(h.agents.agents.value).isEmpty()
    }

    @Test
    fun `agents snapshot lands once inventory reports ready`() = runTest {
        val h = Harness(backgroundScope)
        h.connect()
        h.ready()
        h.reducer.handle("r1", AgentsMessage(agents = listOf(state("p1"))))
        assertThat(h.agents.agents.value.map { it.paneId }).containsExactly("r1::p1")
    }

    @Test
    fun `stale inventory still accepts snapshots`() = runTest {
        val h = Harness(backgroundScope)
        h.connect()
        h.reducer.handle(
            "r1", InventoryStatusMessage(state = "starting", stale = true),
        )
        h.reducer.handle("r1", AgentsMessage(agents = listOf(state("p1"))))
        assertThat(h.agents.agents.value.map { it.paneId }).containsExactly("r1::p1")
    }

    @Test
    fun `workspaces follow the same inventory gate`() = runTest {
        val h = Harness(backgroundScope)
        h.connect()
        h.reducer.handle(
            "r1", WorkspacesMessage(workspaces = listOf(WorkspaceInfo(workspaceId = "w1"))),
        )
        assertThat(h.workspaces.workspaces.value).isEmpty()
        h.ready()
        h.reducer.handle(
            "r1", WorkspacesMessage(workspaces = listOf(WorkspaceInfo(workspaceId = "w1"))),
        )
        assertThat(h.workspaces.workspaces.value.map { it.workspaceId })
            .containsExactly("w1")
        assertThat(h.workspaces.workspaces.value[0].relayLabel).isEqualTo("workstation")
    }

    // ── push_config cross-store effects ──────────────────────────────

    @Test
    fun `push_config resets pane revisions and renormalizes attention`() = runTest {
        val h = Harness(backgroundScope)
        h.connect()
        h.ready()
        h.reducer.handle(
            "r1",
            AgentsMessage(
                agents = listOf(
                    state("p1", status = "blocked").copy(
                        attentionKind = "approval", paneRevision = 9,
                        options = listOf("a", "b"), approvalFingerprint = "fp",
                    ),
                ),
            ),
        )
        assertThat(h.agents.agents.value[0].attentionKind).isEqualTo("approval")

        // Relay restart → new push_config without the capability.
        h.reducer.handle("r1", PushConfigMessage(capabilities = WireField.Present(emptyList())))
        val agent = h.agents.agents.value[0]
        assertThat(agent.paneRevision).isNull()
        assertThat(agent.attentionKind).isEqualTo("unknown")
        assertThat(agent.options).isNull()
    }

    @Test
    fun `push_config without a connection is dropped`() = runTest {
        val h = Harness(backgroundScope)
        assertThat(h.reducer.handle("ghost", PushConfigMessage())).isTrue()
        assertThat(h.connections.connections.value).isEmpty()
    }

    // ── deltas ───────────────────────────────────────────────────────

    @Test
    fun `agent_update and blocked route into the agent store`() = runTest {
        val h = Harness(backgroundScope)
        h.connect()
        h.ready()
        h.reducer.handle("r1", AgentsMessage(agents = listOf(state("p1"))))
        h.reducer.handle(
            "r1", AgentUpdateMessage(paneId = "p1", status = "done", agent = "claude"),
        )
        assertThat(h.agents.agentNow("r1::p1")!!.status).isEqualTo("done")

        h.reducer.handle(
            "r1",
            BlockedMessage(
                paneId = "p2", attentionKind = "question",
                interaction = WireField.Present(
                    Interaction(
                        id = "q", kind = "single_select", question = "?",
                        options = listOf(Option(index = 0, label = "y")), other = Other(),
                    ),
                ),
            ),
        )
        val blocked = h.agents.agentNow("r1::p2")!!
        assertThat(blocked.status).isEqualTo("blocked")
        assertThat(blocked.attentionKind).isEqualTo("question")
        assertThat(blocked.interaction).isNotNull()
    }

    @Test
    fun `agent_update without pane_id is consumed but ignored`() = runTest {
        val h = Harness(backgroundScope)
        assertThat(
            h.reducer.handle("r1", AgentUpdateMessage(status = "done")),
        ).isTrue()
        assertThat(h.agents.agents.value).isEmpty()
    }

    @Test
    fun `pane content with a question promotes the agent row`() = runTest {
        val h = Harness(backgroundScope)
        h.connect()
        h.ready()
        h.reducer.handle("r1", AgentsMessage(agents = listOf(state("p1"))))
        h.reducer.handle(
            "r1",
            PaneContentMessage(
                paneId = "p1", content = "…", attentionKind = "question",
                interaction = WireField.Present(
                    Interaction(
                        id = "q", kind = "single_select", question = "?",
                        options = listOf(Option(index = 0, label = "y")), other = Other(),
                    ),
                ),
            ),
        )
        assertThat(h.agents.agentNow("r1::p1")!!.attentionKind).isEqualTo("question")
    }

    @Test
    fun `messages without store impact are not consumed`() = runTest {
        val h = Harness(backgroundScope)
        assertThat(h.reducer.handle("r1", CommandResultMessage(action = "x"))).isFalse()
        assertThat(h.reducer.handle("r1", PaneProbeMessage(paneId = "p1"))).isFalse()
    }
}
