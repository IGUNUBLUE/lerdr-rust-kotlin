package lerdr.core.store

import lerdr.core.model.AgentUpdateMessage
import lerdr.core.model.AgentsMessage
import lerdr.core.model.AppDeployStatusMessage
import lerdr.core.model.BlockedMessage
import lerdr.core.model.HerdrStatusMessage
import lerdr.core.model.InventoryStatusMessage
import lerdr.core.model.PaneContentMessage
import lerdr.core.model.PaneDeltaMessage
import lerdr.core.model.PushConfigMessage
import lerdr.core.model.PushSubscribedMessage
import lerdr.core.model.PushUnsubscribedMessage
import lerdr.core.model.ServerMessage
import lerdr.core.model.SpeechVoicesMessage
import lerdr.core.model.UpdateStatusMessage
import lerdr.core.model.WorkspacesMessage
import lerdr.core.model.orNull

/**
 * `ServerMessage` → store dispatch — the seam `:core:transport` feeds once a
 * frame is decoded. Mirrors the oracle's `handleMessage`
 * (`frontend/src/lib/store.ts`) for every message type that mutates agent,
 * workspace, or connection state.
 *
 * Returns `true` when the message type was consumed by a store. Types owned
 * by other modules (pane frames for `:core:terminal`, command results and
 * receipts for the request pipeline, uploads, WebRTC signalling, push
 * policy, activity journal) return `false` so the transport can route them.
 */
class StoreReducer(
    private val agentStore: AgentStore,
    private val workspaceStore: WorkspaceStore,
    private val connectionStore: ConnectionStore,
    /** Relay config label lookup — the oracle reads `relayConfigs` per message. */
    private val relayLabel: (String) -> String = { "relay" },
) {
    fun handle(relayId: String, message: ServerMessage): Boolean {
        // Every frame is proof-of-life for the relay's freshness bound.
        connectionStore.noteMessage(relayId)
        return when (message) {
        is PushConfigMessage -> {
            // Requires a live connection entry — same as the oracle's
            // `if (!connection) return`.
            if (connectionStore.connectionNow(relayId) == null) return true
            // Pane revisions are monotonic only per relay process; a fresh
            // handshake can follow a restart, so strip the stale baseline.
            agentStore.resetPaneRevisions(relayId)
            connectionStore.applyPushConfig(relayId, message)
            // Read the capability after the update — the oracle mutates the
            // connection in place before renormalizing agent attention.
            agentStore.renormalizeAttention(relayId, attentionCapable(relayId))
            true
        }
        is HerdrStatusMessage -> {
            connectionStore.applyHerdrStatus(relayId, message)
            true
        }
        is InventoryStatusMessage -> {
            connectionStore.applyInventoryStatus(relayId, message)
            true
        }
        is UpdateStatusMessage -> {
            connectionStore.applyUpdateStatus(relayId, message)
            true
        }
        is AppDeployStatusMessage -> {
            connectionStore.applyAppDeployStatus(relayId, message)
            true
        }
        is SpeechVoicesMessage -> {
            connectionStore.applySpeechVoices(relayId, message)
            true
        }
        is PushSubscribedMessage -> {
            connectionStore.applyPushSubscribed(relayId, message.ok == true)
            true
        }
        is PushUnsubscribedMessage -> {
            connectionStore.applyPushUnsubscribed(relayId, message.ok == true)
            true
        }
        is WorkspacesMessage -> {
            // Not authoritative while inventory is starting/erroring unless
            // the connection was explicitly marked stale.
            if (!connectionStore.acceptsInventorySnapshots(relayId)) return true
            workspaceStore.replaceForRelay(
                relayId, relayLabel(relayId), message.workspaces.orEmpty(),
            )
            true
        }
        is AgentsMessage -> {
            if (!connectionStore.acceptsInventorySnapshots(relayId)) return true
            agentStore.mergeSnapshot(
                relayId,
                relayLabel(relayId),
                message.agents.orEmpty(),
                attentionCapable(relayId),
            )
            true
        }
        is BlockedMessage -> {
            agentStore.applyBlocked(
                relayId, relayLabel(relayId), message, attentionCapable(relayId),
            )
            true
        }
        is AgentUpdateMessage -> {
            if (message.paneId == null) return true
            agentStore.applyAgentUpdate(
                relayId, relayLabel(relayId), message, attentionCapable(relayId),
            )
            true
        }
        is PaneContentMessage -> {
            val paneId = clientPaneId(relayId, message.paneId.orEmpty())
            agentStore.mergePaneInteraction(
                paneId, message.attentionKind, message.interaction.orNull,
            )
            true
        }
        is PaneDeltaMessage -> {
            val paneId = clientPaneId(relayId, message.paneId.orEmpty())
            agentStore.mergePaneInteraction(
                paneId, message.attentionKind, message.interaction.orNull,
            )
            true
        }
        else -> false
        }
    }

    private fun attentionCapable(relayId: String): Boolean =
        connectionStore.connectionNow(relayId)?.attentionCapable == true
}
