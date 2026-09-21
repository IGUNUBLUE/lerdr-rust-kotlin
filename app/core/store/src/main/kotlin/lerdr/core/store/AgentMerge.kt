package lerdr.core.store

import lerdr.core.model.BlockedMessage

/**
 * Port of the oracle merge semantics — `frontend/src/lib/agents.ts` at
 * v0.26.3. Every function here mirrors a TS original; where the TS code
 * contains dead branches (the `!hasAttentionKind` retain paths in
 * `mergeAgentDetails` — `normalizeAgentAttention` always stamps an
 * `attention_kind` own-property) the port notes it and keeps the effective
 * behavior.
 */

// ── identity ─────────────────────────────────────────────────────────

/** `clientPaneId` — the merge key every agent row is stored under. */
fun clientPaneId(relayId: String, rawPaneId: String): String = "$relayId::$rawPaneId"

/** `rawBlocked` — status substring check, separator-tolerant. */
fun rawBlocked(status: String?): Boolean =
    status.orEmpty().trim().lowercase().replace(Regex("[_-]+"), " ").contains("blocked")

fun rawBlocked(agent: Agent?): Boolean = rawBlocked(agent?.status ?: "unknown")

fun rawBlocked(patch: AgentPatch): Boolean = rawBlocked(patch.status)

val ATTENTION_KINDS = setOf(
    BlockedMessage.ATTENTION_APPROVAL,
    BlockedMessage.ATTENTION_QUESTION,
    BlockedMessage.ATTENTION_CHAT,
    BlockedMessage.ATTENTION_UNKNOWN,
)

/** `attentionKind` — the wire value when it is a known kind, else "". */
fun attentionKind(agent: Agent?): String =
    agent?.attentionKind?.takeIf { it in ATTENTION_KINDS } ?: ""

fun attentionKind(patch: AgentPatch): String =
    patch.attentionKind?.takeIf { it in ATTENTION_KINDS } ?: ""

/** `agentStatusGroup` — grouping used by the attention rail and `responding` reconcile. */
enum class AgentStatusGroup { ATTENTION, BLOCKED, WORKING, DONE, READY, OTHER }

fun agentStatusGroup(agent: Agent?): AgentStatusGroup {
    if (agent == null) return AgentStatusGroup.OTHER
    val status = agent.status.orEmpty().ifEmpty { "unknown" }
        .trim().lowercase().replace(Regex("[_-]+"), " ")
    return when {
        status.contains("blocked") -> when (attentionKind(agent)) {
            BlockedMessage.ATTENTION_APPROVAL, BlockedMessage.ATTENTION_QUESTION ->
                AgentStatusGroup.BLOCKED
            BlockedMessage.ATTENTION_CHAT -> AgentStatusGroup.READY
            else -> AgentStatusGroup.ATTENTION
        }
        Regex("(working|running|progress|busy)").containsMatchIn(status) ->
            AgentStatusGroup.WORKING
        Regex("(done|complete|finish|success|unread)").containsMatchIn(status) ->
            AgentStatusGroup.DONE
        status == "idle" || status == "ready" -> AgentStatusGroup.READY
        else -> AgentStatusGroup.OTHER
    }
}

fun agentNeedsResponse(agent: Agent?): Boolean =
    rawBlocked(agent) && attentionKind(agent).let {
        it == BlockedMessage.ATTENTION_APPROVAL || it == BlockedMessage.ATTENTION_QUESTION
    }

fun agentNeedsInspection(agent: Agent?): Boolean =
    rawBlocked(agent) && attentionKind(agent).let {
        it != BlockedMessage.ATTENTION_APPROVAL &&
            it != BlockedMessage.ATTENTION_QUESTION &&
            it != BlockedMessage.ATTENTION_CHAT
    }

// ── normalization ────────────────────────────────────────────────────

/**
 * `normalizeAgent` — binds a patch to its relay and assigns the
 * client-scoped [Agent.paneId], then applies [normalizeAgentAttention].
 */
fun normalizeAgent(
    relayId: String,
    relayLabel: String,
    patch: AgentPatch,
    attentionCapable: Boolean = false,
): Agent {
    val rawPaneId = patch.rawPaneId?.takeIf { it.isNotEmpty() } ?: patch.paneId.orEmpty()
    return normalizeAgentAttention(
        Agent(
            relayId = relayId,
            relayLabel = relayLabel,
            rawPaneId = rawPaneId,
            paneId = clientPaneId(relayId, rawPaneId),
            agent = patch.agent,
            name = patch.name,
            status = patch.status,
            focused = patch.focused,
            cwd = patch.cwd,
            project = patch.project,
            host = patch.host,
            session = patch.session,
            sessionName = patch.sessionName,
            updatedAt = patch.updatedAt ?: 0,
            lastActiveAt = patch.lastActiveAt,
            lastSeenAt = patch.lastSeenAt,
            activitySeq = patch.activitySeq,
            paneRevision = patch.paneRevision,
            eventId = patch.eventId,
            attentionKind = patch.attentionKind,
            prompt = patch.prompt,
            command = patch.command,
            options = patch.options,
            approvalFingerprint = patch.approvalFingerprint,
            interaction = patch.interaction,
            questionLayout = patch.questionLayout ?: false,
            // normalizeAgentTargetFields: always emitted on the result;
            // blank/absent clears rather than keeps.
            serverSessionId = patch.serverSessionId?.trim()?.takeIf { it.isNotEmpty() },
            terminalId = patch.terminalId?.trim()?.takeIf { it.isNotEmpty() },
            generation = patch.generation?.takeIf { it >= 0 },
            agentSessionId = patch.agentSessionId?.trim()?.takeIf { it.isNotEmpty() },
            conversationHistoryAvailable = patch.conversationHistoryAvailable,
            tabId = patch.tabId ?: "",
            tabLabel = patch.tabLabel ?: "",
            tabNumber = patch.tabNumber,
            tabOrder = patch.tabOrder,
            workspaceId = patch.workspaceId ?: "",
        ),
        attentionCapable,
    )
}

/**
 * `normalizeAgentAttention` — non-blocked rows carry no attention payload;
 * blocked rows get a normalized kind and drop the fields of other kinds.
 */
fun normalizeAgentAttention(agent: Agent, capable: Boolean): Agent {
    if (!rawBlocked(agent)) {
        return agent.copy(
            attentionCapable = capable,
            attentionKind = null,
            options = null,
            approvalFingerprint = null,
            interaction = null,
            questionLayout = false,
        )
    }
    val kind = if (capable) {
        attentionKind(agent).ifEmpty { BlockedMessage.ATTENTION_UNKNOWN }
    } else {
        BlockedMessage.ATTENTION_UNKNOWN
    }
    return agent.copy(attentionCapable = capable, attentionKind = kind).let { next ->
        next.copy(
            options = if (kind != BlockedMessage.ATTENTION_APPROVAL) null else next.options,
            approvalFingerprint =
                if (kind != BlockedMessage.ATTENTION_APPROVAL) null else next.approvalFingerprint,
            interaction = if (kind != BlockedMessage.ATTENTION_QUESTION) null else next.interaction,
            questionLayout = if (kind != BlockedMessage.ATTENTION_QUESTION) false else next.questionLayout,
        )
    }
}

// ── revision guard ───────────────────────────────────────────────────

fun agentPaneRevision(agent: Agent?): Long =
    agent?.paneRevision?.takeIf { it > 0 } ?: 0

fun agentUpdatedAt(agent: Agent?): Long = agent?.updatedAt ?: 0

fun agentLastActiveAt(agent: Agent?): Long =
    agent?.lastActiveAt?.takeIf { it > 0 } ?: 0

fun agentActivitySeq(agent: Agent?): Long =
    agent?.activitySeq?.takeIf { it > 0 } ?: 0

/**
 * `staleAgentRevision` — a delta with a lower `pane_revision` than the
 * stored row is dropped wholesale (out-of-order guard).
 */
fun staleAgentRevision(previous: Agent?, next: Agent): Boolean {
    val previousRevision = agentPaneRevision(previous)
    val nextRevision = agentPaneRevision(next)
    return previousRevision > 0 && nextRevision > 0 && nextRevision < previousRevision
}

// ── merge ────────────────────────────────────────────────────────────

private fun retainBlockedDetails(previous: Agent, next: Agent): Agent = next.copy(
    status = previous.status,
    attentionKind = previous.attentionKind,
    attentionCapable = previous.attentionCapable,
    prompt = previous.prompt,
    command = previous.command,
    options = previous.options,
    approvalFingerprint = previous.approvalFingerprint,
    interaction = previous.interaction,
    questionLayout = previous.questionLayout,
)

/**
 * `stabilizeBlockedSnapshot` — flicker guard. A snapshot/delta that drops a
 * blocked agent's status is mistrusted once: the first miss keeps the
 * previous blocked details, the second lets it through. A pending local
 * response ([responding] contains the pane) pins the blocked view until the
 * blocked state actually clears with no question in flight.
 */
fun stabilizeBlockedSnapshot(
    previous: Agent?,
    next: Agent,
    misses: MutableMap<String, Int>,
    responding: Set<String>,
): Agent {
    val paneId = next.paneId
    if (paneId.isEmpty()) return next
    val nextQuestion = rawBlocked(next) &&
        attentionKind(next) == BlockedMessage.ATTENTION_QUESTION &&
        next.interaction != null
    val pendingQuestion = responding.contains(paneId) &&
        previous != null &&
        rawBlocked(previous) &&
        attentionKind(previous) == BlockedMessage.ATTENTION_QUESTION &&
        previous.interaction != null
    if (pendingQuestion && !nextQuestion) {
        misses.remove(paneId)
        return retainBlockedDetails(previous, next)
    }
    if (rawBlocked(next)) {
        misses.remove(paneId)
        return next
    }
    if (previous == null || !rawBlocked(previous) || responding.contains(paneId)) {
        misses.remove(paneId)
        return next
    }
    val count = (misses[paneId] ?: 0) + 1
    if (count >= 2) {
        misses.remove(paneId)
        return next
    }
    misses[paneId] = count
    return retainBlockedDetails(previous, next)
}

/**
 * `mergeAgentDetails` — field-level merge of an incoming normalized row
 * onto the stored one. Spread-merge semantics: fields the patch never
 * carried (mapped to `null` on [next] for keep-previous fields) retain the
 * previous value; attention fields always take [next]'s post-normalization
 * value, which is what the TS literal's dead `!hasAttentionKind` branches
 * reduce to.
 *
 * Returns [previous] itself when nothing changed — the identity-preserving
 * fast path that keeps Compose rows stable.
 */
fun mergeAgentDetails(previous: Agent?, next: Agent): Agent {
    if (previous == null) return next
    val merged = previous.copy(
        // normalizeAgent always writes these on `next`.
        relayLabel = next.relayLabel,
        rawPaneId = next.rawPaneId,
        paneId = next.paneId,
        serverSessionId = next.serverSessionId,
        terminalId = next.terminalId,
        generation = next.generation,
        agentSessionId = next.agentSessionId,
        attentionCapable = next.attentionCapable,
        attentionKind = next.attentionKind,
        prompt = next.prompt,
        command = next.command,
        options = next.options,
        interaction = next.interaction,
        questionLayout = next.questionLayout,
        // Spread-merge fields: absent (null) on `next` keeps previous.
        agent = next.agent ?: previous.agent,
        name = next.name ?: previous.name,
        status = next.status ?: previous.status,
        focused = next.focused ?: previous.focused,
        cwd = next.cwd ?: previous.cwd,
        project = next.project ?: previous.project,
        host = next.host ?: previous.host,
        session = next.session ?: previous.session,
        sessionName = next.sessionName ?: previous.sessionName,
        lastActiveAt = next.lastActiveAt ?: previous.lastActiveAt,
        lastSeenAt = next.lastSeenAt ?: previous.lastSeenAt,
        eventId = next.eventId ?: previous.eventId,
        conversationHistoryAvailable =
            next.conversationHistoryAvailable ?: previous.conversationHistoryAvailable,
        // approval_fingerprint is NOT in the TS override literal — under a
        // blocked+approval row an absent field survives through the spread.
        approvalFingerprint =
            if (rawBlocked(next) && next.attentionKind == BlockedMessage.ATTENTION_APPROVAL) {
                next.approvalFingerprint ?: previous.approvalFingerprint
            } else {
                next.approvalFingerprint
            },
        // Explicit TS overrides.
        tabId = next.tabId.ifEmpty { previous.tabId },
        tabLabel = next.tabLabel.ifEmpty { previous.tabLabel },
        tabNumber = next.tabNumber ?: previous.tabNumber,
        tabOrder = next.tabOrder ?: previous.tabOrder,
        workspaceId = next.workspaceId.ifEmpty { previous.workspaceId },
        updatedAt = maxOf(agentUpdatedAt(previous), agentUpdatedAt(next)),
        // `hasOwnProperty(next,'activity_seq')` — presence-only, may regress.
        activitySeq = next.activitySeq ?: previous.activitySeq,
        paneRevision = (maxOf(agentPaneRevision(previous), agentPaneRevision(next)))
            .takeIf { it > 0 },
    )
    // `mergedAgentEquals` — value-equal merges collapse to the stored
    // instance so keyed lists see the same identity.
    return if (merged == previous) previous else merged
}

/**
 * `mergeAgentList` — full-snapshot merge for one relay. Other relays' rows
 * keep their order and instances up front; the relay's own slice is replaced
 * by the incoming list, per-row merged so equal agents keep their instance.
 * Agents absent from the snapshot disappear (the tombstone rule).
 */
fun mergeAgentList(
    current: List<Agent>,
    relayId: String,
    incoming: List<Agent>,
    misses: MutableMap<String, Int>,
    responding: Set<String>,
): List<Agent> {
    val previous = current.associateBy { it.paneId }
    val retained = current.filter { it.relayId != relayId }
    val merged = incoming.map { agent ->
        val before = previous[agent.paneId]
        if (staleAgentRevision(before, agent)) {
            before!!
        } else {
            mergeAgentDetails(before, stabilizeBlockedSnapshot(before, agent, misses, responding))
        }
    }
    val live = incoming.mapTo(HashSet()) { it.paneId }
    val prefix = "$relayId::"
    misses.keys.removeAll { it.startsWith(prefix) && !live.contains(it) }
    return retained + merged
}

/**
 * `publishAgents` dedup — duplicate pane_ids are upstream corruption; the
 * newest copy wins while keeping its first position (LinkedHashMap
 * overwrite semantics, same as the TS `Map`).
 */
fun dedupeAgents(agents: List<Agent>): List<Agent> {
    val byPane = LinkedHashMap<String, Agent>(agents.size)
    for (agent in agents) byPane[agent.paneId] = agent
    return if (byPane.size == agents.size) agents else byPane.values.toList()
}

// ── ordering (view-layer helpers, ported for the home screen) ────────

private fun hostLabel(agent: Agent): String =
    agent.relayLabel.ifEmpty { agent.host?.takeIf { it.isNotEmpty() } ?: "relay" }

private fun tabName(agent: Agent): String =
    agent.tabLabel.ifEmpty { agent.name.orEmpty() }.trim()

private fun sessionName(agent: Agent): String {
    if (agent.sessionName != null) return agent.sessionName.trim()
    val legacy = agent.session?.trim().orEmpty()
    if (legacy.contains('/') || legacy.contains('\\')) return ""
    if (Regex("^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$", RegexOption.IGNORE_CASE)
            .matches(legacy)
    ) {
        return ""
    }
    return legacy
}

private fun agentContextLabel(agent: Agent): String {
    val name = tabName(agent)
    if (name.isNotEmpty() && name != agent.project) return name
    return agent.cwd.orEmpty().split(Regex("[\\\\/]")).filter { it.isNotEmpty() }.lastOrNull()
        .orEmpty()
}

fun compareAgentUpdatedAt(a: Agent, b: Agent): Int {
    val timestampOrder = agentLastActiveAt(b) - agentLastActiveAt(a)
    if (timestampOrder != 0L) return if (timestampOrder > 0) 1 else -1
    if (a.relayId != b.relayId) return 0
    val seqOrder = agentActivitySeq(b) - agentActivitySeq(a)
    return if (seqOrder > 0) 1 else if (seqOrder < 0) -1 else 0
}

/** `sortedAgents` — the home-view ordering the oracle applies at render. */
fun sortedAgents(agents: List<Agent>): List<Agent> = agents.sortedWith { a, b ->
    compareAgentUpdatedAt(a, b)
        .takeIf { it != 0 }
        ?: hostLabel(a).compareTo(hostLabel(b)).takeIf { it != 0 }
        ?: agentContextLabel(a).compareTo(agentContextLabel(b)).takeIf { it != 0 }
        ?: projectOrAgent(a).compareTo(projectOrAgent(b)).takeIf { it != 0 }
        ?: a.agent.orEmpty().compareTo(b.agent.orEmpty())
}

private fun projectOrAgent(agent: Agent): String =
    agent.project?.takeIf { it.isNotEmpty() } ?: agent.agent.orEmpty()
