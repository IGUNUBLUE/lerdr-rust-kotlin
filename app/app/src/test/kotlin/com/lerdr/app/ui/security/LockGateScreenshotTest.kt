package com.lerdr.app.ui.security

import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage
import com.google.common.truth.Truth.assertThat
import com.lerdr.app.TestApp
import com.lerdr.app.security.LockGate
import com.lerdr.core.designsystem.theme.LerdrTheme
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * Roborazzi coverage for the app-lock gate — the locked brand surface
 * (which also auto-requests one unlock on entry) and the pass-through
 * unlocked state.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = TestApp::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class LockGateScreenshotTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private val options = RoborazziOptions(
        // Dump (the JVM default) paints a semantics-tree overlay whose
        // node text jitters run-to-run — force a plain bitmap capture.
        captureType = RoborazziOptions.CaptureType.Screenshot(),
        compareOptions = RoborazziOptions.CompareOptions(changeThreshold = 0.02f),
    )

    @Test
    fun lockGate_locked() {
        var unlockRequests = 0
        composeRule.setContent {
            LerdrTheme {
                LockGate(
                    locked = true,
                    onUnlockRequest = { unlockRequests++ },
                ) {
                    Text("mission control")
                }
            }
        }
        // The gated content is not composed…
        composeRule.onNodeWithText("mission control").assertDoesNotExist()
        composeRule.onNodeWithText("Unlock").assertIsDisplayed()
        // …and entering the locked state fires exactly one prompt request.
        assertThat(unlockRequests).isEqualTo(1)
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun lockGate_unlocked() {
        var unlockRequests = 0
        composeRule.setContent {
            LerdrTheme {
                LockGate(
                    locked = false,
                    onUnlockRequest = { unlockRequests++ },
                ) {
                    Surface(modifier = Modifier.fillMaxSize()) {
                        Text(
                            "mission control",
                            color = MaterialTheme.colorScheme.onSurface,
                        )
                    }
                }
            }
        }
        composeRule.onNodeWithText("mission control").assertIsDisplayed()
        assertThat(unlockRequests).isEqualTo(0)
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }
}
