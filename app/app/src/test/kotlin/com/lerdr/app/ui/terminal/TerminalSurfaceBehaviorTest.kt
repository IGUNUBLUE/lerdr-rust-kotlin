package com.lerdr.app.ui.terminal

import androidx.activity.ComponentActivity
import androidx.compose.foundation.ScrollState
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performTouchInput
import androidx.compose.ui.test.click
import androidx.compose.ui.unit.dp
import com.lerdr.core.designsystem.theme.LerdrTheme
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/** Exercises committed output and layout changes through the rendered surface. */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = com.lerdr.app.TestApp::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class TerminalSurfaceBehaviorTest {
    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private val state = TerminalSurfaceState(ScrollState(0))
    private val lines = mutableStateOf(List(60) { "history_${it.toString().padStart(2, '0')}" })
    private val revision = mutableStateOf(1L)
    private val height = mutableStateOf(240.dp)

    private fun showSurface() {
        composeRule.setContent {
            LerdrTheme {
                TerminalSurface(
                    rows = parseTerminalRows(lines.value, TERMINAL_FORMAT_ANSI),
                    cursor = null,
                    revision = revision.value,
                    state = state,
                    modifier = Modifier.fillMaxWidth().height(height.value),
                )
            }
        }
    }

    private fun visibleText(): String = composeRule.onAllNodes(
        SemanticsMatcher.keyIsDefined(SemanticsProperties.Text),
    ).fetchSemanticsNodes().single().config[SemanticsProperties.Text]
        .joinToString("\n") { it.text }

    private fun pauseAtHistory(): Int {
        composeRule.runOnIdle { runBlocking { state.revealRow(12) } }
        return composeRule.runOnIdle {
            assertFalse(state.stickToBottom)
            state.scrollState.value
        }
    }

    private fun assertFollowingTail(tail: String = "history_59") {
        assertTrue(visibleText().contains(tail))
        composeRule.runOnIdle {
            assertTrue(state.stickToBottom)
            assertEquals(state.scrollState.maxValue, state.scrollState.value)
            assertFalse(state.hasNewOutput)
        }
    }

    @Test
    fun pausedOutputArrives_withoutMovingTheReader() {
        showSurface()
        val position = pauseAtHistory()
        val before = visibleText()

        composeRule.runOnIdle {
            lines.value = lines.value + "new_write_edge"
            revision.value += 1
        }

        assertEquals(before, visibleText())
        composeRule.runOnIdle {
            assertTrue(state.hasNewOutput)
            assertFalse(state.stickToBottom)
            assertEquals(position, state.scrollState.value)
        }
    }

    @Test
    fun pausedInPlaceRedraw_preservesReadingFrameUntilReturningToLive() {
        showSurface()
        pauseAtHistory()
        val before = visibleText()
        composeRule.runOnIdle {
            lines.value = List(60) { "redrawn_${it.toString().padStart(2, '0')}" }
            revision.value += 1
        }

        assertEquals(before, visibleText())
        composeRule.runOnIdle {
            assertTrue(state.hasNewOutput)
            runBlocking { state.scrollToLive() }
        }
        assertFollowingTail("redrawn_59")
    }

    @Test
    fun explicitlyPausingAFittingFrame_holdsRedrawsWithoutNeedingAScrollGesture() {
        lines.value = listOf("native_prompt_before")
        showSurface()
        composeRule.runOnIdle {
            assertEquals(0, state.scrollState.maxValue)
            state.stickToBottom = false
            lines.value = listOf("native_prompt_after")
            revision.value += 1
        }
        assertTrue(visibleText().contains("native_prompt_before"))
        assertFalse(visibleText().contains("native_prompt_after"))
        composeRule.runOnIdle {
            assertTrue(state.hasNewOutput)
            runBlocking { state.scrollToLive() }
        }
        assertFollowingTail("native_prompt_after")
    }

    @Test
    fun clearingASelectionAfterRedraw_keepsItsReadingFrameUntilExplicitLiveReturn() {
        showSurface()
        val before = visibleText()
        composeRule.runOnIdle {
            state.frozenRows = parseTerminalRows(lines.value, TERMINAL_FORMAT_ANSI)
            state.selectionAnchor = TerminalCell(59, 0)
            state.selectionCursor = TerminalCell(59, 6)
            state.selectionCommitted = true
            state.stickToBottom = false
            lines.value = List(60) { "replacement_${it.toString().padStart(2, '0')}" }
            revision.value += 1
        }
        assertEquals(before, visibleText())
        composeRule.runOnIdle { state.clearSelection() }
        assertEquals(before, visibleText())
        composeRule.runOnIdle {
            assertFalse(state.hasSelection)
            assertTrue(state.hasNewOutput)
            runBlocking { state.scrollToLive() }
        }
        assertFollowingTail("replacement_59")
    }

    @Test
    fun metadataOnlyCommit_doesNotClaimNewOutputOrResumeFollowing() {
        showSurface()
        val position = pauseAtHistory()
        val before = visibleText()
        composeRule.runOnIdle { revision.value += 1 }

        assertEquals(before, visibleText())
        composeRule.runOnIdle {
            assertFalse(state.hasNewOutput)
            assertFalse(state.stickToBottom)
            assertEquals(position, state.scrollState.value)
        }
    }

    @Test
    fun findingAtTheHeldWriteEdge_doesNotReplaceTheCorpusWithNewOutput() {
        showSurface()
        composeRule.runOnIdle { runBlocking { state.revealRow(59) } }
        val before = visibleText()
        composeRule.runOnIdle {
            lines.value = List(60) { "replacement_${it.toString().padStart(2, '0')}" }
            revision.value += 1
        }
        assertEquals(before, visibleText())
        composeRule.runOnIdle {
            assertFalse(state.stickToBottom)
            assertTrue(state.hasNewOutput)
            runBlocking { state.scrollToLive() }
        }
        assertFollowingTail("replacement_59")
    }

    @Test
    fun returningToLive_clearsTheIndicatorAndShowsLatestOutput() {
        showSurface()
        pauseAtHistory()
        composeRule.runOnIdle {
            lines.value = lines.value + "new_write_edge"
            revision.value += 1
        }
        composeRule.runOnIdle {
            assertTrue(state.hasNewOutput)
            runBlocking { state.scrollToLive() }
        }
        assertFollowingTail("new_write_edge")
    }

    @Test
    fun viewportShrinkWhileFollowing_keepsTheWriteEdgeVisibleAfterLayout() {
        showSurface()
        assertFollowingTail()
        composeRule.runOnIdle { height.value = 80.dp }
        assertFollowingTail()

        // A subsequent layout and output commit must use the new bounds too.
        composeRule.runOnIdle {
            height.value = 120.dp
            lines.value = lines.value + "resized_write_edge"
            revision.value += 1
        }
        assertFollowingTail("resized_write_edge")
    }

    @Test
    fun viewportShrinkWhilePaused_doesNotMoveOrRepinTheReader() {
        showSurface()
        val position = pauseAtHistory()
        val firstVisible = visibleText().lineSequence().first()
        composeRule.runOnIdle { height.value = 80.dp }

        assertEquals(firstVisible, visibleText().lineSequence().first())
        assertFalse(visibleText().contains("history_59"))
        composeRule.runOnIdle {
            assertFalse(state.stickToBottom)
            assertFalse(state.hasNewOutput)
            assertEquals(position, state.scrollState.value)
        }
    }

    @Test
    fun fontIncreaseWhileFollowing_keepsTheWriteEdgeVisibleAfterLayout() {
        showSurface()
        assertFollowingTail()
        composeRule.runOnIdle { state.adjustFontScale(0.5f) }
        assertFollowingTail()
    }

    @Test
    fun fontIncreaseWhilePaused_doesNotJumpToLiveOrClaimNewOutput() {
        showSurface()
        val position = pauseAtHistory()
        composeRule.runOnIdle { state.adjustFontScale(0.5f) }

        assertFalse(visibleText().contains("history_59"))
        composeRule.runOnIdle {
            assertFalse(state.stickToBottom)
            assertFalse(state.hasNewOutput)
            assertEquals(position, state.scrollState.value)
        }
    }

    @Test
    fun tapsUseCurrentCallbackAfterRecomposition() {
        val allowed = mutableStateOf(true)
        var controlTaps = 0
        composeRule.setContent {
            val canControl = allowed.value
            LerdrTheme {
                TerminalSurface(
                    rows = parseTerminalRows(listOf("native prompt"), TERMINAL_FORMAT_ANSI),
                    cursor = null,
                    revision = 1,
                    state = state,
                    onTapSurface = { if (canControl) controlTaps++ },
                    modifier = Modifier.fillMaxWidth().height(240.dp).testTag("surface"),
                )
            }
        }
        composeRule.onNodeWithTag("surface").performTouchInput { click(center) }
        composeRule.runOnIdle {
            assertEquals(1, controlTaps)
            allowed.value = false
        }
        composeRule.onNodeWithTag("surface").performTouchInput { click(center) }
        composeRule.runOnIdle { assertEquals(1, controlTaps) }
    }
}
