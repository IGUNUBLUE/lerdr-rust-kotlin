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
import lerdr.core.store.agentNeedsInspection
import lerdr.core.store.agentNeedsResponse
import lerdr.core.store.agentStatusGroup
import lerdr.core.store.attentionKind
import lerdr.core.store.clientPaneId
import lerdr.core.store.sortedAgents

/**
 * Home data seam — repositories expose `Flow`, never suspend-gets
 * (see .devin/skills/android-app). [RealHomeRepository] folds
 * `core:store`'s AgentStore + ConnectionStore + RelayRegistry into
 * [HomeUiState]; [FakeHomeRepository] survives only as a test fixture.
 */
interface HomeRepository {
    val uiState: Flow<HomeUiState>
}

/**
 * Mission-control projection: needs-you rail from blocked/question agents,
 * working/idle groups from the oracle's status grouping, relay strip from
 * the configured endpoints × live connection rows. A 30 s ticker re-derives
 * the relative-age labels while subscribed.
 */
@Singleton
class RealHomeRepository @Inject constructor(
    private val agentStore: AgentStore,
    private val connectionStore: ConnectionStore,
    private val relayRegistry: RelayRegistry,
    private val sessions: SessionRepository,
    private val now: () -> Long = System::currentTimeMillis,
) : HomeRepository {

    private val ticker: Flow<Unit> = flow {
        while (true) {
            emit(Unit)
            delay(AGE_TICK_MS)
        }
    }

    override val uiState: Flow<HomeUiState> = combine(
        agentStore.agents,
        connectionStore.connections,
        relayRegistry.relays,
        sessions.activities,
        ticker,
    ) { agents, connections, relays, activities, _ ->
        project(agents, connections, relays, activities, now())
    }

    private fun project(
        agents: List<Agent>,
        connections: Map<String, RelayConnection>,
        relays: List<RelayEndpoint>,
        activities: List<com.lerdr.app.session.RelayActivity>,
        at: Long,
    ): HomeUiState {
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
                .map { it.toAttentionCard(at) },
            working = sorted
                .filter { agentStatusGroup(it) == AgentStatusGroup.WORKING }
                .map { it.toListItem(working = true, at = at, activity = lastActivity[it.paneId]) },
            idle = sorted
                .filter {
                    !agentNeedsResponse(it) && !agentNeedsInspection(it) &&
                        agentStatusGroup(it) != AgentStatusGroup.WORKING
                }
                .map { it.toListItem(working = false, at = at, activity = lastActivity[it.paneId]) },
            relays = relays.map { endpoint ->
                val connection = connections[endpoint.id]
                RelayCardUi(
                    relayId = endpoint.id,
                    label = endpoint.label,
                    transport = endpoint.transport.displayName(),
                    statusLabel = when (connection?.status) {
                        RelayStatus.CONNECTED -> "connected"
                        RelayStatus.CONNECTING -> "connecting…"
                        else -> "offline"
                    },
                    agentCount = agents.count { it.relayId == endpoint.id },
                    connected = connection?.status == RelayStatus.CONNECTED,
                )
            },
        )
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

    private fun Agent.toAttentionCard(at: Long): AttentionCardUi {
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
        val optionLabels = options ?: interaction?.options?.map { it.label }.orEmpty()
        return AttentionCardUi(
            paneId = paneId,
            agentLabel = displayLabel(),
            kind = kind,
            metaLabel = "$kindLabel · ${ageLabel(at - (lastActiveAt ?: updatedAt))}",
            prompt = prompt ?: interaction?.question ?: command ?: "",
            options = optionLabels.take(MAX_ATTENTION_OPTIONS),
        )
    }

    private fun Agent.toListItem(
        working: Boolean,
        at: Long,
        activity: lerdr.core.model.ActivityEntry?,
    ): AgentListItemUi {
        val statusText = status?.takeIf { it.isNotEmpty() } ?: "unknown"
        return if (working) {
            AgentListItemUi(
                paneId = paneId,
                title = displayLabel(),
                statusLine = activity?.summary?.takeIf { it.isNotEmpty() }
                    ?: prompt ?: command ?: statusText,
                activityLabel = statusText,
                elapsedLabel = elapsedLabel(at - (lastActiveAt ?: updatedAt)),
                working = true,
            )
        } else {
            AgentListItemUi(
                paneId = paneId,
                title = displayLabel(),
                statusLine = "$statusText · ${ageLabel(at - (lastActiveAt ?: updatedAt))} ago",
                activityLabel = null,
                elapsedLabel = "idle",
                working = false,
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
        const val MAX_ATTENTION_OPTIONS = 3
    }
}
