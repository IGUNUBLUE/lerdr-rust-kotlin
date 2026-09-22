package com.lerdr.app.ui.settings

import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Surface
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onRoot
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage
import com.lerdr.app.TestApp
import com.lerdr.app.settings.SpeechCatalogUi
import com.lerdr.app.settings.SpeechSectionContent
import com.lerdr.app.settings.SpeechUiState
import com.lerdr.app.settings.SpeechVoiceRowUi
import com.lerdr.app.speech.SpeechPhase
import com.lerdr.core.designsystem.theme.LerdrTheme
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * Roborazzi coverage for the per-relay Speech section — disabled, enabled,
 * no voice management capability, a populated voice catalog, and an active
 * reading. [SpeechSectionContent] is pure state so no Hilt graph is needed
 * under [TestApp].
 */
@RunWith(RobolectricTestRunner::class)
// Tall virtual display — the voice catalog must land inside the window to
// appear in the `onRoot` capture.
@Config(sdk = [34], application = TestApp::class, qualifiers = "w411dp-h1600dp")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class SpeechSectionScreenshotTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private val options = RoborazziOptions(
        // Dump (the JVM default) paints a semantics-tree overlay whose
        // node text jitters run-to-run — force a plain bitmap capture.
        captureType = RoborazziOptions.CaptureType.Screenshot(),
        compareOptions = RoborazziOptions.CompareOptions(changeThreshold = 0.02f),
    )

    private fun capture(state: SpeechUiState) {
        composeRule.setContent {
            LerdrTheme {
                Surface {
                    Column(modifier = Modifier.padding(LerdrTheme.spacing.medium)) {
                        SpeechSectionContent(
                            uiState = state,
                            onEnabledChange = {},
                            onLanguageChange = {},
                            onSpeakTest = {},
                            onInstallVoice = {},
                            onRemoveVoice = {},
                            onDismissError = {},
                        )
                    }
                }
            }
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun speech_disabled() {
        capture(
            SpeechUiState(
                relayId = "r1",
                relayLabel = "workstation",
                enabled = false,
            ),
        )
    }

    @Test
    fun speech_enabled() {
        capture(
            SpeechUiState(
                relayId = "r1",
                relayLabel = "workstation",
                enabled = true,
                language = "en",
                phase = SpeechPhase.IDLE,
                connected = true,
                synthesisCapable = true,
                speakableLanguages = listOf("en", "fr"),
            ),
        )
    }

    @Test
    fun speech_noCapability() {
        // Relay advertises neither capability — Speak test greys out and the
        // no-voice guidance line appears.
        capture(
            SpeechUiState(
                relayId = "r1",
                relayLabel = "workstation",
                enabled = true,
                language = "en",
                phase = SpeechPhase.IDLE,
                connected = true,
                synthesisCapable = false,
                managementCapable = false,
                speakableLanguages = emptyList(),
            ),
        )
    }

    @Test
    fun speech_voiceCatalog() {
        capture(
            SpeechUiState(
                relayId = "r1",
                relayLabel = "workstation",
                enabled = true,
                language = "en",
                phase = SpeechPhase.IDLE,
                connected = true,
                synthesisCapable = true,
                managementCapable = true,
                speakableLanguages = listOf("en", "fr"),
                catalog = SpeechCatalogUi(
                    cacheDir = "/home/u/.cache/lerdr/voices",
                    engineInstalled = true,
                    rows = listOf(
                        SpeechVoiceRowUi(
                            language = "en",
                            label = "English",
                            stateLabel = "Neural voice cached, 63 MB",
                            installed = true,
                            busy = false,
                        ),
                        SpeechVoiceRowUi(
                            language = "fr",
                            label = "French",
                            stateLabel = "Not downloaded - 65 MB download",
                            installed = false,
                            busy = false,
                        ),
                        SpeechVoiceRowUi(
                            language = "de",
                            label = "German",
                            stateLabel = "No voice on this computer",
                            installed = false,
                            busy = false,
                        ),
                        SpeechVoiceRowUi(
                            language = "es",
                            label = "Spanish",
                            stateLabel = "Not downloaded - 48 MB download",
                            installed = false,
                            busy = true,
                        ),
                        SpeechVoiceRowUi(
                            language = "zh",
                            label = "Chinese",
                            stateLabel = "No voice on this computer",
                            installed = false,
                            busy = false,
                        ),
                    ),
                ),
            ),
        )
    }

    @Test
    fun speech_speaking() {
        capture(
            SpeechUiState(
                relayId = "r1",
                relayLabel = "workstation",
                enabled = true,
                language = "en",
                phase = SpeechPhase.SPEAKING,
                connected = true,
                synthesisCapable = true,
                speakableLanguages = listOf("en"),
            ),
        )
    }
}
