package lerdr.core.data

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.add
import lerdr.core.model.AgentState

/**
 * One composer draft record — Lerdr's `PromptDraftRecord`
 *: `{version, identity, text, updatedAt}`.
 */
data class ComposerDraft(
    /** The pane-scoped identity produced by [composerDraftIdentity]. */
    val identity: String,
    val text: String,
    val updatedAtEpochMs: Long,
)

/** `PromptDraftSaveResult`. */
enum class DraftSaveResult {
    SAVED,
    CLEARED,
    TOO_LARGE,
    UNAVAILABLE,
}

/**
 * `promptDraftIdentity` — a draft belongs to a
 * pane, not a conversation: relay + terminal/pane coordinates + agent kind
 * + cwd, so an agent restart or a same-pane different-cwd session does not
 * resurrect a stale draft. Serialized as a JSON array, same as Lerdr's
 * `JSON.stringify([...])`.
 */
fun composerDraftIdentity(relayId: String, agent: AgentState): String {
    val paneIdentity = agent.terminalId.ifEmpty {
        listOf(agent.workspaceId, agent.tabId, agent.rawPaneId)
            .filter { it.isNotEmpty() }
            .joinToString(":")
    }
    return composerDraftIdentity(relayId, paneIdentity, agent.agent, agent.cwd)
}

/** Field-level overload for callers that don't have an [AgentState]. */
fun composerDraftIdentity(
    relayId: String,
    paneIdentity: String,
    agent: String,
    cwd: String,
): String = buildJsonArray {
    add(relayId)
    add(paneIdentity)
    add(agent)
    add(cwd)
}.toString()

/** The persisted record shape (`version` lets a future format migrate). */
@Serializable
internal data class DraftRecord(
    val version: Int,
    val identity: String,
    val text: String,
    @SerialName("updatedAt") val updatedAtEpochMs: Long,
)
