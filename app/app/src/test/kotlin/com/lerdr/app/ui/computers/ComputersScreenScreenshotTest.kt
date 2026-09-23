package com.lerdr.app.ui.computers

import androidx.activity.ComponentActivity
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onRoot
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage
import com.lerdr.app.computers.ComputersContent
import com.lerdr.app.home.RelayCardUi
import com.lerdr.core.designsystem.theme.LerdrTheme
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * Roborazzi coverage for the Computers tab — one connected relay and one
 * offline relay, plus the empty state.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = com.lerdr.app.TestApp::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class ComputersScreenScreenshotTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private val options = RoborazziOptions(
        captureType = RoborazziOptions.CaptureType.Screenshot(),
        compareOptions = RoborazziOptions.CompareOptions(changeThreshold = 0.02f),
    )

    @Test
    fun computers_empty() {
        composeRule.setContent {
            LerdrTheme {
                ComputersContent(
                    relays = emptyList(),
                    relaySummary = "",
                    onSelectTopLevel = {},
                    onPairDevice = {},
                    onManageDevices = {},
                )
            }
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun computers_relays() {
        composeRule.setContent {
            LerdrTheme {
                ComputersContent(
                    relays = listOf(
                        RelayCardUi(
                            relayId = "sd",
                            label = "sd",
                            transport = "tailscale",
                            statusLabel = "12ms",
                            agentCount = 4,
                            connected = true,
                        ),
                        RelayCardUi(
                            relayId = "workstation",
                            label = "workstation",
                            transport = "gateway",
                            statusLabel = "offline",
                            agentCount = 0,
                            connected = false,
                        ),
                    ),
                    relaySummary = "2 computers · tailscale",
                    onSelectTopLevel = {},
                    onPairDevice = {},
                    onManageDevices = {},
                )
            }
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }
}
