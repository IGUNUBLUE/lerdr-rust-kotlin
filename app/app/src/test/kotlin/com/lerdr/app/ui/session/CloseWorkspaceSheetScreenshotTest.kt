package com.lerdr.app.ui.session

import androidx.activity.ComponentActivity
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onRoot
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage
import com.lerdr.app.session.CloseWorkspaceSheetContent
import com.lerdr.app.session.closePlanFor
import com.lerdr.core.designsystem.theme.LerdrTheme
import lerdr.core.model.WorkspaceWorktree
import lerdr.core.store.RelayWorkspace
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * Roborazzi coverage for `CloseWorkspaceSheetContent` — the single close,
 * the primary-of-group forced group close, the linked-worktree choice, and
 * the inline error line.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = com.lerdr.app.TestApp::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class CloseWorkspaceSheetScreenshotTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private val options = RoborazziOptions(
        captureType = RoborazziOptions.CaptureType.Screenshot(),
        compareOptions = RoborazziOptions.CompareOptions(changeThreshold = 0.02f),
    )

    private fun workspace(
        id: String,
        label: String = id,
        repoKey: String = "",
        linked: Boolean = false,
    ) = RelayWorkspace(
        relayId = "r1",
        relayLabel = "workstation",
        workspaceId = id,
        label = label,
        worktree = if (repoKey.isEmpty()) {
            null
        } else {
            WorkspaceWorktree(
                repoKey = repoKey,
                repoName = "lerdr",
                repoRoot = "/home/u/lerdr",
                checkoutPath = "/home/u/lerdr/$id",
                isLinkedWorktree = linked,
            )
        },
    )

    private fun sheet(
        plan: com.lerdr.app.session.ClosePlan,
        busy: Boolean = false,
        error: String? = null,
    ) {
        composeRule.setContent {
            LerdrTheme {
                CloseWorkspaceSheetContent(
                    plan = plan,
                    busy = busy,
                    error = error,
                    onCloseSingle = {},
                    onCloseGroup = {},
                    onDismiss = {},
                )
            }
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun closeWorkspace_single() {
        sheet(
            closePlanFor(
                listOf(workspace("w1", label = "lerdr")),
                relayId = "r1",
                workspaceId = "w1",
            ),
        )
    }

    @Test
    fun closeWorkspace_primaryGroup() {
        sheet(
            closePlanFor(
                listOf(
                    workspace("main", label = "lerdr", repoKey = "repo"),
                    workspace("wt-1", label = "wt-1", repoKey = "repo", linked = true),
                    workspace("wt-2", label = "wt-2", repoKey = "repo", linked = true),
                ),
                relayId = "r1",
                workspaceId = "main",
            ),
        )
    }

    @Test
    fun closeWorkspace_linkedGroup() {
        sheet(
            closePlanFor(
                listOf(
                    workspace("main", label = "lerdr", repoKey = "repo"),
                    workspace("wt-1", label = "wt-1", repoKey = "repo", linked = true),
                ),
                relayId = "r1",
                workspaceId = "wt-1",
            ),
        )
    }

    @Test
    fun closeWorkspace_error() {
        sheet(
            closePlanFor(
                listOf(workspace("w1", label = "lerdr")),
                relayId = "r1",
                workspaceId = "w1",
            ),
            error = "The workspace group changed — review it and try again.",
        )
    }
}
