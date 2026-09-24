package lerdr.core.store

import com.google.common.truth.Truth.assertThat
import lerdr.core.model.AgentState
import lerdr.core.model.AgentUpdateMessage
import lerdr.core.model.Interaction
import lerdr.core.model.Option
import lerdr.core.model.Other
import org.junit.Test

/**
 * Merge-semantics invariants ported from `frontend/src/lib/agents.ts` +
 * the `handleMessage` branches in `store.ts` (v0.26.3).
 */
class AgentMergeTest {

    private fun state(
        paneId: String,
        status: String = "working",
        agent: String = "claude",
        updatedAt: Long = 0,
        lastActiveAt: Long = 0,
        activitySeq: Long = 0,
        paneRevision: Long = 0,
        tabId: String = "t1",
        workspaceId: String = "w1",
        attentionKind: String = "",
        interaction: Interaction? = null,
        prompt: String = "",
        options: List<String> = emptyList(),
        approvalFingerprint: String = "",
        project: String = "proj",
        sessionName: String = "sess",
        serverSessionId: String = "",
        tokens: Map<String, String> = emptyMap(),
        stateLabels: Map<String, String> = emptyMap(),
    ) = AgentState(
        paneId = paneId,
        rawPaneId = paneId,
        agent = agent,
        status = status,
        updatedAt = updatedAt,
        lastActiveAt = lastActiveAt,
        activitySeq = activitySeq,
        paneRevision = paneRevision,
        tabId = tabId,
        workspaceId = workspaceId,
        attentionKind = attentionKind,
        interaction = interaction,
        prompt = prompt,
        options = options,
        approvalFingerprint = approvalFingerprint,
        project = project,
        sessionName = sessionName,
        serverSessionId = serverSessionId,
        tokens = tokens,
        stateLabels = stateLabels,
    )

    private fun questionInteraction(id: String = "q1") = Interaction(
        id = id,
        kind = "single_select",
        question = "pick one",
        options = listOf(Option(index = 0, label = "yes"), Option(index = 1, label = "no")),
        other = Other(),
    )

    private fun normalize(state: AgentState, capable: Boolean = true) =
        normalizeAgent("r1", "relay one", state.asPatch(), capable)

    // ── identity / keys ──────────────────────────────────────────────

    @Test
    fun `merge key is relay-scoped raw pane id`() {
        val agent = normalize(state(paneId = "p1"))
        assertThat(agent.paneId).isEqualTo("r1::p1")
        assertThat(agent.rawPaneId).isEqualTo("p1")
        assertThat(agent.relayId).isEqualTo("r1")
        assertThat(agent.relayLabel).isEqualTo("relay one")
    }

    @Test
    fun `raw pane id falls back to pane id`() {
        val agent = normalizeAgent(
            "r1", "relay", AgentPatch(paneId = "wire-pane"), attentionCapable = false,
        )
        assertThat(agent.rawPaneId).isEqualTo("wire-pane")
        assertThat(agent.paneId).isEqualTo("r1::wire-pane")
    }

    // ── mergeAgentDetails ────────────────────────────────────────────

    @Test
    fun `equal merge returns the same instance`() {
        val previous = normalize(state("p1"))
        val next = normalize(state("p1"))
        assertThat(mergeAgentDetails(previous, next)).isSameInstanceAs(previous)
    }

    @Test
    fun `changed field produces a new instance`() {
        val previous = normalize(state("p1", status = "working"))
        val next = normalize(state("p1", status = "done"))
        val merged = mergeAgentDetails(previous, next)
        assertThat(merged).isNotSameInstanceAs(previous)
        assertThat(merged.status).isEqualTo("done")
        assertThat(merged.paneId).isEqualTo(previous.paneId)
    }

    @Test
    fun `delta keeps previous fields it did not carry`() {
        val previous = normalize(
            state("p1", agent = "claude", project = "proj-a", sessionName = "morning"),
        )
        // An update carrying only status — like the dispatcher's
        // acknowledge frame — keeps name/project/session of the stored row.
        val next = normalizeAgent(
            "r1", "relay", AgentPatch(paneId = "p1", status = "idle"), true,
        )
        val merged = mergeAgentDetails(previous, next)
        assertThat(merged.status).isEqualTo("idle")
        assertThat(merged.agent).isEqualTo("claude")
        assertThat(merged.project).isEqualTo("proj-a")
        assertThat(merged.sessionName).isEqualTo("morning")
    }

    @Test
    fun `empty tab fields keep previous while present ones win`() {
        val previous = normalize(state("p1", tabId = "tab-9", workspaceId = "ws-9"))
        val next = normalizeAgent(
            "r1", "relay",
            AgentPatch(paneId = "p1", status = "idle", tabId = "", workspaceId = ""),
            true,
        )
        val merged = mergeAgentDetails(previous, next)
        assertThat(merged.tabId).isEqualTo("tab-9")
        assertThat(merged.workspaceId).isEqualTo("ws-9")
    }

    @Test
    fun `updated_at takes the max not the latest`() {
        val previous = normalize(state("p1", updatedAt = 100))
        val next = normalize(state("p1", updatedAt = 40))
        assertThat(mergeAgentDetails(previous, next).updatedAt).isEqualTo(100)
    }

    @Test
    fun `activity_seq survives on presence alone`() {
        val previous = normalize(state("p1", activitySeq = 9))
        // A delta (no activity_seq key) keeps the stored value.
        val delta = normalizeAgent(
            "r1", "relay", AgentPatch(paneId = "p1", status = "idle"), true,
        )
        assertThat(mergeAgentDetails(previous, delta).activitySeq).isEqualTo(9)
        // A snapshot row carrying a smaller value still wins — presence, not max.
        val regressed = normalize(state("p1", activitySeq = 3))
        assertThat(mergeAgentDetails(previous, regressed).activitySeq).isEqualTo(3)
    }

    @Test
    fun `pane_revision keeps the maximum`() {
        val previous = normalize(state("p1", paneRevision = 7))
        val next = normalize(state("p1", paneRevision = 4))
        // max wins — but staleAgentRevision would have dropped this row upstream;
        // mergeAgentDetails itself takes the max.
        assertThat(mergeAgentDetails(previous, next).paneRevision).isEqualTo(7)
    }

    @Test
    fun `attention fields always take next value`() {
        val previous = normalize(
            state(
                "p1", status = "blocked", attentionKind = "approval",
                prompt = "run?", options = listOf("yes", "no"),
                approvalFingerprint = "fp",
            ),
        )
        val next = normalize(state("p1", status = "working"))
        val merged = mergeAgentDetails(previous, next)
        assertThat(merged.attentionKind).isNull()
        assertThat(merged.prompt).isNull()
        assertThat(merged.options).isNull()
        assertThat(merged.approvalFingerprint).isNull()
        assertThat(merged.interaction).isNull()
    }

    @Test
    fun `target fields are rewritten by every merge — deltas clear them`() {
        val previous = normalize(state("p1", serverSessionId = "srv-1"))
        assertThat(previous.serverSessionId).isEqualTo("srv-1")
        // Oracle quirk: normalizeAgentTargetFields always stamps the four
        // target fields, so a delta that doesn't carry them clears them.
        val delta = normalizeAgent(
            "r1", "relay", AgentPatch(paneId = "p1", status = "idle"), true,
        )
        assertThat(mergeAgentDetails(previous, delta).serverSessionId).isNull()
    }

    // ── staleAgentRevision ───────────────────────────────────────────

    @Test
    fun `stale revision detected only when both sides carry one`() {
        val old = normalize(state("p1", paneRevision = 5))
        val stale = normalize(state("p1", paneRevision = 4))
        val fresh = normalize(state("p1", paneRevision = 6))
        val noRevision = normalize(state("p1"))
        assertThat(staleAgentRevision(old, stale)).isTrue()
        assertThat(staleAgentRevision(old, fresh)).isFalse()
        assertThat(staleAgentRevision(old, noRevision)).isFalse()
        assertThat(staleAgentRevision(noRevision, stale)).isFalse()
        assertThat(staleAgentRevision(null, stale)).isFalse()
    }

    // ── stabilizeBlockedSnapshot ─────────────────────────────────────

    @Test
    fun `first non-blocked miss retains blocked details, second clears`() {
        val misses = mutableMapOf<String, Int>()
        val blocked = normalize(
            state(
                "p1", status = "blocked", attentionKind = "approval", prompt = "allow?",
            ),
        )
        val unblocked = normalize(state("p1", status = "working"))

        val first = stabilizeBlockedSnapshot(blocked, unblocked, misses, emptySet())
        assertThat(first.status).isEqualTo("blocked")
        assertThat(first.attentionKind).isEqualTo("approval")
        assertThat(first.prompt).isEqualTo("allow?")
        assertThat(misses["r1::p1"]).isEqualTo(1)

        val second = stabilizeBlockedSnapshot(first, unblocked, misses, emptySet())
        assertThat(second.status).isEqualTo("working")
        assertThat(misses).isEmpty()
    }

    @Test
    fun `responding pane keeps its question until a non-question arrives`() {
        val misses = mutableMapOf<String, Int>()
        val responding = setOf("r1::p1")
        val blocked = normalize(
            state(
                "p1", status = "blocked", attentionKind = "question",
                interaction = questionInteraction(),
            ),
        )
        // A non-question blocked row while responding still keeps the question.
        val plainBlocked = normalize(state("p1", status = "blocked", attentionKind = "unknown"))
        val kept = stabilizeBlockedSnapshot(blocked, plainBlocked, misses, responding)
        assertThat(kept.attentionKind).isEqualTo("question")
        assertThat(kept.interaction).isNotNull()
    }

    @Test
    fun `blocked next passes through and clears the miss counter`() {
        val misses = mutableMapOf("r1::p1" to 1)
        val blocked = normalize(state("p1", status = "blocked", attentionKind = "unknown"))
        val next = normalize(state("p1", status = "blocked", attentionKind = "approval"))
        val out = stabilizeBlockedSnapshot(blocked, next, misses, emptySet())
        assertThat(out.attentionKind).isEqualTo("approval")
        assertThat(misses).isEmpty()
    }

    // ── mergeAgentList ───────────────────────────────────────────────

    @Test
    fun `snapshot replaces the relay slice and tombstones the missing`() {
        val misses = mutableMapOf<String, Int>()
        val first = listOf(normalize(state("p1")), normalize(state("p2")))
        val merged = mergeAgentList(emptyList(), "r1", first, misses, emptySet())
        assertThat(merged.map { it.paneId }).containsExactly("r1::p1", "r1::p2").inOrder()

        val second = listOf(normalize(state("p2")), normalize(state("p3")))
        val next = mergeAgentList(merged, "r1", second, misses, emptySet())
        assertThat(next.map { it.paneId }).containsExactly("r1::p2", "r1::p3").inOrder()
    }

    @Test
    fun `snapshot keeps other relays rows in front`() {
        val misses = mutableMapOf<String, Int>()
        val other = normalizeAgent("r2", "two", AgentState(paneId = "x").asPatch(), false)
        val current = mergeAgentList(emptyList(), "r2", listOf(other), misses, emptySet())
        val merged = mergeAgentList(
            current, "r1", listOf(normalize(state("p1"))), misses, emptySet(),
        )
        assertThat(merged.map { it.paneId }).containsExactly("r2::x", "r1::p1").inOrder()
        assertThat(merged[0]).isSameInstanceAs(other)
    }

    @Test
    fun `unchanged snapshot preserves every instance`() {
        val misses = mutableMapOf<String, Int>()
        val rows = listOf(state("p1"), state("p2")).map { normalize(it) }
        val first = mergeAgentList(emptyList(), "r1", rows, misses, emptySet())
        val again = mergeAgentList(
            first, "r1", listOf(state("p1"), state("p2")).map { normalize(it) },
            misses, emptySet(),
        )
        assertThat(again[0]).isSameInstanceAs(first[0])
        assertThat(again[1]).isSameInstanceAs(first[1])
    }

    @Test
    fun `stale snapshot row keeps the stored instance wholesale`() {
        val misses = mutableMapOf<String, Int>()
        val current = mergeAgentList(
            emptyList(), "r1",
            listOf(normalize(state("p1", status = "working", paneRevision = 9))),
            misses, emptySet(),
        )
        val staleRow = normalize(state("p1", status = "done", paneRevision = 3))
        val merged = mergeAgentList(current, "r1", listOf(staleRow), misses, emptySet())
        assertThat(merged[0]).isSameInstanceAs(current[0])
        assertThat(merged[0].status).isEqualTo("working")
    }

    // ── report_metadata maps (tokens / state_labels) ─────────────────

    @Test
    fun `snapshot adopts tokens and state labels`() {
        val misses = mutableMapOf<String, Int>()
        val row = normalize(
            state(
                "p1",
                tokens = mapOf("lerdr_watching" to "1"),
                stateLabels = mapOf("mode" to "planning"),
            ),
        )
        val merged = mergeAgentList(emptyList(), "r1", listOf(row), misses, emptySet())
        assertThat(merged[0].tokens).containsExactly("lerdr_watching", "1")
        assertThat(merged[0].stateLabels).containsExactly("mode", "planning")
    }

    @Test
    fun `snapshot without the keys clears a previously reported map`() {
        val misses = mutableMapOf<String, Int>()
        val first = mergeAgentList(
            emptyList(), "r1",
            listOf(normalize(state("p1", tokens = mapOf("lerdr_watching" to "1")))),
            misses, emptySet(),
        )
        // TTL expiry / unwatch: the next snapshot simply omits the keys —
        // decode yields empty maps and the stored row must clear, not stick.
        val second = mergeAgentList(
            first, "r1", listOf(normalize(state("p1"))), misses, emptySet(),
        )
        assertThat(second[0].tokens).isEmpty()
        assertThat(second[0].stateLabels).isEmpty()
    }

    @Test
    fun `agent_update delta keeps the stored maps`() {
        val misses = mutableMapOf<String, Int>()
        val first = mergeAgentList(
            emptyList(), "r1",
            listOf(
                normalize(
                    state(
                        "p1",
                        tokens = mapOf("lerdr_watching" to "1"),
                        stateLabels = mapOf("mode" to "planning"),
                    ),
                ),
            ),
            misses, emptySet(),
        )
        // Deltas never carry the maps — absent must keep, never clear.
        val delta = normalizeAgent(
            "r1", "relay one",
            AgentUpdateMessage(paneId = "p1", status = "idle").asPatch(),
            false,
        )
        val merged = mergeAgentDetails(first[0], delta)
        assertThat(merged.tokens).containsExactly("lerdr_watching", "1")
        assertThat(merged.stateLabels).containsExactly("mode", "planning")
    }

    // ── normalizeAgentAttention ──────────────────────────────────────

    @Test
    fun `non-blocked rows drop every attention field`() {
        val agent = normalize(
            state(
                "p1", status = "working", attentionKind = "approval",
                options = listOf("a", "b"), interaction = questionInteraction(),
                prompt = "p", approvalFingerprint = "fp",
            ),
        )
        assertThat(agent.attentionKind).isNull()
        assertThat(agent.options).isNull()
        assertThat(agent.interaction).isNull()
        assertThat(agent.prompt).isEqualTo("p") // prompt isn't cleared by normalization
        assertThat(agent.approvalFingerprint).isNull()
    }

    @Test
    fun `approval keeps options, question keeps interaction, never both`() {
        val approval = normalize(
            state(
                "p1", status = "blocked", attentionKind = "approval",
                options = listOf("yes", "no"), interaction = questionInteraction(),
                approvalFingerprint = "fp",
            ),
        )
        assertThat(approval.options).containsExactly("yes", "no")
        assertThat(approval.approvalFingerprint).isEqualTo("fp")
        assertThat(approval.interaction).isNull()

        val question = normalize(
            state(
                "p1", status = "blocked", attentionKind = "question",
                options = listOf("yes", "no"), interaction = questionInteraction(),
                approvalFingerprint = "fp",
            ),
        )
        assertThat(question.interaction).isNotNull()
        assertThat(question.options).isNull()
        assertThat(question.approvalFingerprint).isNull()
    }

    @Test
    fun `incapable relay normalizes blocked attention to unknown`() {
        val agent = normalize(
            state("p1", status = "blocked", attentionKind = "approval", options = listOf("a", "b")),
            capable = false,
        )
        assertThat(agent.attentionKind).isEqualTo("unknown")
        assertThat(agent.attentionCapable).isFalse()
        assertThat(agent.options).isNull()
    }

    @Test
    fun `unrecognized attention kind becomes unknown when blocked`() {
        val agent = normalize(
            state("p1", status = "blocked", attentionKind = "surprise"),
        )
        assertThat(agent.attentionKind).isEqualTo("unknown")
    }

    // ── grouping / sorting ───────────────────────────────────────────

    @Test
    fun `status groups match the oracle taxonomy`() {
        fun group(status: String, kind: String = "") =
            agentStatusGroup(normalize(state("p1", status = status, attentionKind = kind)))
        assertThat(group("blocked", "approval")).isEqualTo(AgentStatusGroup.BLOCKED)
        assertThat(group("blocked", "question")).isEqualTo(AgentStatusGroup.BLOCKED)
        assertThat(group("blocked", "chat")).isEqualTo(AgentStatusGroup.READY)
        assertThat(group("blocked", "unknown")).isEqualTo(AgentStatusGroup.ATTENTION)
        assertThat(group("blocked")).isEqualTo(AgentStatusGroup.ATTENTION)
        assertThat(group("working")).isEqualTo(AgentStatusGroup.WORKING)
        assertThat(group("done")).isEqualTo(AgentStatusGroup.DONE)
        assertThat(group("idle")).isEqualTo(AgentStatusGroup.READY)
        assertThat(group("whatever")).isEqualTo(AgentStatusGroup.OTHER)
    }

    @Test
    fun `sortedAgents orders by last_active then activity seq`() {
        val a = normalize(state("p1", lastActiveAt = 10, activitySeq = 1))
        val b = normalize(state("p2", lastActiveAt = 20, activitySeq = 1))
        val c = normalize(state("p3", lastActiveAt = 20, activitySeq = 5))
        assertThat(sortedAgents(listOf(a, b, c)).map { it.paneId })
            .containsExactly("r1::p3", "r1::p2", "r1::p1").inOrder()
    }

    @Test
    fun `dedupe keeps first position with the newest copy`() {
        val first = normalize(state("p1", status = "working"))
        val second = normalize(state("p1", status = "done"))
        val tail = normalize(state("p2"))
        val out = dedupeAgents(listOf(first, tail, second))
        assertThat(out.map { it.paneId }).containsExactly("r1::p1", "r1::p2").inOrder()
        assertThat(out[0].status).isEqualTo("done")
        assertThat(out[0]).isSameInstanceAs(second)
    }
}
