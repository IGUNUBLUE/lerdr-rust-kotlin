package com.lerdr.app.ui.session

import androidx.activity.ComponentActivity
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Stop
import androidx.compose.material.icons.filled.Tune
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.test.assertHeightIsAtLeast
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.unit.dp
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage
import com.google.common.truth.Truth.assertThat
import com.lerdr.app.session.SessionActionsSheet
import com.lerdr.app.session.SessionBarAction
import com.lerdr.app.session.SessionMode
import com.lerdr.app.session.SessionTopBar
import com.lerdr.core.designsystem.theme.LerdrTheme
import org.junit.After
import org.robolectric.RuntimeEnvironment
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.text.TextLayoutResult
import androidx.compose.ui.test.performSemanticsAction
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = com.lerdr.app.TestApp::class, qualifiers = "w393dp-h851dp")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class SessionActionsSheetTest {
    @get:Rule val composeRule = createAndroidComposeRule<ComponentActivity>()
    private val options = RoborazziOptions(captureType = RoborazziOptions.CaptureType.Screenshot())

    @After fun restoreFontScale() { RuntimeEnvironment.setFontScale(1f) }

    private fun actions() = listOf(
        SessionBarAction("Find in terminal", Icons.Default.Search, {}, "Search terminal output and history"),
        SessionBarAction("Text size", Icons.Default.Tune, {}, "Adjust terminal reading size"),
        SessionBarAction("Manage agent", Icons.Default.Tune, {}, "Rename, restart and transcript options", "Agent"),
        SessionBarAction("Manage worktrees", Icons.Default.Tune, {}, "Browse working copies", "Workspace"),
        SessionBarAction("Stop agent", Icons.Default.Stop, {}, "Closes this agent's pane on the computer", "End session", true),
        SessionBarAction("Close workspace", Icons.Default.Close, {}, "Closes every pane in this workspace", "End session", true),
    )

    @Test
    fun actionSheet_groups() {
        composeRule.setContent { LerdrTheme { SessionActionsSheet(actions(), {}) } }
        composeRule.onNodeWithText("View").assertIsDisplayed()
        composeRule.mainClock.advanceTimeBy(1000)
        composeRule.waitForIdle()
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun actionSheet_largeText() {
        RuntimeEnvironment.setFontScale(2f)
        composeRule.setContent { LerdrTheme { SessionActionsSheet(actions(), {}) } }
        composeRule.onNodeWithText("Close workspace").performScrollTo()
            .assertIsDisplayed().assertHeightIsAtLeast(48.dp)
        composeRule.onNodeWithText("Closes every pane in this workspace").assertIsDisplayed()
        val layouts = mutableListOf<TextLayoutResult>()
        composeRule.onNodeWithText("Close workspace", useUnmergedTree = true)
            .performSemanticsAction(SemanticsActions.GetTextLayoutResult) { it(layouts) }
        assertThat(layouts.single().getLineBottom(0))
            .isAtLeast(40f * composeRule.activity.resources.displayMetrics.density)
        composeRule.mainClock.advanceTimeBy(1000)
        composeRule.waitForIdle()
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun overflowDismissesBeforeInvokingAction() {
        var called = 0
        composeRule.setContent {
            LerdrTheme {
                SessionTopBar(
                    title = "Agent", breadcrumb = "Workspace", statusLabel = "idle",
                    statusColor = Color.Gray, mode = SessionMode.TERMINAL,
                    onSelectMode = {}, onBack = {},
                    actions = listOf(SessionBarAction("Find in terminal", Icons.Default.Search, { called++ })),
                )
            }
        }
        composeRule.onNodeWithContentDescription("Session actions").performClick()
        composeRule.onNodeWithText("Session actions").assertIsDisplayed()
        composeRule.onNodeWithText("Find in terminal").performClick()
        composeRule.onNodeWithText("Session actions").assertDoesNotExist()
        composeRule.runOnIdle { assertThat(called).isEqualTo(1) }
    }
}
