package com.lerdr.app.ui.designsystem

import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Surface
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onRoot
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage
import com.lerdr.app.TestApp
import com.lerdr.core.designsystem.components.LerdrStatus
import com.lerdr.core.designsystem.components.LerdrStatusChip
import com.lerdr.core.designsystem.theme.LerdrTheme
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * Roborazzi coverage for the morphing status chip — one golden per status,
 * both schemes, so the idle/working/attention/error shape + container
 * mapping can't silently regress.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = TestApp::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class StatusChipScreenshotTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private val options = RoborazziOptions(
        // Dump (the JVM default) paints a semantics-tree overlay whose
        // node text jitters run-to-run — force a plain bitmap capture.
        captureType = RoborazziOptions.CaptureType.Screenshot(),
        compareOptions = RoborazziOptions.CompareOptions(changeThreshold = 0.02f),
    )

    @Test
    fun statusChip_allStatuses() {
        composeRule.setContent {
            LerdrTheme {
                Surface {
                    Column(
                        verticalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.small),
                        modifier = Modifier.padding(LerdrTheme.spacing.medium),
                    ) {
                        LerdrStatus.entries.forEach { status ->
                            LerdrStatusChip(status = status, label = status.stateLabel)
                        }
                    }
                }
            }
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun statusChip_allStatusesLight() {
        composeRule.setContent {
            LerdrTheme(darkTheme = false) {
                Surface {
                    Column(
                        verticalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.small),
                        modifier = Modifier.padding(LerdrTheme.spacing.medium),
                    ) {
                        LerdrStatus.entries.forEach { status ->
                            LerdrStatusChip(status = status, label = status.stateLabel)
                        }
                    }
                }
            }
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }
}
