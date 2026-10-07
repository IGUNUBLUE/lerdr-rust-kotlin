package com.lerdr.app.ui.session

import androidx.activity.ComponentActivity
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.performTextInput
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Density
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage
import com.lerdr.app.session.TerminalContent
import com.lerdr.app.session.TerminalUiState
import com.lerdr.app.ui.terminal.TERMINAL_FORMAT_ANSI
import com.lerdr.app.ui.terminal.parseTerminalRows
import com.lerdr.core.designsystem.theme.LerdrTheme
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * Roborazzi coverage for Terminal mode — the leased-grid meta row and
 * lifecycle chip, the no-echo/secret-prompt states, and the reader gate.
 * Records via `./gradlew :app:recordRoborazziDebug`, verifies via
 * `:app:verifyRoborazziDebug`.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = com.lerdr.app.TestApp::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class TerminalScreenScreenshotTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private val options = RoborazziOptions(
        // Dump (the JVM default) paints a semantics-tree overlay whose
        // node text jitters run-to-run — force a plain bitmap capture.
        captureType = RoborazziOptions.CaptureType.Screenshot(),
        compareOptions = RoborazziOptions.CompareOptions(changeThreshold = 0.02f),
    )

    private val uiRows = parseTerminalRows(
        listOf(
            "lerdr git:(main) $ cargo test -p lerdr-e2ee",
            "running 14 tests  test handshake_credential … [32mok[0m",
            "test golden_vectors … [32mok[0m",
            "[1mtest result: ok.[0m 14 passed; 0 failed",
            "$ ",
        ),
        TERMINAL_FORMAT_ANSI,
    )

    private fun baseState() = TerminalUiState(
        paneId = "r1::%1",
        title = "claude",
        provider = "claude",
        breadcrumb = "lerdr · main · sd",
        statusLabel = "working",
        connected = true,
        waitingForContent = false,
        leaseColumns = 92,
        leaseRows = 42,
        canControl = true,
        rows = uiRows,
        revision = 1,
    )

    private fun show(uiState: TerminalUiState, fontScale: Float = 1f, systemFontScale: Float = 1f) {
        composeRule.setContent {
            val density = LocalDensity.current
            CompositionLocalProvider(LocalDensity provides Density(density.density, systemFontScale)) {
                LerdrTheme {
                    TerminalContent(
                        uiState = uiState,
                        onOpenFeed = {},
                        onOpenFiles = {},
                        onBack = {},
                        onSendKeys = {},
                        onSendText = { true },
                        onSendSecret = { true },
                        onViewportMeasured = { _, _ -> },
                        onRefresh = {},
                        terminalFontScale = fontScale,
                    )
                }
            }
        }
    }

    /** Lease geometry stays in the meta row while the header shows lifecycle. */
    @Test
    fun terminal_liveLease() {
        show(baseState())
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun terminal_blockedWithLease() {
        show(baseState().copy(statusLabel = "blocked"))
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    /**
     * `no_echo` prompt + `secret_input` capability — the banner explains
     * the ask and the bar is the masked password field; a typed answer
     * renders as dots, never text.
     */
    @Test
    fun terminal_secretPrompt() {
        show(
            baseState().copy(
                noEcho = true,
                noEchoPrompt = "Password:",
                secretInputSupported = true,
            ),
        )
        composeRule.onNodeWithTag("terminalSecretField").performTextInput("hunter2")
        composeRule.waitForIdle()
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    /**
     * Unsupported hidden prompts keep the password editor inert.
     */
    @Test
    fun terminal_secretPromptUnsupported() {
        show(
            baseState().copy(
                noEcho = true,
                noEchoPrompt = "Password:",
                secretInputSupported = false,
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    /**
     * Bundled Nerd Font coverage — powerline separators, private-use icons,
     * box drawing, and the prompt glyphs agent statuslines draw. The point
     * of the golden is the glyphs actually paint, not the text content.
     */
    @Test
    fun terminal_nerdFontGlyphs() {
        val rows = parseTerminalRows(
            listOf(
                "[36m╭─[0m [35m\uE0B0[0m [34m\uF07B lerdr[0m [33m\uE0A0 main[0m \uE0B1",
                "[32m\uF00C[0m build ok  \uF489 cargo  \uE795 rust  \uF24F node",
                "\u2500\u2500 box \u2502 \u256D\u256E\u2570\u256F  spinner \u280B\u2819\u2838  dots \u25CF\u25CB\u25C9",
                "[32m\u276F[0m [1m$[0m \u2588",
            ),
            TERMINAL_FORMAT_ANSI,
        )
        show(baseState().copy(rows = rows))
        composeRule.waitForIdle()
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    /** Reader role — the connection/control band explains the disabled input. */
    @Test
    fun terminal_readOnly() {
        show(baseState().copy(canControl = false))
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    /** No session yet — the "waiting" placeholder inside the dark card. */
    @Test
    fun terminal_waitingForContent() {
        show(
            baseState().copy(
                connected = false,
                waitingForContent = true,
                leaseColumns = 0,
                leaseRows = 0,
                statusLabel = "",
                rows = emptyList(),
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun terminal_offlineDraft() {
        show(baseState().copy(connected = false))
        composeRule.onNodeWithTag("terminalInputField").performTextInput("draft retained while offline")
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun terminal_reconnecting() {
        show(baseState().copy(connected = false, connecting = true))
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun terminal_largeTextControls() {
        show(baseState().copy(connected = false, connecting = true), systemFontScale = 2f)
        composeRule.onNodeWithText("Enter").assertIsDisplayed()
        composeRule.onNodeWithContentDescription("More terminal keys").assertIsDisplayed()
        composeRule.onNodeWithText("Reconnecting… · input paused").assertIsDisplayed()
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun terminal_largeTextReadOnly() {
        show(baseState().copy(canControl = false), systemFontScale = 2f)
        composeRule.onNodeWithTag("terminalInputField").assertDoesNotExist()
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }
    /**
     * Explicit fit-width for a wide cached/observed frame — optional,
     * never the default geometry of a controlled phone terminal.
     */
    @Test
    fun terminal_wideTuiFit() {
        val pad = "─".repeat(158)
        val wideRows = parseTerminalRows(
            listOf(
                "╭$pad╮",
                "│ π coordinator — lerdr-terminal-demo" +
                    " ".repeat(121) + "│",
                "│ ⠹ Working…  *2 subagents  ·  helper A timing  ·  helper B report" +
                    " ".repeat(97) + "│",
                "╰$pad╯",
                "$ ",
            ),
            TERMINAL_FORMAT_ANSI,
        )
        show(
            baseState().copy(
                provider = "omp",
                statusLabel = "idle",
                leaseColumns = 0,
                leaseRows = 0,
                rows = wideRows,
            ),
            fontScale = 0.35f,
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }
}
