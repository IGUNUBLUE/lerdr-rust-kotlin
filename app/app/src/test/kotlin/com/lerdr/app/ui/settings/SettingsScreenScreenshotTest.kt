package com.lerdr.app.ui.settings

import androidx.activity.ComponentActivity
import androidx.compose.material3.SnackbarHostState
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.performSemanticsAction
import androidx.compose.ui.text.TextLayoutResult
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.dp
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertHeightIsAtLeast
import androidx.compose.ui.test.assertIsNotSelected
import androidx.compose.ui.test.assertIsSelected
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.hasScrollAction
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performScrollToNode
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage
import com.lerdr.app.settings.RelayRowUi
import com.lerdr.app.settings.SettingsContent
import com.lerdr.app.settings.SettingsUiState
import com.lerdr.app.settings.ThemeMode
import com.lerdr.core.designsystem.theme.LerdrTheme
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
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
                    onOpenRelay = {},
                    onRevalidateAll = {},
                    onThemeMode = {},
                    onAppLockChange = {},
                    onOpenNotificationSettings = {},
                    onCheckUpdate = {},
                    onUpdateAction = {},
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
                    onOpenRelay = {},
                    onRevalidateAll = {},
                    onThemeMode = {},
                    onAppLockChange = {},
                    onOpenNotificationSettings = {},
                    onCheckUpdate = {},
                    onUpdateAction = {},
                )
            }
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun settings_notificationsLargeTextKeepsHeadingReadable() {
        composeRule.setContent {
            val density = LocalDensity.current
            CompositionLocalProvider(LocalDensity provides Density(density.density, 2f)) {
                LerdrTheme {
                    SettingsContent(
                        uiState = SettingsUiState(),
                        appVersion = "0.1.0",
                        notificationsEnabled = false,
                        appLockReady = true,
                        snackbarHostState = SnackbarHostState(),
                        onSelectTopLevel = {},
                        onOpenRelay = {},
                        onRevalidateAll = {},
                        onThemeMode = {},
                        onAppLockChange = {},
                        onOpenNotificationSettings = {},
                        onCheckUpdate = {},
                        onUpdateAction = {},
                    )
                }
            }
        }
        composeRule.onNode(hasScrollAction()).performScrollToNode(hasText("System settings"))
        val layouts = mutableListOf<TextLayoutResult>()
        composeRule.onNodeWithText("Notifications").performSemanticsAction(
            SemanticsActions.GetTextLayoutResult,
        ) { it(layouts) }
        assertEquals(1, layouts.single().lineCount)
        composeRule.onNodeWithText("System settings").assertIsDisplayed()
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    @Config(qualifiers = "w393dp-h851dp")
    fun settings_themeLargeTextKeepsCompleteLabelsAndSelection() {
        val choices = listOf(
            ThemeMode.SYSTEM to "System",
            ThemeMode.LIGHT to "Light",
            ThemeMode.DARK to "Dark",
        )
        composeRule.setContent {
            val density = LocalDensity.current
            CompositionLocalProvider(LocalDensity provides Density(density.density, 2f)) {
                var themeMode by remember { mutableStateOf(ThemeMode.SYSTEM) }
                LerdrTheme {
                    SettingsContent(
                        uiState = SettingsUiState(themeMode = themeMode),
                        appVersion = "0.1.0",
                        notificationsEnabled = true,
                        appLockReady = true,
                        snackbarHostState = SnackbarHostState(),
                        onSelectTopLevel = {},
                        onOpenRelay = {},
                        onRevalidateAll = {},
                        onThemeMode = {
                            themeMode = it
                        },
                        onAppLockChange = {},
                        onOpenNotificationSettings = {},
                        onCheckUpdate = {},
                        onUpdateAction = {},
                    )
                }
            }
        }
        composeRule.onNode(hasScrollAction()).performScrollToNode(hasText("Dark"))
        choices.forEach { (_, label) ->
            composeRule.onNodeWithText(label)
                .assertIsDisplayed()
                .assertHeightIsAtLeast(48.dp)
            val layouts = mutableListOf<TextLayoutResult>()
            composeRule.onNodeWithText(label, useUnmergedTree = true)
                .performSemanticsAction(SemanticsActions.GetTextLayoutResult) { it(layouts) }
            assertEquals(1, layouts.single().lineCount)
            assertFalse(layouts.single().hasVisualOverflow)
        }
        composeRule.onNodeWithText("System").assertIsSelected()
        composeRule.onNodeWithText("Light").assertIsNotSelected()
        composeRule.onNodeWithText("Dark").assertIsNotSelected()
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)

        listOf(ThemeMode.LIGHT, ThemeMode.DARK, ThemeMode.SYSTEM).forEach { selected ->
            composeRule.onNodeWithText(choices.first { it.first == selected }.second).performClick()
            choices.forEach { (mode, label) ->
                val choice = composeRule.onNodeWithText(label)
                if (mode == selected) choice.assertIsSelected() else choice.assertIsNotSelected()
            }
        }
    }

    @Test
    fun settings_about() {
        composeRule.setContent {
            LerdrTheme {
                SettingsContent(
                    uiState = SettingsUiState(),
                    appVersion = "0.1.0",
                    notificationsEnabled = true,
                    appLockReady = true,
                    snackbarHostState = SnackbarHostState(),
                    onSelectTopLevel = {},
                    onOpenRelay = {},
                    onRevalidateAll = {},
                    onThemeMode = {},
                    onAppLockChange = {},
                    onOpenNotificationSettings = {},
                    onCheckUpdate = {},
                    onUpdateAction = {},
                )
            }
        }
        composeRule.onNode(hasScrollAction()).performScrollToNode(hasText("Protocol"))
        composeRule.onNodeWithText("Protocol").assertIsDisplayed()
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }
}
