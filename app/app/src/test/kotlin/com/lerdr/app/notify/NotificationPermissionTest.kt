package com.lerdr.app.notify

import android.Manifest
import android.content.Context
import androidx.activity.ComponentActivity
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage
import com.google.common.truth.Truth.assertThat
import com.lerdr.core.designsystem.theme.LerdrTheme
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = com.lerdr.app.TestApp::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class NotificationPermissionTest {
    @get:Rule val composeRule = createAndroidComposeRule<ComponentActivity>()

    @Before
    fun resetOffer() {
        val activity = composeRule.activity
        activity.getSharedPreferences("notification_permission", Context.MODE_PRIVATE)
            .edit().clear().commit()
        shadowOf(activity.application).denyPermissions(Manifest.permission.POST_NOTIFICATIONS)
    }

    @Test
    fun `unpaired app does not ask and dismissing after pairing is remembered`() {
        val paired = mutableStateOf(false)
        composeRule.setContent {
            LerdrTheme { RequestPostNotificationsPermission(hasPairedComputer = paired.value) }
        }
        composeRule.onNodeWithText("Know when an agent needs you").assertDoesNotExist()
        composeRule.runOnIdle { paired.value = true }
        composeRule.onNodeWithText("Know when an agent needs you").assertIsDisplayed()
        composeRule.onNodeWithText("Not now").performClick()
        composeRule.onNodeWithText("Know when an agent needs you").assertDoesNotExist()
        composeRule.runOnIdle { paired.value = false }
        composeRule.runOnIdle { paired.value = true }
        composeRule.onNodeWithText("Know when an agent needs you").assertDoesNotExist()
        assertThat(composeRule.activity.getSharedPreferences(
            "notification_permission", Context.MODE_PRIVATE,
        ).getBoolean("offered", false)).isTrue()
    }

    @Test
    fun `already granted permission does not show the offer`() {
        shadowOf(composeRule.activity.application)
            .grantPermissions(Manifest.permission.POST_NOTIFICATIONS)
        composeRule.setContent {
            LerdrTheme { RequestPostNotificationsPermission(hasPairedComputer = true) }
        }
        composeRule.onNodeWithText("Know when an agent needs you").assertDoesNotExist()
    }

    @Test
    fun `only an explicit enable tap requests the system permission`() {
        composeRule.setContent {
            LerdrTheme { RequestPostNotificationsPermission(hasPairedComputer = true) }
        }
        assertThat(shadowOf(composeRule.activity).lastRequestedPermission).isNull()
        composeRule.onNodeWithText("Enable notifications").performClick()
        assertThat(shadowOf(composeRule.activity).lastRequestedPermission.requestedPermissions)
            .asList().containsExactly(Manifest.permission.POST_NOTIFICATIONS)
    }

    @Test
    fun permissionRationaleScreenshot() {
        composeRule.setContent {
            LerdrTheme { RequestPostNotificationsPermission(hasPairedComputer = true) }
        }
        composeRule.onNodeWithText("Enable notifications").assertIsDisplayed()
        composeRule.onRoot().captureRoboImage(roborazziOptions = RoborazziOptions(
            captureType = RoborazziOptions.CaptureType.Screenshot(),
        ))
    }
}
