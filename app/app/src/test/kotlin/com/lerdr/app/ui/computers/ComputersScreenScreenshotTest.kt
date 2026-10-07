package com.lerdr.app.ui.computers

import androidx.activity.ComponentActivity
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.navigation3.runtime.NavBackStack
import com.lerdr.navigation.LerdrKey
import com.lerdr.navigation.LerdrNavigator
import org.junit.Assert.assertEquals
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
 * Roborazzi coverage for the Computers tab — latency-band chips
 * (good/fair/poor), connected + offline + unmeasured variants, and the
 * empty state with the pair-device CTA.
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
                    onOpenRelay = {},
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
                            rttMs = 12,
                        ),
                        RelayCardUi(
                            relayId = "lan",
                            label = "lan-box",
                            transport = "direct",
                            statusLabel = "94ms",
                            agentCount = 2,
                            connected = true,
                            rttMs = 94,
                        ),
                        RelayCardUi(
                            relayId = "wan",
                            label = "wan-host",
                            transport = "tls",
                            statusLabel = "212ms",
                            agentCount = 1,
                            connected = true,
                            rttMs = 212,
                        ),
                        RelayCardUi(
                            relayId = "unmeasured",
                            label = "unmeasured",
                            transport = "direct",
                            statusLabel = "connected",
                            agentCount = 0,
                            connected = true,
                            rttMs = -1,
                        ),
                        RelayCardUi(
                            relayId = "workstation",
                            label = "workstation",
                            transport = "gateway",
                            statusLabel = "offline",
                            agentCount = 0,
                            connected = false,
                            rttMs = -1,
                        ),
                    ),
                    relaySummary = "5 computers · 1 offline",
                    onSelectTopLevel = {},
                    onPairDevice = {},
                    onOpenRelay = {},
                )
            }
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }
    @Test
    fun computerSelectionOpensItsDetailAndBackReturnsToComputers() {
        val navigator = LerdrNavigator(NavBackStack(LerdrKey.Home, LerdrKey.Computers))
        composeRule.setContent {
            LerdrTheme {
                ComputersContent(
                    relays = listOf(
                        RelayCardUi("one", "First computer", "tailscale", "12ms", 1, true, 12),
                        RelayCardUi("two", "Second computer", "tailscale", "offline", 0, false, -1),
                    ),
                    relaySummary = "2 computers",
                    onSelectTopLevel = {},
                    onPairDevice = {},
                    onOpenRelay = { navigator.navigate(LerdrKey.RelayDetail(it)) },
                )
            }
        }
        composeRule.onNodeWithText("Second computer").performClick()
        assertEquals(LerdrKey.RelayDetail("two"), navigator.backStack.last())
        navigator.goBack()
        assertEquals(LerdrKey.Computers, navigator.backStack.last())
        composeRule.onNodeWithText("First computer").performClick()
        assertEquals(LerdrKey.RelayDetail("one"), navigator.backStack.last())
    }

}
