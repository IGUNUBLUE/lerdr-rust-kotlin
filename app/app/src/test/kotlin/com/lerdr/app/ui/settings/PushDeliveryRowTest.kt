package com.lerdr.app.ui.settings

import androidx.activity.ComponentActivity
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.dp
import androidx.compose.ui.test.assertHeightIsAtLeast
import androidx.compose.material3.Surface
import androidx.compose.ui.test.assertIsSelected
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage
import com.google.common.truth.Truth.assertThat
import com.lerdr.app.TestApp
import com.lerdr.app.push.PushStage
import com.lerdr.app.push.PushUiState
import com.lerdr.app.settings.PushDeliveryRow
import com.lerdr.core.designsystem.theme.LerdrTheme
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = TestApp::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class PushDeliveryRowTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()
    private val sunup = "org.unifiedpush.distributor.sunup"

    @Test fun setupGuideAndProviderSelection() {
        var setup = 0
        var change = 0
        var cancel = 0
        var selected = ""
        compose.setContent {
            LerdrTheme { Surface {
                PushDeliveryRow(PushUiState(stage = PushStage.SUBSCRIBED,
                    distributor = sunup, endpointHost = "updates.push.services.mozilla.com",
                    subscribedRelays = setOf("r1"), choosingDistributor = true,
                    distributors = listOf("io.heckel.ntfy", sunup)), "r1", true,
                    { selected = it }, { change++ }, { cancel++ }, { setup++ })
            } }
        }
        compose.onNodeWithText("Sunup").assertIsSelected()
        compose.onNodeWithText("Cancel").performClick()
        compose.onNodeWithText("ntfy").performClick()
        compose.onNodeWithText("Sunup setup guide").performClick()
        assertThat(cancel).isEqualTo(1)
        assertThat(selected).isEqualTo("io.heckel.ntfy")
        assertThat(setup).isEqualTo(1)
        assertThat(change).isEqualTo(0)
        capture()
    }

    @Test fun setupWithoutDistributor() {
        var setup = false
        var change = false
        compose.setContent {
            LerdrTheme { Surface {
                PushDeliveryRow(PushUiState(stage = PushStage.NO_DISTRIBUTOR), "r1", true,
                    {}, { change = true }, {}, { setup = true })
            } }
        }
        compose.onNodeWithText("Set up Sunup").performClick()
        compose.onNodeWithText("Change provider").performClick()
        assertThat(setup).isTrue()
        assertThat(change).isTrue()
        capture()
    }

    @Test
    @Config(qualifiers = "w393dp-h851dp")
    fun setupAtLargeText() {
        compose.setContent {
            val density = LocalDensity.current
            CompositionLocalProvider(LocalDensity provides Density(density.density, 2f)) {
                LerdrTheme { Surface {
                    PushDeliveryRow(PushUiState(stage = PushStage.NO_DISTRIBUTOR), "r1", true,
                        {}, {}, {}, {})
                } }
            }
        }
        compose.onNodeWithText("Set up Sunup").assertHeightIsAtLeast(48.dp)
        capture()
    }

    private fun capture() {
        compose.onRoot().captureRoboImage(roborazziOptions = RoborazziOptions(
            captureType = RoborazziOptions.CaptureType.Screenshot(),
            compareOptions = RoborazziOptions.CompareOptions(changeThreshold = 0.02f),
        ))
    }
}
