package com.lerdr.app.session

import androidx.activity.ComponentActivity
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.assertHeightIsAtLeast
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.isDialog
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performKeyInput
import androidx.compose.ui.test.performSemanticsAction
import androidx.compose.ui.test.pressKey
import androidx.compose.ui.unit.dp
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage
import com.lerdr.core.designsystem.theme.LerdrTheme
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import org.robolectric.shadows.ShadowDialog
import org.robolectric.shadows.ShadowViewRootImpl
import org.robolectric.shadow.api.Shadow
import org.robolectric.util.ReflectionHelpers

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = com.lerdr.app.TestApp::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class TabOrderUiTest {
    @get:Rule val composeRule = createAndroidComposeRule<ComponentActivity>()
    private val options = RoborazziOptions(
        captureType = RoborazziOptions.CaptureType.Screenshot(),
        compareOptions = RoborazziOptions.CompareOptions(changeThreshold = 0.02f),
    )

    @Test
    fun `unavailable controller action is omitted rather than disabled`() {
        val state = mutableStateOf(TabOrderUiState(available = true, position = 2, tabCount = 3))
        composeRule.setContent { LerdrTheme { TabOrderMenuItem(state.value, onClick = {}) } }
        composeRule.onNodeWithTag("session-bar:reorder-tab").assertIsDisplayed()
        composeRule.runOnIdle { state.value = state.value.copy(available = false) }
        composeRule.onNodeWithTag("session-bar:reorder-tab").assertDoesNotExist()
    }

    @Test
    fun `dialog follows authoritative boundaries and blocks both moves while waiting`() {
        val state = mutableStateOf(TabOrderUiState(available = true, position = 1, tabCount = 3))
        composeRule.setContent {
            LerdrTheme { TabOrderDialog(state.value, onMoveLeft = {}, onMoveRight = {}, onDismiss = {}) }
        }
        composeRule.onNodeWithTag("tab-order:left").assertIsNotEnabled().assertHeightIsAtLeast(48.dp)
        composeRule.onNodeWithTag("tab-order:right").assertIsEnabled().assertHeightIsAtLeast(48.dp)
        composeRule.runOnIdle { state.value = state.value.copy(busy = true, waitingForOrder = true) }
        composeRule.onNodeWithText("Position 1 of 3 in this workspace").assertIsDisplayed()
        composeRule.onNodeWithText("Waiting for updated tab order…").assertIsDisplayed()
        composeRule.onNodeWithTag("tab-order:left").assertIsNotEnabled()
        composeRule.onNodeWithTag("tab-order:right").assertIsNotEnabled()
        composeRule.runOnIdle { state.value = state.value.copy(position = 3, busy = false, waitingForOrder = false) }
        composeRule.onNodeWithText("Position 3 of 3 in this workspace").assertIsDisplayed()
        composeRule.onNodeWithTag("tab-order:left").assertIsEnabled()
        composeRule.onNodeWithTag("tab-order:right").assertIsNotEnabled()
    }

    @Test
    fun `menu reaches dialog and keyboard can dismiss a pending move without changing its state`() {
        val pending = TabOrderUiState(available = true, position = 2, tabCount = 3, busy = true)
        composeRule.setContent {
            var open by remember { mutableStateOf(false) }
            LerdrTheme {
                TabOrderMenuItem(pending, onClick = { open = true })
                if (open) TabOrderDialog(pending, onMoveLeft = {}, onMoveRight = {}, onDismiss = { open = false })
            }
        }
        composeRule.onNodeWithTag("session-bar:reorder-tab").performClick()
        composeRule.onNodeWithText("Moving tab…").assertIsDisplayed()
        composeRule.runOnIdle {
            val decor = requireNotNull(ShadowDialog.getLatestDialog().window).decorView
            val root = ReflectionHelpers.callInstanceMethod<Any>(decor, "getViewRootImpl")
            Shadow.extract<ShadowViewRootImpl>(root).callWindowFocusChanged(true)
            ReflectionHelpers.callInstanceMethod<Boolean>(
                root,
                "ensureTouchMode",
                ReflectionHelpers.ClassParameter.from(Boolean::class.javaPrimitiveType!!, false),
            )
        }
        composeRule.onNodeWithTag("tab-order:done")
            .performSemanticsAction(SemanticsActions.RequestFocus) { it() }
            .performKeyInput { pressKey(Key.Enter) }
        composeRule.onNodeWithTag("tab-order:done").assertDoesNotExist()
        composeRule.onNodeWithTag("session-bar:reorder-tab").performClick()
        composeRule.onNodeWithText("Moving tab…").assertIsDisplayed()
        composeRule.onNodeWithTag("tab-order:right").assertIsNotEnabled()
    }

    @Test
    fun tabOrder_refusal() {
        composeRule.setContent {
            LerdrTheme {
                TabOrderDialog(
                    TabOrderUiState(available = true, position = 2, tabCount = 3, error = "Tab is unavailable"),
                    onMoveLeft = {}, onMoveRight = {}, onDismiss = {},
                )
            }
        }
        composeRule.onNodeWithText("Tab is unavailable").assertIsDisplayed()
        composeRule.onNodeWithText("Position 2 of 3 in this workspace").assertIsDisplayed()
        composeRule.onNodeWithTag("tab-order:left").assertIsEnabled()
        composeRule.onNode(isDialog()).captureRoboImage(roborazziOptions = options)
    }
}
