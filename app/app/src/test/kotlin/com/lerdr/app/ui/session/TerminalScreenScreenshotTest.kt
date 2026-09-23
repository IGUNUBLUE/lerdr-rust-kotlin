package com.lerdr.app.ui.session

import androidx.activity.ComponentActivity
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performTextInput
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
 * amber chip, the no-echo/secret-prompt states, and the reader gate.
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
        statusLabel = "lease 92×42",
        connected = true,
        waitingForContent = false,
        leaseColumns = 92,
        leaseRows = 42,
        canControl = true,
        rows = uiRows,
        revision = 1,
    )

    private fun show(uiState: TerminalUiState) {
        composeRule.setContent {
            LerdrTheme {
                TerminalContent(
                    uiState = uiState,
                    onOpenFeed = {},
                    onOpenFiles = {},
                    onBack = {},
                    onSendKeys = {},
                    onSendText = {},
                    onSendSecret = {},
                    onViewportMeasured = { _, _ -> },
                    onRefresh = {},
                )
            }
        }
    }

    /** Lease held — amber status chip + the `── pane 92×42 ──` meta row. */
    @Test
    fun terminal_liveLease() {
        show(baseState())
        composeRule.onNodeWithText("pane 92×42").assertExists()
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
        composeRule.onNodeWithText(
            "The terminal is asking for a hidden value: Password:",
            substring = true,
        ).assertExists()
        composeRule.onNodeWithTag("terminalSecretField").performTextInput("hunter2")
        composeRule.waitForIdle()
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    /**
     * `no_echo` without `secret_input` — the bar stays in plain mode and
     * the banner carries the too-old-relay inline error instead.
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
        composeRule.onNodeWithText(
            "too old to accept a hidden value",
            substring = true,
        ).assertExists()
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    /** Reader role — keys/input disabled, the read-only hint chip shows. */
    @Test
    fun terminal_readOnly() {
        show(baseState().copy(canControl = false))
        composeRule.onNodeWithText("read-only").assertExists()
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
        composeRule.onNodeWithText("Waiting for relay…").assertExists()
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }
}
