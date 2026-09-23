package com.lerdr.app.ui.session

import androidx.activity.ComponentActivity
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onRoot
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage
import com.lerdr.app.session.SessionMode
import com.lerdr.app.session.SessionStatusVariant
import com.lerdr.app.session.SessionTitleEditor
import com.lerdr.app.session.SessionTopBar
import com.lerdr.core.designsystem.theme.LerdrTheme
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * Roborazzi coverage for `SessionTopBar` — the status chip variants
 * (neutral/working, waiting cookie, error-sharp, amber lease), the
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
    fun topBar_lease() {
        bar(
            statusLabel = "lease 92×42",
            statusColor = green,
            statusVariant = SessionStatusVariant.LEASE,
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
