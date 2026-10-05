package com.lerdr.app.ui.terminal

import androidx.activity.ComponentActivity
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.SemanticsNodeInteraction
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.junit4.StateRestorationTester
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performImeAction
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.test.performTextInputSelection
import androidx.compose.ui.test.performTextReplacement
import androidx.compose.ui.text.TextRange
import com.lerdr.core.designsystem.theme.LerdrTheme
import kotlinx.coroutines.CompletableDeferred
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = com.lerdr.app.TestApp::class)
class TerminalInputBarBehaviorTest {
    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    @Test
    fun pendingAndFailureKeepDraftAndSelectionUntilAcknowledgedRetry() {
        var acknowledgement = CompletableDeferred<Boolean>()
        var submissions = 0
        composeRule.setContent {
            LerdrTheme {
                TerminalInputBar(onSendText = {
                    submissions++
                    acknowledgement.await()
                })
            }
        }
        val field = composeRule.onNodeWithTag("terminalInputField")
        val send = composeRule.onNodeWithTag("terminalSendButton")
        field.performTextInput("alpha omega")
        field.performTextInputSelection(TextRange(11, 6))
        send.performClick()
        field.assertTextEquals("alpha omega")
        field.assertIsEnabled()
        send.assertIsNotEnabled()
        composeRule.onNodeWithContentDescription("Sending text").assertExists()
        field.performImeAction()
        send.performClick()
        composeRule.runOnIdle { assertEquals(1, submissions) }

        composeRule.runOnIdle { acknowledgement.complete(false) }
        field.assertTextEquals("alpha omega")
        field.performTextInput("beta")
        field.assertTextEquals("alpha beta")
        send.assertIsEnabled()
        composeRule.onNodeWithContentDescription("Sending text").assertDoesNotExist()
        composeRule.runOnIdle { acknowledgement = CompletableDeferred() }
        field.performImeAction()
        composeRule.runOnIdle {
            assertEquals(2, submissions)
            acknowledgement.complete(true)
        }
        field.assertEditableLength(0).assertSelection(TextRange.Zero)
        send.assertIsNotEnabled()
    }

    @Test
    fun acknowledgementPreservesNextDraftAndItsCaret() {
        val acknowledgement = CompletableDeferred<Boolean>()
        val submissions = mutableListOf<String>()
        composeRule.setContent {
            LerdrTheme {
                TerminalInputBar(onSendText = {
                    submissions += it
                    if (submissions.size == 1) acknowledgement.await() else true
                })
            }
        }
        val field = composeRule.onNodeWithTag("terminalInputField")
        field.performTextInput("first command")
        field.performImeAction()
        field.performTextReplacement("next command")
        field.performTextInputSelection(TextRange(4))
        composeRule.runOnIdle { acknowledgement.complete(true) }
        field.assertTextEquals("next command").assertSelection(TextRange(4))
        composeRule.onNodeWithTag("terminalSendButton").assertIsEnabled()
        field.performTextInput(" safe")
        field.performImeAction()
        field.assertEditableLength(0)
        composeRule.runOnIdle {
            assertEquals(listOf("first command", "next safe command"), submissions)
        }
    }

    @Test
    fun editedDraftIsRetainedEvenWhenItsTextReturnsToSubmittedText() {
        val acknowledgement = CompletableDeferred<Boolean>()
        composeRule.setContent {
            LerdrTheme {
                TerminalInputBar(onSendText = { acknowledgement.await() })
            }
        }
        val field = composeRule.onNodeWithTag("terminalInputField")
        field.performTextInput("same command")
        field.performImeAction()
        field.performTextReplacement("different command")
        field.performTextReplacement("same command")
        field.performTextInputSelection(TextRange(4, 0))
        composeRule.runOnIdle { acknowledgement.complete(true) }
        field.assertTextEquals("same command")
        field.performTextInput("next")
        field.assertTextEquals("next command")
        composeRule.onNodeWithTag("terminalSendButton").assertIsEnabled()
    }

    @Test
    fun failedSendDoesNotUndoEditsMadeWhileWaiting() {
        val acknowledgement = CompletableDeferred<Boolean>()
        composeRule.setContent {
            LerdrTheme {
                TerminalInputBar(onSendText = { acknowledgement.await() })
            }
        }
        val field = composeRule.onNodeWithTag("terminalInputField")
        field.performTextInput("old command")
        field.performImeAction()
        field.performTextReplacement("revised command")
        field.performTextInputSelection(TextRange(7, 0))
        composeRule.runOnIdle { acknowledgement.complete(false) }
        field.assertTextEquals("revised command")
        field.performTextInput("next")
        field.assertTextEquals("next command")
        composeRule.onNodeWithTag("terminalSendButton").assertIsEnabled()
    }

    @Test
    fun disconnectedControllerCanEditButCannotSubmitUntilReconnect() {
        val connected = mutableStateOf(false)
        val acknowledgement = CompletableDeferred<Boolean>()
        var submissions = 0
        composeRule.setContent {
            LerdrTheme {
                TerminalInputBar(
                    onSendText = { submissions++; acknowledgement.await() },
                    canSend = connected.value,
                )
            }
        }
        val field = composeRule.onNodeWithTag("terminalInputField")
        val send = composeRule.onNodeWithTag("terminalSendButton")
        field.assertIsEnabled().performTextInput("offline command")
        field.performTextInputSelection(TextRange(7))
        send.assertIsNotEnabled().performClick()
        field.performImeAction()
        field.assertTextEquals("offline command").assertSelection(TextRange(7))
        composeRule.runOnIdle {
            assertEquals(0, submissions)
            connected.value = true
        }
        send.assertIsEnabled().performClick()
        composeRule.runOnIdle { connected.value = false }
        field.performImeAction()
        composeRule.runOnIdle {
            assertEquals(1, submissions)
            acknowledgement.complete(false)
        }
        field.assertTextEquals("offline command").assertSelection(TextRange(7))
        field.assertIsEnabled()
        send.assertIsNotEnabled()
    }

    @Test
    fun savedPlainSelectionStillReplacesTheSelectedRangeAfterRestore() {
        val restoration = StateRestorationTester(composeRule)
        restoration.setContent {
            LerdrTheme { TerminalInputBar(onSendText = { true }) }
        }
        val field = composeRule.onNodeWithTag("terminalInputField")
        field.performTextInput("alpha omega")
        field.performTextInputSelection(TextRange(11, 6))
        restoration.emulateSavedInstanceStateRestore()
        field.performTextInput("beta")
        field.assertTextEquals("alpha beta").assertSelection(TextRange(10))
    }

    @Test
    fun secretFailureKeepsAnswerAndCaretAndSuccessClearsIt() {
        var acknowledgement = CompletableDeferred<Boolean>()
        var secretSubmissions = 0
        var plainSubmissions = 0
        composeRule.setContent {
            LerdrTheme {
                TerminalInputBar(
                    onSendText = { plainSubmissions++; true },
                    secretMode = true,
                    onSendSecret = { secretSubmissions++; acknowledgement.await() },
                )
            }
        }
        val field = composeRule.onNodeWithTag("terminalSecretField")
        val send = composeRule.onNodeWithTag("terminalSendButton")
        field.performTextInput("synthetic-answer")
        field.performTextInputSelection(TextRange(4))
        field.performImeAction()
        composeRule.onNodeWithContentDescription("Sending password").assertExists()
        field.assertEditableLength(16).assertSelection(TextRange(4))
        field.performImeAction()
        composeRule.runOnIdle {
            assertEquals(1, secretSubmissions)
            assertEquals(0, plainSubmissions)
            acknowledgement.complete(false)
        }
        field.assertEditableLength(16).assertSelection(TextRange(4))
        send.assertIsEnabled()
        composeRule.runOnIdle { acknowledgement = CompletableDeferred() }
        send.performClick()
        composeRule.runOnIdle { acknowledgement.complete(true) }
        field.assertEditableLength(0).assertSelection(TextRange.Zero)
        send.assertIsNotEnabled()
        composeRule.runOnIdle { assertEquals(2, secretSubmissions) }
    }

    @Test
    fun leavingSecretModeClearsAnswerAndOldAcknowledgementCannotEraseNewAnswer() {
        val secretMode = mutableStateOf(false)
        val acknowledgement = CompletableDeferred<Boolean>()
        composeRule.setContent {
            LerdrTheme {
                TerminalInputBar(
                    onSendText = { true },
                    secretMode = secretMode.value,
                    onSendSecret = { acknowledgement.await() },
                )
            }
        }
        composeRule.onNodeWithTag("terminalInputField").performTextInput("plain draft")
        composeRule.runOnIdle { secretMode.value = true }
        val secretField = composeRule.onNodeWithTag("terminalSecretField")
        secretField.performTextInput("first answer")
        secretField.performImeAction()
        composeRule.runOnIdle { secretMode.value = false }
        composeRule.onNodeWithTag("terminalInputField").assertTextEquals("plain draft")
        composeRule.runOnIdle { secretMode.value = true }
        secretField.assertEditableLength(0)
        secretField.performTextInput("second answer")
        secretField.performTextInputSelection(TextRange(6))
        composeRule.runOnIdle { acknowledgement.complete(true) }
        secretField.assertEditableLength(13).assertSelection(TextRange(6))
        composeRule.onNodeWithTag("terminalSendButton").assertIsEnabled()
    }

    @Test
    fun savedStateDoesNotRestoreSecretAnswerOrPendingSend() {
        val restoration = StateRestorationTester(composeRule)
        val acknowledgement = CompletableDeferred<Boolean>()
        restoration.setContent {
            LerdrTheme {
                TerminalInputBar(
                    onSendText = { true },
                    secretMode = true,
                    onSendSecret = { acknowledgement.await() },
                )
            }
        }
        val field = composeRule.onNodeWithTag("terminalSecretField")
        field.performTextInput("synthetic-answer")
        field.performTextInputSelection(TextRange(4))
        field.performImeAction()
        restoration.emulateSavedInstanceStateRestore()
        field.assertEditableLength(0).assertSelection(TextRange.Zero)
        composeRule.onNodeWithContentDescription("Sending password").assertDoesNotExist()
        composeRule.onNodeWithTag("terminalSendButton").assertIsNotEnabled()
    }

    @Test
    fun latchedCtrlAtMiddleCaretDoesNotInsertLetterOrMoveCaret() {
        val ctrlLatched = mutableStateOf(false)
        val chords = mutableListOf<Char>()
        composeRule.setContent {
            LerdrTheme {
                TerminalInputBar(
                    onSendText = { true },
                    ctrlLatched = ctrlLatched.value,
                    onCtrlChord = { chords += it },
                )
            }
        }
        val field = composeRule.onNodeWithTag("terminalInputField")
        field.performTextInput("alpha omega")
        field.performTextInputSelection(TextRange(6))
        composeRule.runOnIdle { ctrlLatched.value = true }
        field.performTextInput("C")
        field.assertTextEquals("alpha omega").assertSelection(TextRange(6))
        composeRule.runOnIdle {
            assertEquals(listOf('C'), chords)
            ctrlLatched.value = false
        }
        field.performTextInput("beta ")
        field.assertTextEquals("alpha beta omega")
    }

    @Test
    fun latchedCtrlOverSelectionKeepsSelectedTextAndRange() {
        val ctrlLatched = mutableStateOf(false)
        val chords = mutableListOf<Char>()
        composeRule.setContent {
            LerdrTheme {
                TerminalInputBar(
                    onSendText = { true },
                    ctrlLatched = ctrlLatched.value,
                    onCtrlChord = { chords += it },
                )
            }
        }
        val field = composeRule.onNodeWithTag("terminalInputField")
        field.performTextInput("alpha omega")
        field.performTextInputSelection(TextRange(11, 6))
        composeRule.runOnIdle { ctrlLatched.value = true }
        field.performTextInput("r")
        field.assertTextEquals("alpha omega")
        composeRule.runOnIdle {
            assertEquals(listOf('r'), chords)
            ctrlLatched.value = false
        }
        field.performTextInput("beta")
        field.assertTextEquals("alpha beta")
    }

    @Test
    fun latchedCtrlLeavesUnicodePunctuationAndPastedTextAsDraftEdits() {
        val chords = mutableListOf<Char>()
        composeRule.setContent {
            LerdrTheme {
                TerminalInputBar(
                    onSendText = { true },
                    ctrlLatched = true,
                    onCtrlChord = { chords += it },
                )
            }
        }
        val field = composeRule.onNodeWithTag("terminalInputField")
        field.performTextInput("left right")
        field.performTextInputSelection(TextRange(5))
        field.performTextInput("é")
        field.performTextInput("Ｃ")
        field.performTextInput("?")
        field.performTextInput("abc")
        field.assertTextEquals("left éＣ?abcright")
        composeRule.runOnIdle { assertEquals(emptyList<Char>(), chords) }
    }

    private fun SemanticsNodeInteraction.assertSelection(range: TextRange) =
        assert(SemanticsMatcher.expectValue(SemanticsProperties.TextSelectionRange, range))

    private fun SemanticsNodeInteraction.assertEditableLength(length: Int) =
        assert(SemanticsMatcher("Editable text has length $length") {
            it.config[SemanticsProperties.EditableText].length == length
        })
}
