package com.lerdr.app.ui.activity

import androidx.activity.ComponentActivity
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onRoot
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage
import com.lerdr.app.activity.ActivityContent
import com.lerdr.app.activity.ActivityItemKind
import com.lerdr.app.activity.ActivityItemUi
import com.lerdr.app.activity.ActivityUiState
import com.lerdr.app.activity.RelayFilterUi
import com.lerdr.core.designsystem.theme.LerdrTheme
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * Roborazzi coverage for the activity journal — empty state plus a
 * populated, multi-relay list with the filter chips selected.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = com.lerdr.app.TestApp::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class ActivityScreenScreenshotTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private val options = RoborazziOptions(
        // Dump (the JVM default) paints a semantics-tree overlay whose
        // node text jitters run-to-run — force a plain bitmap capture.
        captureType = RoborazziOptions.CaptureType.Screenshot(),
        compareOptions = RoborazziOptions.CompareOptions(changeThreshold = 0.02f),
    )

    private fun item(
        key: String,
        kind: ActivityItemKind,
        headline: String,
        detail: String = "",
        relay: String = "workstation",
        age: String = "5m ago",
    ) = ActivityItemUi(
        key = key,
        relayId = relay,
        relayLabel = relay,
        headline = headline,
        detail = detail,
        timestampEpochMs = 1_758_200_000_000,
        timestampLabel = "14:32",
        ageLabel = age,
        kind = kind,
    )

    @Test
    fun activity_empty() {
        composeRule.setContent {
            LerdrTheme {
                ActivityContent(
                    uiState = ActivityUiState(),
                    onSelectTopLevel = {},
                    onSelectFilter = {},
                    onRefresh = {},
                )
            }
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun activity_populated() {
        composeRule.setContent {
            LerdrTheme {
                ActivityContent(
                    uiState = ActivityUiState(
                        items = listOf(
                            item(
                                "a1",
                                ActivityItemKind.ACTION,
                                "send_text → herdr-mobile-relay",
                                detail = "cargo test -p lerdr-coord",
                                age = "1m ago",
                            ),
                            item(
                                "a2",
                                ActivityItemKind.CONNECTED,
                                "Connected to workstation",
                                detail = "wss://192.168.1.10:8443",
                            ),
                            item(
                                "a3",
                                ActivityItemKind.PAIRING_REQUIRED,
                                "New device wants to pair",
                                detail = "Pixel 9 · controller",
                                relay = "desktop",
                                age = "22m ago",
                            ),
                        ),
                        filters = listOf(
                            RelayFilterUi("workstation", "workstation", selected = true),
                            RelayFilterUi("desktop", "desktop", selected = false),
                        ),
                        selectedFilter = "workstation",
                    ),
                    onSelectTopLevel = {},
                    onSelectFilter = {},
                    onRefresh = {},
                )
            }
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }
}
