package com.lerdr.app.ui.settings

import androidx.activity.ComponentActivity
import androidx.compose.material3.SnackbarHostState
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onRoot
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage
import com.lerdr.app.settings.RelayRowUi
import com.lerdr.app.settings.SettingsContent
import com.lerdr.app.settings.SettingsUiState
import com.lerdr.app.settings.ThemeMode
import com.lerdr.core.designsystem.theme.LerdrTheme
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * Roborazzi coverage for Settings — the no-relays empty state and a
 * populated list with one connected relay plus one auth-rejected relay.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = com.lerdr.app.TestApp::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class SettingsScreenScreenshotTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private val options = RoborazziOptions(
        // Dump (the JVM default) paints a semantics-tree overlay whose
        // node text jitters run-to-run — force a plain bitmap capture.
        captureType = RoborazziOptions.CaptureType.Screenshot(),
        compareOptions = RoborazziOptions.CompareOptions(changeThreshold = 0.02f),
    )

    @Test
    fun settings_empty() {
        composeRule.setContent {
            LerdrTheme {
                SettingsContent(
                    uiState = SettingsUiState(),
                    appVersion = "0.1.0",
                    notificationsEnabled = true,
                    appLockReady = true,
                    snackbarHostState = SnackbarHostState(),
                    onSelectTopLevel = {},
                    onReconnectRelay = {},
                    onForgetRelay = {},
                    onRevalidateAll = {},
                    onThemeMode = {},
                    onAppLockChange = {},
                    onOpenNotificationSettings = {},
                )
            }
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun settings_relays() {
        composeRule.setContent {
            LerdrTheme {
                SettingsContent(
                    uiState = SettingsUiState(
                        relays = listOf(
                            RelayRowUi(
                                relayId = "r1",
                                label = "workstation",
                                origin = "wss://192.168.1.10:8443",
                                statusLabel = "Connected",
                                detailLabel = "unix socket · v0.9.1 · protocol 3",
                                connected = true,
                                authRejected = false,
                                canReconnect = true,
                            ),
                            RelayRowUi(
                                relayId = "r2",
                                label = "desktop",
                                origin = "wss://desktop.local:8443",
                                statusLabel = "Auth rejected",
                                detailLabel = "Credential revoked — re-pair",
                                connected = false,
                                authRejected = true,
                                canReconnect = false,
                            ),
                        ),
                        themeMode = ThemeMode.SYSTEM,
                    ),
                    appVersion = "0.1.0",
                    notificationsEnabled = true,
                    appLockReady = true,
                    snackbarHostState = SnackbarHostState(),
                    onSelectTopLevel = {},
                    onReconnectRelay = {},
                    onForgetRelay = {},
                    onRevalidateAll = {},
                    onThemeMode = {},
                    onAppLockChange = {},
                    onOpenNotificationSettings = {},
                )
            }
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }
}
