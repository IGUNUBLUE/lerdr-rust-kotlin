package com.lerdr.app.session

import androidx.compose.runtime.Immutable
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
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
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull
import lerdr.core.model.ActionReceiptMessage
import lerdr.core.model.CommandResultMessage
import lerdr.core.model.Inbound
import lerdr.core.store.AgentInventoryState
import lerdr.core.store.WorkspaceStore
import lerdr.core.transport.CommandException

/**
 * `WorktreeInfo` — one `worktrees[]` row of the `worktree_list` result
 * (`herdr.Worktree`, `frontend/src/lib/types.ts`). `branch` and
 * `openWorkspaceId` are genuinely nullable on the wire.
 */
@Immutable
data class WorktreeEntry(
    val path: String,
    val branch: String? = null,
    val isBare: Boolean = false,
    val isDetached: Boolean = false,
    val isPrunable: Boolean = false,
    val isLinkedWorktree: Boolean = false,
    val label: String = "",
    val openWorkspaceId: String? = null,
) {
    /** `worktree.branch || worktree.label` — the row title. */
    val title: String get() = branch?.takeIf { it.isNotEmpty() } ?: label

    /** The oracle's row gate: openable unless already open, bare, or prunable. */
    val openable: Boolean get() = openWorkspaceId == null && !isBare && !isPrunable
}

/** `WorktreeSource` — the repository the listing belongs to. */
@Immutable
data class WorktreeSource(
    val repoKey: String = "",
    val repoName: String = "",
    val repoRoot: String = "",
    val sourceCheckoutPath: String = "",
    val sourceWorkspaceId: String? = null,
)

/** `WorktreeListing` — `command_result.data` of `worktree_list`. */
@Immutable
data class WorktreeListing(
    val source: WorktreeSource,
    val worktrees: List<WorktreeEntry>,
)

/** Everything the worktrees sheet renders — list, create form, confirm state. */
@Immutable
data class WorktreesUiState(
    val relayId: String,
    val workspaceId: String,
    /** Sheet title: `{label} Worktrees`. */
    val workspaceLabel: String = "Workspace",
    /** The workspace's cwd — the sheet's fallback description. */
    val workspacePath: String = "",
    /** `worktree.is_linked_worktree` — only then is Remove offered. */
    val linkedWorktree: Boolean = false,
    /** `worktree_management` capability gate (oracle `worktreeManagementAvailable`). */
    val managementAvailable: Boolean = false,
    val loading: Boolean = true,
    val listing: WorktreeListing? = null,
    val error: String? = null,
    val busy: Boolean = false,
    /** Inline status line — the oracle's `form-status` (errors tinted). */
    val status: String? = null,
    val statusError: Boolean = false,
    /** Remove-confirm dialog open. */
    val confirmRemove: Boolean = false,
    /** The dialog's force mode after a `dirty_worktree_requires_force` reply. */
    val confirmForce: Boolean = false,
    /** The workspace is gone — the sheet should dismiss. */
    val shouldDismiss: Boolean = false,
    // Create form drafts (oracle maxlengths: branch/base 512, label 128).
    val branchDraft: String = "",
    val baseDraft: String = "",
    val labelDraft: String = "",
)

/**
 * `parseWorktreeListing` — the oracle's `listWorktrees` validation: `source`
 * and a `worktrees` array must be present, everything else degrades to
 * defaults. Invalid payloads throw like the oracle's CommandError.
 */
internal fun parseWorktreeListing(data: JsonElement?): WorktreeListing {
    val obj = data as? JsonObject
    val source = obj?.get("source") as? JsonObject
    val worktrees = obj?.get("worktrees") as? kotlinx.serialization.json.JsonArray
    if (source == null || worktrees == null) {
        throw CommandException("Relay returned an invalid worktree listing")
    }
    return WorktreeListing(
        source = WorktreeSource(
            repoKey = source.stringField("repo_key").orEmpty(),
            repoName = source.stringField("repo_name").orEmpty(),
            repoRoot = source.stringField("repo_root").orEmpty(),
            sourceCheckoutPath = source.stringField("source_checkout_path").orEmpty(),
            sourceWorkspaceId = source.stringField("source_workspace_id"),
        ),
        worktrees = worktrees.mapNotNull { element ->
            val entry = element as? JsonObject ?: return@mapNotNull null
            WorktreeEntry(
                path = entry.stringField("path").orEmpty(),
                branch = entry.stringField("branch")?.takeIf { it.isNotEmpty() },
                isBare = entry.booleanField("is_bare") == true,
                isDetached = entry.booleanField("is_detached") == true,
                isPrunable = entry.booleanField("is_prunable") == true,
                isLinkedWorktree = entry.booleanField("is_linked_worktree") == true,
                label = entry.stringField("label").orEmpty(),
                openWorkspaceId = entry.stringField("open_workspace_id")
                    ?.takeIf { it.isNotEmpty() },
            )
        },
    )
}

/**
 * Worktree management for one workspace — the sheet's mutation point. Ports
 * the oracle's worktree dialog flows (`showWorktrees`/`createWorktree`/
 * `openWorktree`/`confirmAction('remove')` in WorkspaceManager.svelte):
 *
 * - `worktree_list{workspace_id}` → `command_result.data` listing (30 s);
 * - `worktree_create{workspace_id, branch, base?, label?}` (75 s);
 * - `worktree_open{workspace_id, path}` — exactly one of path/branch (75 s);
 * - `worktree_remove{workspace_id, force}` (75 s), with the oracle's
 *   force escalation: a `dirty_worktree_requires_force` refusal keeps the
 *   confirm dialog open in force mode.
 *
 * The relay answers refusals as `command_result{ok:false, data:{code,
 * force_available}}` plus an `action_receipt{error.code}` — the thrown
 * [CommandException] drops `data`, so a frames collector correlates the
 * refusal by the `action_id` this VM assigns (the oracle sends none — the
 * relay echoes it into the receipt, which is exactly what the watch needs).
 *
 * After every successful mutation `refresh_agents` fans out (the oracle's
 * `requestAgents`) and the listing reloads.
 */
class WorktreesViewModel(
    private val relayId: String,
    private val workspaceId: String,
    private val sessions: SessionRepository,
    workspaces: WorkspaceStore,
) : ViewModel() {

    /** Stale-load guard — the oracle's `worktreeLoadGeneration`. */
    private var loadGeneration = 0

    /**
     * Workspace-row latch — `shouldDismiss` fires only after the row was
     * seen once, so a sheet opened before the first `workspaces` snapshot
     * does not close itself.
     */
    private var workspaceSeen = false
    private val workspaceGone = MutableStateFlow(false)

    /**
     * One in-flight remove watch — the sheet serializes its own mutations
     * behind `busy`, so a single slot is enough.
     */
    private class RemoveWatch(
        val actionId: String,
        val forceAvailable: CompletableDeferred<Boolean>,
    )

    private val removeWatch = AtomicReference<RemoveWatch?>()

    private data class WorktreesLocal(
        val loading: Boolean = true,
        val listing: WorktreeListing? = null,
        val error: String? = null,
        val busy: Boolean = false,
        val status: String? = null,
        val statusError: Boolean = false,
        val confirmRemove: Boolean = false,
        val confirmForce: Boolean = false,
        val removed: Boolean = false,
        val branchDraft: String = "",
        val baseDraft: String = "",
        val labelDraft: String = "",
    )

    private val local = MutableStateFlow(WorktreesLocal())

    val uiState: StateFlow<WorktreesUiState> = combine(
        workspaces.workspaces,
        sessions.connection(relayId),
        workspaceGone,
        local,
    ) { all, connection, gone, local ->
        val workspace = all.firstOrNull {
            it.relayId == relayId && it.workspaceId == workspaceId
        }
        WorktreesUiState(
            relayId = relayId,
            workspaceId = workspaceId,
            workspaceLabel = workspace?.label ?: "Workspace",
            workspacePath = workspace?.cwd
                ?: workspace?.worktree?.checkoutPath.orEmpty(),
            linkedWorktree = workspace?.worktree?.isLinkedWorktree == true,
            managementAvailable = connection?.capabilities
                ?.contains(WORKTREE_MANAGEMENT_CAPABILITY) == true,
            loading = local.loading,
            listing = local.listing,
            error = local.error,
            busy = local.busy,
            status = local.status,
            statusError = local.statusError,
            confirmRemove = local.confirmRemove,
            confirmForce = local.confirmForce,
            shouldDismiss = local.removed || gone,
            branchDraft = local.branchDraft,
            baseDraft = local.baseDraft,
            labelDraft = local.labelDraft,
        )
    }.stateIn(
        viewModelScope,
        SharingStarted.WhileSubscribed(5_000),
        WorktreesUiState(relayId, workspaceId),
    )

    init {
        // Watch the workspace row — once seen, its disappearance (removed by
        // this sheet or another client) dismisses the sheet.
        viewModelScope.launch {
            workspaces.workspaces.collect { list ->
                val found = list.any {
                    it.relayId == relayId && it.workspaceId == workspaceId
                }
                if (found) workspaceSeen = true
                workspaceGone.value = workspaceSeen && !found
            }
        }
        // Remove-outcome watch — correlates the refusal signals the
        // command_result exception drops (`data.force_available` /
        // `action_receipt.error.code`).
        viewModelScope.launch {
            sessions.frames.collect { frame ->
                if (frame.relayId != relayId) return@collect
                val watch = removeWatch.get() ?: return@collect
                when (val message = frame.message) {
                    is ActionReceiptMessage -> {
                        val receipt = message.receipt ?: return@collect
                        if (receipt.actionId == watch.actionId) {
                            watch.forceAvailable.complete(
                                receipt.error?.code == DIRTY_WORKTREE_CODE,
                            )
                        }
                    }
                    is CommandResultMessage -> {
                        // Only a positive data signal completes here — a
                        // bare failure must not pre-empt the receipt that
                        // follows it on the wire.
                        if (message.action == WORKTREE_REMOVE && message.ok == false &&
                            forceAvailable(message.data)
                        ) {
                            watch.forceAvailable.complete(true)
                        }
                    }
                    else -> Unit
                }
            }
        }
        refresh()
    }

    /**
     * `workspaceManagementAvailable('worktree_management')` + `sendCommand`'s
     * inventory gate — every `worktree_*` type sits in the oracle's
     * `INVENTORY_REQUIRED_COMMANDS`, so a non-ready inventory refuses before
     * the frame leaves.
     */
    private fun requireWorktreeManagement() {
        val connection = sessions.connectionNow(relayId)
        if (connection?.capabilities?.contains(WORKTREE_MANAGEMENT_CAPABILITY) != true) {
            throw CommandException("This relay does not support worktree management")
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
     * `showWorktrees` — (re)loads the listing. Generation-guarded so a slow
     * answer can never overwrite a newer one.
     */
    fun refresh() {
        val generation = ++loadGeneration
        local.update {
            it.copy(loading = true, error = null)
        }
        viewModelScope.launch {
            try {
                requireWorktreeManagement()
                val result = sessions.request(
                    relayId,
                    Inbound(type = WORKTREE_LIST, workspaceId = workspaceId),
                    timeoutMs = LIST_TIMEOUT_MS,
                )
                val listing = parseWorktreeListing(result.data)
                if (generation != loadGeneration) return@launch
                local.update {
                    it.copy(loading = false, listing = listing, error = null)
                }
            } catch (failure: Exception) {
                if (generation != loadGeneration) return@launch
                local.update {
                    it.copy(
                        loading = false,
                        error = failure.message ?: "Worktrees could not be listed",
                    )
                }
            }
        }
    }

    fun onBranchDraftChange(value: String) {
        local.update { it.copy(branchDraft = value.take(MAX_VALUE_RUNES)) }
    }

    fun onBaseDraftChange(value: String) {
        local.update { it.copy(baseDraft = value.take(MAX_VALUE_RUNES)) }
    }

    fun onLabelDraftChange(value: String) {
        local.update { it.copy(labelDraft = value.take(MAX_LABEL_RUNES)) }
    }

    /** `createWorktree` — `worktree_create`; the form clears on success. */
    fun createWorktree() {
        val branch = local.value.branchDraft.trim()
        if (local.value.busy || branch.isEmpty()) return
        local.update { it.copy(busy = true) }
        viewModelScope.launch {
            try {
                requireWorktreeManagement()
                sessions.request(
                    relayId,
                    Inbound(
                        type = WORKTREE_CREATE,
                        workspaceId = workspaceId,
                        branch = branch,
                        base = local.value.baseDraft.trim(),
                        label = local.value.labelDraft.trim(),
                    ),
                    timeoutMs = MUTATION_TIMEOUT_MS,
                )
                sessions.refreshAgents()
                local.update {
                    it.copy(
                        busy = false,
                        branchDraft = "",
                        baseDraft = "",
                        labelDraft = "",
                        status = "Created worktree $branch.",
                        statusError = false,
                    )
                }
                refresh()
            } catch (failure: Exception) {
                local.update { it.copy(busy = false) }
                reportMutationFailure(
                    failure,
                    unknownHint = "Check the worktree list before retrying.",
                )
            }
        }
    }

    /**
     * `openWorktree` — `worktree_open` with `path` only (the relay requires
     * exactly one of path/branch). Opening adds a workspace on the desktop
     * without stealing focus (`focus:false` is baked into the relay).
     */
    fun openWorktree(path: String, label: String) {
        if (local.value.busy || path.isEmpty()) return
        local.update { it.copy(busy = true) }
        viewModelScope.launch {
            try {
                requireWorktreeManagement()
                sessions.request(
                    relayId,
                    Inbound(
                        type = WORKTREE_OPEN,
                        workspaceId = workspaceId,
                        path = path,
                    ),
                    timeoutMs = MUTATION_TIMEOUT_MS,
                )
                sessions.refreshAgents()
                local.update {
                    it.copy(
                        busy = false,
                        status = "Opened worktree $label.",
                        statusError = false,
                    )
                }
                refresh()
            } catch (failure: Exception) {
                local.update { it.copy(busy = false) }
                reportMutationFailure(
                    failure,
                    unknownHint = "Check the workspace list before retrying.",
                )
            }
        }
    }

    /** Opens the remove confirm — only for a linked worktree workspace. */
    fun requestRemove() {
        if (!uiState.value.linkedWorktree || local.value.busy) return
        local.update { it.copy(confirmRemove = true, confirmForce = false) }
    }

    fun dismissRemove() {
        if (local.value.busy) return
        local.update { it.copy(confirmRemove = false, confirmForce = false) }
    }

    /**
     * `confirmAction('remove')` — `worktree_remove{workspace_id, force}`.
     * A `dirty_worktree_requires_force` refusal keeps the dialog open in
     * force mode (the oracle's `{...action, force: true}`); success marks
     * the sheet for dismissal.
     */
    fun confirmRemove() {
        val state = uiState.value
        if (local.value.busy || !state.confirmRemove) return
        val actionId = "worktree-remove-${UUID.randomUUID()}"
        val watch = RemoveWatch(actionId, CompletableDeferred())
        removeWatch.set(watch)
        local.update { it.copy(busy = true) }
        viewModelScope.launch {
            try {
                sessions.request(
                    relayId,
                    Inbound(
                        type = WORKTREE_REMOVE,
                        workspaceId = workspaceId,
                        force = state.confirmForce,
                        actionId = actionId,
                    ),
                    timeoutMs = MUTATION_TIMEOUT_MS,
                )
                sessions.refreshAgents()
                local.update {
                    it.copy(
                        busy = false,
                        confirmRemove = false,
                        confirmForce = false,
                        removed = true,
                        status = "Removed worktree ${state.workspaceLabel}.",
                        statusError = false,
                    )
                }
            } catch (failure: Exception) {
                // The refusal frames arrive on `frames` before the request's
                // deferred settles; the await below just hands the collector
                // a scheduling slot.
                val forceAvailable = withTimeoutOrNull(RECEIPT_GRACE_MS) {
                    watch.forceAvailable.await()
                } == true
                local.update { it.copy(busy = false) }
                if (forceAvailable && !state.confirmForce) {
                    local.update { it.copy(confirmForce = true) }
                } else {
                    local.update {
                        it.copy(
                            confirmRemove = false,
                            confirmForce = false,
                            status = failure.message ?: "Worktree could not be removed",
                            statusError = true,
                        )
                    }
                }
            } finally {
                removeWatch.compareAndSet(watch, null)
            }
        }
    }

    /**
     * Oracle `setStatus` for mutations — `dispatched_unknown` appends the
     * refresh hint and reloads the listing (the mutation may have landed).
     */
    private fun reportMutationFailure(failure: Exception, unknownHint: String) {
        val message = failure.message ?: "Command failed"
        val ambiguous = (failure as? CommandException)?.dispatchedUnknown == true
        local.update {
            it.copy(
                status = if (ambiguous) "$message $unknownHint" else message,
                statusError = true,
            )
        }
        if (ambiguous) refresh()
    }

    companion object {
        const val WORKTREE_MANAGEMENT_CAPABILITY = "worktree_management"
        const val WORKTREE_LIST = "worktree_list"
        const val WORKTREE_CREATE = "worktree_create"
        const val WORKTREE_OPEN = "worktree_open"
        const val WORKTREE_REMOVE = "worktree_remove"
        const val DIRTY_WORKTREE_CODE = "dirty_worktree_requires_force"

        /** Oracle timeouts: list 30 s, mutations 75 s. */
        const val LIST_TIMEOUT_MS = 30_000L
        const val MUTATION_TIMEOUT_MS = 75_000L

        /**
         * Grace for the receipt/failure frame to land after the request
         * threw — the frame is already buffered, this only needs one
         * scheduling slot on a healthy relay.
         */
        const val RECEIPT_GRACE_MS = 1_000L

        /** Relay `worktreeValueMaxRunes` — values beyond it fail outright. */
        const val MAX_VALUE_RUNES = 512
        const val MAX_LABEL_RUNES = 128
    }
}

/** `command_result.data.force_available` (or the code that implies it). */
private fun forceAvailable(data: JsonElement?): Boolean {
    val obj = data as? JsonObject ?: return false
    if (obj["force_available"] == JsonPrimitive(true)) return true
    return (obj["code"] as? JsonPrimitive)
        ?.contentOrNull == WorktreesViewModel.DIRTY_WORKTREE_CODE
}

private fun JsonObject.stringField(name: String): String? =
    (this[name] as? JsonPrimitive)?.takeIf { it.isString }?.contentOrNull

private fun JsonObject.booleanField(name: String): Boolean? =
    (this[name] as? JsonPrimitive)?.booleanOrNull
