package com.lerdr.app.ui.home

import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onRoot
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage
import com.lerdr.app.home.AgentGroupUi
import com.lerdr.app.home.AgentListItemUi
import com.lerdr.app.home.AttentionCardUi
import com.lerdr.app.home.AttentionKind
import com.lerdr.app.home.DirectoryBrowserUi
import com.lerdr.app.home.HomeContent
import com.lerdr.app.home.HomeFabMenu
import com.lerdr.app.home.HomeUiState
import com.lerdr.app.home.LaunchRelayOption
import com.lerdr.app.home.LaunchUiState
import com.lerdr.app.home.LaunchWorkspaceOption
import com.lerdr.app.home.NewAgentSheetContent
import com.lerdr.app.home.NewWorkspaceSheetContent
import com.lerdr.app.session.DirectoryEntry
import com.lerdr.app.session.DirectoryListing
import com.lerdr.core.designsystem.theme.LerdrTheme
import lerdr.core.model.AgentProfile
import lerdr.core.model.Interaction
import lerdr.core.model.Option
import lerdr.core.model.Other
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * Roborazzi coverage for mission control — needs-you cards with inline
 * answers, `relay ▸ workspace` grouped rows with provider-logo avatars,
 * and the empty state.
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
                            AgentGroupUi(
                                key = "sd\u0000lerdr",
                                relayLabel = "sd",
                                label = "lerdr",
                                watchingDevices = 2,
                                agents = listOf(
                                    AgentListItemUi(
                                        paneId = "sd::%3",
                                        relayId = "sd",
                                        title = "claude · api-server",
                                        statusLine = "Editing handler.go",
                                        activityLabel = "running tests…",
                                        elapsedLabel = "1:24",
                                        working = true,
                                        controllable = true,
                                        provider = "claude",
                                        watching = true,
                                        stateLabels = listOf("planning"),
                                    ),
                                ),
                            ),
                        ),
                        idle = listOf(
                            AgentGroupUi(
                                key = "sd\u0000web",
                                relayLabel = "sd",
                                label = "web",
                                agents = listOf(
                                    AgentListItemUi(
                                        paneId = "sd::%5",
                                        relayId = "sd",
                                        title = "codex · web",
                                        statusLine = "ready · 12m ago",
                                        activityLabel = null,
                                        elapsedLabel = "idle",
                                        working = false,
                                        controllable = true,
                                        provider = "codex",
                                    ),
                                    AgentListItemUi(
                                        paneId = "sd::%6",
                                        relayId = "sd",
                                        title = "devin · herdr",
                                        statusLine = "idle · 1h ago",
                                        activityLabel = null,
                                        elapsedLabel = "idle",
                                        working = false,
                                        controllable = true,
                                        provider = "devin",
                                    ),
                                    AgentListItemUi(
                                        paneId = "sd::%7",
                                        relayId = "sd",
                                        title = "agent · dotfiles",
                                        statusLine = "idle · 2h ago",
                                        activityLabel = null,
                                        elapsedLabel = "idle",
                                        working = false,
                                        controllable = true,
                                        provider = null,
                                    ),
                                ),
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

    @Test
    fun home_needs_you() {
        composeRule.setContent {
            LerdrTheme {
                HomeContent(
                    uiState = HomeUiState(
                        live = true,
                        relaySummary = "1 computer · tailscale",
                        needsYou = listOf(
                            AttentionCardUi(
                                paneId = "sd::%1",
                                relayId = "sd",
                                agentLabel = "claude · lerdr",
                                kind = AttentionKind.APPROVAL,
                                metaLabel = "approval · 40s",
                                prompt = "Run go test ./internal/… ?",
                                options = listOf("Allow", "Always allow", "Deny"),
                                controllable = true,
                                provider = "claude",
                            ),
                            AttentionCardUi(
                                paneId = "sd::%2",
                                relayId = "sd",
                                agentLabel = "devin · herdr",
                                kind = AttentionKind.QUESTION,
                                metaLabel = "question · 3 options",
                                prompt = "Which module should own the delta cache?",
                                interaction = Interaction(
                                    id = "q1",
                                    kind = "single_select",
                                    question = "Which module should own the delta cache?",
                                    options = listOf(
                                        Option(index = 0, label = "core:store"),
                                        Option(index = 1, label = "session"),
                                        Option(index = 2, label = "relay"),
                                    ),
                                    other = Other(hidden = true),
                                    questionTotal = 1,
                                ),
                                controllable = true,
                                provider = "devin",
                            ),
                            AttentionCardUi(
                                paneId = "sd::%8",
                                relayId = "sd",
                                agentLabel = "codex · web",
                                kind = AttentionKind.QUESTION,
                                metaLabel = "question · 2 options",
                                prompt = "Pick every file to include.",
                                interaction = Interaction(
                                    id = "q2",
                                    kind = "multi_select",
                                    question = "Pick every file to include.",
                                    options = listOf(
                                        Option(index = 0, label = "a.kt"),
                                        Option(index = 1, label = "b.kt"),
                                    ),
                                    other = Other(hidden = true),
                                    questionTotal = 1,
                                ),
                                controllable = true,
                                provider = "codex",
                            ),
                            AttentionCardUi(
                                paneId = "sd::%9",
                                relayId = "sd",
                                agentLabel = "claude · lerdr",
                                kind = AttentionKind.APPROVAL,
                                metaLabel = "approval · 2m",
                                prompt = "Waiting for agent…",
                                options = listOf("Allow", "Deny"),
                                responding = true,
                                controllable = true,
                                provider = "claude",
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

    @Test
    fun home_empty() {
        composeRule.setContent {
            LerdrTheme {
                HomeContent(
                    uiState = HomeUiState(),
                    onOpenAgent = {},
                    onSelectTopLevel = {},
                )
            }
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun home_refreshing() {
        // The oracle's `pullRefreshing` window — the pull indicator holds
        // over the list for ~900 ms after the gesture fires.
        composeRule.setContent {
            LerdrTheme {
                HomeContent(
                    uiState = HomeUiState(
                        live = true,
                        relaySummary = "1 computer · tailscale",
                        idle = listOf(
                            AgentGroupUi(
                                key = "sd web",
                                relayLabel = "sd",
                                label = "web",
                                agents = listOf(
                                    AgentListItemUi(
                                        paneId = "sd::%5",
                                        relayId = "sd",
                                        title = "codex · web",
                                        statusLine = "ready · 12m ago",
                                        activityLabel = null,
                                        elapsedLabel = "idle",
                                        working = false,
                                        controllable = true,
                                        provider = "codex",
                                    ),
                                ),
                            ),
                        ),
                    ),
                    onOpenAgent = {},
                    onSelectTopLevel = {},
                    refreshing = true,
                )
            }
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun home_fab_menu_expanded() {
        composeRule.setContent {
            LerdrTheme {
                Box(
                    contentAlignment = Alignment.BottomEnd,
                    modifier = Modifier
                        .fillMaxSize()
                        .padding(LerdrTheme.spacing.large),
                ) {
                    HomeFabMenu(
                        expanded = true,
                        onExpandedChange = {},
                        onNewAgent = {},
                        onNewWorkspace = {},
                    )
                }
            }
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun home_new_agent_sheet() {
        composeRule.setContent {
            LerdrTheme {
                NewAgentSheetContent(
                    uiState = LaunchUiState(
                        relays = listOf(
                            LaunchRelayOption("sd", "sd"),
                            LaunchRelayOption("workstation", "workstation"),
                        ),
                        relayId = "sd",
                        profiles = listOf(
                            AgentProfile(id = "claude", label = "Claude"),
                            AgentProfile(id = "codex", label = "Codex"),
                        ),
                        profileId = "claude",
                        name = "lerdr-claude",
                        cwd = "/home/u/lerdr",
                        cwdLabel = "lerdr",
                        directoryReady = true,
                        workspaces = listOf(LaunchWorkspaceOption("w1", "lerdr")),
                        workspaceId = "w1",
                        workspaceTargetLabel = "lerdr",
                        directory = DirectoryBrowserUi(
                            open = false,
                            supported = true,
                            listing = DirectoryListing(
                                currentPath = "/home/u/lerdr",
                                currentLabel = "lerdr",
                                parent = "/home/u",
                                directories = listOf(
                                    DirectoryEntry("app", "/home/u/lerdr/app"),
                                    DirectoryEntry("relay", "/home/u/lerdr/relay"),
                                ),
                            ),
                        ),
                    ),
                    onRelaySelect = {},
                    onProfileSelect = {},
                    onWorkspaceSelect = {},
                    onNameChange = {},
                    onPromptChange = {},
                    onBrowseDirectories = {},
                    onSubmit = {},
                )
            }
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun home_new_workspace_sheet() {
        composeRule.setContent {
            LerdrTheme {
                NewWorkspaceSheetContent(
                    uiState = LaunchUiState(
                        relays = listOf(LaunchRelayOption("sd", "sd")),
                        relayId = "sd",
                        cwd = "/home/u/lerdr",
                        cwdLabel = "lerdr",
                        workspaceLabel = "lerdr",
                    ),
                    onRelaySelect = {},
                    onLabelChange = {},
                    onBrowseDirectories = {},
                    onSubmit = {},
                )
            }
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }
}
