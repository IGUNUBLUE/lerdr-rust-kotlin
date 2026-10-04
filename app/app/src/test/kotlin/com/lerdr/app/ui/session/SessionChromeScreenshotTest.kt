package com.lerdr.app.ui.session

import androidx.activity.ComponentActivity
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.assertHeightIsAtLeast
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotSelected
import androidx.compose.ui.test.assertIsSelected
import androidx.compose.ui.test.hasAnyAncestor
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performSemanticsAction
import androidx.compose.ui.text.TextLayoutResult
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.dp
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage
import com.lerdr.app.session.SessionMode
import com.lerdr.app.session.SessionStatusVariant
import com.lerdr.app.session.SessionTitleEditor
import com.lerdr.app.session.SessionTopBar
import com.lerdr.core.designsystem.theme.LerdrTheme
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * Roborazzi coverage for `SessionTopBar` — the status chip variants
 * (neutral/working, waiting cookie, error-sharp), the
 * light-blue-pill mode switch, and the inline rename field.
 *
 * The bar renders without `tabsPaneId` so the goldens stay hermetic — the
 * repo-backed affordances (title rename, ⋯ manage, tab strip) only mount
 * when a live pane id is passed.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = com.lerdr.app.TestApp::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class SessionChromeScreenshotTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private val options = RoborazziOptions(
        captureType = RoborazziOptions.CaptureType.Screenshot(),
        compareOptions = RoborazziOptions.CompareOptions(changeThreshold = 0.02f),
    )

    private fun bar(
        statusLabel: String,
        statusColor: Color,
        statusVariant: SessionStatusVariant = SessionStatusVariant.NEUTRAL,
    ) {
        composeRule.setContent {
            LerdrTheme {
                SessionTopBar(
                    title = "lerdr",
                    breadcrumb = "workstation · lerdr",
                    statusLabel = statusLabel,
                    statusColor = statusColor,
                    statusVariant = statusVariant,
                    mode = SessionMode.FEED,
                    onSelectMode = {},
                    onBack = {},
                    provider = "claude",
                )
            }
        }
    }

    private val green = Color(0xFF7BD88F)
    private val grey = Color(0xFF8A93A8)

    @Test
    fun topBar_working() {
        bar(statusLabel = "working", statusColor = green)
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun topBar_waiting() {
        bar(
            statusLabel = "blocked",
            statusColor = grey,
            statusVariant = SessionStatusVariant.WAITING,
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun topBar_error() {
        bar(
            statusLabel = "offline",
            statusColor = grey,
            statusVariant = SessionStatusVariant.ERROR,
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun topBar_terminalModeSelected() {
        composeRule.setContent {
            LerdrTheme {
                SessionTopBar(
                    title = "lerdr",
                    breadcrumb = "workstation · lerdr",
                    statusLabel = "idle",
                    statusColor = grey,
                    mode = SessionMode.TERMINAL,
                    onSelectMode = {},
                    onBack = {},
                )
            }
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    @Config(qualifiers = "w393dp-h851dp")
    fun topBar_largeTextKeepsCompleteModesAndSelection() {
        composeRule.setContent {
            val density = LocalDensity.current
            CompositionLocalProvider(LocalDensity provides Density(density.density, 2f)) {
                var mode by remember { mutableStateOf(SessionMode.TERMINAL) }
                LerdrTheme {
                    SessionTopBar(
                        title = "owned session",
                        breadcrumb = "work · owned relay",
                        statusLabel = "idle",
                        statusColor = grey,
                        mode = mode,
                        onSelectMode = { mode = it },
                        onBack = {},
                    )
                }
            }
        }
        val selectedMode = composeRule.onNodeWithTag("session-mode:terminal")
        selectedMode.assertIsDisplayed().assertIsSelected().assertHeightIsAtLeast(48.dp)
        val selectedLayouts = mutableListOf<TextLayoutResult>()
        composeRule.onNodeWithText("Terminal", useUnmergedTree = true)
            .performSemanticsAction(SemanticsActions.GetTextLayoutResult) { it(selectedLayouts) }
        assertEquals(1, selectedLayouts.single().lineCount)
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)

        var expectedMode = SessionMode.TERMINAL
        listOf(SessionMode.FILES, SessionMode.FEED, SessionMode.TERMINAL).forEach { nextMode ->
            composeRule.onNodeWithContentDescription("Session mode").performClick()
            SessionMode.entries.forEach { option ->
                val tag = "session-mode-option:${option.label.lowercase()}"
                val choice = composeRule.onNodeWithTag(tag)
                choice.assertIsDisplayed().assertHeightIsAtLeast(48.dp)
                if (option == expectedMode) choice.assertIsSelected() else choice.assertIsNotSelected()
                val layouts = mutableListOf<TextLayoutResult>()
                composeRule.onNode(
                    hasText(option.label) and hasAnyAncestor(hasTestTag(tag)),
                    useUnmergedTree = true,
                ).performSemanticsAction(SemanticsActions.GetTextLayoutResult) { it(layouts) }
                assertEquals(1, layouts.single().lineCount)
            }
            composeRule.onNodeWithTag("session-mode-option:${nextMode.label.lowercase()}").performClick()
            composeRule.onNodeWithTag("session-mode:${nextMode.label.lowercase()}")
                .assertIsDisplayed().assertIsSelected()
            expectedMode = nextMode
        }
    }

    @Test
    fun topBar_titleEditor() {
        composeRule.setContent {
            LerdrTheme {
                SessionTitleEditor(
                    draft = "renamed session",
                    busy = false,
                    error = null,
                    onDraftChange = {},
                    onConfirm = {},
                    onCancel = {},
                )
            }
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }
}
