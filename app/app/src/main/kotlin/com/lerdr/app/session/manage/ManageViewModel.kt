package com.lerdr.app.session.manage

import androidx.compose.runtime.Immutable
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.lerdr.app.session.SessionRepository
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import lerdr.core.store.Agent
import lerdr.core.store.AgentInventoryState
import lerdr.core.store.WorkspaceStore
import lerdr.core.transport.CommandException

/**
 * Which destructive action is mid-confirm — the oracle's
 * `confirming` ('clear' | 'stop') in ManageDialog.svelte.
 */
enum class ManageConfirm {
    CLEAR,
    STOP,
}

/**
 * Everything [ManageSheetContent] renders — the Compose port of the oracle's
 * `ManageDialog`: the editable name (`agent_rename`), the action menu
 * (`agent_restart` / `agent_clear` / `agent_stop` / `copy_agent_response`),
 * and a metadata block (pane id, cwd, workspace, agent identity, relay).
 */
@Immutable
data class ManageUiState(
    val paneId: String,
    /** Display title — `displayName(agent)` parity. */
    val title: String = "",
    /** Normalized agent identity ("claude", "codex"…) for badges/logos. */
    val provider: String? = null,
    /** `hostLabel(agent)` — the relay's display name. */
    val relayLabel: String = "",
    val rawPaneId: String = "",
    val cwd: String = "",
    /** The pane's workspace label — empty when the pane has none. */
    val workspaceLabel: String = "",
    /** `sessionName(agent)` — the agent-side session title when reported. */
    val sessionName: String = "",
    /** The oracle's `readOnly` gate — mutations render only for controllers. */
    val canControl: Boolean = false,
    /** Name-field draft — prefilled from [title] while untouched. */
    val nameDraft: String = "",
    /** Save affordance gate — a draft that differs from the live name. */
    val nameDirty: Boolean = false,
    val busy: Boolean = false,
    /** `confirming` — the confirm panel replaces the action list. */
    val confirming: ManageConfirm? = null,
    /** Inline status line — the oracle's toast text (errors tinted). */
    val status: String? = null,
    val statusError: Boolean = false,
    /**
     * One-shot clipboard payload — `copy_agent_response`'s `data.text`.
     * The sheet writes it to the clipboard then calls `consumeClipboard`.
     */
    val clipboardText: String? = null,
    /** Rename/stop/clear succeeded or the agent vanished — dismiss. */
    val shouldDismiss: Boolean = false,
)

/**
 * Oracle `sessionName` — `session_name` wins; a legacy `session` value that
 * looks like a path or a UUID is not a name at all.
 */
internal fun sessionNameOf(agent: Agent): String {
    val named = agent.sessionName?.trim().orEmpty()
    if (named.isNotEmpty()) return named
    val legacy = agent.session?.trim().orEmpty()
    if (legacy.isEmpty() || legacy.contains('/') || legacy.contains('\\')) return ""
    if (LEGACY_UUID.matches(legacy)) return ""
    return legacy
}

private val LEGACY_UUID =
    Regex("^[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}$")

/** Oracle `displayName` — `project || name || tab_label || agent || 'agent'`. */
private fun displayNameOf(agent: Agent): String =
    agent.project?.takeIf { it.isNotEmpty() }
        ?: agent.name?.takeIf { it.isNotEmpty() }
        ?: agent.tabLabel.takeIf { it.isNotEmpty() }
        ?: agent.agent?.takeIf { it.isNotEmpty() }
        ?: "agent"

/**
 * Manage-sheet mutation point — every action mirrors the oracle's
 * ManageDialog handler:
 *
 * - `saveRename` → `agent_rename{name}` (the oracle's `renameTab`);
 * - `restart` → `agent_restart`;
 * - `confirmAction` → `agent_clear` (45 s relay window) / `agent_stop`,
 *   the confirmation panel replacing the menu exactly like the oracle's
 *   `beginConfirm` (destructive focus lands on Cancel, never on Enter);
 * - `copyResponse` → `copy_agent_response`, `data.text` landing in
 *   [ManageUiState.clipboardText] for the sheet to hand to the clipboard.
 *
 * All mutations gate on `canControl` (the oracle's `readOnly`) plus the
 * `INVENTORY_REQUIRED_COMMANDS` check the oracle runs inside `sendCommand`.
 */
class ManageViewModel(
    private val paneId: String,
    private val sessions: SessionRepository,
    workspaces: WorkspaceStore,
) : ViewModel() {

    private val relayId = paneId.substringBefore("::")

    /**
     * Agent-row latch — `agentGone` fires only after the row was seen once,
     * so a sheet opened before the first `agents` snapshot stays open.
     */
    private var agentSeen = false
    private val agentGone = MutableStateFlow(false)

    private data class ManageLocal(
        /** null = untouched — the field tracks the live name. */
        val nameDraft: String? = null,
        val busy: Boolean = false,
        val confirming: ManageConfirm? = null,
        val status: String? = null,
        val statusError: Boolean = false,
        val clipboardText: String? = null,
        val dismiss: Boolean = false,
    )

    private val local = MutableStateFlow(ManageLocal())

    val uiState: StateFlow<ManageUiState> = combine(
        sessions.agent(paneId),
        sessions.connection(relayId),
        workspaces.workspaces,
        agentGone,
        local,
    ) { agent, _, allWorkspaces, gone, local ->
        val workspace = agent?.workspaceId?.takeIf { it.isNotEmpty() }?.let { id ->
            allWorkspaces.firstOrNull { it.relayId == relayId && it.workspaceId == id }
        }
        val title = agent?.let(::displayNameOf) ?: paneId.substringAfter("::")
        val draft = local.nameDraft ?: title
        ManageUiState(
            paneId = paneId,
            title = title,
            provider = agent?.agent?.takeIf { it.isNotEmpty() },
            relayLabel = agent?.relayLabel.orEmpty(),
            rawPaneId = agent?.rawPaneId.orEmpty(),
            cwd = agent?.cwd.orEmpty(),
            workspaceLabel = workspace?.label ?: agent?.workspaceId.orEmpty(),
            sessionName = agent?.let(::sessionNameOf).orEmpty(),
            canControl = sessions.canControl(relayId),
            nameDraft = draft,
            nameDirty = draft.trim() != title && draft.isNotBlank(),
            busy = local.busy,
            confirming = local.confirming,
            status = local.status,
            statusError = local.statusError,
            clipboardText = local.clipboardText,
            shouldDismiss = local.dismiss || gone,
        )
    }.stateIn(
        viewModelScope,
        SharingStarted.WhileSubscribed(5_000),
        ManageUiState(paneId),
    )

    init {
        viewModelScope.launch {
            sessions.agent(paneId).collect { agent ->
                if (agent != null) agentSeen = true
                agentGone.value = agentSeen && agent == null
            }
        }
    }

    /**
     * The oracle's `INVENTORY_REQUIRED_COMMANDS` gate — `sendCommand` refuses
     * these types client-side while Herdr's inventory is not `ready`.
     */
    private fun requireInventoryReady() {
        val inventory = sessions.connectionNow(relayId)?.inventory
        if (inventory?.state != AgentInventoryState.READY) {
            throw CommandException(
                inventory?.message?.ifEmpty { null }
                    ?: "Herdr agent inventory is not ready on this computer",
            )
        }
    }

    fun onNameDraftChange(value: String) {
        local.update { it.copy(nameDraft = value.take(MAX_NAME_RUNES)) }
    }

    /** `renameTab` — `agent_rename{name}`; the oracle closes the dialog on success. */
    fun saveRename() {
        if (local.value.busy || !uiState.value.canControl) return
        val name = uiState.value.nameDraft.trim()
        if (name.isEmpty()) {
            local.update {
                it.copy(status = "Enter a new name.", statusError = true)
            }
            return
        }
        if (!uiState.value.nameDirty) return
        local.update { it.copy(busy = true, status = null) }
        viewModelScope.launch {
            try {
                requireInventoryReady()
                sessions.renameAgent(paneId, name)
                local.update {
                    it.copy(
                        busy = false,
                        status = "Renamed to $name.",
                        statusError = false,
                        dismiss = true,
                    )
                }
            } catch (failure: Exception) {
                local.update {
                    it.copy(
                        busy = false,
                        status = failure.message ?: "The rename could not be sent",
                        statusError = true,
                    )
                }
            }
        }
    }

    /** `agent_restart` — respawn the pane's process in place. */
    fun restart() {
        if (local.value.busy || !uiState.value.canControl) return
        local.update { it.copy(busy = true, status = null) }
        viewModelScope.launch {
            try {
                requireInventoryReady()
                sessions.restartAgent(paneId)
                local.update {
                    it.copy(
                        busy = false,
                        status = "Restart requested.",
                        statusError = false,
                    )
                }
            } catch (failure: Exception) {
                local.update {
                    it.copy(
                        busy = false,
                        status = failure.message ?: "The agent could not be restarted",
                        statusError = true,
                    )
                }
            }
        }
    }

    /** `beginConfirm` — the panel that replaces the menu (focus on Cancel). */
    fun beginConfirm(action: ManageConfirm) {
        if (local.value.busy || !uiState.value.canControl) return
        local.update { it.copy(confirming = action, status = null) }
    }

    fun cancelConfirm() {
        if (local.value.busy) return
        local.update { it.copy(confirming = null) }
    }

    /**
     * `clearAgent`/`stopAgent` — `agent_clear` keeps the relay's 45 s window
     * and may return `data.warning` (surfaced as the status line); success
     * dismisses the sheet like the oracle closing its dialog.
     */
    fun confirmAction() {
        val action = local.value.confirming ?: return
        if (local.value.busy || !uiState.value.canControl) return
        local.update { it.copy(busy = true, status = null) }
        viewModelScope.launch {
            try {
                requireInventoryReady()
                when (action) {
                    ManageConfirm.CLEAR -> {
                        val result = sessions.clearAgent(paneId)
                        val warning = (result.data as? JsonObject)
                            ?.get("warning")
                            ?.let { it as? JsonPrimitive }
                            ?.contentOrNull
                            ?.takeIf { it.isNotEmpty() }
                        local.update {
                            it.copy(
                                busy = false,
                                confirming = null,
                                status = warning ?: "Agent cleared.",
                                statusError = warning != null,
                                dismiss = true,
                            )
                        }
                    }
                    ManageConfirm.STOP -> {
                        sessions.stopAgent(paneId)
                        local.update {
                            it.copy(
                                busy = false,
                                confirming = null,
                                status = "Agent stopped.",
                                statusError = false,
                                dismiss = true,
                            )
                        }
                    }
                }
            } catch (failure: Exception) {
                local.update {
                    it.copy(
                        busy = false,
                        confirming = null,
                        status = failure.message ?: "The action could not be sent",
                        statusError = true,
                    )
                }
            }
        }
    }

    /**
     * `copy_agent_response` — the relay answers with `data.text`, the rendered
     * reply. An empty payload means the agent has nothing to copy yet (the
     * oracle's transcript fallback is out of scope for the sheet).
     */
    fun copyResponse() {
        if (local.value.busy || !uiState.value.canControl) return
        local.update { it.copy(busy = true, status = null) }
        viewModelScope.launch {
            try {
                requireInventoryReady()
                val result = sessions.copyAgentResponse(paneId)
                val text = (result.data as? JsonObject)
                    ?.get("text")
                    ?.let { it as? JsonPrimitive }
                    ?.contentOrNull
                    .orEmpty()
                local.update {
                    if (text.isBlank()) {
                        it.copy(
                            busy = false,
                            status = "The agent has no response to copy.",
                            statusError = true,
                        )
                    } else {
                        it.copy(
                            busy = false,
                            clipboardText = text,
                            status = "Last response copied to the clipboard.",
                            statusError = false,
                        )
                    }
                }
            } catch (failure: Exception) {
                local.update {
                    it.copy(
                        busy = false,
                        status = failure.message
                            ?: "The agent response could not be copied",
                        statusError = true,
                    )
                }
            }
        }
    }

    /** The sheet wrote [ManageUiState.clipboardText] to the clipboard. */
    fun consumeClipboard() {
        local.update { it.copy(clipboardText = null) }
    }

    companion object {
        /** Oracle `maxlength` for the tab/session name field. */
        const val MAX_NAME_RUNES = 128
    }
}
