package com.lerdr.app.ui.home

import androidx.activity.ComponentActivity
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onRoot
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage
import com.lerdr.app.home.AgentListItemUi
import com.lerdr.app.home.HomeContent
import com.lerdr.app.home.HomeUiState
import com.lerdr.core.designsystem.theme.LerdrTheme
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * Roborazzi coverage for mission control — agent rows with provider-logo
 * avatars (claude/codex) alongside the letter monogram for unknown agents.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = com.lerdr.app.TestApp::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class HomeScreenScreenshotTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private val options = RoborazziOptions(
        captureType = RoborazziOptions.CaptureType.Screenshot(),
        compareOptions = RoborazziOptions.CompareOptions(changeThreshold = 0.02f),
    )

    @Test
    fun home_agents() {
        composeRule.setContent {
            LerdrTheme {
                HomeContent(
                    uiState = HomeUiState(
                        live = true,
                        relaySummary = "1 computer · tailscale",
                        working = listOf(
                            AgentListItemUi(
                                paneId = "sd::%3",
                                title = "claude · api-server",
                                statusLine = "Editing handler.go",
                                activityLabel = "running tests…",
                                elapsedLabel = "1:24",
                                working = true,
                                provider = "claude",
                            ),
                        ),
                        idle = listOf(
                            AgentListItemUi(
                                paneId = "sd::%5",
                                title = "codex · web",
                                statusLine = "ready · 12m ago",
                                activityLabel = null,
                                elapsedLabel = "idle",
                                working = false,
                                provider = "codex",
                            ),
                            AgentListItemUi(
                                paneId = "sd::%6",
                                title = "devin · herdr",
                                statusLine = "idle · 1h ago",
                                activityLabel = null,
                                elapsedLabel = "idle",
                                working = false,
                                provider = "devin",
                            ),
                            AgentListItemUi(
                                paneId = "sd::%7",
                                title = "agent · dotfiles",
                                statusLine = "idle · 2h ago",
                                activityLabel = null,
                                elapsedLabel = "idle",
                                working = false,
                                provider = null,
                            ),
                        ),
                    ),
                    onOpenAgent = {},
                    onSelectTopLevel = {},
                )
            }
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }
}
