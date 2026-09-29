package com.lerdr.app.session

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.tooling.preview.Preview
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.lerdr.core.designsystem.theme.LerdrTheme
import dagger.hilt.android.EntryPointAccessors
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonPrimitive
import lerdr.core.store.RelayWorkspace
import lerdr.core.transport.CommandException

/** Test tags — see `CloseWorkspaceSheetScreenshotTest`. */
object CloseWorkspaceTags {
    const val CONTENT = "close-workspace:content"
    const val CONFIRM = "close-workspace:confirm"
    const val CONFIRM_GROUP = "close-workspace:confirm-group"
    const val CANCEL = "close-workspace:cancel"
    const val STATUS = "close-workspace:status"
}

/**
 * What the confirmation resolves to. The relay decides the close shape:
 *
 * - [Mode.SINGLE] — a standalone workspace, or a linked worktree whose
 *   siblings stay open. `workspace_close{workspace_id}` only.
 * - [Mode.PRIMARY_GROUP] — the selected workspace is the non-linked member
 *   of a repo group with linked worktrees; the relay refuses a single close
 *   (`workspace_group_close_required`), so the only offer is the whole
 *   group via `workspace_close{close_group, expected_workspace_ids}`.
 * - [Mode.LINKED_GROUP] — the selected workspace is a linked worktree in a
 *   group: it can close alone, or the caller can close the whole group by
 *   targeting the primary instead (`workspace_group_primary_required`
 *   refuses group closes aimed at a linked member).
 */
@androidx.compose.runtime.Immutable
data class ClosePlan(
    val target: RelayWorkspace,
    val group: List<RelayWorkspace>,
    val primary: RelayWorkspace,
    /** Consent escalation — the relay's own `workspace_ids` after a
     * `workspace_group_close_required` refusal. */
    val consentedGroupIds: List<String> = emptyList(),
) {
    enum class Mode { SINGLE, PRIMARY_GROUP, LINKED_GROUP }

    /**
     * The group's size for copy — a consent set that outgrew the stale
     * store snapshot wins (its ids came from the relay's own list).
     */
    val groupSize: Int get() = maxOf(group.size, consentedGroupIds.size)
    val isLinked: Boolean get() = target.worktree?.isLinkedWorktree == true
    val mode: Mode
        get() = when {
            // A close_required consent set forces the group path — the
            // store's snapshot was stale, not the relay's refusal.
            consentedGroupIds.size > 1 -> Mode.PRIMARY_GROUP
            groupSize <= 1 -> Mode.SINGLE
            isLinked -> Mode.LINKED_GROUP
            else -> Mode.PRIMARY_GROUP
        }

    /** `workspace_close` target — the primary when the group closes. */
    val singleTargetId: String get() = target.workspaceId
    val groupTargetId: String
        get() = if (isLinked) primary.workspaceId else target.workspaceId
    val expectedIds: List<String>
        get() = consentedGroupIds.ifEmpty { group.map { it.workspaceId } }
}

/**
 * Lerdr's `workspaceGroupIDs`: workspaces sharing the target's
 * `worktree.repo_key` form the close group; the primary is the first
 * non-linked member in snapshot order (the store keeps wire order).
 * A target without a repo key — or absent from the snapshot — degrades to
 * a single close, which is exactly what the relay computes too.
 */
internal fun closePlanFor(
    workspaces: List<RelayWorkspace>,
    relayId: String,
    workspaceId: String,
    consentedGroupIds: List<String> = emptyList(),
): ClosePlan {
    val relayRows = workspaces.filter { it.relayId == relayId }
    val target = relayRows.firstOrNull { it.workspaceId == workspaceId }
        ?: RelayWorkspace(
            relayId = relayId,
            relayLabel = relayRows.firstOrNull()?.relayLabel.orEmpty(),
            workspaceId = workspaceId,
        )
    val repoKey = target.worktree?.repoKey.orEmpty()
    val group = if (repoKey.isEmpty()) {
        listOf(target)
    } else {
        relayRows.filter { it.worktree?.repoKey == repoKey }
    }
    val primary = group.firstOrNull { it.worktree?.isLinkedWorktree != true }
        ?: target
    return ClosePlan(
        target = target,
        group = group.ifEmpty { listOf(target) },
        primary = primary,
        consentedGroupIds = consentedGroupIds,
    )
}

/** `command_result.data.code` — survives the [CommandException] throw. */
internal fun closeRefusalCode(error: CommandException): String? =
    (error.data as? JsonObject)?.get("code")
        ?.let { it as? JsonPrimitive }?.jsonPrimitive?.content

/** `command_result.data.workspace_ids` — the relay's authoritative group. */
internal fun closeRefusalWorkspaceIds(error: CommandException): List<String> =
    (error.data as? JsonObject)?.get("workspace_ids")
        ?.let { runCatching { it.jsonArray.map { id -> id.jsonPrimitive.content } } }
        ?.getOrNull()
        .orEmpty()

/**
 * User-facing close failure text — `workspace_group_*` refusals get a
 * rewrite that matches the dialog's vocabulary; everything else passes the
 * relay's public message through (Lerdr's `refusal_message` wording).
 */
internal fun closeErrorMessage(error: Exception): String {
    val command = error as? CommandException ?: return fallbackMessage(error)
    if (command.dispatchedUnknown) {
        return "The close may have run — check the workspace list."
    }
    return when (closeRefusalCode(command)) {
        "workspace_group_primary_required" ->
            "Close the group's main workspace to close the whole group."
        "workspace_group_changed" ->
            "The workspace group changed — review it and try again."
        "workspace_group_consent_invalid" ->
            "The group confirmation was invalid — confirm again."
        "workspace_group_validation_unavailable" ->
            "The workspace list could not be verified — try again."
        else -> fallbackMessage(error)
    }
}

private fun fallbackMessage(error: Exception): String =
    error.message ?: "The workspace could not be closed"

/**
 * `workspace_close` confirmation — the session ⋯ entry's sheet. Shows the
 * group copy before the destructive tap, escalates to the relay's own
 * `workspace_ids` consent set on a `workspace_group_close_required`
 * refusal, and reports through [onClosed] once the close is confirmed so
 * the session screen can leave its dead pane.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun CloseWorkspaceSheet(
    relayId: String,
    workspaceId: String,
    onClosed: () -> Unit,
    onDismiss: () -> Unit,
) {
    val appContext = LocalContext.current.applicationContext
    val entryPoint = remember(appContext) {
        EntryPointAccessors.fromApplication(appContext, WorktreesEntryPoint::class.java)
    }
    val sessions = remember(entryPoint) { entryPoint.sessionRepository() }
    val workspaceStore = remember(entryPoint) { entryPoint.workspaceStore() }
    val workspaces by workspaceStore.workspaces.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()

    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var consentedGroupIds by remember { mutableStateOf<List<String>>(emptyList()) }
    var workspaceSeen by remember { mutableStateOf(false) }

    val plan = remember(workspaces, workspaceId, consentedGroupIds) {
        closePlanFor(workspaces, relayId, workspaceId, consentedGroupIds)
    }

    // The workspace row leaving the store — closed here or by another
    // client — dismisses the sheet once it was seen (the first snapshot
    // may lag the open).
    LaunchedEffect(workspaces) {
        val found = workspaces.any {
            it.relayId == relayId && it.workspaceId == workspaceId
        }
        if (found) workspaceSeen = true else if (workspaceSeen) onDismiss()
    }

    fun close(closeGroup: Boolean) {
        if (busy) return
        busy = true
        error = null
        scope.launch {
            try {
                sessions.closeWorkspace(
                    relayId = relayId,
                    workspaceId = if (closeGroup) plan.groupTargetId else plan.singleTargetId,
                    closeGroup = closeGroup,
                    expectedWorkspaceIds = plan.expectedIds,
                )
                onClosed()
            } catch (failure: Exception) {
                busy = false
                val code = (failure as? CommandException)?.let(::closeRefusalCode)
                val ids = (failure as? CommandException)
                    ?.let(::closeRefusalWorkspaceIds)
                    .orEmpty()
                if (code == "workspace_group_close_required" && ids.size > 1) {
                    // The store's snapshot was stale — re-ask with the
                    // relay's authoritative group before retrying.
                    consentedGroupIds = ids
                } else {
                    error = closeErrorMessage(failure)
                }
            }
        }
    }

    ModalBottomSheet(
        onDismissRequest = { if (!busy) onDismiss() },
    ) {
        CloseWorkspaceSheetContent(
            plan = plan,
            busy = busy,
            error = error,
            onCloseSingle = { close(closeGroup = false) },
            onCloseGroup = { close(closeGroup = true) },
            onDismiss = onDismiss,
        )
    }
}

/** Stateless sheet body — the piece screenshot tests render directly. */
@Composable
fun CloseWorkspaceSheetContent(
    plan: ClosePlan,
    busy: Boolean,
    error: String?,
    onCloseSingle: () -> Unit,
    onCloseGroup: () -> Unit,
    onDismiss: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = LerdrTheme.extendedColors
    val label = plan.target.label
    val cancelFocus = remember { FocusRequester() }
    Column(
        modifier = modifier
            .fillMaxWidth()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = LerdrTheme.spacing.large)
            .padding(bottom = LerdrTheme.spacing.extraLarge)
            .testTag(CloseWorkspaceTags.CONTENT),
        verticalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.medium),
    ) {
        Column(verticalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.extraSmall)) {
            Text("Close workspace", style = MaterialTheme.typography.titleLarge)
            Text(
                text = listOf(label, plan.target.relayLabel)
                    .filter { it.isNotBlank() }
                    .joinToString(" · "),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }

        val linkedCount = plan.groupSize - 1
        val linkedNoun = if (linkedCount == 1) "worktree" else "worktrees"
        Text(
            text = when (plan.mode) {
                ClosePlan.Mode.SINGLE ->
                    "Close \"$label\"? Its panes and tabs close on the computer."
                ClosePlan.Mode.PRIMARY_GROUP ->
                    "\"$label\" is grouped with $linkedCount linked " +
                        "$linkedNoun — all ${plan.groupSize} workspaces close."
                ClosePlan.Mode.LINKED_GROUP ->
                    "\"$label\" is a linked worktree in a group of " +
                        "${plan.groupSize} workspaces. Close only it, or the " +
                        "whole group."
            },
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurface,
        )

        Row(horizontalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.small)) {
            OutlinedButton(
                onClick = onDismiss,
                enabled = !busy,
                modifier = Modifier
                    .weight(1f)
                    .focusRequester(cancelFocus)
                    .testTag(CloseWorkspaceTags.CANCEL),
            ) {
                Text("Cancel")
            }
            when (plan.mode) {
                ClosePlan.Mode.PRIMARY_GROUP -> Button(
                    onClick = onCloseGroup,
                    enabled = !busy,
                    colors = ButtonDefaults.buttonColors(
                        containerColor = colors.danger,
                        contentColor = colors.onDanger,
                    ),
                    modifier = Modifier
                        .weight(1f)
                        .testTag(CloseWorkspaceTags.CONFIRM_GROUP),
                ) {
                    Text("Close all ${plan.groupSize} workspaces")
                }
                else -> Button(
                    onClick = onCloseSingle,
                    enabled = !busy,
                    colors = ButtonDefaults.buttonColors(
                        containerColor = colors.danger,
                        contentColor = colors.onDanger,
                    ),
                    modifier = Modifier
                        .weight(1f)
                        .testTag(CloseWorkspaceTags.CONFIRM),
                ) {
                    Text("Close workspace")
                }
            }
        }
        // A linked worktree may close alone or take the whole group with
        // it (the relay closes the group through the primary).
        if (plan.mode == ClosePlan.Mode.LINKED_GROUP) {
            OutlinedButton(
                onClick = onCloseGroup,
                enabled = !busy,
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag(CloseWorkspaceTags.CONFIRM_GROUP),
            ) {
                Text("Close all ${plan.groupSize} workspaces")
            }
        }

        error?.let {
            Text(
                text = it,
                style = MaterialTheme.typography.bodySmall,
                color = colors.danger,
                modifier = Modifier
                    .semantics { liveRegion = LiveRegionMode.Polite }
                    .testTag(CloseWorkspaceTags.STATUS),
            )
        }
    }
}

@Preview(showBackground = true)
@Composable
private fun CloseWorkspaceSheetContentPreview() {
    LerdrTheme {
        CloseWorkspaceSheetContent(
            plan = ClosePlan(
                target = RelayWorkspace(
                    relayId = "r",
                    relayLabel = "workstation",
                    workspaceId = "w1",
                    label = "lerdr",
                ),
                group = listOf(
                    RelayWorkspace("r", "workstation", "w1", label = "lerdr"),
                ),
                primary = RelayWorkspace("r", "workstation", "w1", label = "lerdr"),
            ),
            busy = false,
            error = null,
            onCloseSingle = {},
            onCloseGroup = {},
            onDismiss = {},
        )
    }
}
