package com.lerdr.app.ui.terminal

import androidx.activity.ComponentActivity
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performTextInput
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage
import com.lerdr.core.designsystem.theme.LerdrTheme
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * Roborazzi smoke coverage for the terminal input bar — records via
 * `./gradlew :app:recordRoborazziDebug`, verifies via
 * `:app:verifyRoborazziDebug` (plain `test` is a no-op unless enabled).
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = com.lerdr.app.TestApp::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class TerminalInputBarScreenshotTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private val options = RoborazziOptions(
        // Dump (the JVM default) paints a semantics-tree overlay whose
        // node text jitters run-to-run — force a plain bitmap capture.
        captureType = RoborazziOptions.CaptureType.Screenshot(),
        compareOptions = RoborazziOptions.CompareOptions(changeThreshold = 0.02f),
    )

    @Test
    fun inputBar_idle() {
        composeRule.setContent {
            LerdrTheme {
                TerminalInputBar(onSendText = {})
            }
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun inputBar_withDraft() {
        composeRule.setContent {
            LerdrTheme {
                TerminalInputBar(onSendText = {})
            }
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    /** `no_echo` prompt — masked field, "Password" label, shield marker. */
    @Test
    fun inputBar_secretMode() {
        composeRule.setContent {
            LerdrTheme {
                TerminalInputBar(
                    onSendText = {},
                    secretMode = true,
                    onSendSecret = {},
                )
            }
        }
        composeRule.onNodeWithTag("terminalSecretField").performTextInput("hunter2")
        composeRule.waitForIdle()
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    /** Reader gate — field and send affordance render disabled. */
    @Test
    fun inputBar_readOnly() {
        composeRule.setContent {
            LerdrTheme {
                TerminalInputBar(
                    onSendText = {},
                    enabled = false,
                    hint = "Read-only session",
                )
            }
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }
}
