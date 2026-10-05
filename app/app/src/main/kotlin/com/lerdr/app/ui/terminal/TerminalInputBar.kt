package com.lerdr.app.ui.terminal

import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.Shield
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.ui.tooling.preview.PreviewLightDark
import androidx.compose.ui.unit.dp
import com.lerdr.core.designsystem.theme.LerdrTheme
import kotlinx.coroutines.launch

/**
 * Terminal input row — the `send_input` seam of terminal mode: Send is
 * the composer action (text + Enter as one action, like the feed
 * composer). Literal `send_text` injection stays reachable through the
 * special-keys bar (Enter is a key chip there).
 *
 * The field buffers a draft locally until the send callback acknowledges
 * delivery. Edits made while waiting become the next draft and are not cleared.
 * [enabled] controls editing permission; [canSend] separately gates delivery.
 * [focusRequester] backs that bar's "show keyboard" affordance.
 *
 * Secret mode ([secretMode] while the pane reports `no_echo`): the same
 * row becomes the hidden-prompt answer — masked glyphs, "Password" label,
 * shield marker — and Send rides `send_secret`, never `send_text`. The
 * secret draft deliberately uses plain `remember` (not `rememberSaveable`)
 * and resets on mode exit: a hidden-prompt answer must never reach this
 * phone's saved state (Lerdr's `secretValue` — "never saved on this
 * phone and never written to activity").
 *
 * Plain M3 throughout — `core:designsystem` has no text-field or chip
 * wrapper yet, so stable M3 components are used directly.
 */
@Composable
fun TerminalInputBar(
    onSendText: suspend (String) -> Boolean,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    canSend: Boolean = enabled,
    hint: String = "Inject text…",
    focusRequester: FocusRequester = remember { FocusRequester() },
    ctrlLatched: Boolean = false,
    onCtrlChord: (Char) -> Unit = {},
    secretMode: Boolean = false,
    onSendSecret: suspend (String) -> Boolean = { false },
) {
    val spacing = LerdrTheme.spacing
    val draft = rememberSaveable(stateSaver = TextFieldValue.Saver) {
        mutableStateOf(TextFieldValue())
    }
    val draftRevision = remember { mutableLongStateOf(0L) }
    // Keyed on the mode so a stale answer can't survive the prompt that
    // authored it — Lerdr clears `secretValue` when secretMode ends.
    val secretDraft = remember(secretMode) { mutableStateOf(TextFieldValue()) }
    val secretRevision = remember(secretMode) { mutableLongStateOf(0L) }
    val scope = rememberCoroutineScope()
    // Null means idle; otherwise this records the in-flight action's mode,
    // even if the visible field switches modes before acknowledgement.
    var sendingSecret by remember { mutableStateOf<Boolean?>(null) }
    val sending = sendingSecret != null
    val canSubmit = enabled && canSend && !sending &&
        (if (secretMode) secretDraft.value else draft.value).text.isNotEmpty()

    fun submit() {
        val activeDraft = if (secretMode) secretDraft else draft
        val revision = if (secretMode) secretRevision else draftRevision
        val text = activeDraft.value.text
        if (text.isEmpty() || !enabled || !canSend || sendingSecret != null) return
        val submittedRevision = revision.longValue
        val send = if (secretMode) onSendSecret else onSendText
        sendingSecret = secretMode
        scope.launch {
            try {
                if (send(text) && revision.longValue == submittedRevision) {
                    activeDraft.value = TextFieldValue()
                }
            } finally {
                sendingSecret = null
            }
        }
    }

    Surface(color = MaterialTheme.colorScheme.surfaceContainerLow, modifier = modifier) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = spacing.medium, vertical = spacing.small),
        ) {
            OutlinedTextField(
                value = if (secretMode) secretDraft.value else draft.value,
                onValueChange = { next ->
                    val activeDraft = if (secretMode) secretDraft else draft
                    val revision = if (secretMode) secretRevision else draftRevision
                    val current = activeDraft.value
                    val chord = if (ctrlLatched && enabled && canSend) {
                        typedCtrlLetter(current, next)
                    } else {
                        null
                    }
                    if (chord != null) {
                        // Keep the complete draft, including a selected range.
                        onCtrlChord(chord)
                    } else {
                        if (next.text != current.text) revision.longValue++
                        activeDraft.value = next
                    }
                },
                placeholder = { Text(hint) },
                label = if (secretMode) {
                    { Text("Password") }
                } else {
                    null
                },
                leadingIcon = if (secretMode) {
                    {
                        Icon(
                            Icons.Filled.Shield,
                            contentDescription = null,
                            tint = LerdrTheme.extendedColors.terminalAccent,
                        )
                    }
                } else {
                    null
                },
                supportingText = if (secretMode) {
                    {
                        // Lerdr's `.secret-prompt` hint — verbatim.
                        Text(
                            "Typed straight into the terminal: never saved " +
                                "on this phone and never written to activity.",
                        )
                    }
                } else {
                    null
                },
                visualTransformation = if (secretMode) {
                    PasswordVisualTransformation()
                } else {
                    androidx.compose.ui.text.input.VisualTransformation.None
                },
                enabled = enabled,
                singleLine = true,
                keyboardOptions = KeyboardOptions(
                    // Terminal context — no suggestion strip. Secret mode
                    // switches to the password IME; Send is the literal
                    // inject/answer action (Enter is a key chip).
                    autoCorrectEnabled = false,
                    keyboardType = if (secretMode) {
                        KeyboardType.Password
                    } else {
                        KeyboardType.Ascii
                    },
                    imeAction = ImeAction.Send,
                ),
                keyboardActions = KeyboardActions(onSend = { submit() }),
                shape = MaterialTheme.shapes.medium,
                textStyle = LerdrTheme.terminalStyle,
                modifier = Modifier
                    .weight(1f)
                    .focusRequester(focusRequester)
                    .testTag(if (secretMode) "terminalSecretField" else "terminalInputField"),
            )
            IconButton(
                onClick = ::submit,
                enabled = canSubmit,
                modifier = Modifier
                    .testTag("terminalSendButton")
                    .semantics {
                        if (sending) {
                            stateDescription = "Sending"
                            liveRegion = LiveRegionMode.Polite
                        }
                    },
            ) {
                if (sending) {
                    CircularProgressIndicator(
                        modifier = Modifier
                            .size(24.dp)
                            .semantics {
                                contentDescription = if (sendingSecret == true) {
                                    "Sending password"
                                } else {
                                    "Sending text"
                                }
                            },
                        strokeWidth = 2.dp,
                    )
                } else {
                    Icon(
                        Icons.AutoMirrored.Filled.Send,
                        contentDescription = if (secretMode) {
                            "Send password"
                        } else {
                            "Send text"
                        },
                        tint = if (canSubmit) MaterialTheme.colorScheme.primary
                            else MaterialTheme.colorScheme.onSurface.copy(alpha = 0.38f),
                    )
                }
            }
        }
    }
}

/** Only a single ASCII letter replacing the current selection is a Ctrl chord. */
private fun typedCtrlLetter(current: TextFieldValue, next: TextFieldValue): Char? {
    if (next.text == current.text) return null
    val start = current.selection.min
    val end = current.selection.max
    if (next.text.length != current.text.length - (end - start) + 1 ||
        !next.text.regionMatches(0, current.text, 0, start) ||
        !next.text.regionMatches(start + 1, current.text, end, current.text.length - end)
    ) {
        return null
    }
    return next.text[start].takeIf { it in 'a'..'z' || it in 'A'..'Z' }
}

@PreviewLightDark
@Composable
private fun TerminalInputBarPreview() {
    LerdrTheme {
        TerminalInputBar(onSendText = { true })
    }
}
