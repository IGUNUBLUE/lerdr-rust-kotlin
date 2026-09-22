package com.lerdr.app.session

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowLeft
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
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
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import lerdr.core.model.Inbound
import lerdr.core.store.Agent
import lerdr.core.store.AgentInventoryState
import lerdr.core.store.RelayStatus
import lerdr.core.store.sortedAgents

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
    /** Chip currently in reorder mode (long-press toggles). */
    val reorderTabId: String? = null,
    val busy: Boolean = false,
    /** Inline failure text — the oracle routes these to a toast. */
    val error: String? = null,
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
 * Per-workspace tab strip state — watches the viewed agent's workspace tabs
 * and owns `tab_reorder`. Ports `pendingTabOrder`: an optimistic ordering is
 * applied until the relay's `agents` snapshot confirms it (or a membership
 * change invalidates it); a failed send reverts immediately.
 */
class WorkspaceTabsViewModel(
    private val paneId: String,
    private val sessions: SessionRepository,
) : ViewModel() {

    private val relayId = paneId.substringBefore("::")

    /** Optimistic order scoped to the workspace it was issued against. */
    private data class PendingOrder(val workspaceId: String, val order: List<String>)

    private data class TabsLocal(
        val reorderTabId: String? = null,
        val busy: Boolean = false,
        val error: String? = null,
        val pending: PendingOrder? = null,
    )

    private val local = MutableStateFlow(TabsLocal())

    val uiState: StateFlow<WorkspaceTabsUiState> = combine(
        sessions.agents,
        sessions.agent(paneId),
        sessions.connection(relayId),
        local,
    ) { agents, self, connection, local ->
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
        WorkspaceTabsUiState(
            tabs = tabs,
            // Grouping key of the viewed agent: `tab_id || pane_id`.
            activeTabId = self?.tabId?.ifEmpty { self.paneId },
            reorderAvailable = connection?.status == RelayStatus.CONNECTED &&
                connection.inventory.state == AgentInventoryState.READY &&
                connection.capabilities.contains(TAB_REORDER_CAPABILITY),
            reorderTabId = local.reorderTabId,
            busy = local.busy,
            error = local.error,
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

    /** Long-press toggles reorder mode on a chip; taps elsewhere leave it. */
    fun toggleReorder(tabId: String) {
        if (!uiState.value.reorderAvailable) return
        local.update {
            it.copy(reorderTabId = if (it.reorderTabId == tabId) null else tabId)
        }
    }

    fun clearReorder() {
        local.update { it.copy(reorderTabId = null) }
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
        if (state.busy || delta == 0) return
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

    companion object {
        const val TAB_REORDER_CAPABILITY = "tab_reorder"
    }
}

/**
 * Singleton seam for the strip — `SessionRepository` is the only dependency
 * (tabs ride the shared `agents` flow; no `WorkspaceStore` needed).
 */
@EntryPoint
@InstallIn(SingletonComponent::class)
interface WorkspaceTabsEntryPoint {
    fun sessionRepository(): SessionRepository
}

/**
 * Horizontal strip of the current agent's workspace tabs — select switches
 * the session; long-press a chip to expose move-left/right affordances that
 * send `tab_reorder`. Renders nothing for workspace-less agents.
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
        WorkspaceTabsViewModel(paneId, entryPoint.sessionRepository())
    }
    val uiState by viewModel.uiState.collectAsStateWithLifecycle()
    WorkspaceTabsStripContent(
        uiState = uiState,
        onSelectTab = { tab -> viewModel.agentForTab(tab.tabId)?.let(onSelectTab) },
        onToggleReorder = { tab -> viewModel.toggleReorder(tab.tabId) },
        onMoveTab = viewModel::moveTab,
        modifier = modifier,
    )
}

/** Stateless strip — state in, events out (screenshots + previews). */
@Composable
fun WorkspaceTabsStripContent(
    uiState: WorkspaceTabsUiState,
    onSelectTab: (WorkspaceTabUi) -> Unit,
    onToggleReorder: (WorkspaceTabUi) -> Unit,
    onMoveTab: (tabId: String, delta: Int) -> Unit,
    modifier: Modifier = Modifier,
) {
    if (uiState.tabs.isEmpty()) return
    val spacing = LerdrTheme.spacing
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
                TabChip(
                    tab = tab,
                    active = tab.tabId == uiState.activeTabId,
                    reordering = uiState.reorderTabId == tab.tabId,
                    reorderAvailable = uiState.reorderAvailable && uiState.tabs.size > 1,
                    busy = uiState.busy,
                    canMoveLeft = uiState.tabs.firstOrNull()?.tabId != tab.tabId,
                    canMoveRight = uiState.tabs.lastOrNull()?.tabId != tab.tabId,
                    onSelect = { onSelectTab(tab) },
                    onToggleReorder = { onToggleReorder(tab) },
                    onMove = { delta -> onMoveTab(tab.tabId, delta) },
                )
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
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun TabChip(
    tab: WorkspaceTabUi,
    active: Boolean,
    reordering: Boolean,
    reorderAvailable: Boolean,
    busy: Boolean,
    canMoveLeft: Boolean,
    canMoveRight: Boolean,
    onSelect: () -> Unit,
    onToggleReorder: () -> Unit,
    onMove: (Int) -> Unit,
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
                onLongClick = { if (reorderAvailable) onToggleReorder() },
            ),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            if (reordering) {
                IconButton(
                    onClick = { onMove(-1) },
                    enabled = canMoveLeft && !busy,
                    colors = IconButtonDefaults.iconButtonColors(contentColor = content),
                    modifier = Modifier.testTag(WorkspaceTabsStripTags.moveLeft(tab.tabId)),
                ) {
                    Icon(
                        Icons.AutoMirrored.Filled.KeyboardArrowLeft,
                        contentDescription = "Move ${tab.label} left",
                    )
                }
            }
            Text(
                if (tab.paneCount > 1) "${tab.label} ·${tab.paneCount}" else tab.label,
                style = MaterialTheme.typography.labelMedium,
                fontWeight = if (active) FontWeight.Bold else FontWeight.Normal,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.padding(
                    start = if (reordering) 0.dp else spacing.small + spacing.extraSmall,
                    end = if (reordering) 0.dp else spacing.small + spacing.extraSmall,
                ),
            )
            if (reordering) {
                IconButton(
                    onClick = { onMove(1) },
                    enabled = canMoveRight && !busy,
                    colors = IconButtonDefaults.iconButtonColors(contentColor = content),
                    modifier = Modifier.testTag(WorkspaceTabsStripTags.moveRight(tab.tabId)),
                ) {
                    Icon(
                        Icons.AutoMirrored.Filled.KeyboardArrowRight,
                        contentDescription = "Move ${tab.label} right",
                    )
                }
            }
        }
    }
}

/** Stable semantics keys for tests and the screenshot suite. */
object WorkspaceTabsStripTags {
    const val STRIP = "workspace-tabs:strip"
    const val ERROR = "workspace-tabs:error"
    fun tab(tabId: String) = "workspace-tabs:tab:$tabId"
    fun moveLeft(tabId: String) = "workspace-tabs:move-left:$tabId"
    fun moveRight(tabId: String) = "workspace-tabs:move-right:$tabId"
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
                ),
                onSelectTab = {},
                onToggleReorder = {},
                onMoveTab = { _, _ -> },
            )
        }
    }
}
