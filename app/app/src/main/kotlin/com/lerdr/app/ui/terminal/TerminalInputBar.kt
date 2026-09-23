package com.lerdr.app.ui.terminal

import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.ui.tooling.preview.PreviewLightDark
import com.lerdr.core.designsystem.theme.LerdrTheme

/**
 * Terminal input row — the `send_text` / `send_keys` seam of terminal mode
 * (docs/04 §Terminal mode: typing injects literal text, Enter is a key).
 *
 * The field buffers a draft locally; Send injects it verbatim via
 * `send_text` (no Enter appended — the pane echoes or ignores it). The
 * key chips live on the special-keys bar above — one bar per the mockup.
 * [focusRequester] backs that bar's "show keyboard" affordance.
 *
 * Plain M3 throughout — `core:designsystem` has no text-field or chip
 * wrapper yet, so stable M3 components are used directly.
 */
@Composable
fun TerminalInputBar(
    onSendText: (String) -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    hint: String = "Inject text…",
    focusRequester: FocusRequester = remember { FocusRequester() },
    ctrlLatched: Boolean = false,
    onCtrlChord: (Char) -> Unit = {},
) {
    val spacing = LerdrTheme.spacing
    var draft by rememberSaveable { mutableStateOf("") }

    fun submit() {
        val text = draft
        if (text.isEmpty() || !enabled) return
        draft = ""
        onSendText(text)
    }

    Surface(color = MaterialTheme.colorScheme.surfaceContainerLow, modifier = modifier) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = spacing.medium, vertical = spacing.small),
        ) {
            OutlinedTextField(
                value = draft,
                onValueChange = { next ->
                    // Latched Ctrl turns the next typed letter into the
                    // chord — the letter never enters the draft.
                    val appended = next.length == draft.length + 1 &&
                        next.startsWith(draft)
                    if (ctrlLatched && appended && next.last().isLetter()) {
                        onCtrlChord(next.last())
                    } else {
                        draft = next
                    }
                },
                placeholder = { Text(hint) },
                enabled = enabled,
                singleLine = true,
                keyboardOptions = KeyboardOptions(
                    // Terminal context — no suggestion strip, Send is the
                    // literal-inject action (Enter is a key chip).
                    keyboardType = KeyboardType.Ascii,
                    imeAction = ImeAction.Send,
                ),
                keyboardActions = KeyboardActions(onSend = { submit() }),
                shape = MaterialTheme.shapes.medium,
                textStyle = LerdrTheme.terminalStyle,
                modifier = Modifier
                    .weight(1f)
                    .focusRequester(focusRequester),
            )
            IconButton(onClick = ::submit, enabled = enabled && draft.isNotEmpty()) {
                Icon(
                    Icons.AutoMirrored.Filled.Send,
                    contentDescription = "Send text",
                    tint = MaterialTheme.colorScheme.primary,
                )
            }
        }
    }
}

@PreviewLightDark
@Composable
private fun TerminalInputBarPreview() {
    LerdrTheme {
        TerminalInputBar(onSendText = {})
    }
}
