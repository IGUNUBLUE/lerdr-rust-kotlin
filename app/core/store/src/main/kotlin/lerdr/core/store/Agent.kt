package lerdr.core.store

import lerdr.core.model.AgentState
import lerdr.core.model.AgentUpdateMessage
import lerdr.core.model.BlockedMessage
import lerdr.core.model.Interaction
import lerdr.core.model.orNull

/**
 * Store-facing agent row — the Kotlin counterpart of the oracle's `Agent`
 * (`frontend/src/lib/types.ts`), which is the wire `AgentState` plus
 * client-assigned fields.
 *
 * Nullability mirrors the TS optionals: fields that can be absent on the
 * wire stay nullable so merge can distinguish "absent" (keep previous) from
 * "present but empty" (override). Fields the merge always rewrites
 * unconditionally ([prompt], [command], [options], [interaction],
 * [questionLayout], [attentionKind]) are nullable too — `null` there is the
 * cleared state the reducer produces for non-blocked agents.
 */
data class Agent(
    /** Config identity of the relay this agent was learned from. */
    val relayId: String,
    /** Display label of that relay (snapshot at merge time). */
    val relayLabel: String,
    /** Server-local pane identity (`raw_pane_id`, falling back to `pane_id`). */
    val rawPaneId: String,
    /** Client-scoped identity: `"$relayId::$rawPaneId"` — the merge key. */
    val paneId: String,
    val agent: String? = null,
    val name: String? = null,
    val status: String? = null,
    val focused: Boolean? = null,
    val cwd: String? = null,
    val project: String? = null,
    val host: String? = null,
    val session: String? = null,
    val sessionName: String? = null,
    val updatedAt: Long = 0,
    val lastActiveAt: Long? = null,
    val lastSeenAt: Long? = null,
    val activitySeq: Long? = null,
    /** Monotonic per relay process; `null` = never reported. */
    val paneRevision: Long? = null,
    val eventId: String? = null,
    /** Normalized attention kind (`approval`/`question`/`chat`/`unknown`), null when not blocked. */
    val attentionKind: String? = null,
    /** Whether the relay claimed `attention_classification` when this row was normalized. */
    val attentionCapable: Boolean = false,
    val prompt: String? = null,
    val command: String? = null,
    val options: List<String>? = null,
    val approvalFingerprint: String? = null,
    val interaction: Interaction? = null,
    val questionLayout: Boolean = false,
    // Target fields (`normalizeAgentTargetFields` in the oracle): rewritten on
    // every merge — deltas that don't carry them clear them.
    val serverSessionId: String? = null,
    val terminalId: String? = null,
    val generation: Long? = null,
    val agentSessionId: String? = null,
    val conversationHistoryAvailable: Boolean? = null,
    val tabId: String = "",
    val tabLabel: String = "",
    val tabNumber: Int? = null,
    val tabOrder: Int? = null,
    val workspaceId: String = "",
)

/**
 * Presence-aware partial agent — the Kotlin `Partial<Agent>` the oracle
 * builds by spreading a raw message. `null` everywhere means "the wire
 * message did not carry this key".
 *
 * Converters live on the wire DTOs: snapshot rows ([AgentState.asPatch])
 * treat Go `omitempty` zero values as absent, while delta messages
 * ([AgentUpdateMessage.asPatch], [BlockedMessage.asPatch]) already model
 * presence with nullable fields.
 */
data class AgentPatch(
    val paneId: String? = null,
    val rawPaneId: String? = null,
    val terminalId: String? = null,
    val serverSessionId: String? = null,
    val generation: Long? = null,
    val agentSessionId: String? = null,
    val tabId: String? = null,
    val tabLabel: String? = null,
    val tabNumber: Int? = null,
    val tabOrder: Int? = null,
    val workspaceId: String? = null,
    val agent: String? = null,
    val name: String? = null,
    val status: String? = null,
    val focused: Boolean? = null,
    val cwd: String? = null,
    val project: String? = null,
    val host: String? = null,
    val session: String? = null,
    val sessionName: String? = null,
    val updatedAt: Long? = null,
    val lastActiveAt: Long? = null,
    val lastSeenAt: Long? = null,
    val activitySeq: Long? = null,
    val eventId: String? = null,
    val attentionKind: String? = null,
    val prompt: String? = null,
    val command: String? = null,
    val options: List<String>? = null,
    val approvalFingerprint: String? = null,
    val interaction: Interaction? = null,
    val questionLayout: Boolean? = null,
    val conversationHistoryAvailable: Boolean? = null,
    val paneRevision: Long? = null,
) {
    /**
     * The oracle's `{ ...before, ...message }` spread: fields this patch
     * leaves absent fall back to [previous]'s values. Used for `agent_update`
     * frames that report a blocked status without an `attention_kind` key.
     */
    fun fillingFrom(previous: Agent): AgentPatch = AgentPatch(
        paneId = paneId ?: previous.rawPaneId,
        rawPaneId = rawPaneId ?: previous.rawPaneId,
        terminalId = terminalId ?: previous.terminalId,
        serverSessionId = serverSessionId ?: previous.serverSessionId,
        generation = generation ?: previous.generation,
        agentSessionId = agentSessionId ?: previous.agentSessionId,
        tabId = tabId ?: previous.tabId,
        tabLabel = tabLabel ?: previous.tabLabel,
        tabNumber = tabNumber ?: previous.tabNumber,
        tabOrder = tabOrder ?: previous.tabOrder,
        workspaceId = workspaceId ?: previous.workspaceId,
        agent = agent ?: previous.agent,
        name = name ?: previous.name,
        status = status ?: previous.status,
        focused = focused ?: previous.focused,
        cwd = cwd ?: previous.cwd,
        project = project ?: previous.project,
        host = host ?: previous.host,
        session = session ?: previous.session,
        sessionName = sessionName ?: previous.sessionName,
        updatedAt = updatedAt ?: previous.updatedAt,
        lastActiveAt = lastActiveAt ?: previous.lastActiveAt,
        lastSeenAt = lastSeenAt ?: previous.lastSeenAt,
        activitySeq = activitySeq ?: previous.activitySeq,
        eventId = eventId ?: previous.eventId,
        attentionKind = attentionKind ?: previous.attentionKind,
        prompt = prompt ?: previous.prompt,
        command = command ?: previous.command,
        options = options ?: previous.options,
        approvalFingerprint = approvalFingerprint ?: previous.approvalFingerprint,
        interaction = interaction ?: previous.interaction,
        questionLayout = questionLayout ?: previous.questionLayout,
        conversationHistoryAvailable = conversationHistoryAvailable
            ?: previous.conversationHistoryAvailable,
        paneRevision = paneRevision ?: previous.paneRevision,
    )
}

/** Snapshot row → patch. Go `omitempty` fields decode to zero values, which are mapped back to absent. */
fun AgentState.asPatch(): AgentPatch = AgentPatch(
    paneId = paneId,
    rawPaneId = rawPaneId,
    terminalId = terminalId,
    serverSessionId = serverSessionId.ifEmpty { null },
    generation = generation,
    agentSessionId = agentSessionId.ifEmpty { null },
    tabId = tabId,
    tabLabel = tabLabel,
    tabNumber = tabNumber,
    tabOrder = tabOrder.takeIf { it != 0 },
    workspaceId = workspaceId,
    agent = agent,
    name = name,
    status = status,
    focused = focused,
    cwd = cwd,
    project = project,
    host = host,
    session = session,
    sessionName = sessionName,
    updatedAt = updatedAt,
    lastActiveAt = lastActiveAt.takeIf { it != 0L },
    lastSeenAt = lastSeenAt.takeIf { it != 0L },
    activitySeq = activitySeq.takeIf { it != 0L },
    eventId = blockedEventId.ifEmpty { null },
    attentionKind = attentionKind.ifEmpty { null },
    prompt = prompt.ifEmpty { null },
    command = command.ifEmpty { null },
    options = options.takeIf { it.isNotEmpty() },
    approvalFingerprint = approvalFingerprint.ifEmpty { null },
    interaction = interaction,
    questionLayout = questionLayout.takeIf { it },
    conversationHistoryAvailable = conversationHistoryAvailable.takeIf { it },
    paneRevision = paneRevision.takeIf { it != 0L },
)

/** `agent_update` frame → patch. Every field the relay omits stays absent. */
fun AgentUpdateMessage.asPatch(): AgentPatch = AgentPatch(
    paneId = paneId,
    rawPaneId = rawPaneId,
    agent = agent,
    status = status,
    cwd = cwd,
    project = project,
    host = host,
    session = session,
    sessionName = sessionName,
    updatedAt = updatedAt,
    eventId = eventId,
    attentionKind = attentionKind,
    tabId = tabId,
    tabLabel = tabLabel,
    tabNumber = tabNumber,
    workspaceId = workspaceId,
    paneRevision = paneRevision,
)

/**
 * `blocked` frame → patch. The oracle forces `status: 'blocked'` onto the
 * message before normalizing, so the wire's own status field never wins.
 */
fun BlockedMessage.asPatch(): AgentPatch = AgentPatch(
    paneId = paneId,
    rawPaneId = rawPaneId,
    terminalId = terminalId,
    serverSessionId = serverSessionId,
    generation = generation,
    agentSessionId = agentSessionId,
    tabId = tabId,
    tabLabel = tabLabel,
    tabNumber = tabNumber,
    workspaceId = workspaceId,
    agent = agent,
    name = name,
    status = AgentState.STATUS_BLOCKED,
    cwd = cwd,
    project = project,
    host = host,
    session = session,
    sessionName = sessionName,
    updatedAt = updatedAt,
    eventId = eventId,
    attentionKind = attentionKind,
    prompt = prompt,
    command = command,
    options = options.orNull,
    approvalFingerprint = approvalFingerprint,
    interaction = interaction.orNull,
    questionLayout = questionLayout,
    paneRevision = paneRevision,
)
