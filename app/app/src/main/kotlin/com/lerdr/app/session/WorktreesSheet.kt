package com.lerdr.app.session

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.PreviewLightDark
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lerdr.core.designsystem.components.LerdrWavyProgressIndicator
import com.lerdr.core.designsystem.theme.LerdrTheme
import dagger.hilt.EntryPoint
import dagger.hilt.InstallIn
import dagger.hilt.android.EntryPointAccessors
import dagger.hilt.components.SingletonComponent
import lerdr.core.store.WorkspaceStore

/**
 * Singleton seams the worktrees sheet pulls through `EntryPointAccessors` —
 * `hilt-navigation-compose` is absent, so the `viewModel {}` factory in
 * [WorktreesSheet] resolves its dependencies here (`SessionRepository` is
 * `@Singleton`; `WorkspaceStore` is bound in `AppModule`).
 */
@EntryPoint
@InstallIn(SingletonComponent::class)
interface WorktreesEntryPoint {
    fun sessionRepository(): SessionRepository
    fun workspaceStore(): WorkspaceStore
}

/**
 * Worktree management for one workspace — the Compose port of the oracle's
 * `worktree-manager-dialog` in WorkspaceManager.svelte: list / create /
 * open / remove Git worktrees, on a modal bottom sheet.
 *
 * Wiring: call from a session-screen affordance when `agent.workspaceId`
 * is non-empty — e.g. a trailing icon in `SessionTopBar` or a row in the
 * workspace section. Self-dismisses when the workspace leaves the store.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun WorktreesSheet(
    relayId: String,
    workspaceId: String,
    onDismiss: () -> Unit,
) {
    val appContext = LocalContext.current.applicationContext
    val viewModel: WorktreesViewModel = viewModel(key = "worktrees:$relayId:$workspaceId") {
        val entryPoint =
            EntryPointAccessors.fromApplication(appContext, WorktreesEntryPoint::class.java)
        WorktreesViewModel(
            relayId = relayId,
            workspaceId = workspaceId,
            sessions = entryPoint.sessionRepository(),
            workspaces = entryPoint.workspaceStore(),
        )
    }
    val uiState by viewModel.uiState.collectAsStateWithLifecycle()
    // A successful remove (or the workspace closing under us) ends the sheet.
    LaunchedEffect(uiState.shouldDismiss) {
        if (uiState.shouldDismiss) onDismiss()
    }
    ModalBottomSheet(onDismissRequest = onDismiss) {
        WorktreesSheetContent(
            uiState = uiState,
            onRefresh = viewModel::refresh,
            onBranchDraftChange = viewModel::onBranchDraftChange,
            onBaseDraftChange = viewModel::onBaseDraftChange,
            onLabelDraftChange = viewModel::onLabelDraftChange,
            onCreate = viewModel::createWorktree,
            onOpenWorktree = viewModel::openWorktree,
            onRequestRemove = viewModel::requestRemove,
            onDismiss = onDismiss,
        )
    }
    if (uiState.confirmRemove) {
        RemoveWorktreeDialog(
            workspaceLabel = uiState.workspaceLabel,
            force = uiState.confirmForce,
            busy = uiState.busy,
            onConfirm = viewModel::confirmRemove,
            onDismiss = viewModel::dismissRemove,
        )
    }
}

/**
 * Stateless sheet body — state in, events out, so previews and the
 * Roborazzi goldens render the same tree the sheet composes.
 */
@Composable
fun WorktreesSheetContent(
    uiState: WorktreesUiState,
    onRefresh: () -> Unit,
    onBranchDraftChange: (String) -> Unit,
    onBaseDraftChange: (String) -> Unit,
    onLabelDraftChange: (String) -> Unit,
    onCreate: () -> Unit,
    onOpenWorktree: (path: String, label: String) -> Unit,
    onRequestRemove: () -> Unit,
    onDismiss: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val spacing = LerdrTheme.spacing
    Column(
        modifier = modifier
            .fillMaxWidth()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = spacing.large)
            .padding(bottom = spacing.large)
            .testTag(WorktreesSheetTags.CONTENT),
    ) {
        Text(
            "${uiState.workspaceLabel} Worktrees",
            style = MaterialTheme.typography.titleLarge,
        )
        Text(
            uiState.listing?.source?.repoRoot
                ?: uiState.workspacePath.ifEmpty { "List, open, or create Git worktrees." },
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
        Spacer(Modifier.height(spacing.medium))

        when {
            uiState.loading -> LoadingRow()
            uiState.error != null -> ErrorRow(
                message = uiState.error,
                enabled = !uiState.busy,
                onRetry = onRefresh,
            )
            else -> {
                val listing = uiState.listing
                if (listing != null) {
                    if (listing.worktrees.isEmpty()) {
                        Text(
                            "No worktrees listed for this repository.",
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    listing.worktrees.forEach { worktree ->
                        WorktreeRow(
                            worktree = worktree,
                            enabled = !uiState.busy,
                            onOpen = { onOpenWorktree(worktree.path, worktree.title) },
                        )
                    }
                }
                Spacer(Modifier.height(spacing.medium))
                CreateWorktreeForm(
                    uiState = uiState,
                    onBranchDraftChange = onBranchDraftChange,
                    onBaseDraftChange = onBaseDraftChange,
                    onLabelDraftChange = onLabelDraftChange,
                    onCreate = onCreate,
                    onDismiss = onDismiss,
                )
            }
        }

        if (uiState.linkedWorktree) {
            Spacer(Modifier.height(spacing.medium))
            HorizontalDivider()
            Spacer(Modifier.height(spacing.medium))
            OutlinedButton(
                onClick = onRequestRemove,
                enabled = !uiState.busy && uiState.managementAvailable,
                colors = ButtonDefaults.outlinedButtonColors(
                    contentColor = MaterialTheme.colorScheme.error,
                ),
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag(WorktreesSheetTags.REMOVE_BUTTON),
            ) {
                Text("Remove Worktree")
            }
        }

        uiState.status?.let { status ->
            Spacer(Modifier.height(spacing.medium))
            Text(
                status,
                style = MaterialTheme.typography.bodySmall,
                color = if (uiState.statusError) {
                    MaterialTheme.colorScheme.error
                } else {
                    MaterialTheme.colorScheme.onSurfaceVariant
                },
                modifier = Modifier.testTag(WorktreesSheetTags.STATUS),
            )
        }
    }
}

@Composable
private fun LoadingRow() {
    val spacing = LerdrTheme.spacing
    Column(
        verticalArrangement = Arrangement.spacedBy(spacing.small),
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = spacing.small)
            .testTag(WorktreesSheetTags.LOADING),
    ) {
        LerdrWavyProgressIndicator(modifier = Modifier.fillMaxWidth())
        Text(
            "Loading worktrees…",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun ErrorRow(message: String, enabled: Boolean, onRetry: () -> Unit) {
    val spacing = LerdrTheme.spacing
    Column(
        verticalArrangement = Arrangement.spacedBy(spacing.small),
        modifier = Modifier
            .fillMaxWidth()
            .testTag(WorktreesSheetTags.ERROR),
    ) {
        Text(
            message,
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.error,
        )
        TextButton(onClick = onRetry, enabled = enabled) {
            Text("Retry")
        }
    }
}

/**
 * One `worktrees[]` row — `{branch || label}` over `path`, with the oracle's
 * trailing state: open → "Open" chip, bare/prunable → "Unavailable", else an
 * Open button.
 */
@Composable
private fun WorktreeRow(
    worktree: WorktreeEntry,
    enabled: Boolean,
    onOpen: () -> Unit,
) {
    val spacing = LerdrTheme.spacing
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = spacing.small)
            .testTag(WorktreesSheetTags.ROW),
    ) {
        Column(Modifier.weight(1f)) {
            Text(
                worktree.title.ifEmpty { worktree.path },
                style = MaterialTheme.typography.titleSmall,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                worktree.path,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        Spacer(Modifier.width(spacing.small))
        when {
            worktree.openWorkspaceId != null -> StateChip("Open")
            worktree.isBare || worktree.isPrunable -> StateChip("Unavailable")
            else -> FilledTonalButton(
                onClick = onOpen,
                enabled = enabled,
            ) {
                Text("Open")
            }
        }
    }
}

@Composable
private fun StateChip(label: String) {
    Surface(
        color = MaterialTheme.colorScheme.surfaceContainerHighest,
        contentColor = MaterialTheme.colorScheme.onSurfaceVariant,
        shape = MaterialTheme.shapes.small,
    ) {
        Text(
            label,
            style = MaterialTheme.typography.labelMedium,
            modifier = Modifier.padding(
                horizontal = LerdrTheme.spacing.small,
                vertical = LerdrTheme.spacing.extraSmall,
            ),
        )
    }
}

/** The oracle's `worktree-create-form` — branch required; base and label optional. */
@Composable
private fun CreateWorktreeForm(
    uiState: WorktreesUiState,
    onBranchDraftChange: (String) -> Unit,
    onBaseDraftChange: (String) -> Unit,
    onLabelDraftChange: (String) -> Unit,
    onCreate: () -> Unit,
    onDismiss: () -> Unit,
) {
    val spacing = LerdrTheme.spacing
    HorizontalDivider()
    Spacer(Modifier.height(spacing.medium))
    Text("Create Worktree", style = MaterialTheme.typography.titleSmall)
    Spacer(Modifier.height(spacing.small))
    OutlinedTextField(
        value = uiState.branchDraft,
        onValueChange = onBranchDraftChange,
        label = { Text("Branch") },
        placeholder = { Text("fix/issue-14") },
        singleLine = true,
        enabled = !uiState.busy,
        modifier = Modifier
            .fillMaxWidth()
            .testTag(WorktreesSheetTags.BRANCH_FIELD),
    )
    Spacer(Modifier.height(spacing.small))
    OutlinedTextField(
        value = uiState.baseDraft,
        onValueChange = onBaseDraftChange,
        label = { Text("Base ref") },
        placeholder = { Text("main") },
        supportingText = { Text("Optional — defaults to HEAD") },
        singleLine = true,
        enabled = !uiState.busy,
        modifier = Modifier
            .fillMaxWidth()
            .testTag(WorktreesSheetTags.BASE_FIELD),
    )
    Spacer(Modifier.height(spacing.small))
    OutlinedTextField(
        value = uiState.labelDraft,
        onValueChange = onLabelDraftChange,
        label = { Text("Workspace label") },
        supportingText = { Text("Optional") },
        singleLine = true,
        enabled = !uiState.busy,
        modifier = Modifier
            .fillMaxWidth()
            .testTag(WorktreesSheetTags.LABEL_FIELD),
    )
    Spacer(Modifier.height(spacing.medium))
    Row(horizontalArrangement = Arrangement.End, modifier = Modifier.fillMaxWidth()) {
        TextButton(onClick = onDismiss, enabled = !uiState.busy) {
            Text("Cancel")
        }
        Spacer(Modifier.width(spacing.small))
        Button(
            onClick = onCreate,
            enabled = !uiState.busy && uiState.branchDraft.isNotBlank(),
            modifier = Modifier.testTag(WorktreesSheetTags.CREATE_BUTTON),
        ) {
            Text("Confirm")
        }
    }
}

/**
 * The oracle's destructive-worktree dialog. `force` flips title/body/button
 * to the discard-changes copy after a `dirty_worktree_requires_force` reply.
 */
@Composable
fun RemoveWorktreeDialog(
    workspaceLabel: String,
    force: Boolean,
    busy: Boolean,
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = { if (!busy) onDismiss() },
        title = {
            Text(if (force) "Force remove $workspaceLabel?" else "Remove $workspaceLabel?")
        },
        text = {
            Text(
                if (force) {
                    "The checkout has uncommitted changes. Force removal permanently " +
                        "discards those checkout changes; the Git branch is retained."
                } else {
                    "This closes the Herdr workspace and removes its linked checkout. " +
                        "The Git branch is retained."
                },
            )
        },
        confirmButton = {
            Button(
                onClick = onConfirm,
                enabled = !busy,
                colors = ButtonDefaults.buttonColors(
                    containerColor = MaterialTheme.colorScheme.error,
                    contentColor = MaterialTheme.colorScheme.onError,
                ),
                modifier = Modifier.testTag(WorktreesSheetTags.REMOVE_CONFIRM),
            ) {
                Text(if (force) "Force Remove" else "Remove Worktree")
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss, enabled = !busy) {
                Text("Cancel")
            }
        },
    )
}

/** Stable semantics keys for tests and the screenshot suite. */
object WorktreesSheetTags {
    const val CONTENT = "worktrees:content"
    const val LOADING = "worktrees:loading"
    const val ERROR = "worktrees:error"
    const val ROW = "worktrees:row"
    const val STATUS = "worktrees:status"
    const val REMOVE_BUTTON = "worktrees:remove"
    const val REMOVE_CONFIRM = "worktrees:remove-confirm"
    const val BRANCH_FIELD = "worktrees:branch"
    const val BASE_FIELD = "worktrees:base"
    const val LABEL_FIELD = "worktrees:label"
    const val CREATE_BUTTON = "worktrees:create"
}

@PreviewLightDark
@Composable
private fun WorktreesSheetContentPreview() {
    LerdrTheme {
        Surface {
            WorktreesSheetContent(
                uiState = WorktreesUiState(
                    relayId = "r1",
                    workspaceId = "w1",
                    workspaceLabel = "lerdr",
                    workspacePath = "/home/u/lerdr",
                    loading = false,
                    listing = WorktreeListing(
                        source = WorktreeSource(
                            repoKey = "repo",
                            repoName = "lerdr",
                            repoRoot = "/home/u/lerdr",
                            sourceCheckoutPath = "/home/u/lerdr",
                            sourceWorkspaceId = "w1",
                        ),
                        worktrees = listOf(
                            WorktreeEntry(
                                path = "/home/u/lerdr",
                                branch = "main",
                                isLinkedWorktree = false,
                                label = "main",
                                openWorkspaceId = "w1",
                            ),
                            WorktreeEntry(
                                path = "/home/u/worktrees/fix-14",
                                branch = "fix/issue-14",
                                isLinkedWorktree = true,
                                label = "fix/issue-14",
                            ),
                        ),
                    ),
                ),
                onRefresh = {},
                onBranchDraftChange = {},
                onBaseDraftChange = {},
                onLabelDraftChange = {},
                onCreate = {},
                onOpenWorktree = { _, _ -> },
                onRequestRemove = {},
                onDismiss = {},
            )
        }
    }
}
