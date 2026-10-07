package com.lerdr.app.ui.pairing

import androidx.activity.ComponentActivity
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextClearance
import androidx.compose.ui.test.performTextInput
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage
import com.lerdr.app.pairing.PairingContent
import com.lerdr.app.pairing.PairingUiState
import com.lerdr.core.designsystem.theme.LerdrTheme
import com.lerdr.navigation.SetupLink
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = com.lerdr.app.TestApp::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class PairingScreenBehaviorTest {
    @get:Rule val composeRule = createAndroidComposeRule<ComponentActivity>()

    private val original = SetupLink(
        setup = "s".repeat(43),
        label = "Original computer",
        relay = "ws://127.0.0.1:8377",
    )
    private val replacement = "lerdr://pair#setup=${"r".repeat(43)}" +
        "&label=Replacement&relay=ws://127.0.0.1:8378"
    private val connections = mutableListOf<String>()

    private fun show() {
        composeRule.setContent {
            LerdrTheme {
                PairingContent(
                    setupLink = original,
                    uiState = PairingUiState(),
                    onConnectLink = { connections += it.displayName },
                    onConnectPasted = { connections += it },
                    onConnectScanned = {},
                    onBack = {},
                )
            }
        }
    }

    @Test
    fun untouchedDeepLinkRequiresConfirmation() {
        show()
        assertEquals(emptyList<String>(), connections)
        composeRule.onNodeWithText("Original computer").assertExists()
        composeRule.onNodeWithText("Connect").performClick()
        assertEquals(listOf("Original computer"), connections)
    }

    @Test
    fun pastedReplacementIsTheConfirmedTarget() {
        show()
        composeRule.onNodeWithText("Setup link").performTextInput(replacement)
        composeRule.onNodeWithText("Original computer").assertDoesNotExist()
        composeRule.onNodeWithText("Replacement").assertExists()
        composeRule.onRoot().captureRoboImage(
            roborazziOptions = RoborazziOptions(captureType = RoborazziOptions.CaptureType.Screenshot()),
        )
        composeRule.onNodeWithText("Connect").performClick()
        assertEquals(listOf(replacement), connections)
    }

    @Test
    fun invalidOrClearedReplacementNeverRestoresOldTarget() {
        show()
        val field = composeRule.onNodeWithText("Setup link")
        field.performTextInput("invalid replacement")
        composeRule.onNodeWithText("Connect").assertIsNotEnabled()
        composeRule.onNodeWithText("Original computer").assertDoesNotExist()
        field.performTextClearance()
        composeRule.onNodeWithText("Connect").assertIsNotEnabled()
        assertEquals(emptyList<String>(), connections)
    }
}
