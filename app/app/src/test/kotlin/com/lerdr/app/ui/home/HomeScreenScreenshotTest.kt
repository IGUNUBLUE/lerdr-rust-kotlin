package com.lerdr.app.ui.home

import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.text.TextLayoutResult
import androidx.compose.ui.unit.Density
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performSemanticsAction
import androidx.navigation3.runtime.NavBackStack
import com.google.common.truth.Truth.assertThat
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
import com.lerdr.app.home.RelayCardUi
import com.lerdr.app.home.LaunchRelayOption
import com.lerdr.app.home.LaunchUiState
import com.lerdr.app.home.LaunchWorkspaceOption
import com.lerdr.app.home.NewAgentSheetContent
import com.lerdr.app.home.NewWorkspaceSheetContent
import com.lerdr.app.session.DirectoryEntry
import com.lerdr.app.session.DirectoryListing
import com.lerdr.core.designsystem.theme.LerdrTheme
import com.lerdr.navigation.LerdrKey
import com.lerdr.navigation.LerdrNavigator
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
    fun home_reader() {
        val canLaunch = mutableStateOf(false)
        composeRule.setContent {
            LerdrTheme {
                HomeContent(
                    uiState = HomeUiState(
                        live = true,
                        canLaunch = canLaunch.value,
                        relaySummary = "1 computer",
                        relays = listOf(
                            RelayCardUi(
                                relayId = "reader",
                                label = "workstation",
                                transport = "websocket",
                                statusLabel = "connected",
                                agentCount = 0,
                                connected = true,
                            ),
                        ),
                    ),
                    onOpenAgent = {},
                    onOpenAttention = {},
                    onSelectTopLevel = {},
                )
            }
        }
        composeRule.onNodeWithContentDescription("New agent or workspace")
            .assertDoesNotExist()
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
        composeRule.runOnIdle { canLaunch.value = true }
        composeRule.onNodeWithContentDescription("New agent or workspace")
            .performClick()
        composeRule.onNodeWithText("New agent").assertExists()
        composeRule.runOnIdle { canLaunch.value = false }
        composeRule.onNodeWithText("New agent").assertDoesNotExist()
        composeRule.onNodeWithText("New workspace").assertDoesNotExist()
    }

    @Test
    fun home_agents() {
        composeRule.setContent {
            LerdrTheme {
                HomeContent(
                    uiState = HomeUiState(
                        live = true,
                        canLaunch = true,
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
                                    AgentListItemUi(
                                        paneId = "sd::%9",
                                        relayId = "sd",
                                        title = "omp · vime",
                                        statusLine = "coordinate the timing cohort",
                                        activityLabel = "orchestrating · 2",
                                        elapsedLabel = "working",
                                        working = true,
                                        controllable = true,
                                        provider = "omp",
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
                                        statusLine = "idle",
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
                    onOpenAttention = {},
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
                        canLaunch = true,
                        relaySummary = "1 computer · tailscale",
                        needsYou = listOf(
                            AttentionCardUi(
                                paneId = "sd::%1",
                                relayId = "sd",
                                agentLabel = "claude · lerdr",
                                kind = AttentionKind.APPROVAL,
                                metaLabel = "approval · 40s",
                                prompt = "Run cargo test -p lerdr-coord?",
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
                    onOpenAttention = {},
                    onSelectTopLevel = {},
                )
            }
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun home_nativeApprovalScopeReadable() {
        val labels = listOf(
            "Yes, proceed (y)",
            "Yes, and don't ask again for commands that start\n" +
                "with `cat -- /home/l/.local/state/lerdr-audit/\n" +
                "physical-ixsij1wu/providers/work/lerdr-audit-\n" +
                "owned-note.txt` (p)",
            "No, and tell Codex what to do differently (esc)",
        )
        composeRule.setContent {
            LerdrTheme {
                HomeContent(
                    uiState = HomeUiState(
                        needsYou = listOf(
                            AttentionCardUi(
                                paneId = "r1::w1T:p2",
                                agentLabel = "codex",
                                kind = AttentionKind.APPROVAL,
                                metaLabel = "approval",
                                prompt = "May I read the isolated audit note?",
                                options = labels,
                                controllable = true,
                            ),
                        ),
                    ),
                    onOpenAgent = {},
                    onOpenAttention = {},
                    onSelectTopLevel = {},
                )
            }
        }
        labels.forEach { label ->
            val layouts = mutableListOf<TextLayoutResult>()
            composeRule.onNodeWithText(label, useUnmergedTree = true)
                .performSemanticsAction(SemanticsActions.GetTextLayoutResult) { it(layouts) }
            val layout = layouts.single()
            assertThat(layout.getLineEnd(layout.lineCount - 1, visibleEnd = true))
                .isEqualTo(label.length)
            assertThat(layout.didOverflowHeight).isFalse()
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
                    onOpenAttention = {},
                    onSelectTopLevel = {},
                )
            }
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun home_large_font_navigation() {
        composeRule.setContent {
            val density = LocalDensity.current
            CompositionLocalProvider(
                LocalDensity provides Density(density.density, fontScale = 2f),
            ) {
                LerdrTheme {
                    HomeContent(
                        uiState = HomeUiState(),
                        onOpenAgent = {},
                        onOpenAttention = {},
                        onSelectTopLevel = {},
                    )
                }
            }
        }
        val layouts = mutableListOf<TextLayoutResult>()
        composeRule.onNodeWithText("Computers", useUnmergedTree = true)
            .performSemanticsAction(SemanticsActions.GetTextLayoutResult) { action ->
                action(layouts)
            }
        assertThat(layouts.single().lineCount).isEqualTo(1)
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun home_refreshing() {
        // Lerdr's `pullRefreshing` window — the pull indicator holds
        // over the list for ~900 ms after the gesture fires.
        composeRule.setContent {
            LerdrTheme {
                HomeContent(
                    uiState = HomeUiState(
                        live = true,
                        canLaunch = true,
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
                    onOpenAttention = {},
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
                    onCwdChange = {},
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
                        directoryReady = true,
                        workspaceLabel = "lerdr",
                    ),
                    onRelaySelect = {},
                    onLabelChange = {},
                    onBrowseDirectories = {},
                    onCwdChange = {},
                    onSubmit = {},
                )
            }
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun approvalButtonsKeepTheirOriginalWirePositions() {
        var selectedIndex: Int? = null
        composeRule.setContent {
            LerdrTheme {
                HomeContent(
                    uiState = HomeUiState(
                        needsYou = listOf(
                            AttentionCardUi(
                                paneId = "r1::%1",
                                agentLabel = "claude",
                                kind = AttentionKind.APPROVAL,
                                metaLabel = "approval",
                                prompt = "Run tests?",
                                options = listOf("", "Allow", "Deny"),
                                controllable = true,
                            ),
                        ),
                    ),
                    onOpenAgent = {},
                    onOpenAttention = {},
                    onSelectTopLevel = {},
                    onRespond = { _, index -> selectedIndex = index },
                )
            }
        }
        composeRule.onNodeWithText("Allow").performClick()
        composeRule.runOnIdle { assertThat(selectedIndex).isEqualTo(1) }
        composeRule.onNodeWithText("Deny").performClick()
        composeRule.runOnIdle { assertThat(selectedIndex).isEqualTo(2) }
    }

    @Test
    fun fullQuestionFormOpensFeedRatherThanTerminal() {
        val navigator = LerdrNavigator(NavBackStack<LerdrKey>(LerdrKey.Home))
        composeRule.setContent {
            LerdrTheme {
                HomeContent(
                    uiState = HomeUiState(
                        needsYou = listOf(
                            AttentionCardUi(
                                paneId = "r1::%1",
                                agentLabel = "claude",
                                kind = AttentionKind.QUESTION,
                                metaLabel = "question",
                                prompt = "Pick modules",
                                interaction = Interaction(
                                    id = "q1",
                                    kind = "multi_select",
                                    question = "Pick modules",
                                    options = listOf(
                                        Option(index = 0, label = "store"),
                                        Option(index = 1, label = "session"),
                                    ),
                                    other = Other(hidden = true),
                                ),
                                controllable = true,
                            ),
                        ),
                    ),
                    onOpenAgent = navigator::openAgent,
                    onOpenAttention = navigator::openFeed,
                    onSelectTopLevel = navigator::navigateTopLevel,
                )
            }
        }
        composeRule.onNodeWithText("Choose options (2)").performClick()
        composeRule.runOnIdle {
            assertThat(navigator.backStack.toList())
                .containsExactly(LerdrKey.Home, LerdrKey.AgentFeed("r1::%1")).inOrder()
        }
    }
}
