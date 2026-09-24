package com.lerdr.app.home

import com.lerdr.app.session.SessionRepository
import javax.inject.Inject
import javax.inject.Singleton
import kotlin.math.max
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.flow
import lerdr.core.data.RelayEndpoint
import lerdr.core.data.RelayRegistry
import lerdr.core.data.RelayTransport
import lerdr.core.model.BlockedMessage
import lerdr.core.store.Agent
import lerdr.core.store.AgentStatusGroup
import lerdr.core.store.AgentStore
import lerdr.core.store.ConnectionStore
import lerdr.core.store.RelayConnection
import lerdr.core.store.RelayStatus
import lerdr.core.store.RelayWorkspace
import lerdr.core.store.WorkspaceStore
import lerdr.core.store.agentNeedsInspection
import lerdr.core.store.agentNeedsResponse
import lerdr.core.store.agentStatusGroup
import lerdr.core.store.attentionKind
import lerdr.core.store.clientPaneId
import lerdr.core.store.sortedAgents

/**
 * Home data seam — repositories expose `Flow`, never suspend-gets
 * (see .devin/skills/android-app). [RealHomeRepository] folds
 * `core:store`'s AgentStore + ConnectionStore + WorkspaceStore +
 * RelayRegistry into [HomeUiState]; [FakeHomeRepository] survives only as a
 * test fixture.
 */
interface HomeRepository {
    val uiState: Flow<HomeUiState>
}

/** One projection's raw inputs — bundles the five-store combine. */
private data class HomeInputs(
    val agents: List<Agent>,
    val connections: Map<String, RelayConnection>,
    val relays: List<RelayEndpoint>,
    val workspaces: List<RelayWorkspace>,
    val activities: List<com.lerdr.app.session.RelayActivity>,
)

/**
 * Mission-control projection: needs-you rail from blocked/question agents,
 * working/idle agents grouped by `relay ▸ workspace` (the oracle's
 * `workspaceGroups`), relay strip from the configured endpoints × live
 * connection rows. `agentStore.responding` folds the in-flight answer set
 * onto attention cards; a 30 s ticker re-derives the relative-age labels
 * while subscribed.
 */
@Singleton
class RealHomeRepository @Inject constructor(
    private val agentStore: AgentStore,
    private val connectionStore: ConnectionStore,
    private val relayRegistry: RelayRegistry,
    private val sessions: SessionRepository,
    private val workspaceStore: WorkspaceStore,
    private val now: () -> Long = System::currentTimeMillis,
) : HomeRepository {

    private val ticker: Flow<Unit> = flow {
        while (true) {
            emit(Unit)
            delay(AGE_TICK_MS)
        }
    }

    override val uiState: Flow<HomeUiState> = combine(
        combine(
            agentStore.agents,
            connectionStore.connections,
            relayRegistry.relays,
            workspaceStore.workspaces,
            sessions.activities,
            ::HomeInputs,
        ),
        combine(sessions.responding, ticker) { responding, _ -> responding },
    ) { inputs, responding ->
        project(inputs, responding, now())
    }

    private fun project(
        inputs: HomeInputs,
        responding: Set<String>,
        at: Long,
    ): HomeUiState {
        val agents = inputs.agents
        val connections = inputs.connections
        val relays = inputs.relays
        val workspaces = inputs.workspaces
        val activities = inputs.activities
        val sorted = sortedAgents(agents)
        val lastActivity = activities
            .filter { it.entry.paneId.isNotEmpty() }
            .groupBy { clientPaneId(it.relayId, it.entry.paneId) }
            .mapValues { (_, items) -> items.first().entry }

        return HomeUiState(
            live = connections.values.any { it.status == RelayStatus.CONNECTED },
            relaySummary = relaySummary(relays, connections),
            needsYou = sorted
                .filter { agentNeedsResponse(it) || agentNeedsInspection(it) }
                .map { it.toAttentionCard(at, responding) },
            working = sorted
                .filter { agentStatusGroup(it) == AgentStatusGroup.WORKING }
                .toGroups(workspaces) {
                    it.toListItem(working = true, at = at, activity = lastActivity[it.paneId])
                },
            idle = sorted
                .filter {
                    !agentNeedsResponse(it) && !agentNeedsInspection(it) &&
                        agentStatusGroup(it) != AgentStatusGroup.WORKING
                }
                .toGroups(workspaces) {
                    it.toListItem(working = false, at = at, activity = lastActivity[it.paneId])
                },
            relays = relays.map { endpoint ->
                val connection = connections[endpoint.id]
                RelayCardUi(
                    relayId = endpoint.id,
                    label = endpoint.label,
                    transport = endpoint.transport.displayName(),
                    statusLabel = when {
                        connection?.status == RelayStatus.CONNECTING -> "connecting…"
                        connection?.status != RelayStatus.CONNECTED -> "offline"
                        connection.rttMs >= 0 -> "${connection.rttMs}ms"
                        else -> "connected"
                    },
                    agentCount = agents.count { it.relayId == endpoint.id },
                    connected = connection?.status == RelayStatus.CONNECTED,
                    rttMs = connection?.rttMs ?: -1,
                )
            },
        )
    }

    /**
     * The oracle's `workspaceGroups` (`workspaces.ts`): bucket agents by
     * `workspaceIdentity` (`workspace_id`, else `cwd`, else the pane's raw
     * id), prefer the `workspaces` snapshot's label, then order groups by
     * recency → label → host. Status sections group independently, matching
     * the oracle's per-status `workspaceGroupTrees` split.
     */
    private fun List<Agent>.toGroups(
        workspaces: List<RelayWorkspace>,
        map: (Agent) -> AgentListItemUi,
    ): List<AgentGroupUi> {
        val records = workspaces.associateBy {
            it.relayId + "\u0000" + it.workspaceId
        }
        // Order groups like the oracle: newest member activity first, then
        // label, then host — case-insensitive so ordering stays stable.
        return groupBy(::workspaceIdentity)
            .entries
            .sortedWith(
                compareByDescending<Map.Entry<String, List<Agent>>> { (_, members) ->
                    members.maxOf { it.lastActiveAt ?: it.updatedAt }
                }.thenBy { (_, members) -> groupLabel(members).lowercase() }
                    .thenBy { (_, members) -> members.first().relayLabel.lowercase() },
            )
            .map { (key, members) ->
                val record = records[key]
                AgentGroupUi(
                    key = key,
                    relayLabel = record?.relayLabel
                        ?: members.first().relayLabel,
                    label = record?.label ?: groupLabel(members),
                    agents = members.map(map),
                    watchingDevices = record?.tokens
                        ?.get(WATCHING_DEVICES_TOKEN)
                        ?.toIntOrNull()
                        ?.takeIf { it > 0 },
                )
            }
    }

    /**
     * `workspaceIdentity` — `relay_id + \u0000 + (workspace_id || cwd ||
     * raw_pane_id || pane_id)`: an agent without a workspace lands in a
     * per-cwd group, one without either in a singleton pane group.
     */
    private fun workspaceIdentity(agent: Agent): String =
        agent.relayId + "\u0000" + (
            agent.workspaceId.ifEmpty {
                agent.cwd?.takeIf { it.isNotEmpty() }
                    ?: agent.rawPaneId.ifEmpty { agent.paneId }
            }
            )

    /**
     * The oracle's `groupLabel` — sole distinct project, else sole distinct
     * cwd basename, else the first tab label, else "Workspace".
     */
    private fun groupLabel(agents: List<Agent>): String {
        val projects = agents
            .map { it.project.orEmpty() }
            .filter { it.isNotEmpty() }
            .distinct()
        if (projects.size == 1) return projects.single()
        val cwdNames = agents.map { pathBase(it.cwd.orEmpty()) }.distinct()
        if (cwdNames.size == 1) return cwdNames.single()
        return agents.firstNotNullOfOrNull { it.tabLabel.takeIf(String::isNotEmpty) }
            ?: "Workspace"
    }

    private fun relaySummary(
        relays: List<RelayEndpoint>,
        connections: Map<String, RelayConnection>,
    ): String = buildString {
        append(relays.size)
        append(if (relays.size == 1) " computer" else " computers")
        val connecting = connections.values.count { it.status == RelayStatus.CONNECTING }
        if (connecting > 0) append(" · $connecting connecting")
    }

    private fun Agent.toAttentionCard(
        at: Long,
        responding: Set<String>,
    ): AttentionCardUi {
        val kind = when (attentionKind(this)) {
            BlockedMessage.ATTENTION_APPROVAL -> AttentionKind.APPROVAL
            BlockedMessage.ATTENTION_QUESTION -> AttentionKind.QUESTION
            else -> AttentionKind.CHAT
        }
        val kindLabel = when (kind) {
            AttentionKind.APPROVAL -> "approval"
            AttentionKind.QUESTION -> "question"
            AttentionKind.CHAT -> "attention"
        }
        // `approvalOptions` — capable + ≥ 2 real labels, full list so the UI
        // can score the deny tone against the true last index.
        val approvalOptions = if (kind == AttentionKind.APPROVAL) {
            options?.filter { it.isNotEmpty() }?.takeIf { it.size >= 2 }
        } else {
            null
        }
        // `questionInteraction` — a usable single/multi-select payload.
        val question = interaction?.takeIf {
            kind == AttentionKind.QUESTION &&
                it.kindOrNull != null &&
                it.id.isNotEmpty() && it.question.isNotEmpty() &&
                it.options.isNotEmpty()
        }
        return AttentionCardUi(
            paneId = paneId,
            relayId = relayId,
            agentLabel = displayLabel(),
            kind = kind,
            metaLabel = "$kindLabel · ${ageLabel(at - (lastActiveAt ?: updatedAt))}",
            prompt = prompt ?: question?.question ?: command ?: "",
            options = approvalOptions.orEmpty(),
            interaction = question,
            responding = paneId in responding,
            controllable = sessions.canControl(relayId),
            provider = agent?.takeIf { it.isNotEmpty() },
        )
    }

    private fun Agent.toListItem(
        working: Boolean,
        at: Long,
        activity: lerdr.core.model.ActivityEntry?,
    ): AgentListItemUi {
        val statusText = status?.takeIf { it.isNotEmpty() } ?: "unknown"
        val watching = tokens?.containsKey(WATCHING_TOKEN) == true
        val labels = stateLabels?.values?.filter { it.isNotEmpty() }.orEmpty()
        return if (working) {
            AgentListItemUi(
                paneId = paneId,
                relayId = relayId,
                title = displayLabel(),
                statusLine = activity?.summary?.takeIf { it.isNotEmpty() }
                    ?: prompt ?: command ?: statusText,
                activityLabel = statusText,
                elapsedLabel = elapsedLabel(at - (lastActiveAt ?: updatedAt)),
                working = true,
                controllable = sessions.canControl(relayId),
                provider = agent?.takeIf { it.isNotEmpty() },
                watching = watching,
                stateLabels = labels,
            )
        } else {
            AgentListItemUi(
                paneId = paneId,
                relayId = relayId,
                title = displayLabel(),
                statusLine = "$statusText · ${ageLabel(at - (lastActiveAt ?: updatedAt))} ago",
                activityLabel = null,
                elapsedLabel = "idle",
                working = false,
                controllable = sessions.canControl(relayId),
                provider = agent?.takeIf { it.isNotEmpty() },
                watching = watching,
                stateLabels = labels,
            )
        }
    }

    /** "claude · lerdr" — agent name + project/workspace context. */
    private fun Agent.displayLabel(): String {
        val name = (name ?: agent)?.takeIf { it.isNotEmpty() } ?: "agent"
        val context = project?.takeIf { it.isNotEmpty() }
            ?: cwd?.substringAfterLast('/')?.takeIf { it.isNotEmpty() }
            ?: relayLabel
        return "$name · $context"
    }

    private fun RelayTransport.displayName(): String = when (this) {
        RelayTransport.WEBSOCKET -> "direct"
        RelayTransport.WEBSOCKET_TLS -> "tls"
    }

    private fun elapsedLabel(ms: Long): String {
        val totalSeconds = max(0, ms) / 1_000
        return "%d:%02d".format(totalSeconds / 60, totalSeconds % 60)
    }

    private fun ageLabel(ms: Long): String {
        val seconds = max(0, ms) / 1_000
        return when {
            seconds < 60 -> "${seconds}s"
            seconds < 3_600 -> "${seconds / 60}m"
            else -> "${seconds / 3_600}h"
        }
    }

    private companion object {
        const val AGE_TICK_MS = 30_000L

        /** `pane.report_metadata` token set while a lerdr device watches. */
        const val WATCHING_TOKEN = "lerdr_watching"

        /** `workspace.report_metadata` token — connected device count. */
        const val WATCHING_DEVICES_TOKEN = "lerdr_devices"
    }
}

/**
 * `pathBase` — basename of a filesystem path, both separators, trailing
 * slashes stripped; "workspace" when nothing remains (the oracle's
 * WorkspaceManager fallback).
 */
internal fun pathBase(path: String): String =
    path.trimEnd('/', '\\')
        .split('/', '\\')
        .filter { it.isNotEmpty() }
        .lastOrNull() ?: "workspace"
