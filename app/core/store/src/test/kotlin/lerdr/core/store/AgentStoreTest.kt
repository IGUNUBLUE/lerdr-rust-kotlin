package lerdr.core.store

import app.cash.turbine.test
import com.google.common.truth.Truth.assertThat
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runTest
import lerdr.core.model.AgentState
import lerdr.core.model.AgentUpdateMessage
import lerdr.core.model.BlockedMessage
import lerdr.core.model.Interaction
import lerdr.core.model.Option
import lerdr.core.model.Other
import lerdr.core.model.WireField
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class AgentStoreTest {

    private fun store(scope: CoroutineScope) = AgentStore(scope)

    private fun state(
        paneId: String,
        status: String = "working",
        attentionKind: String = "",
        paneRevision: Long = 0,
        updatedAt: Long = 0,
        project: String = "proj",
    ) = AgentState(
        paneId = paneId,
        rawPaneId = paneId,
        agent = "claude",
        status = status,
        attentionKind = attentionKind,
        paneRevision = paneRevision,
        updatedAt = updatedAt,
        project = project,
    )

    private fun questionInteraction() = Interaction(
        id = "q1",
        kind = "single_select",
        question = "continue?",
        options = listOf(Option(index = 0, label = "yes"), Option(index = 1, label = "no")),
        other = Other(),
    )

    // ── identity-preserving merge ────────────────────────────────────

    @Test
    fun `identical snapshot preserves row instances and emits once`() = runTest {
        val store = store(backgroundScope)
        store.mergeSnapshot("r1", "relay", listOf(state("p1"), state("p2")), true)
        val first = store.agents.value

        store.agents.test {
            assertThat(awaitItem()).isEqualTo(first)
            store.mergeSnapshot("r1", "relay", listOf(state("p1"), state("p2")), true)
            // Value-equal rows collapse to the stored instances — the list
            // itself compares equal, so StateFlow does not re-emit.
            expectNoEvents()
            cancelAndIgnoreRemainingEvents()
        }
        assertThat(store.agents.value[0]).isSameInstanceAs(first[0])
        assertThat(store.agents.value[1]).isSameInstanceAs(first[1])
    }

    @Test
    fun `changed row gets a new instance while untouched rows keep theirs`() = runTest {
        val store = store(backgroundScope)
        store.mergeSnapshot("r1", "relay", listOf(state("p1"), state("p2")), true)
        val first = store.agents.value

        store.mergeSnapshot(
            "r1", "relay", listOf(state("p1"), state("p2", status = "done")), true,
        )
        val second = store.agents.value
        assertThat(second[0]).isSameInstanceAs(first[0])
        assertThat(second[1]).isNotSameInstanceAs(first[1])
        assertThat(second[1].status).isEqualTo("done")
    }

    @Test
    fun `snapshot order is authoritative within the relay slice`() = runTest {
        val store = store(backgroundScope)
        store.mergeSnapshot("r1", "relay", listOf(state("p1"), state("p2")), true)
        store.mergeSnapshot("r1", "relay", listOf(state("p2"), state("p1")), true)
        assertThat(store.agents.value.map { it.paneId })
            .containsExactly("r1::p2", "r1::p1").inOrder()
    }

    @Test
    fun `disappeared agents are tombstoned by the next snapshot`() = runTest {
        val store = store(backgroundScope)
        store.mergeSnapshot("r1", "relay", listOf(state("p1"), state("p2")), true)
        store.mergeSnapshot("r1", "relay", listOf(state("p2")), true)
        assertThat(store.agents.value.map { it.paneId }).containsExactly("r1::p2")
    }

    @Test
    fun `empty snapshot clears only that relay`() = runTest {
        val store = store(backgroundScope)
        store.mergeSnapshot("r1", "relay", listOf(state("p1")), true)
        store.mergeSnapshot("r2", "two", listOf(state("x1")), true)
        store.mergeSnapshot("r1", "relay", emptyList(), true)
        assertThat(store.agents.value.map { it.paneId }).containsExactly("r2::x1")
    }

    @Test
    fun `agents from different relays coexist`() = runTest {
        val store = store(backgroundScope)
        store.mergeSnapshot("r1", "relay", listOf(state("p1")), true)
        store.mergeSnapshot("r2", "two", listOf(state("x1"), state("x2")), true)
        assertThat(store.agents.value.map { it.paneId })
            .containsExactly("r1::p1", "r2::x1", "r2::x2").inOrder()
        // r2's resnapshot keeps r1's row untouched and first.
        store.mergeSnapshot("r2", "two", listOf(state("x2")), true)
        assertThat(store.agents.value.map { it.paneId })
            .containsExactly("r1::p1", "r2::x2").inOrder()
    }

    // ── deltas ───────────────────────────────────────────────────────

    @Test
    fun `agent_update on an unknown pane appends a row`() = runTest {
        val store = store(backgroundScope)
        store.applyAgentUpdate(
            "r1", "relay",
            AgentUpdateMessage(paneId = "p9", status = "working", agent = "codex"),
            true,
        )
        assertThat(store.agents.value.map { it.paneId }).containsExactly("r1::p9")
        assertThat(store.agents.value[0].agent).isEqualTo("codex")
    }

    @Test
    fun `agent_update with stale pane_revision is dropped`() = runTest {
        val store = store(backgroundScope)
        store.mergeSnapshot("r1", "relay", listOf(state("p1", paneRevision = 5)), true)
        store.applyAgentUpdate(
            "r1", "relay",
            AgentUpdateMessage(paneId = "p1", status = "done", paneRevision = 4),
            true,
        )
        assertThat(store.agents.value[0].status).isEqualTo("working")
    }

    @Test
    fun `out-of-order deltas do not corrupt newer state`() = runTest {
        val store = store(backgroundScope)
        store.mergeSnapshot("r1", "relay", listOf(state("p1", paneRevision = 5)), true)
        store.applyAgentUpdate(
            "r1", "relay",
            AgentUpdateMessage(paneId = "p1", status = "done", paneRevision = 6),
            true,
        )
        store.applyAgentUpdate(
            "r1", "relay",
            AgentUpdateMessage(paneId = "p1", status = "working", paneRevision = 4),
            true,
        )
        assertThat(store.agents.value[0].status).isEqualTo("done")
        assertThat(store.agents.value[0].paneRevision).isEqualTo(6)
    }

    @Test
    fun `blocked message upserts and clears responding`() = runTest {
        val store = store(backgroundScope)
        store.mergeSnapshot("r1", "relay", listOf(state("p1")), true)
        store.markResponding("r1::p1")
        assertThat(store.responding.value).containsExactly("r1::p1")

        store.applyBlocked(
            "r1", "relay",
            BlockedMessage(
                paneId = "p1", rawPaneId = "p1", status = "blocked",
                attentionKind = "approval", options = WireField.Present(listOf("y", "n")),
            ),
            true,
        )
        val agent = store.agentNow("r1::p1")!!
        assertThat(agent.status).isEqualTo("blocked")
        assertThat(agent.attentionKind).isEqualTo("approval")
        assertThat(agent.options).containsExactly("y", "n")
        assertThat(store.responding.value).isEmpty()
    }

    @Test
    fun `blocked on an unknown pane appends`() = runTest {
        val store = store(backgroundScope)
        store.applyBlocked(
            "r1", "relay",
            BlockedMessage(paneId = "p7", attentionKind = "question"),
            true,
        )
        assertThat(store.agents.value.map { it.paneId }).containsExactly("r1::p7")
    }

    @Test
    fun `flicker guard keeps a blocked row blocked for one snapshot`() = runTest {
        val store = store(backgroundScope)
        store.mergeSnapshot(
            "r1", "relay",
            listOf(state("p1", status = "blocked", attentionKind = "approval")),
            true,
        )
        store.mergeSnapshot("r1", "relay", listOf(state("p1", status = "working")), true)
        assertThat(store.agents.value[0].status).isEqualTo("blocked")
        assertThat(store.agents.value[0].attentionKind).isEqualTo("approval")

        store.mergeSnapshot("r1", "relay", listOf(state("p1", status = "working")), true)
        assertThat(store.agents.value[0].status).isEqualTo("working")
    }

    @Test
    fun `markResponding expiry releases the pane`() = runTest {
        val store = store(backgroundScope)
        store.markResponding("r1::p1")
        assertThat(store.responding.value).containsExactly("r1::p1")
        advanceTimeBy(AgentStore.RESPONDING_TIMEOUT_MS + 1)
        assertThat(store.responding.value).isEmpty()
    }

    @Test
    fun `reconcile drops responding ids once the agent leaves blocked`() = runTest {
        val store = store(backgroundScope)
        store.mergeSnapshot(
            "r1", "relay",
            listOf(state("p1", status = "blocked", attentionKind = "approval")),
            true,
        )
        store.markResponding("r1::p1")
        // A non-blocked row while `responding` short-circuits the flicker
        // guard (the user already answered — trust the new state), and the
        // reconcile pass then drops the pane from `responding`.
        store.mergeSnapshot("r1", "relay", listOf(state("p1", status = "idle")), true)
        assertThat(store.agents.value[0].status).isEqualTo("idle")
        assertThat(store.responding.value).isEmpty()
    }

    // ── attention / push_config side effects ─────────────────────────

    @Test
    fun `pane interaction promotes the row to a question`() = runTest {
        val store = store(backgroundScope)
        store.mergeSnapshot("r1", "relay", listOf(state("p1")), true)
        store.mergePaneInteraction("r1::p1", "question", questionInteraction())
        val agent = store.agentNow("r1::p1")!!
        assertThat(agent.status).isEqualTo("blocked")
        assertThat(agent.attentionKind).isEqualTo("question")
        assertThat(agent.interaction).isNotNull()
    }

    @Test
    fun `pane interaction is ignored on attention-incapable relays`() = runTest {
        val store = store(backgroundScope)
        store.mergeSnapshot("r1", "relay", listOf(state("p1")), false)
        store.mergePaneInteraction("r1::p1", "question", questionInteraction())
        assertThat(store.agentNow("r1::p1")!!.status).isEqualTo("working")
    }

    @Test
    fun `resetPaneRevisions strips the baseline so a restart cannot look stale`() = runTest {
        val store = store(backgroundScope)
        store.mergeSnapshot("r1", "relay", listOf(state("p1", paneRevision = 9)), true)
        store.resetPaneRevisions("r1")
        assertThat(store.agents.value[0].paneRevision).isNull()
        // A post-restart snapshot with a lower revision now merges cleanly.
        store.mergeSnapshot("r1", "relay", listOf(state("p1", paneRevision = 1)), true)
        assertThat(store.agents.value[0].paneRevision).isEqualTo(1)
    }

    @Test
    fun `renormalizeAttention rewrites kinds when capability lands`() = runTest {
        val store = store(backgroundScope)
        store.mergeSnapshot(
            "r1", "relay",
            listOf(
                state("p1", status = "blocked", attentionKind = "approval")
                    .copy(options = listOf("y", "n")),
            ),
            true,
        )
        assertThat(store.agents.value[0].attentionKind).isEqualTo("approval")

        // Capability loss flips the kind to unknown and drops the payload.
        store.renormalizeAttention("r1", false)
        val degraded = store.agents.value[0]
        assertThat(degraded.attentionKind).isEqualTo("unknown")
        assertThat(degraded.options).isNull()

        // Once rewritten, the wire kind is gone — regaining the capability
        // cannot recover it (the next snapshot re-arms it). Oracle-faithful.
        store.renormalizeAttention("r1", true)
        assertThat(store.agents.value[0].attentionKind).isEqualTo("unknown")
    }

    @Test
    fun `removeRelay purges its rows`() = runTest {
        val store = store(backgroundScope)
        store.mergeSnapshot("r1", "relay", listOf(state("p1")), true)
        store.mergeSnapshot("r2", "two", listOf(state("x1")), true)
        store.removeRelay("r1")
        assertThat(store.agents.value.map { it.paneId }).containsExactly("r2::x1")
    }

    @Test
    fun `blocked without raw_pane_id keys on pane_id`() = runTest {
        val store = store(backgroundScope)
        store.mergeSnapshot("r1", "relay", listOf(state("p1", status = "working")), true)
        store.applyBlocked(
            "r1", "relay",
            BlockedMessage(paneId = "p1", rawPaneId = "", status = "blocked"),
            true,
        )
        // raw_pane_id falls back to pane_id — same key, in-place update.
        assertThat(store.agents.value.map { it.paneId }).containsExactly("r1::p1")
        assertThat(store.agents.value[0].status).isEqualTo("blocked")
    }
}
