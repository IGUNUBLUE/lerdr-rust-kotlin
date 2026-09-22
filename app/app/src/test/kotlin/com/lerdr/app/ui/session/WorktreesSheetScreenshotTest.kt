package com.lerdr.app.ui.session

import androidx.activity.ComponentActivity
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onRoot
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage
import com.lerdr.app.session.RemoveWorktreeDialog
import com.lerdr.app.session.WorktreeEntry
import com.lerdr.app.session.WorktreeListing
import com.lerdr.app.session.WorktreeSource
import com.lerdr.app.session.WorktreesSheetContent
import com.lerdr.app.session.WorktreesUiState
import com.lerdr.core.designsystem.theme.LerdrTheme
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * Roborazzi coverage for the worktrees sheet — loading/error/empty, the
 * populated list with every row state, the create form with drafts, the
 * linked-worktree remove affordance, and both remove-dialog modes.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = com.lerdr.app.TestApp::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class WorktreesSheetScreenshotTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private val options = RoborazziOptions(
        captureType = RoborazziOptions.CaptureType.Screenshot(),
        compareOptions = RoborazziOptions.CompareOptions(changeThreshold = 0.02f),
    )

    private val listing = WorktreeListing(
        source = WorktreeSource(
            repoKey = "k1",
            repoName = "lerdr",
            repoRoot = "/home/u/lerdr",
            sourceCheckoutPath = "/home/u/lerdr",
            sourceWorkspaceId = "w0",
        ),
        worktrees = listOf(
            // The main checkout — already open in workspace w0.
            WorktreeEntry(
                path = "/home/u/lerdr",
                branch = "main",
                label = "main",
                openWorkspaceId = "w0",
            ),
            // A linked worktree that can be opened.
            WorktreeEntry(
                path = "/home/u/worktrees/fix-14",
                branch = "fix/issue-14",
                isLinkedWorktree = true,
                label = "fix/issue-14",
            ),
            // A prunable row — "Unavailable" chip.
            WorktreeEntry(
                path = "/home/u/worktrees/stale",
                branch = "stale-branch",
                isPrunable = true,
            ),
        ),
    )

    private fun baseState() = WorktreesUiState(
        relayId = "r1",
        workspaceId = "w1",
        workspaceLabel = "lerdr",
        workspacePath = "/home/u/worktrees/fix-14",
        linkedWorktree = true,
        managementAvailable = true,
        loading = false,
        listing = listing,
    )

    private fun show(uiState: WorktreesUiState) {
        composeRule.setContent {
            LerdrTheme {
                WorktreesSheetContent(
                    uiState = uiState,
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

    private fun showDialog(force: Boolean) {
        composeRule.setContent {
            LerdrTheme {
                RemoveWorktreeDialog(
                    workspaceLabel = "lerdr",
                    force = force,
                    busy = false,
                    onConfirm = {},
                    onDismiss = {},
                )
            }
        }
    }

    @Test
    fun worktrees_loading() {
        show(baseState().copy(loading = true, listing = null))
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun worktrees_error() {
        show(
            baseState().copy(
                listing = null,
                error = "This relay does not support worktree management",
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun worktrees_populated() {
        show(baseState())
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun worktrees_empty() {
        show(
            baseState().copy(
                listing = listing.copy(worktrees = emptyList()),
                linkedWorktree = false,
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun worktrees_createDrafts() {
        show(
            baseState().copy(
                branchDraft = "fix/issue-27",
                baseDraft = "main",
                labelDraft = "issue 27",
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun worktrees_statusError() {
        show(
            baseState().copy(
                status = "Check the worktree list before retrying.",
                statusError = true,
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun worktrees_removeConfirm() {
        showDialog(force = false)
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun worktrees_removeConfirmForce() {
        showDialog(force = true)
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }
}
