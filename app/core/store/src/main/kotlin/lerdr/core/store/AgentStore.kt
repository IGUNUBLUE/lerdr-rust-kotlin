package lerdr.core.store

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import lerdr.core.model.AgentState
import lerdr.core.model.AgentUpdateMessage
import lerdr.core.model.BlockedMessage
import lerdr.core.model.Interaction

/**
 * Agent inventory store — StateFlow-based port of the oracle's `agents` and
 * `responding` writables (`frontend/src/lib/store.ts`).
 *
 * The list exposed through [agents] preserves instance identity: rows that
 * merge to an equal value keep their previous object, so Compose keys on
 * [Agent.paneId] see stable instances and skip recomposition.
 *
 * All mutations are safe to call from any single-threaded dispatcher; the
 * internal lock also makes multi-threaded callers safe.
 */
class AgentStore(
    private val scope: CoroutineScope,
    private val respondingTimeoutMs: Long = RESPONDING_TIMEOUT_MS,
) {
    private val lock = Any()

    private val _agents = MutableStateFlow<List<Agent>>(emptyList())

    /** All known agents across relays: other relays' rows first, then each relay's snapshot order. */
    val agents: StateFlow<List<Agent>> = _agents.asStateFlow()

    private val _responding = MutableStateFlow<Set<String>>(emptySet())

    /**
     * Pane ids the user just answered, while the relay hasn't confirmed.
     * Drives the flicker guard in [stabilizeBlockedSnapshot].
     */
    val responding: StateFlow<Set<String>> = _responding.asStateFlow()

    /** Per-pane count of consecutive non-blocked sightings while blocked (flicker guard). */
    private val blockedSnapshotMisses = mutableMapOf<String, Int>()
    private val respondingTimers = mutableMapOf<String, Job>()

    /** Emits the agent row for [paneId] (client-scoped `"$relayId::$rawPaneId"`), or null. */
    fun agent(paneId: String): Flow<Agent?> =
        _agents.map { list -> list.firstOrNull { it.paneId == paneId } }
            .distinctUntilChanged()

    /** Current row for [paneId], for reducer internals and tests. */
    fun agentNow(paneId: String): Agent? =
        synchronized(lock) { _agents.value.firstOrNull { it.paneId == paneId } }

    /**
     * `agents` snapshot — replaces the relay's slice wholesale; absent rows
     * are tombstoned, equal rows keep their instance.
     */
    fun mergeSnapshot(
        relayId: String,
        relayLabel: String,
        rows: List<AgentState>,
        attentionCapable: Boolean,
    ) {
        synchronized(lock) {
            val incoming = rows.map { normalizeAgent(relayId, relayLabel, it.asPatch(), attentionCapable) }
            _agents.value = mergeAgentList(
                _agents.value, relayId, incoming, blockedSnapshotMisses, _responding.value,
            )
            reconcileRespondingLocked()
            publishLocked()
        }
    }

    /** `blocked` broadcast — upsert a single row, forced `status: "blocked"`. */
    fun applyBlocked(
        relayId: String,
        relayLabel: String,
        message: BlockedMessage,
        attentionCapable: Boolean,
    ) {
        synchronized(lock) {
            val next = normalizeAgent(relayId, relayLabel, message.asPatch(), attentionCapable)
            val index = _agents.value.indexOfFirst { it.paneId == next.paneId }
            val before = index.takeIf { it >= 0 }?.let { _agents.value[it] }
            if (staleAgentRevision(before, next)) return
            blockedSnapshotMisses.remove(next.paneId)
            _agents.value = if (index >= 0) {
                _agents.value.toMutableList().apply {
                    set(index, mergeAgentDetails(before, next))
                }
            } else {
                _agents.value + next
            }
            removeRespondingLocked(next.paneId)
            publishLocked()
        }
    }

    /**
     * `agent_update` delta — upsert keyed on the *message* `pane_id` (the
     * oracle's lookup key; [Agent.paneId] itself derives from `raw_pane_id`).
     */
    fun applyAgentUpdate(
        relayId: String,
        relayLabel: String,
        message: AgentUpdateMessage,
        attentionCapable: Boolean,
    ) {
        val rawPaneId = message.paneId ?: return
        synchronized(lock) {
            val paneId = clientPaneId(relayId, rawPaneId)
            val index = _agents.value.indexOfFirst { it.paneId == paneId }
            val before = index.takeIf { it >= 0 }?.let { _agents.value[it] }
            // Blocked delta without attention_kind keeps the previous row's
            // attention payload via a spread (the oracle's `source` merge).
            val patch = message.asPatch().let { patch ->
                if (before != null && rawBlocked(patch) && patch.attentionKind == null) {
                    patch.fillingFrom(before)
                } else {
                    patch
                }
            }
            val next = normalizeAgent(relayId, relayLabel, patch, attentionCapable)
            if (staleAgentRevision(before, next)) return
            val stabilized =
                stabilizeBlockedSnapshot(before, next, blockedSnapshotMisses, _responding.value)
            _agents.value = if (index >= 0) {
                _agents.value.toMutableList().apply {
                    set(index, mergeAgentDetails(before, stabilized))
                }
            } else {
                _agents.value + stabilized
            }
            reconcileRespondingLocked()
            publishLocked()
        }
    }

    /**
     * `mergePaneInteraction` — pane frames carrying a question interaction
     * promote the row to blocked/question, but only on attention-capable
     * relays.
     */
    fun mergePaneInteraction(paneId: String, attentionKind: String?, interaction: Interaction?) {
        if (attentionKind != BlockedMessage.ATTENTION_QUESTION || interaction == null) return
        synchronized(lock) {
            val index = _agents.value.indexOfFirst { it.paneId == paneId }
            if (index < 0) return
            val agent = _agents.value[index]
            if (!agent.attentionCapable) return
            _agents.value = _agents.value.toMutableList().apply {
                set(
                    index,
                    agent.copy(
                        status = AgentState.STATUS_BLOCKED,
                        attentionKind = BlockedMessage.ATTENTION_QUESTION,
                        interaction = interaction,
                    ),
                )
            }
            blockedSnapshotMisses.remove(paneId)
            publishLocked()
        }
    }

    /**
     * `push_config` side effect — a new handshake may follow a relay restart,
     * so process-local pane revisions are stripped before the fresh snapshot.
     */
    fun resetPaneRevisions(relayId: String) {
        synchronized(lock) {
            _agents.value = _agents.value.map { agent ->
                if (agent.relayId != relayId || agent.paneRevision == null) agent
                else agent.copy(paneRevision = null)
            }
            publishLocked()
        }
    }

    /** Re-runs attention normalization when the relay's capability set changes. */
    fun renormalizeAttention(relayId: String, attentionCapable: Boolean) {
        synchronized(lock) {
            _agents.value = _agents.value.map { agent ->
                if (agent.relayId == relayId) normalizeAgentAttention(agent, attentionCapable)
                else agent
            }
            publishLocked()
        }
    }

    /** Relay removed or config reset — drops every row it owned. */
    fun removeRelay(relayId: String) {
        synchronized(lock) {
            _agents.value = _agents.value.filter { it.relayId != relayId }
            val prefix = "$relayId::"
            blockedSnapshotMisses.keys.removeAll { it.startsWith(prefix) }
            publishLocked()
        }
    }

    /** Marks a pane as answered locally; auto-expires after [respondingTimeoutMs]. */
    fun markResponding(paneId: String) {
        synchronized(lock) {
            _responding.value = _responding.value + paneId
            respondingTimers.remove(paneId)?.cancel()
            respondingTimers[paneId] = scope.launch {
                delay(respondingTimeoutMs)
                synchronized(lock) {
                    if (respondingTimers.remove(paneId) != null) {
                        removeRespondingLocked(paneId)
                    }
                }
            }
        }
    }

    fun clearResponding(paneId: String) {
        synchronized(lock) { removeRespondingLocked(paneId) }
    }

    /** Drops everything (relay config reset). */
    fun clear() {
        synchronized(lock) {
            _agents.value = emptyList()
            blockedSnapshotMisses.clear()
            respondingTimers.values.forEach { it.cancel() }
            respondingTimers.clear()
            _responding.value = emptySet()
            publishLocked()
        }
    }

    private fun removeRespondingLocked(paneId: String) {
        respondingTimers.remove(paneId)?.cancel()
        if (_responding.value.contains(paneId)) {
            _responding.value = _responding.value - paneId
        }
    }

    /** `reconcileResponding` — responding ids that are no longer blocked get released. */
    private fun reconcileRespondingLocked() {
        val blocked = _agents.value
            .filter { agentStatusGroup(it) == AgentStatusGroup.BLOCKED }
            .mapTo(HashSet()) { it.paneId }
        val stale = _responding.value.filter { it !in blocked }
        if (stale.isEmpty()) return
        for (paneId in stale) respondingTimers.remove(paneId)?.cancel()
        _responding.value = _responding.value - stale.toSet()
    }

    private fun publishLocked() {
        _agents.value = dedupeAgents(_agents.value)
    }

    companion object {
        /** The oracle's 10s `markResponding` expiry. */
        const val RESPONDING_TIMEOUT_MS = 10_000L
    }
}
