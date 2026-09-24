package com.lerdr.app.ui.terminal

import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.material3.Surface
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.isRoot
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.test.longClick
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.test.performTouchInput
import androidx.compose.ui.unit.dp
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage
import com.lerdr.app.session.TerminalContent
import com.lerdr.app.session.TerminalUiState
import com.lerdr.core.designsystem.theme.LerdrTheme
import lerdr.core.model.PaneSearchResult
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

    /**
     * `pane_search` negotiated — the bar annotates hits living in the
     * pane's full scrollback beyond the rendered buffer.
     */
    @Test
    fun findOpen_scrollbackCount() {
        composeRule.setContent {
            LerdrTheme {
                TerminalContent(
                    uiState = terminalState().copy(paneSearchSupported = true),
                    onOpenFeed = {},
                    onOpenFiles = {},
                    onBack = {},
                    onSendKeys = {},
                    onSendText = {},
                    onViewportMeasured = { _, _ -> },
                    onRefresh = {},
                    onPaneSearch = { PaneSearchResult(total = 47) },
                )
            }
        }
        composeRule.onNodeWithContentDescription("Session actions").performClick()
        composeRule.onNodeWithText("Find in terminal").performClick()
        composeRule.onNodeWithTag("terminalFindField").performTextInput("ok")
        // Step past the scrollback-count debounce, then settle.
        composeRule.mainClock.advanceTimeBy(500)
        composeRule.waitForIdle()
        composeRule.onNodeWithText("1 of 4 · 47 in scrollback").assertExists()
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    /**
     * Long-press-drag selects a cell range — the menu opens at the release
     * point carrying the selection actions, and copy lands the marked
     * text on the clipboard.
     */
    @Test
    fun selectionDrag_copySelectionItem() {
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
                        contentPadding = PaddingValues(8.dp),
                    )
                }
            }
        }
        composeRule.onRoot().performTouchInput {
            down(Offset(40f, 40f))
            advanceEventTime(800) // well past the long-press timeout
            moveTo(Offset(220f, 80f))
            advanceEventTime(50)
            up()
        }
        composeRule.waitForIdle()
        composeRule.onNodeWithText("Copy selection").assertExists().performClick()
        composeRule.waitForIdle()
        val clipboard = composeRule.activity
            .getSystemService(android.content.Context.CLIPBOARD_SERVICE)
            as android.content.ClipboardManager
        val copied = clipboard.primaryClip?.getItemAt(0)?.text?.toString()
        org.junit.Assert.assertTrue(copied.orEmpty().isNotBlank())
    }

    /**
     * A held press without travel still opens the row menu — the
     * select-vs-menu split is the drag distance, and Copy/Share
     * selection must stay absent.
     */
    @Test
    fun longPressStill_opensRowMenu() {
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
                        contentPadding = PaddingValues(8.dp),
                    )
                }
            }
        }
        composeRule.onRoot().performTouchInput { longClick(Offset(120f, 100f)) }
        composeRule.waitForIdle()
        composeRule.onNodeWithText("Copy line").assertExists()
        composeRule.onNodeWithText("Copy selection").assertDoesNotExist()
    }

    /**
     * The selection overlay up close — a committed drag paints its cell
     * range under the text (golden captures the root; the menu popup is
     * a separate window).
     */
    @Test
    fun selectionHighlight_committedDrag() {
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
                        contentPadding = PaddingValues(8.dp),
                    )
                }
            }
        }
        composeRule.onRoot().performTouchInput {
            down(Offset(40f, 40f))
            advanceEventTime(800)
            moveTo(Offset(200f, 90f))
            advanceEventTime(50)
            up()
        }
        composeRule.waitForIdle()
        composeRule.onNodeWithText("Copy selection").assertExists()
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    /**
     * Committed selections grow teardrop handles — dragging one re-ranges
     * the selection, and a tap inside the range reopens the copy/share
     * menu carrying the adjusted text.
     */
    @Test
    fun selectionHandleDrag_adjustsRange() {
        lateinit var surfaceState: TerminalSurfaceState
        composeRule.setContent {
            LerdrTheme {
                Surface(
                    color = LerdrTheme.extendedColors.terminalSurface,
                    modifier = Modifier
                        .fillMaxWidth()
                        .height(300.dp),
                ) {
                    surfaceState = rememberTerminalSurfaceState()
                    TerminalSurface(
                        rows = uiRows,
                        cursor = null,
                        revision = 1,
                        state = surfaceState,
                        contentPadding = PaddingValues(8.dp),
                    )
                }
            }
        }
        composeRule.onRoot().performTouchInput {
            down(Offset(40f, 40f))
            advanceEventTime(800)
            moveTo(Offset(200f, 90f))
            advanceEventTime(50)
            up()
        }
        composeRule.waitForIdle()
        val committed = composeRule.runOnIdle {
            surfaceState.selectionAnchor!! to surfaceState.selectionCursor!!
        }
        // The commit opened the menu popup — a second root in the tree —
        // so injected touches aim at the main root explicitly.
        val mainRoot = composeRule.onAllNodes(isRoot())[0]
        val handles = composeRule.runOnIdle { surfaceState.selectionHandles!! }
        // Handles live in content space; the injection point is viewport
        // space — add the content padding back.
        val padPx = with(composeRule.density) { 8.dp.toPx() }
        val startHandle = handles.first + Offset(padPx, padPx)
        mainRoot.performTouchInput {
            down(startHandle)
            advanceEventTime(50)
            moveTo(startHandle + Offset(60f, 0f))
            advanceEventTime(50)
            up()
        }
        composeRule.waitForIdle()
        val adjusted = composeRule.runOnIdle {
            surfaceState.selectionAnchor!! to surfaceState.selectionCursor!!
        }
        org.junit.Assert.assertNotEquals(
            "handle drag should re-range the selection",
            committed, adjusted,
        )
        // Tap inside the adjusted range → selection survives (the menu
        // reopens); tap outside → the selection clears.
        val newHandles = composeRule.runOnIdle { surfaceState.selectionHandles!! }
        val insideX = newHandles.first.x + 2f + padPx
        val insideY = newHandles.first.y - surfaceState.rowHeightPx / 2f + padPx
        mainRoot.performTouchInput {
            down(Offset(insideX, insideY))
            advanceEventTime(30)
            up()
        }
        composeRule.waitForIdle()
        composeRule.runOnIdle {
            org.junit.Assert.assertTrue(
                "tap inside the selection must not clear it",
                surfaceState.hasSelection,
            )
        }
        mainRoot.performTouchInput {
            down(Offset(150f, 290f))
            advanceEventTime(30)
            up()
        }
        composeRule.waitForIdle()
        composeRule.runOnIdle {
            org.junit.Assert.assertFalse(
                "tap outside the selection should clear it",
                surfaceState.hasSelection,
            )
        }
    }

    /**
     * `pane_links` negotiated — the long-press menu gains the server
     * hit-test item once resolve reports link regions at the cell.
     */
    @Test
    fun linkMenu_serverLinkItem() {
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
                        contentPadding = PaddingValues(8.dp),
                        paneLinksSupported = true,
                        onResolveLink = { _, _ -> true },
                    )
                }
            }
        }
        composeRule.onRoot().performTouchInput { longClick(Offset(120f, 100f)) }
        composeRule.waitForIdle()
        composeRule.onNodeWithText("Open link on desktop").assertExists()
    }

    /**
     * No `pane_links`, or resolve reporting no regions — the menu keeps
     * only its client-side items.
     */
    @Test
    fun linkMenu_noServerLink() {
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
                        contentPadding = PaddingValues(8.dp),
                        paneLinksSupported = true,
                        onResolveLink = { _, _ -> false },
                    )
                }
            }
        }
        composeRule.onRoot().performTouchInput { longClick(Offset(120f, 100f)) }
        composeRule.waitForIdle()
        composeRule.onNodeWithText("Open link on desktop").assertDoesNotExist()
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
