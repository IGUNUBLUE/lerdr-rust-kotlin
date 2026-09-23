package com.lerdr.app.ui.session

import androidx.activity.ComponentActivity
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onRoot
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage
import com.lerdr.app.session.DirectoryEntry
import com.lerdr.app.session.DirectoryListing
import com.lerdr.app.session.WorkspaceCreateContent
import com.lerdr.app.session.WorkspaceTabUi
import com.lerdr.app.session.WorkspaceTabsStripContent
import com.lerdr.app.session.WorkspaceTabsUiState
import com.lerdr.core.designsystem.theme.LerdrTheme
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * Roborazzi coverage for the workspace tab strip — controller strip with
 * the "+" affordance, the open tab action menu, the rename + close dialogs
 * (solo and group), the create sheet (browser + plain-cwd modes), and the
 * reader gate.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = com.lerdr.app.TestApp::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class SessionTabsStripScreenshotTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private val options = RoborazziOptions(
        captureType = RoborazziOptions.CaptureType.Screenshot(),
        compareOptions = RoborazziOptions.CompareOptions(changeThreshold = 0.02f),
    )

    private fun tabs() = listOf(
        WorkspaceTabUi("tabA", "main", 1, 0, "r1::%1", 1),
        WorkspaceTabUi("tabB", "tests", 2, 1, "r1::%2", 2),
        WorkspaceTabUi("tabC", "review", 3, 2, "r1::%3", 1),
    )

    private fun baseState() = WorkspaceTabsUiState(
        tabs = tabs(),
        activeTabId = "tabA",
        reorderAvailable = true,
        canControl = true,
        managementAvailable = true,
        workspaceLabel = "lerdr",
    )

    private fun show(uiState: WorkspaceTabsUiState) {
        composeRule.setContent {
            LerdrTheme {
                WorkspaceTabsStripContent(
                    uiState = uiState,
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

    @Test
    fun strip_controller() {
        show(baseState())
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun strip_reader() {
        show(baseState().copy(canControl = false))
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun strip_menuOpen() {
        show(baseState().copy(menuTabId = "tabB"))
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun strip_renameDialog() {
        show(baseState().copy(renameOpen = true, renameDraft = "lerdr"))
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun strip_closeConfirm() {
        show(baseState().copy(confirmClose = true))
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun strip_closeConfirmGroup() {
        show(
            baseState().copy(
                confirmClose = true,
                confirmGroup = true,
                groupMembers = listOf("lerdr", "fix", "review"),
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun strip_statusLine() {
        show(
            baseState().copy(
                status = "Workspace group changed. Review the current group before closing it.",
                statusError = true,
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    private fun showCreate(uiState: WorkspaceTabsUiState) {
        composeRule.setContent {
            LerdrTheme {
                WorkspaceCreateContent(
                    uiState = uiState,
                    onCwdChange = {},
                    onLabelChange = {},
                    onToggleDirectoryBrowser = {},
                    onBrowseDirectory = {},
                    onConfirm = {},
                    onCancel = {},
                )
            }
        }
    }

    @Test
    fun createSheet_plainCwd() {
        showCreate(
            baseState().copy(
                createOpen = true,
                directoryBrowserAvailable = false,
                createCwd = "/home/u/projects/new",
                createLabel = "new",
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun createSheet_directoryBrowser() {
        showCreate(
            baseState().copy(
                createOpen = true,
                directoryBrowserAvailable = true,
                directoryOpen = true,
                directory = DirectoryListing(
                    currentPath = "/home/u",
                    currentLabel = "u",
                    parent = "/",
                    directories = listOf(
                        DirectoryEntry("lerdr", "/home/u/lerdr"),
                        DirectoryEntry("tmp", "/home/u/tmp"),
                    ),
                ),
                createCwd = "/home/u",
                createLabel = "u",
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }
}
