package com.lerdr.app.ui.session

import androidx.activity.ComponentActivity
import androidx.compose.runtime.MutableState
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.click
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.test.performTouchInput
import com.lerdr.app.session.TerminalContent
import com.lerdr.app.session.TerminalUiState
import com.lerdr.core.designsystem.theme.LerdrTheme
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = com.lerdr.app.TestApp::class)
class TerminalControlBehaviorTest {
    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private fun show(state: MutableState<TerminalUiState>, keys: MutableList<List<String>>) {
        composeRule.setContent {
            LerdrTheme {
                TerminalContent(
                    uiState = state.value,
                    onOpenFeed = {},
                    onOpenFiles = {},
                    onBack = {},
                    onSendKeys = { keys += it },
                    onSendText = { true },
                    onViewportMeasured = { _, _ -> },
                    onRefresh = {},
                )
            }
        }
    }

    private fun controller() = TerminalUiState(
        paneId = "relay::pane",
        connected = true,
        canControl = true,
    )

    @Test
    fun interruptAndEofRequireConfirmationAndCancellationSendsNothing() {
        val keys = mutableListOf<List<String>>()
        show(mutableStateOf(controller()), keys)
        composeRule.onNodeWithContentDescription("More terminal keys").performClick()
        composeRule.onNodeWithText("C-c").performClick()
        composeRule.runOnIdle { assertEquals(emptyList<List<String>>(), keys) }
        composeRule.onNodeWithText("Cancel").performClick()
        composeRule.runOnIdle { assertEquals(emptyList<List<String>>(), keys) }
        composeRule.onNodeWithContentDescription("More terminal keys").performClick()
        composeRule.onNodeWithText("C-d").performClick()
        composeRule.runOnIdle { assertEquals(emptyList<List<String>>(), keys) }
        composeRule.onNodeWithText("Send Ctrl+D").performClick()
        composeRule.runOnIdle { assertEquals(listOf(listOf("Ctrl+D")), keys) }
    }

    @Test
    fun enterSendsDirectlyFromKeyBarWithoutOpeningSheet() {
        val keys = mutableListOf<List<String>>()
        show(mutableStateOf(controller()), keys)
        composeRule.onNodeWithText("Enter").assertIsEnabled().performTouchInput { click(center) }
        composeRule.onNodeWithContentDescription("Up").assertIsDisplayed().performTouchInput { click(center) }
        composeRule.onNodeWithContentDescription("Down").assertIsDisplayed().performTouchInput { click(center) }
        composeRule.runOnIdle {
            assertEquals(listOf(listOf("Enter"), listOf("Up"), listOf("Down")), keys)
        }
        composeRule.onNodeWithText("C-c").assertDoesNotExist()
    }

    @Test
    fun extraKeysRemainReachableWithoutHorizontalScrolling() {
        val keys = mutableListOf<List<String>>()
        show(mutableStateOf(controller()), keys)
        composeRule.onNodeWithContentDescription("More terminal keys").performClick()
        composeRule.onNodeWithText("Esc").assertIsDisplayed().performClick()
        composeRule.runOnIdle { assertEquals(listOf(listOf("Escape")), keys) }
        composeRule.onNodeWithContentDescription("More terminal keys").performClick()
        composeRule.onNodeWithText("Tab").assertIsDisplayed().performClick()
        composeRule.runOnIdle { assertEquals(listOf(listOf("Escape"), listOf("Tab")), keys) }
    }

    @Test
    fun disconnectDropsPendingInterruptWithoutRearmingOnReconnect() {
        val keys = mutableListOf<List<String>>()
        val state = mutableStateOf(controller())
        show(state, keys)
        composeRule.onNodeWithContentDescription("More terminal keys").performClick()
        composeRule.onNodeWithText("C-c").performClick()
        composeRule.runOnIdle { state.value = state.value.copy(connected = false, connecting = true) }
        composeRule.onNodeWithText("Send Ctrl+C").assertDoesNotExist()
        composeRule.onNodeWithTag("terminalInputField").assertIsEnabled().performTextInput("offline draft")
        composeRule.onNodeWithTag("terminalSendButton").assertIsNotEnabled()
        composeRule.runOnIdle { state.value = state.value.copy(connected = true, connecting = false) }
        composeRule.onNodeWithText("Send Ctrl+C").assertDoesNotExist()
        composeRule.onNodeWithTag("terminalSendButton").assertIsEnabled()
        composeRule.runOnIdle { assertEquals(emptyList<List<String>>(), keys) }
    }

    @Test
    fun readersAndUnsupportedPasswordPromptsCannotEditOrSend() {
        val keys = mutableListOf<List<String>>()
        val state = mutableStateOf(controller().copy(canControl = false))
        show(state, keys)
        composeRule.onNodeWithTag("terminalInputField").assertDoesNotExist()
        composeRule.onNodeWithContentDescription("More terminal keys").assertDoesNotExist()
        composeRule.onNodeWithText("Observing · read-only").assertIsDisplayed()
        composeRule.runOnIdle { state.value = controller().copy(noEcho = true, secretInputSupported = false) }
        composeRule.onNodeWithTag("terminalInputField").assertDoesNotExist()
        composeRule.onNodeWithTag("terminalSecretField").assertIsNotEnabled()
        composeRule.onNodeWithTag("terminalSendButton").assertIsNotEnabled()
        composeRule.runOnIdle { assertEquals(emptyList<List<String>>(), keys) }
    }
}
