package com.lerdr.app.ui.settings

import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Surface
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onRoot
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage
import com.lerdr.app.TestApp
import com.lerdr.app.settings.PushPolicyContent
import com.lerdr.app.settings.PushPolicyUi
import com.lerdr.app.settings.PushPolicyUiState
import com.lerdr.app.settings.PushTestUi
import com.lerdr.core.designsystem.theme.LerdrTheme
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * Roborazzi coverage for the per-relay push-policy card — populated,
 * disconnected, and error/test-outcome states. [PushPolicyContent] is pure
 * state so no Hilt graph is needed under [TestApp].
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = TestApp::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class PushPolicySectionScreenshotTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private val options = RoborazziOptions(
        // Dump (the JVM default) paints a semantics-tree overlay whose
        // node text jitters run-to-run — force a plain bitmap capture.
        captureType = RoborazziOptions.CaptureType.Screenshot(),
        compareOptions = RoborazziOptions.CompareOptions(changeThreshold = 0.02f),
    )

    private fun capture(state: PushPolicyUiState) {
        composeRule.setContent {
            LerdrTheme {
                Surface {
                    Column(modifier = Modifier.padding(LerdrTheme.spacing.medium)) {
                        PushPolicyContent(
                            uiState = state,
                            onCategoryChange = { _, _ -> },
                            onSettleMs = {},
                            onCooldownMs = {},
                            onSnoozeOff = {},
                            onSnoozeFor = {},
                            onSnoozeIndefinitely = {},
                            onUpdateOnce = {},
                            onSendTest = {},
                            onRetry = {},
                        )
                    }
                }
            }
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun pushPolicy_populated() {
        capture(
            PushPolicyUiState(
                relayId = "r1",
                relayLabel = "workstation",
                connected = true,
                capabilitiesKnown = true,
                supported = true,
                policy = PushPolicyUi(
                    deviceId = "dev-1",
                    categories = PushPolicyUi.DEFAULT_CATEGORIES +
                        ("finished" to true),
                    settleMs = 5_000,
                    cooldownMs = 60_000,
                    snoozed = true,
                    snoozeUntil = "2030-01-01T00:00:00Z",
                    updateOnce = true,
                ),
            ),
        )
    }

    @Test
    fun pushPolicy_disconnected() {
        capture(
            PushPolicyUiState(
                relayId = "r1",
                relayLabel = "workstation",
                connected = false,
            ),
        )
    }

    @Test
    fun pushPolicy_error() {
        capture(
            PushPolicyUiState(
                relayId = "r1",
                relayLabel = "workstation",
                connected = true,
                capabilitiesKnown = true,
                supported = true,
                policy = PushPolicyUi(deviceId = "dev-1"),
                policyError =
                    "The relay did not save this notification policy (push_invalid_duration).",
                test = PushTestUi.Rejected("rate_limited"),
            ),
        )
    }
}
