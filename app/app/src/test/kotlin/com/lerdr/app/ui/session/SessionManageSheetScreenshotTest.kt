package com.lerdr.app.ui.session

import androidx.activity.ComponentActivity
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onRoot
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage
import com.lerdr.app.session.manage.ManageConfirm
import com.lerdr.app.session.manage.ManageSheetContent
import com.lerdr.app.session.manage.ManageUiState
import com.lerdr.core.designsystem.theme.LerdrTheme
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * Roborazzi coverage for the manage sheet — controller actions, the reader
 * gate, both confirm modes (clear/stop), a status line, and metadata.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = com.lerdr.app.TestApp::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class SessionManageSheetScreenshotTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private val options = RoborazziOptions(
        captureType = RoborazziOptions.CaptureType.Screenshot(),
        compareOptions = RoborazziOptions.CompareOptions(changeThreshold = 0.02f),
    )

    private fun baseState() = ManageUiState(
        paneId = "r1::%12",
        title = "lerdr",
        provider = "claude",
        relayLabel = "workstation",
        rawPaneId = "%12",
        cwd = "/home/u/Projects/lerdr",
        workspaceLabel = "lerdr",
        sessionName = "Fix the login bug",
        canControl = true,
        nameDraft = "lerdr",
    )

    private fun show(uiState: ManageUiState) {
        composeRule.setContent {
            LerdrTheme {
                ManageSheetContent(
                    uiState = uiState,
                    onNameDraftChange = {},
                    onSaveName = {},
                    onCopyResponse = {},
                    onRestart = {},
                    onBeginConfirm = {},
                    onCancelConfirm = {},
                    onConfirmAction = {},
                )
            }
        }
    }

    @Test
    fun manageSheet_controller() {
        show(baseState())
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun manageSheet_reader() {
        show(baseState().copy(canControl = false))
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun manageSheet_nameDirty() {
        show(baseState().copy(nameDraft = "lerdr renamed", nameDirty = true))
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun manageSheet_confirmClear() {
        show(baseState().copy(confirming = ManageConfirm.CLEAR))
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun manageSheet_confirmStop() {
        show(baseState().copy(confirming = ManageConfirm.STOP))
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun manageSheet_statusError() {
        show(
            baseState().copy(
                status = "Herdr agent inventory is not ready on this computer",
                statusError = true,
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }
}
