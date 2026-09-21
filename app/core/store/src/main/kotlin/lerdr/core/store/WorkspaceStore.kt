package lerdr.core.store

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import lerdr.core.model.WorkspaceInfo
import lerdr.core.model.WorkspaceWorktree

/**
 * Store-facing workspace row — the oracle's `RelayWorkspace`
 * (`frontend/src/lib/types.ts`): wire `WorkspaceInfo` plus relay identity.
 */
data class RelayWorkspace(
    val relayId: String,
    val relayLabel: String,
    val workspaceId: String,
    val number: Int = 0,
    val label: String = "Workspace",
    val focused: Boolean = false,
    val paneCount: Int = 0,
    val tabCount: Int = 0,
    val activeTabId: String = "",
    val agentStatus: String = "",
    val cwd: String = "",
    val worktree: WorkspaceWorktree? = null,
)

/** `normalizeWorkspace` — drops rows without a `workspace_id`, caps the label. */
fun normalizeWorkspace(
    relayId: String,
    relayLabel: String,
    info: WorkspaceInfo,
): RelayWorkspace? {
    if (info.workspaceId.isEmpty()) return null
    return RelayWorkspace(
        relayId = relayId,
        relayLabel = relayLabel,
        workspaceId = info.workspaceId,
        number = info.number,
        label = info.label.ifEmpty { "Workspace" }.take(256),
        focused = info.focused,
        paneCount = info.paneCount,
        tabCount = info.tabCount,
        activeTabId = info.activeTabId,
        agentStatus = info.agentStatus,
        cwd = info.cwd,
        worktree = info.worktree,
    )
}

/**
 * Workspace store — port of the oracle's `workspaces` writable. Ordering is
 * cross-relay concatenation: other relays' rows first, then this relay's
 * `workspaces` message order. Rows that merge equal keep their previous
 * instance, matching the agent store's identity-preservation contract.
 */
class WorkspaceStore {
    private val lock = Any()

    private val _workspaces = MutableStateFlow<List<RelayWorkspace>>(emptyList())
    val workspaces: StateFlow<List<RelayWorkspace>> = _workspaces.asStateFlow()

    /**
     * `workspaces` message — replaces this relay's slice; absent workspaces
     * are tombstoned. Snapshot order is authoritative within the slice.
     */
    fun replaceForRelay(relayId: String, relayLabel: String, incoming: List<WorkspaceInfo>) {
        synchronized(lock) {
            val previousById = _workspaces.value
                .filter { it.relayId == relayId }
                .associateBy { it.workspaceId }
            val normalized = incoming.mapNotNull { info ->
                normalizeWorkspace(relayId, relayLabel, info)?.let { next ->
                    // Instance preservation: equal rows keep the stored object.
                    previousById[next.workspaceId]?.takeIf { it == next } ?: next
                }
            }
            _workspaces.value =
                _workspaces.value.filter { it.relayId != relayId } + normalized
        }
    }

    fun removeRelay(relayId: String) {
        synchronized(lock) {
            _workspaces.value = _workspaces.value.filter { it.relayId != relayId }
        }
    }

    fun clear() {
        synchronized(lock) { _workspaces.value = emptyList() }
    }
}
