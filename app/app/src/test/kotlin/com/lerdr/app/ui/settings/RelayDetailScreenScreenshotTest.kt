package com.lerdr.app.ui.settings

import androidx.activity.ComponentActivity
import androidx.compose.material3.SnackbarHostState
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onRoot
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage
import com.lerdr.app.settings.RelayDetailContent
import com.lerdr.app.settings.RelayRowUi
import com.lerdr.core.designsystem.theme.LerdrTheme
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * Roborazzi coverage for the relay detail screen — the second level of
 * the Settings hierarchy, with the status card and lifecycle actions.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = com.lerdr.app.TestApp::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class RelayDetailScreenScreenshotTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private val options = RoborazziOptions(
        captureType = RoborazziOptions.CaptureType.Screenshot(),
        compareOptions = RoborazziOptions.CompareOptions(changeThreshold = 0.02f),
    )

    @Test
    fun relayDetail_connected() {
        composeRule.setContent {
            LerdrTheme {
                RelayDetailContent(
                    relay = RelayRowUi(
                        relayId = "r1",
                        label = "workstation",
                        origin = "wss://192.168.1.10:8443",
                        statusLabel = "Connected",
                        detailLabel = "unix socket · v0.9.1 · protocol 3",
                        connected = true,
                        authRejected = false,
                        canReconnect = true,
                    ),
                    snackbarHostState = SnackbarHostState(),
                    onBack = {},
                    onReconnect = {},
                    onForget = {},
                )
            }
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }
}
