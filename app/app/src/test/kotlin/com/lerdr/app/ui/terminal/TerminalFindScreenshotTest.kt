package com.lerdr.app.ui.terminal

import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.material3.Surface
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.unit.dp
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage
import com.lerdr.app.session.TerminalContent
import com.lerdr.app.session.TerminalUiState
import com.lerdr.core.designsystem.theme.LerdrTheme
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * Roborazzi coverage for find-in-buffer — the bar, the match/`active` mark
 * treatment, and the no-match count. Records via
 * `./gradlew :app:recordRoborazziDebug`, verifies via
 * `:app:verifyRoborazziDebug`.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = com.lerdr.app.TestApp::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class TerminalFindScreenshotTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private val options = RoborazziOptions(
        // Dump (the JVM default) paints a semantics-tree overlay whose
        // node text jitters run-to-run — force a plain bitmap capture.
        captureType = RoborazziOptions.CaptureType.Screenshot(),
        compareOptions = RoborazziOptions.CompareOptions(changeThreshold = 0.02f),
    )

    private val lines = buildList {
        add("lerdr git:(main) \$ cargo test -p lerdr-e2ee")
        repeat(6) { add("  compiling crate-$it … done") }
        add("test handshake_credential … [32mok[0m")
        add("test pane_delta_apply … [32mok[0m")
        add("test fingerprint_chain … [32mok[0m")
        add("test result: ok. 24 passed; 0 failed")
        add("\$ ")
    }
    private val uiRows = parseTerminalRows(lines, TERMINAL_FORMAT_ANSI)

    private fun terminalState() = TerminalUiState(
        paneId = "r1::%1",
        title = "claude",
        breadcrumb = "lerdr · main · sd",
        statusLabel = "live",
        connected = true,
        waitingForContent = false,
        rows = uiRows,
        revision = 1,
    )

    @Test
    fun findOpen_matchesHighlighted() {
        composeRule.setContent {
            LerdrTheme {
                TerminalContent(
                    uiState = terminalState(),
                    onOpenFeed = {},
                    onOpenFiles = {},
                    onBack = {},
                    onSendKeys = {},
                    onSendText = {},
                    onViewportMeasured = { _, _ -> },
                    onRefresh = {},
                )
            }
        }
        composeRule.onNodeWithContentDescription("Session actions").performClick()
        composeRule.onNodeWithText("Find in terminal").performClick()
        composeRule.onNodeWithTag("terminalFindField").performTextInput("ok")
        composeRule.waitForIdle()
        composeRule.onNodeWithText("1 of 4").assertExists()
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    /**
     * The mark layer up close — a fixed-height surface so the active
     * match (orange) and the other matches (yellow) are all in frame.
     * Ranges are computed through the real corpus pipeline.
     */
    @Test
    fun findHighlights_activeAndMatch() {
        val findRows = terminalFindRows(uiRows)
        val matches = findTerminalText(terminalSearchText(findRows), "ok").matches
        val ranges = terminalFindRanges(
            findRows,
            terminalRowOffsets(findRows),
            matches,
            activeIndex = 1,
        )
        composeRule.setContent {
            LerdrTheme {
                Surface(
                    color = LerdrTheme.extendedColors.terminalSurface,
                    modifier = Modifier
                        .fillMaxWidth()
                        .height(300.dp),
                ) {
                    TerminalSurface(
                        rows = uiRows,
                        cursor = null,
                        revision = 1,
                        findRanges = ranges,
                        contentPadding = PaddingValues(8.dp),
                    )
                }
            }
        }
        composeRule.waitForIdle()
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun findOpen_noMatches() {
        composeRule.setContent {
            LerdrTheme {
                TerminalContent(
                    uiState = terminalState(),
                    onOpenFeed = {},
                    onOpenFiles = {},
                    onBack = {},
                    onSendKeys = {},
                    onSendText = {},
                    onViewportMeasured = { _, _ -> },
                    onRefresh = {},
                )
            }
        }
        composeRule.onNodeWithContentDescription("Session actions").performClick()
        composeRule.onNodeWithText("Find in terminal").performClick()
        composeRule.onNodeWithTag("terminalFindField").performTextInput("zzz")
        composeRule.waitForIdle()
        composeRule.onNodeWithText("No matches").assertExists()
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }
}
