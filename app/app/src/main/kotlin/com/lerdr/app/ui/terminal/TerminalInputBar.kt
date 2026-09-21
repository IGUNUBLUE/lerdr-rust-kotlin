package com.lerdr.app.ui.terminal

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
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
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
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
 * `send_text` (no Enter appended — the pane echoes or ignores it), and the
 * key chips fire `send_keys` chords. Wire names match the oracle's
 * `"Ctrl+C"`/`"Escape"` spellings.
 *
 * Plain M3 throughout — `core:designsystem` has no text-field or chip
 * wrapper yet, so stable M3 components are used directly.
 */
@Composable
fun TerminalInputBar(
    onSendText: (String) -> Unit,
    onSendKeys: (List<String>) -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    hint: String = "Inject text…",
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
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(vertical = spacing.small),
        ) {
            Row(
                horizontalArrangement = Arrangement.spacedBy(spacing.small),
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .fillMaxWidth()
                    .horizontalScroll(rememberScrollState())
                    .padding(horizontal = spacing.medium),
            ) {
                TERMINAL_KEYS.forEach { (label, key) ->
                    Surface(
                        color = MaterialTheme.colorScheme.surfaceContainerHighest,
                        contentColor = MaterialTheme.colorScheme.onSurface,
                        shape = MaterialTheme.shapes.small,
                        enabled = enabled,
                        onClick = { onSendKeys(listOf(key)) },
                    ) {
                        Text(
                            label,
                            style = MaterialTheme.typography.labelLarge,
                            modifier = Modifier.padding(
                                horizontal = spacing.small + spacing.extraSmall,
                                vertical = spacing.extraSmall,
                            ),
                        )
                    }
                }
            }
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = spacing.medium)
                    .padding(top = spacing.small),
            ) {
                OutlinedTextField(
                    value = draft,
                    onValueChange = { draft = it },
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
                    modifier = Modifier.weight(1f),
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
}

/** label → wire key name the relay understands (oracle spellings). */
private val TERMINAL_KEYS = listOf(
    "Enter" to "Enter",
    "Tab" to "Tab",
    "Esc" to "Escape",
    "Ctrl-C" to "Ctrl+C",
)

@PreviewLightDark
@Composable
private fun TerminalInputBarPreview() {
    LerdrTheme {
        TerminalInputBar(onSendText = {}, onSendKeys = {})
    }
}
