package com.lerdr.app.session

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowLeft
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Folder
import androidx.compose.material.icons.filled.KeyboardArrowUp
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.PreviewLightDark
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lerdr.core.designsystem.theme.LerdrTheme
import dagger.hilt.EntryPoint
import dagger.hilt.InstallIn
import dagger.hilt.android.EntryPointAccessors
import dagger.hilt.components.SingletonComponent
import java.util.UUID
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import lerdr.core.model.ActionReceiptMessage
import lerdr.core.model.CommandResultMessage
import lerdr.core.model.Inbound
import lerdr.core.store.Agent
import lerdr.core.store.AgentInventoryState
import lerdr.core.store.RelayStatus
import lerdr.core.store.RelayWorkspace
import lerdr.core.store.WorkspaceStore
import lerdr.core.store.sortedAgents
import lerdr.core.transport.CommandException

/**
 * `WorkspaceTabUi` — one tab of a workspace's agent panes, the oracle's
 * `WorkspaceTab` (`frontend/src/lib/workspaces.ts`): agents grouped by
 * `tab_id` (a pane without a tab id stands alone), ordered by Herdr's
 * `tab_order` with `tab_number` as the stable fallback.
 */
@Immutable
data class WorkspaceTabUi(
    val tabId: String,
    val label: String,
    val number: Int,
    val order: Int,
    /** The tab's representative pane — reorder target and select payload. */
    val paneId: String,
    val paneCount: Int,
)

/** Everything [WorkspaceTabsStripContent] renders. */
@Immutable
data class WorkspaceTabsUiState(
    val tabs: List<WorkspaceTabUi> = emptyList(),
    /** The viewed agent's tab — marked in the strip. */
    val activeTabId: String? = null,
    /** `tabOrderingAvailable` — connected + inventory ready + capability. */
    val reorderAvailable: Boolean = false,
    /**
     * The oracle's `readOnly` gate — readers see the strip but no menus,
     * move items, or the "+" affordance (`docs/04` hides mutations).
     */
    val canControl: Boolean = false,
    /** `workspace_management` capability — rename/close/create visibility. */
    val managementAvailable: Boolean = false,
    /** `directory_browser` capability — the create sheet's folder picker. */
    val directoryBrowserAvailable: Boolean = false,
    /** Tab whose overflow menu is open (long-press). */
    val menuTabId: String? = null,
    /** The viewed pane's workspace — the rename/close target. */
    val workspaceLabel: String = "",
    val busy: Boolean = false,
    /** Inline status line — the oracle's toast text (errors tinted). */
    val status: String? = null,
    val statusError: Boolean = false,
    /** Reorder failure text (pre-existing behavior). */
    val error: String? = null,
    // Rename dialog (`workspace_rename`).
    val renameOpen: Boolean = false,
    val renameDraft: String = "",
    // Close confirm (`workspace_close`) — `confirmGroup` mirrors the
    // oracle's `close_group` kind after a `workspace_group_close_required`
    // refusal; `groupMembers` renders the confirm list, primary first.
    val confirmClose: Boolean = false,
    val confirmGroup: Boolean = false,
    val groupMembers: List<String> = emptyList(),
    // Create sheet (`workspace_create` + `list_directories` browser).
    val createOpen: Boolean = false,
    val createCwd: String = "",
    val createLabel: String = "",
    val directoryOpen: Boolean = false,
    val directory: DirectoryListing? = null,
    val directoryLoading: Boolean = false,
    val directoryError: String? = null,
)

/** Oracle `tabName` — the Herdr tab label, else the pane's own name. */
private fun tabNameOf(agent: Agent): String =
    agent.tabLabel.ifEmpty { agent.name.orEmpty() }.trim()

/** Oracle `displayName` — `project || name || tab_label || agent || 'agent'`. */
private fun displayNameOf(agent: Agent): String =
    agent.project?.takeIf { it.isNotEmpty() }
        ?: agent.name?.takeIf { it.isNotEmpty() }
        ?: agent.tabLabel.takeIf { it.isNotEmpty() }
        ?: agent.agent?.takeIf { it.isNotEmpty() }
        ?: "agent"

/**
 * `computeWorkspaceGroups`'s tab stage — group agents by `tab_id` (pane id
 * fallback), label `tabName || displayName`, sort `order, number, label`.
 */
internal fun workspaceTabs(agents: List<Agent>): List<WorkspaceTabUi> {
    val ordered = sortedAgents(agents)
    val byTab = LinkedHashMap<String, MutableList<Agent>>()
    for (agent in ordered) {
        val id = agent.tabId.ifEmpty { agent.paneId }
        byTab.getOrPut(id) { mutableListOf() }.add(agent)
    }
    return byTab.map { (id, tabAgents) ->
        val first = sortedAgents(tabAgents)[0]
        WorkspaceTabUi(
            tabId = id,
            label = tabNameOf(first).ifEmpty { displayNameOf(first) },
            // `Number(x) || MAX` — a wire 0 reads as absent (Go omitempty),
            // so it sorts last. Tab numbers are identities; tab_order is
            // Herdr's visual position.
            number = first.tabNumber?.takeIf { it != 0 } ?: Int.MAX_VALUE,
            order = first.tabOrder?.takeIf { it != 0 } ?: Int.MAX_VALUE,
            paneId = first.paneId,
            paneCount = tabAgents.size,
        )
    }.sortedWith(compareBy({ it.order }, { it.number }, { it.label }))
}

/**
 * `relayWorkspaceTrees`'s group shape — the workspaces sharing one linked-
 * worktree `repo_key` on a relay, primary (non-linked) first, children by
 * `number, label`. A workspace with no group yields just itself.
 */
internal fun workspaceGroupIds(
    workspaces: List<RelayWorkspace>,
    relayId: String,
    workspaceId: String,
): List<String> {
    val onRelay = workspaces.filter { it.relayId == relayId }
    val self = onRelay.firstOrNull { it.workspaceId == workspaceId }
        ?: return listOf(workspaceId)
    val repoKey = self.worktree?.repoKey.orEmpty()
    if (repoKey.isEmpty()) return listOf(workspaceId)
    val primary = if (self.worktree?.isLinkedWorktree != true) {
        self
    } else {
        onRelay.firstOrNull {
            it.worktree?.isLinkedWorktree == false && it.worktree?.repoKey == repoKey
        } ?: return listOf(workspaceId)
    }
    val children = onRelay.filter {
        it.worktree?.isLinkedWorktree == true &&
            it.worktree?.repoKey == repoKey &&
            it.workspaceId != primary.workspaceId
    }.sortedWith(compareBy({ it.number }, { it.label }))
    return listOf(primary.workspaceId) + children.map { it.workspaceId }
}

/** Oracle `pathBase` — a path's last segment for the workspace label. */
internal fun pathBaseOf(path: String): String =
    path.trimEnd('/', '\\').split('/', '\\')
        .lastOrNull { it.isNotEmpty() } ?: "workspace"

/**
 * A `workspace_close` refusal the thrown [CommandException] would drop —
 * `command_result{ok:false, data:{code, workspace_ids?}}` or the matching
 * `action_receipt{error.code}` (which carries no workspace_ids).
 */
internal data class CloseRefusal(val code: String, val workspaceIds: List<String>)

/**
 * Per-workspace tab strip state — watches the viewed agent's workspace tabs
 * and owns `tab_reorder` plus the workspace mutations the oracle's
 * WorkspaceManager exposes (`workspace_rename`, `workspace_close` with its
 * linked-worktree group escalation, `workspace_create` + the
 * `list_directories` browser).
 *
 * Ports `pendingTabOrder`: an optimistic ordering is applied until the
 * relay's `agents` snapshot confirms it (or a membership change
 * invalidates it); a failed send reverts immediately.
 *
 * Group-close parity: `workspace_close` refusals arrive as
 * `command_result{ok:false, data:{code, workspace_ids}}` — the thrown
 * exception drops `data`, so a frames collector correlates the refusal by
 * the `action_id` this VM assigns, exactly like the worktree force watch.
 */
class WorkspaceTabsViewModel(
    private val paneId: String,
    private val sessions: SessionRepository,
    private val workspaces: WorkspaceStore,
) : ViewModel() {

    private val relayId = paneId.substringBefore("::")

    /** Optimistic order scoped to the workspace it was issued against. */
    private data class PendingOrder(val workspaceId: String, val order: List<String>)

    /**
     * One in-flight close watch — mutations serialize behind `busy`, so a
     * single slot is enough (the worktree remove watch's precedent).
     */
    private class CloseWatch(
        val actionId: String,
        val refusal: CompletableDeferred<CloseRefusal>,
    )

    private val closeWatch = AtomicReference<CloseWatch?>()

    /** Directory-browser generation — a stale listing never lands. */
    private var directoryGeneration = 0

    private data class TabsLocal(
        val menuTabId: String? = null,
        val busy: Boolean = false,
        val error: String? = null,
        val status: String? = null,
        val statusError: Boolean = false,
        val pending: PendingOrder? = null,
        val renameOpen: Boolean = false,
        val renameDraft: String = "",
        val confirmClose: Boolean = false,
        val confirmGroup: Boolean = false,
        /** The captured close target — a workspace switch must not re-aim it. */
        val closeWorkspaceId: String = "",
        val closeWorkspaceLabel: String = "",
        val closeExpectedIds: List<String> = emptyList(),
        val groupMembers: List<String> = emptyList(),
        val createOpen: Boolean = false,
        val createCwd: String = "",
        val createLabel: String = "",
        val directoryOpen: Boolean = false,
        val directory: DirectoryListing? = null,
        val directoryLoading: Boolean = false,
        val directoryError: String? = null,
    )

    private val local = MutableStateFlow(TabsLocal())

    val uiState: StateFlow<WorkspaceTabsUiState> = combine(
        sessions.agents,
        sessions.agent(paneId),
        sessions.connection(relayId),
        workspaces.workspaces,
        local,
    ) { agents, self, connection, allWorkspaces, local ->
        val workspaceId = self?.workspaceId.orEmpty()
        var tabs = if (workspaceId.isEmpty()) {
            emptyList()
        } else {
            workspaceTabs(
                agents.filter { it.relayId == relayId && it.workspaceId == workspaceId },
            )
        }
        val pending = local.pending
        if (pending != null && pending.workspaceId == workspaceId) {
            val rank = pending.order.withIndex().associate { (i, id) -> id to i }
            tabs = tabs.sortedBy { rank[it.tabId] ?: Int.MAX_VALUE }
        }
        val workspace = allWorkspaces.firstOrNull {
            it.relayId == relayId && it.workspaceId == workspaceId
        }
        WorkspaceTabsUiState(
            tabs = tabs,
            // Grouping key of the viewed agent: `tab_id || pane_id`.
            activeTabId = self?.tabId?.ifEmpty { self.paneId },
            reorderAvailable = connection?.status == RelayStatus.CONNECTED &&
                connection.inventory.state == AgentInventoryState.READY &&
                connection.capabilities.contains(TAB_REORDER_CAPABILITY),
            canControl = sessions.canControl(relayId),
            managementAvailable = connection?.capabilities
                ?.contains(WORKSPACE_MANAGEMENT_CAPABILITY) == true,
            directoryBrowserAvailable = connection?.capabilities
                ?.contains(DIRECTORY_BROWSER_CAPABILITY) == true,
            menuTabId = local.menuTabId,
            workspaceLabel = workspace?.label.orEmpty(),
            busy = local.busy,
            status = local.status,
            statusError = local.statusError,
            error = local.error,
            renameOpen = local.renameOpen,
            renameDraft = local.renameDraft,
            confirmClose = local.confirmClose,
            confirmGroup = local.confirmGroup,
            groupMembers = local.groupMembers,
            createOpen = local.createOpen,
            createCwd = local.createCwd,
            createLabel = local.createLabel,
            directoryOpen = local.directoryOpen,
            directory = local.directory,
            directoryLoading = local.directoryLoading,
            directoryError = local.directoryError,
        )
    }.stateIn(
        viewModelScope,
        SharingStarted.WhileSubscribed(5_000),
        WorkspaceTabsUiState(),
    )

    init {
        // `pendingTabOrder` lifecycle — clear once the snapshot order equals
        // the optimistic one; a changed membership invalidates it outright.
        viewModelScope.launch {
            combine(sessions.agents, sessions.agent(paneId)) { agents, self ->
                agents to self
            }.collect { (agents, self) ->
                val pending = local.value.pending ?: return@collect
                val workspaceId = self?.workspaceId.orEmpty()
                if (workspaceId != pending.workspaceId) {
                    local.update { it.copy(pending = null) }
                    return@collect
                }
                val ids = workspaceTabs(
                    agents.filter { it.relayId == relayId && it.workspaceId == workspaceId },
                ).map { it.tabId }
                if (ids == pending.order ||
                    ids.size != pending.order.size ||
                    ids.toSet() != pending.order.toSet()
                ) {
                    local.update { it.copy(pending = null) }
                }
            }
        }
        // The oracle's close_group staleness effect — while the group
        // confirm is open, any drift between the expected id set and the
        // live tree cancels it.
        viewModelScope.launch {
            workspaces.workspaces.collect { list ->
                val snapshot = local.value
                if (!snapshot.confirmClose || !snapshot.confirmGroup) return@collect
                val current = workspaceGroupIds(list, relayId, snapshot.closeWorkspaceId)
                if (current != snapshot.closeExpectedIds) {
                    local.update {
                        it.copy(
                            confirmClose = false,
                            confirmGroup = false,
                            closeExpectedIds = emptyList(),
                            groupMembers = emptyList(),
                        )
                    }
                }
            }
        }
        // Close-refusal watch — `command_result.data.code` +
        // `data.workspace_ids` or `action_receipt.error.code`, correlated
        // by the `action_id` this VM assigns.
        viewModelScope.launch {
            sessions.frames.collect { frame ->
                if (frame.relayId != relayId) return@collect
                val watch = closeWatch.get() ?: return@collect
                when (val message = frame.message) {
                    is CommandResultMessage -> {
                        if (message.action == WORKSPACE_CLOSE && message.ok == false) {
                            val data = message.data as? JsonObject
                            val code = (data?.get("code") as? JsonPrimitive)
                                ?.contentOrNull ?: return@collect
                            val ids = (data?.get("workspace_ids") as? JsonArray)
                                ?.mapNotNull { (it as? JsonPrimitive)?.contentOrNull }
                                .orEmpty()
                            watch.refusal.complete(CloseRefusal(code, ids))
                        }
                    }
                    is ActionReceiptMessage -> {
                        val receipt = message.receipt ?: return@collect
                        if (receipt.actionId != watch.actionId) return@collect
                        val code = receipt.error?.code ?: return@collect
                        if (code.startsWith("workspace_group")) {
                            watch.refusal.complete(CloseRefusal(code, emptyList()))
                        }
                    }
                    else -> Unit
                }
            }
        }
    }

    private fun workspaceAgents(): Pair<String, List<Agent>> {
        val self = sessions.agents.value.firstOrNull { it.paneId == paneId }
        val workspaceId = self?.workspaceId.orEmpty()
        return workspaceId to sessions.agents.value.filter {
            it.relayId == relayId && it.workspaceId == workspaceId
        }
    }

    /** The tab's representative agent — `tab.agents[0]` in the oracle. */
    fun agentForTab(tabId: String): Agent? {
        val (_, agents) = workspaceAgents()
        return sortedAgents(
            agents.filter { it.tabId.ifEmpty { it.paneId } == tabId },
        ).firstOrNull()
    }

    /**
     * The oracle's `workspaceManagementAvailable('workspace_management')`
     * + the `INVENTORY_REQUIRED_COMMANDS` check `sendCommand` runs.
     */
    private fun requireWorkspaceManagement() {
        val connection = sessions.connectionNow(relayId)
        if (connection?.capabilities?.contains(WORKSPACE_MANAGEMENT_CAPABILITY) != true) {
            throw CommandException("This relay does not support workspace management")
        }
        val inventory = connection.inventory
        if (inventory.state != AgentInventoryState.READY) {
            throw CommandException(
                inventory.message.ifEmpty {
                    "Herdr agent inventory is not ready on this computer"
                },
            )
        }
    }

    /**
     * Long-press opens the tab's action menu — controllers only, and only
     * when at least one action (reorder or workspace ops) is offered.
     */
    fun openMenu(tabId: String) {
        val state = uiState.value
        if (!state.canControl) return
        val reorder = state.reorderAvailable && state.tabs.size > 1
        if (!reorder && !state.managementAvailable) return
        local.update { it.copy(menuTabId = tabId, error = null, status = null) }
    }

    fun dismissMenu() {
        local.update { it.copy(menuTabId = null) }
    }

    fun clearError() {
        local.update { it.copy(error = null) }
    }

    /**
     * `handleTabOrderKey` — move [tabId] one slot. Herdr's `insert_index`
     * addresses the pre-move list (the server shifts down by one when the
     * source sat before it): left = `index - 1`, right = `index + 2`.
     */
    fun moveTab(tabId: String, delta: Int) {
        val state = uiState.value
        if (state.busy || delta == 0 || !state.canControl) return
        if (!state.reorderAvailable) {
            local.update { it.copy(error = "This relay does not support tab ordering") }
            return
        }
        val tabs = state.tabs
        val index = tabs.indexOfFirst { it.tabId == tabId }
        if (index < 0 || tabs.getOrNull(index + delta) == null) return
        val insertIndex = if (delta > 0) index + 2 else index - 1
        val order = tabs.map { it.tabId }.toMutableList().apply {
            val moved = removeAt(index)
            add(index + delta, moved)
        }
        commitReorder(tabId, insertIndex, order)
    }

    /** `commitReorder` — optimistic order + `tab_reorder{insert_index}`. */
    private fun commitReorder(tabId: String, insertIndex: Int, order: List<String>) {
        val agent = agentForTab(tabId) ?: return
        val target = agent.wireTarget()
        if (target == null) {
            local.update {
                it.copy(error = "This agent no longer has an exact terminal identity")
            }
            return
        }
        val (workspaceId, _) = workspaceAgents()
        local.update {
            it.copy(
                busy = true,
                error = null,
                pending = PendingOrder(workspaceId, order),
            )
        }
        viewModelScope.launch {
            try {
                sessions.request(
                    relayId,
                    Inbound(type = "tab_reorder", insertIndex = insertIndex)
                        .withPaneTarget(agent, target),
                )
                local.update { it.copy(busy = false) }
            } catch (failure: Exception) {
                local.update {
                    it.copy(
                        busy = false,
                        pending = null,
                        error = failure.message ?: "Tab order could not be updated",
                    )
                }
            }
        }
    }

    // ── workspace rename ──────────────────────────────────────────────

    /** Menu "Rename workspace" — prefills the dialog with the live label. */
    fun requestRename() {
        val state = uiState.value
        if (!state.canControl || !state.managementAvailable || local.value.busy) return
        local.update {
            it.copy(
                menuTabId = null,
                renameOpen = true,
                renameDraft = state.workspaceLabel,
                status = null,
            )
        }
    }

    fun onRenameDraftChange(value: String) {
        local.update { it.copy(renameDraft = value.take(MAX_LABEL_RUNES)) }
    }

    fun dismissRename() {
        if (local.value.busy) return
        local.update { it.copy(renameOpen = false) }
    }

    /** `workspace_rename{workspace_id,label}` — the oracle's rename flow. */
    fun confirmRename() {
        val state = uiState.value
        if (local.value.busy || !state.canControl) return
        val self = sessions.agents.value.firstOrNull { it.paneId == paneId } ?: return
        val workspaceId = self.workspaceId.takeIf { it.isNotEmpty() } ?: return
        val label = state.renameDraft.trim()
        if (label.isEmpty()) return
        local.update { it.copy(busy = true, status = null) }
        viewModelScope.launch {
            try {
                sessions.renameWorkspace(relayId, workspaceId, label)
                local.update {
                    it.copy(
                        busy = false,
                        renameOpen = false,
                        status = "Renamed workspace $label.",
                        statusError = false,
                    )
                }
            } catch (failure: Exception) {
                local.update {
                    it.copy(
                        busy = false,
                        renameOpen = false,
                        status = failure.message ?: "Workspace could not be renamed",
                        statusError = true,
                    )
                }
            }
        }
    }

    // ── workspace close ───────────────────────────────────────────────

    /** Menu "Close workspace" — opens the destructive confirm. */
    fun requestClose() {
        val state = uiState.value
        if (!state.canControl || !state.managementAvailable || local.value.busy) return
        val self = sessions.agents.value.firstOrNull { it.paneId == paneId } ?: return
        val workspaceId = self.workspaceId.takeIf { it.isNotEmpty() } ?: return
        local.update {
            it.copy(
                menuTabId = null,
                confirmClose = true,
                confirmGroup = false,
                closeWorkspaceId = workspaceId,
                closeWorkspaceLabel = state.workspaceLabel,
                closeExpectedIds = emptyList(),
                groupMembers = emptyList(),
                status = null,
            )
        }
    }

    fun dismissClose() {
        if (local.value.busy) return
        local.update {
            it.copy(
                confirmClose = false,
                confirmGroup = false,
                closeExpectedIds = emptyList(),
                groupMembers = emptyList(),
            )
        }
    }

    /**
     * `confirmAction` for `close`/`close_group` — `workspace_close`. A
     * `workspace_group_close_required` refusal switches the dialog into
     * group mode with the relay's id set (or the locally computed tree);
     * `workspace_group_changed`/`workspace_group_consent_invalid` cancel
     * with the oracle's review message.
     */
    fun confirmClose() {
        val state = uiState.value
        if (local.value.busy || !state.confirmClose || !state.canControl) return
        val workspaceId = local.value.closeWorkspaceId.takeIf { it.isNotEmpty() }
            ?: return
        val closeGroup = state.confirmGroup
        val expectedIds = local.value.closeExpectedIds
        val actionId = "workspace-close-${UUID.randomUUID()}"
        val watch = CloseWatch(actionId, CompletableDeferred())
        closeWatch.set(watch)
        local.update { it.copy(busy = true, status = null) }
        viewModelScope.launch {
            try {
                requireWorkspaceManagement()
                sessions.request(
                    relayId,
                    Inbound(
                        type = WORKSPACE_CLOSE,
                        workspaceId = workspaceId,
                        closeGroup = closeGroup,
                        expectedWorkspaceIds = if (closeGroup) expectedIds else emptyList(),
                        actionId = actionId,
                    ),
                    timeoutMs = WORKSPACE_CLOSE_TIMEOUT_MS,
                )
                sessions.refreshAgents()
                local.update {
                    it.copy(
                        busy = false,
                        confirmClose = false,
                        confirmGroup = false,
                        closeExpectedIds = emptyList(),
                        groupMembers = emptyList(),
                        status = "Closed workspace" +
                            (if (closeGroup) " group" else "") +
                            " ${it.closeWorkspaceLabel}.",
                        statusError = false,
                    )
                }
            } catch (failure: Exception) {
                // The refusal frames land on `frames` before the request's
                // deferred settles — the await just yields a slot.
                val refusal = withTimeoutOrNull(RECEIPT_GRACE_MS) {
                    watch.refusal.await()
                }
                local.update { it.copy(busy = false) }
                when (refusal?.code) {
                    "workspace_group_close_required" -> {
                        val ids = refusal.workspaceIds.ifEmpty {
                            workspaceGroupIds(
                                workspaces.workspaces.value,
                                relayId,
                                workspaceId,
                            )
                        }
                        if (ids.size < 2) {
                            local.update {
                                it.copy(
                                    confirmClose = false,
                                    status = "Workspace group inventory is stale. " +
                                        "Refresh and try again.",
                                    statusError = true,
                                )
                            }
                        } else {
                            val labels = ids.map { id ->
                                workspaces.workspaces.value.firstOrNull {
                                    it.relayId == relayId && it.workspaceId == id
                                }?.label ?: id
                            }
                            local.update {
                                it.copy(
                                    confirmClose = true,
                                    confirmGroup = true,
                                    closeExpectedIds = ids,
                                    groupMembers = labels,
                                    status = "Review the workspace group " +
                                        "before closing it.",
                                    statusError = false,
                                )
                            }
                        }
                    }
                    "workspace_group_changed",
                    "workspace_group_consent_invalid" -> local.update {
                        it.copy(
                            confirmClose = false,
                            confirmGroup = false,
                            closeExpectedIds = emptyList(),
                            groupMembers = emptyList(),
                            status = "Workspace group changed. Review the " +
                                "current group before closing it.",
                            statusError = true,
                        )
                    }
                    else -> {
                        val message = failure.message ?: "Workspace could not be closed"
                        val ambiguous =
                            (failure as? CommandException)?.dispatchedUnknown == true
                        local.update {
                            it.copy(
                                confirmClose = false,
                                confirmGroup = false,
                                closeExpectedIds = emptyList(),
                                groupMembers = emptyList(),
                                status = if (ambiguous) {
                                    "$message Check the workspace list before retrying."
                                } else {
                                    message
                                },
                                statusError = true,
                            )
                        }
                    }
                }
            } finally {
                closeWatch.compareAndSet(watch, null)
            }
        }
    }

    // ── workspace create ──────────────────────────────────────────────

    /** The "+" chip — opens the create sheet and primes the browser. */
    fun requestCreate() {
        val state = uiState.value
        if (!state.canControl || !state.managementAvailable || local.value.busy) return
        local.update {
            it.copy(
                createOpen = true,
                createCwd = "",
                createLabel = "",
                directoryOpen = false,
                directory = null,
                directoryError = null,
                status = null,
            )
        }
        if (state.directoryBrowserAvailable) loadDirectory("")
    }

    fun dismissCreate() {
        if (local.value.busy) return
        local.update { it.copy(createOpen = false, directoryOpen = false) }
    }

    fun onCreateCwdChange(value: String) {
        local.update { it.copy(createCwd = value) }
    }

    fun onCreateLabelChange(value: String) {
        local.update { it.copy(createLabel = value.take(MAX_LABEL_RUNES)) }
    }

    fun toggleDirectoryBrowser() {
        local.update { it.copy(directoryOpen = !it.directoryOpen) }
    }

    /**
     * `loadDirectory` — `list_directories{path}` behind the
     * `directory_browser` capability; the result seeds the cwd field and,
     * while the label is untouched, `pathBase` fills it (oracle parity).
     */
    fun loadDirectory(path: String) {
        if (!uiState.value.directoryBrowserAvailable) return
        val generation = ++directoryGeneration
        local.update { it.copy(directoryLoading = true, directoryError = null) }
        viewModelScope.launch {
            try {
                val listing = sessions.listDirectories(relayId, path)
                if (generation != directoryGeneration) return@launch
                local.update {
                    it.copy(
                        directoryLoading = false,
                        directory = listing,
                        directoryError = null,
                        createCwd = listing.currentPath,
                        createLabel = it.createLabel.ifEmpty {
                            pathBaseOf(listing.currentPath)
                        },
                    )
                }
            } catch (failure: Exception) {
                if (generation != directoryGeneration) return@launch
                local.update {
                    it.copy(
                        directoryLoading = false,
                        directoryError = failure.message
                            ?: "Directories could not be listed",
                    )
                }
            }
        }
    }

    /**
     * `workspace_create{cwd,label}` (45 s window) — the oracle closes the
     * dialog on success and, on `dispatched_unknown`, refuses to leave it
     * primed for a blind retry.
     */
    fun confirmCreate() {
        val state = uiState.value
        if (local.value.busy || !state.canControl) return
        val cwd = state.createCwd.trim()
        val label = state.createLabel.trim()
        if (cwd.isEmpty() || label.isEmpty()) return
        local.update { it.copy(busy = true, status = null) }
        viewModelScope.launch {
            try {
                sessions.createWorkspace(relayId, cwd, label)
                local.update {
                    it.copy(
                        busy = false,
                        createOpen = false,
                        createCwd = "",
                        createLabel = "",
                        directoryOpen = false,
                        status = "Created workspace $label.",
                        statusError = false,
                    )
                }
            } catch (failure: Exception) {
                val ambiguous =
                    (failure as? CommandException)?.dispatchedUnknown == true
                local.update {
                    it.copy(
                        busy = false,
                        createOpen = !ambiguous,
                        directoryOpen = false,
                        status = failure.message?.let { message ->
                            if (ambiguous) {
                                "$message Check the workspace list before retrying."
                            } else {
                                message
                            }
                        } ?: "Workspace could not be created",
                        statusError = true,
                    )
                }
            }
        }
    }

    companion object {
        const val TAB_REORDER_CAPABILITY = "tab_reorder"
        const val WORKSPACE_MANAGEMENT_CAPABILITY = "workspace_management"
        const val DIRECTORY_BROWSER_CAPABILITY = "directory_browser"
        const val WORKSPACE_CLOSE = "workspace_close"
        const val WORKSPACE_CLOSE_TIMEOUT_MS = 30_000L

        /**
         * Grace for the refusal frame to land after the request threw —
         * the frame is already buffered, this only needs one scheduling
         * slot on a healthy relay.
         */
        const val RECEIPT_GRACE_MS = 1_000L

        /** Oracle `maxlength` for workspace labels. */
        const val MAX_LABEL_RUNES = 128
    }
}

/**
 * Singleton seam for the strip — `SessionRepository` + `WorkspaceStore`
 * (the strip needs the workspace rows for labels and linked-worktree
 * groups).
 */
@EntryPoint
@InstallIn(SingletonComponent::class)
interface WorkspaceTabsEntryPoint {
    fun sessionRepository(): SessionRepository
    fun workspaceStore(): WorkspaceStore
}

/**
 * Horizontal strip of the current agent's workspace tabs — select switches
 * the session; long-press a chip (controller only) opens its action menu:
 * move left/right (`tab_reorder`), rename the workspace
 * (`workspace_rename`), or close it (`workspace_close`, with the oracle's
 * linked-worktree group confirm). A trailing "+" chip opens the
 * `workspace_create` sheet with the `list_directories` browser when the
 * relay advertises `directory_browser`.
 *
 * Renders nothing for workspace-less agents.
 *
 * Wiring: place under `SessionTopBar` in the session screens —
 * `WorkspaceTabsStrip(paneId = paneId, onSelectTab = { navigator.openAgent(it.paneId) })`.
 */
@Composable
fun WorkspaceTabsStrip(
    paneId: String,
    onSelectTab: (Agent) -> Unit,
    modifier: Modifier = Modifier,
) {
    val appContext = LocalContext.current.applicationContext
    val viewModel: WorkspaceTabsViewModel = viewModel(key = "workspace-tabs:$paneId") {
        val entryPoint =
            EntryPointAccessors.fromApplication(appContext, WorkspaceTabsEntryPoint::class.java)
        WorkspaceTabsViewModel(
            paneId,
            entryPoint.sessionRepository(),
            entryPoint.workspaceStore(),
        )
    }
    val uiState by viewModel.uiState.collectAsStateWithLifecycle()
    WorkspaceTabsStripContent(
        uiState = uiState,
        onSelectTab = { tab -> viewModel.agentForTab(tab.tabId)?.let(onSelectTab) },
        onOpenMenu = { tab -> viewModel.openMenu(tab.tabId) },
        onDismissMenu = viewModel::dismissMenu,
        onMoveTab = viewModel::moveTab,
        onRequestRename = viewModel::requestRename,
        onRenameDraftChange = viewModel::onRenameDraftChange,
        onConfirmRename = viewModel::confirmRename,
        onDismissRename = viewModel::dismissRename,
        onRequestClose = viewModel::requestClose,
        onConfirmClose = viewModel::confirmClose,
        onDismissClose = viewModel::dismissClose,
        onRequestCreate = viewModel::requestCreate,
        onDismissCreate = viewModel::dismissCreate,
        onCreateCwdChange = viewModel::onCreateCwdChange,
        onCreateLabelChange = viewModel::onCreateLabelChange,
        onToggleDirectoryBrowser = viewModel::toggleDirectoryBrowser,
        onBrowseDirectory = viewModel::loadDirectory,
        onConfirmCreate = viewModel::confirmCreate,
        modifier = modifier,
    )
}

/** Stateless strip — state in, events out (screenshots + previews). */
@OptIn(ExperimentalFoundationApi::class, ExperimentalMaterial3Api::class)
@Composable
fun WorkspaceTabsStripContent(
    uiState: WorkspaceTabsUiState,
    onSelectTab: (WorkspaceTabUi) -> Unit,
    onOpenMenu: (WorkspaceTabUi) -> Unit,
    onDismissMenu: () -> Unit,
    onMoveTab: (tabId: String, delta: Int) -> Unit,
    onRequestRename: () -> Unit,
    onRenameDraftChange: (String) -> Unit,
    onConfirmRename: () -> Unit,
    onDismissRename: () -> Unit,
    onRequestClose: () -> Unit,
    onConfirmClose: () -> Unit,
    onDismissClose: () -> Unit,
    onRequestCreate: () -> Unit,
    onDismissCreate: () -> Unit,
    onCreateCwdChange: (String) -> Unit,
    onCreateLabelChange: (String) -> Unit,
    onToggleDirectoryBrowser: () -> Unit,
    onBrowseDirectory: (String) -> Unit,
    onConfirmCreate: () -> Unit,
    modifier: Modifier = Modifier,
) {
    if (uiState.tabs.isEmpty()) return
    val spacing = LerdrTheme.spacing
    val colors = LerdrTheme.extendedColors
    val reordering = uiState.canControl && uiState.reorderAvailable && uiState.tabs.size > 1
    Column(modifier = modifier.fillMaxWidth()) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(spacing.small),
            modifier = Modifier
                .fillMaxWidth()
                .horizontalScroll(rememberScrollState())
                .padding(horizontal = spacing.medium, vertical = spacing.extraSmall)
                .testTag(WorkspaceTabsStripTags.STRIP),
        ) {
            uiState.tabs.forEach { tab ->
                Box {
                    TabChip(
                        tab = tab,
                        active = tab.tabId == uiState.activeTabId,
                        menuEnabled = uiState.canControl,
                        busy = uiState.busy,
                        onSelect = { onSelectTab(tab) },
                        onOpenMenu = { onOpenMenu(tab) },
                    )
                    DropdownMenu(
                        expanded = uiState.menuTabId == tab.tabId,
                        onDismissRequest = onDismissMenu,
                    ) {
                        if (reordering) {
                            val canMoveLeft =
                                uiState.tabs.firstOrNull()?.tabId != tab.tabId
                            val canMoveRight =
                                uiState.tabs.lastOrNull()?.tabId != tab.tabId
                            DropdownMenuItem(
                                text = { Text("Move left") },
                                leadingIcon = {
                                    Icon(
                                        Icons.AutoMirrored.Filled.KeyboardArrowLeft,
                                        contentDescription = null,
                                    )
                                },
                                enabled = canMoveLeft && !uiState.busy,
                                onClick = { onMoveTab(tab.tabId, -1) },
                                modifier = Modifier.testTag(
                                    WorkspaceTabsStripTags.moveLeft(tab.tabId),
                                ),
                            )
                            DropdownMenuItem(
                                text = { Text("Move right") },
                                leadingIcon = {
                                    Icon(
                                        Icons.AutoMirrored.Filled.KeyboardArrowRight,
                                        contentDescription = null,
                                    )
                                },
                                enabled = canMoveRight && !uiState.busy,
                                onClick = { onMoveTab(tab.tabId, 1) },
                                modifier = Modifier.testTag(
                                    WorkspaceTabsStripTags.moveRight(tab.tabId),
                                ),
                            )
                            if (uiState.managementAvailable) {
                                HorizontalDivider()
                            }
                        }
                        if (uiState.managementAvailable) {
                            DropdownMenuItem(
                                text = { Text("Rename workspace") },
                                enabled = !uiState.busy,
                                onClick = onRequestRename,
                                modifier = Modifier.testTag(
                                    WorkspaceTabsStripTags.RENAME,
                                ),
                            )
                            DropdownMenuItem(
                                text = {
                                    Text(
                                        "Close workspace",
                                        color = colors.danger,
                                    )
                                },
                                enabled = !uiState.busy,
                                onClick = onRequestClose,
                                modifier = Modifier.testTag(
                                    WorkspaceTabsStripTags.CLOSE,
                                ),
                            )
                        }
                    }
                }
            }
            if (uiState.canControl && uiState.managementAvailable) {
                Surface(
                    color = MaterialTheme.colorScheme.surfaceContainerHighest,
                    contentColor = MaterialTheme.colorScheme.onSurfaceVariant,
                    shape = MaterialTheme.shapes.small,
                    modifier = Modifier
                        .testTag(WorkspaceTabsStripTags.CREATE)
                        .combinedClickable(
                            onClickLabel = "New workspace",
                            onClick = onRequestCreate,
                        ),
                ) {
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier.padding(
                            horizontal = spacing.small,
                            vertical = spacing.small,
                        ),
                    ) {
                        Icon(
                            Icons.Default.Add,
                            contentDescription = null,
                            modifier = Modifier.size(16.dp),
                        )
                        Spacer(Modifier.width(spacing.extraSmall))
                        Text("New", style = MaterialTheme.typography.labelMedium)
                    }
                }
            }
        }
        uiState.error?.let { error ->
            Text(
                error,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier
                    .padding(horizontal = spacing.medium)
                    .testTag(WorkspaceTabsStripTags.ERROR),
            )
        }
        uiState.status?.let { status ->
            Text(
                status,
                style = MaterialTheme.typography.labelSmall,
                color = if (uiState.statusError) {
                    colors.danger
                } else {
                    MaterialTheme.colorScheme.onSurfaceVariant
                },
                modifier = Modifier
                    .padding(horizontal = spacing.medium)
                    .semantics { liveRegion = LiveRegionMode.Polite }
                    .testTag(WorkspaceTabsStripTags.STATUS),
            )
        }
    }

    if (uiState.renameOpen) {
        WorkspaceRenameDialog(
            draft = uiState.renameDraft,
            busy = uiState.busy,
            onDraftChange = onRenameDraftChange,
            onConfirm = onConfirmRename,
            onDismiss = onDismissRename,
        )
    }
    if (uiState.confirmClose) {
        WorkspaceCloseDialog(
            uiState = uiState,
            onConfirm = onConfirmClose,
            onDismiss = onDismissClose,
        )
    }
    if (uiState.createOpen) {
        ModalBottomSheet(onDismissRequest = onDismissCreate) {
            WorkspaceCreateContent(
                uiState = uiState,
                onCwdChange = onCreateCwdChange,
                onLabelChange = onCreateLabelChange,
                onToggleDirectoryBrowser = onToggleDirectoryBrowser,
                onBrowseDirectory = onBrowseDirectory,
                onConfirm = onConfirmCreate,
                onCancel = onDismissCreate,
            )
        }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun TabChip(
    tab: WorkspaceTabUi,
    active: Boolean,
    menuEnabled: Boolean,
    busy: Boolean,
    onSelect: () -> Unit,
    onOpenMenu: () -> Unit,
) {
    val spacing = LerdrTheme.spacing
    val container = if (active) {
        MaterialTheme.colorScheme.secondaryContainer
    } else {
        MaterialTheme.colorScheme.surfaceContainerHighest
    }
    val content = if (active) {
        MaterialTheme.colorScheme.onSecondaryContainer
    } else {
        MaterialTheme.colorScheme.onSurfaceVariant
    }
    Surface(
        color = container,
        contentColor = content,
        shape = MaterialTheme.shapes.small,
        modifier = Modifier
            .testTag(WorkspaceTabsStripTags.tab(tab.tabId))
            .combinedClickable(
                onClick = onSelect,
                onLongClick = { if (menuEnabled && !busy) onOpenMenu() },
            ),
    ) {
        Text(
            if (tab.paneCount > 1) "${tab.label} ·${tab.paneCount}" else tab.label,
            style = MaterialTheme.typography.labelMedium,
            fontWeight = if (active) FontWeight.Bold else FontWeight.Normal,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.padding(
                horizontal = spacing.small + spacing.extraSmall,
                vertical = spacing.small,
            ),
        )
    }
}

/** `workspace_rename` dialog — the oracle's rename flow. */
@Composable
private fun WorkspaceRenameDialog(
    draft: String,
    busy: Boolean,
    onDraftChange: (String) -> Unit,
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Rename workspace") },
        text = {
            OutlinedTextField(
                value = draft,
                onValueChange = onDraftChange,
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag(WorkspaceTabsStripTags.RENAME_FIELD),
                label = { Text("Workspace label") },
                singleLine = true,
                enabled = !busy,
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
                keyboardActions = KeyboardActions(onDone = { onConfirm() }),
            )
        },
        confirmButton = {
            TextButton(
                onClick = onConfirm,
                enabled = !busy && draft.isNotBlank(),
                modifier = Modifier.testTag(WorkspaceTabsStripTags.RENAME_CONFIRM),
            ) {
                Text("Rename")
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss, enabled = !busy) {
                Text("Cancel")
            }
        },
    )
}

/**
 * The oracle's `workspace-destructive-dialog` — solo close or, after a
 * `workspace_group_close_required` refusal, the group variant listing the
 * members that will close.
 */
@Composable
private fun WorkspaceCloseDialog(
    uiState: WorkspaceTabsUiState,
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
) {
    val colors = LerdrTheme.extendedColors
    AlertDialog(
        onDismissRequest = onDismiss,
        title = {
            Text(
                if (uiState.confirmGroup) {
                    "Close ${uiState.workspaceLabel} group?"
                } else {
                    "Close ${uiState.workspaceLabel.ifBlank { "workspace" }}?"
                },
            )
        },
        text = {
            Column(
                verticalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.small),
            ) {
                Text(
                    if (uiState.confirmGroup) {
                        "All running panes in the currently open group will " +
                            "close. Group membership can change until the " +
                            "command runs. Git checkouts and branches are " +
                            "not removed."
                    } else {
                        "Every pane in this workspace will close. Git " +
                            "checkouts are not removed."
                    },
                    style = MaterialTheme.typography.bodyMedium,
                )
                if (uiState.confirmGroup && uiState.groupMembers.isNotEmpty()) {
                    Column(
                        verticalArrangement = Arrangement.spacedBy(
                            LerdrTheme.spacing.extraSmall,
                        ),
                        modifier = Modifier.testTag(WorkspaceTabsStripTags.GROUP_LIST),
                    ) {
                        uiState.groupMembers.forEach { member ->
                            Text(
                                "• $member",
                                style = MaterialTheme.typography.bodyMedium,
                                fontWeight = FontWeight.Bold,
                            )
                        }
                    }
                }
            }
        },
        confirmButton = {
            Button(
                onClick = onConfirm,
                enabled = !uiState.busy,
                colors = ButtonDefaults.buttonColors(
                    containerColor = colors.danger,
                    contentColor = colors.onDanger,
                ),
                modifier = Modifier.testTag(WorkspaceTabsStripTags.CLOSE_CONFIRM),
            ) {
                Text(
                    if (uiState.confirmGroup) {
                        "Close Workspace Group"
                    } else {
                        "Close Workspace"
                    },
                )
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss, enabled = !uiState.busy) {
                Text("Cancel")
            }
        },
    )
}

/**
 * `workspace_create` sheet — the oracle's create dialog: a working
 * directory (the `list_directories` browser when the relay advertises
 * `directory_browser`, a plain text field otherwise) plus the label
 * (`pathBase` auto-fills while untouched).
 */
@Composable
fun WorkspaceCreateContent(
    uiState: WorkspaceTabsUiState,
    onCwdChange: (String) -> Unit,
    onLabelChange: (String) -> Unit,
    onToggleDirectoryBrowser: () -> Unit,
    onBrowseDirectory: (String) -> Unit,
    onConfirm: () -> Unit,
    onCancel: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val spacing = LerdrTheme.spacing
    Column(
        modifier = modifier
            .fillMaxWidth()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = spacing.large)
            .padding(bottom = spacing.extraLarge)
            .testTag(WorkspaceTabsStripTags.CREATE_CONTENT),
        verticalArrangement = Arrangement.spacedBy(spacing.medium),
    ) {
        Column(verticalArrangement = Arrangement.spacedBy(spacing.extraSmall)) {
            Text("Create workspace", style = MaterialTheme.typography.titleLarge)
            Text(
                "Create an empty Herdr workspace with its initial tab. " +
                    "Start an agent later to add a second tab.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }

        Column(verticalArrangement = Arrangement.spacedBy(spacing.extraSmall)) {
            Text(
                "Working directory",
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (uiState.directoryBrowserAvailable) {
                DirectoryBrowser(uiState, onToggleDirectoryBrowser, onBrowseDirectory)
            } else {
                OutlinedTextField(
                    value = uiState.createCwd,
                    onValueChange = onCwdChange,
                    modifier = Modifier
                        .fillMaxWidth()
                        .testTag(WorkspaceTabsStripTags.CREATE_CWD),
                    placeholder = { Text("/home/user/project") },
                    singleLine = true,
                    enabled = !uiState.busy,
                )
            }
        }

        OutlinedTextField(
            value = uiState.createLabel,
            onValueChange = onLabelChange,
            modifier = Modifier
                .fillMaxWidth()
                .testTag(WorkspaceTabsStripTags.CREATE_LABEL),
            label = { Text("Label") },
            singleLine = true,
            enabled = !uiState.busy,
            keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
            keyboardActions = KeyboardActions(onDone = { onConfirm() }),
        )

        Row(horizontalArrangement = Arrangement.spacedBy(spacing.small)) {
            OutlinedButton(
                onClick = onCancel,
                enabled = !uiState.busy,
                modifier = Modifier.weight(1f),
            ) {
                Text("Cancel")
            }
            Button(
                onClick = onConfirm,
                enabled = !uiState.busy &&
                    uiState.createCwd.isNotBlank() &&
                    uiState.createLabel.isNotBlank(),
                modifier = Modifier
                    .weight(1f)
                    .testTag(WorkspaceTabsStripTags.CREATE_CONFIRM),
            ) {
                Text("Confirm")
            }
        }
    }
}

/**
 * The oracle's directory browser — a toolbar (↑ parent, current folder
 * toggle) over a subdirectory list; selecting a row descends and seeds
 * the workspace label from the path base.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun DirectoryBrowser(
    uiState: WorkspaceTabsUiState,
    onToggle: () -> Unit,
    onBrowse: (String) -> Unit,
) {
    val spacing = LerdrTheme.spacing
    Column(modifier = Modifier.testTag(WorkspaceTabsStripTags.DIR_BROWSER)) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(spacing.extraSmall),
        ) {
            IconButton(
                onClick = { uiState.directory?.parent?.let(onBrowse) },
                enabled = !uiState.directory?.parent.isNullOrEmpty() &&
                    !uiState.directoryLoading,
            ) {
                Icon(
                    Icons.Default.KeyboardArrowUp,
                    contentDescription = "Parent folder",
                )
            }
            Surface(
                color = MaterialTheme.colorScheme.surfaceContainerHighest,
                shape = MaterialTheme.shapes.small,
                modifier = Modifier
                    .weight(1f)
                    .combinedClickable(
                        onClickLabel = "Browse folders",
                        onClick = onToggle,
                    ),
            ) {
                Text(
                    uiState.directory?.currentLabel
                        ?: uiState.createCwd.ifEmpty {
                            if (uiState.directoryLoading) "Loading…" else "Unavailable"
                        },
                    style = MaterialTheme.typography.bodyMedium,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.padding(
                        horizontal = spacing.small,
                        vertical = spacing.small,
                    ),
                )
            }
        }
        if (uiState.directoryOpen) {
            when {
                uiState.directoryLoading -> Text(
                    "Loading folders…",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(start = spacing.medium),
                )
                uiState.directoryError != null -> Text(
                    uiState.directoryError,
                    style = MaterialTheme.typography.bodySmall,
                    color = LerdrTheme.extendedColors.danger,
                    modifier = Modifier
                        .padding(start = spacing.medium)
                        .testTag(WorkspaceTabsStripTags.DIR_ERROR),
                )
                else -> Column(
                    modifier = Modifier
                        .heightIn(max = 240.dp)
                        .verticalScroll(rememberScrollState()),
                ) {
                    uiState.directory?.parent?.takeIf { it.isNotEmpty() }?.let { parent ->
                        DirectoryRow(
                            label = "↰ Parent folder",
                            onClick = { onBrowse(parent) },
                        )
                    }
                    uiState.directory?.directories?.forEach { entry ->
                        DirectoryRow(
                            label = entry.name,
                            onClick = { onBrowse(entry.path) },
                            tag = WorkspaceTabsStripTags.dirEntry(entry.path),
                        )
                    }
                    if (uiState.directory?.directories?.isEmpty() == true) {
                        Text(
                            "No subdirectories",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.padding(
                                horizontal = spacing.medium,
                                vertical = spacing.small,
                            ),
                        )
                    }
                }
            }
        }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun DirectoryRow(
    label: String,
    onClick: () -> Unit,
    tag: String? = null,
) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .then(if (tag != null) Modifier.testTag(tag) else Modifier)
            .combinedClickable(onClick = onClick)
            .padding(
                horizontal = LerdrTheme.spacing.medium,
                vertical = LerdrTheme.spacing.small,
            ),
    ) {
        Icon(
            Icons.Default.Folder,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.size(18.dp),
        )
        Spacer(Modifier.width(LerdrTheme.spacing.small))
        Text(
            label,
            style = MaterialTheme.typography.bodyMedium,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

/** Stable semantics keys for tests and the screenshot suite. */
object WorkspaceTabsStripTags {
    const val STRIP = "workspace-tabs:strip"
    const val ERROR = "workspace-tabs:error"
    const val STATUS = "workspace-tabs:status"
    const val RENAME = "workspace-tabs:rename"
    const val CLOSE = "workspace-tabs:close"
    const val RENAME_FIELD = "workspace-tabs:rename-field"
    const val RENAME_CONFIRM = "workspace-tabs:rename-confirm"
    const val CLOSE_CONFIRM = "workspace-tabs:close-confirm"
    const val GROUP_LIST = "workspace-tabs:group-list"
    const val CREATE = "workspace-tabs:create"
    const val CREATE_CONTENT = "workspace-tabs:create-content"
    const val CREATE_CWD = "workspace-tabs:create-cwd"
    const val CREATE_LABEL = "workspace-tabs:create-label"
    const val CREATE_CONFIRM = "workspace-tabs:create-confirm"
    const val DIR_BROWSER = "workspace-tabs:dir-browser"
    const val DIR_ERROR = "workspace-tabs:dir-error"
    fun tab(tabId: String) = "workspace-tabs:tab:$tabId"
    fun moveLeft(tabId: String) = "workspace-tabs:move-left:$tabId"
    fun moveRight(tabId: String) = "workspace-tabs:move-right:$tabId"
    fun dirEntry(path: String) = "workspace-tabs:dir:$path"
}

@PreviewLightDark
@Composable
private fun WorkspaceTabsStripContentPreview() {
    LerdrTheme {
        Surface {
            WorkspaceTabsStripContent(
                uiState = WorkspaceTabsUiState(
                    tabs = listOf(
                        WorkspaceTabUi("t1", "main", 1, 0, "r1::%1", 1),
                        WorkspaceTabUi("t2", "tests", 2, 1, "r1::%2", 2),
                        WorkspaceTabUi("t3", "review", 3, 2, "r1::%3", 1),
                    ),
                    activeTabId = "t1",
                    reorderAvailable = true,
                    canControl = true,
                    managementAvailable = true,
                ),
                onSelectTab = {},
                onOpenMenu = {},
                onDismissMenu = {},
                onMoveTab = { _, _ -> },
                onRequestRename = {},
                onRenameDraftChange = {},
                onConfirmRename = {},
                onDismissRename = {},
                onRequestClose = {},
                onConfirmClose = {},
                onDismissClose = {},
                onRequestCreate = {},
                onDismissCreate = {},
                onCreateCwdChange = {},
                onCreateLabelChange = {},
                onToggleDirectoryBrowser = {},
                onBrowseDirectory = {},
                onConfirmCreate = {},
            )
        }
    }
}
