package com.lerdr.app.settings

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.selection.toggleable
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.RecordVoiceOver
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExposedDropdownMenuBox
import androidx.compose.material3.ExposedDropdownMenuDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ExposedDropdownMenuAnchorType
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontWeight
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lerdr.app.speech.SPEECH_LANGUAGES
import com.lerdr.app.speech.SpeechEntryPoint
import com.lerdr.app.speech.SpeechPhase
import com.lerdr.app.speech.speechLanguageLabel
import com.lerdr.core.designsystem.components.liveRegionPolite
import com.lerdr.core.designsystem.theme.LerdrTheme
import dagger.hilt.android.EntryPointAccessors

/**
 * Speech settings for one relay (docs/04 §Settings; the oracle's
 * `SettingsView` §Speech) — the read-aloud toggle, language picker, a
 * Speak test, and the per-relay voice catalog when the relay advertises
 * `speech_voice_management`.
 *
 * Self-contained: dependencies resolve through [SpeechEntryPoint] and the
 * section owns its [SpeechViewModel] keyed by [relayId], so the
 * orchestrator drops it into the Settings list as one item.
 */
@Composable
fun SpeechSection(relayId: String, modifier: Modifier = Modifier) {
    val context = LocalContext.current
    val appContext = context.applicationContext
    val entryPoint = remember(appContext) {
        EntryPointAccessors.fromApplication(appContext, SpeechEntryPoint::class.java)
    }
    val viewModel: SpeechViewModel = viewModel(key = "speech-$relayId") {
        SpeechViewModel(
            relayId,
            entryPoint.sessionRepository(),
            entryPoint.speechPreferences(),
            entryPoint.relaySpeechPlayer(),
        )
    }
    val uiState by viewModel.uiState.collectAsStateWithLifecycle()

    SpeechSectionContent(
        uiState = uiState,
        onEnabledChange = viewModel::setEnabled,
        onLanguageChange = viewModel::setLanguage,
        onSpeakTest = viewModel::toggleSpeakTest,
        onInstallVoice = viewModel::installVoice,
        onRemoveVoice = viewModel::removeVoice,
        onDismissError = viewModel::dismissError,
        modifier = modifier,
    )
}

/**
 * Stateless section body — screenshot tests drive it with fixed states.
 */
@Composable
fun SpeechSectionContent(
    uiState: SpeechUiState,
    onEnabledChange: (Boolean) -> Unit,
    onLanguageChange: (String) -> Unit,
    onSpeakTest: () -> Unit,
    onInstallVoice: (String) -> Unit,
    onRemoveVoice: (String) -> Unit,
    onDismissError: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val spacing = LerdrTheme.spacing
    Column(modifier = modifier.fillMaxWidth()) {
        SpeechSectionHeader()

        // Row-owned toggle: Role.Switch announces "on/off" once and the
        // whole row is the touch target; the Switch renders state only.
        ListItem(
            modifier = Modifier.toggleable(
                value = uiState.enabled,
                role = Role.Switch,
                onValueChange = onEnabledChange,
            ),
            headlineContent = { Text("Read responses aloud") },
            supportingContent = {
                Text(
                    "Enabled automatically the first time a connected relay " +
                        "offers a compatible voice; after that, this setting " +
                        "stays under your control. The relay synthesizes each " +
                        "response with its own voice and streams the audio " +
                        "here encrypted, so reading continues while the " +
                        "screen is off.",
                )
            },
            leadingContent = {
                Icon(
                    Icons.Default.RecordVoiceOver,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            },
            trailingContent = {
                Switch(checked = uiState.enabled, onCheckedChange = null)
            },
        )

        LanguagePicker(
            enabled = uiState.enabled,
            selected = uiState.language,
            onSelect = onLanguageChange,
            modifier = Modifier.padding(horizontal = spacing.medium),
        )

        if (uiState.speaking) {
            SpeechHint(
                text = "Reading aloud…",
                modifier = Modifier.padding(horizontal = spacing.medium),
            )
        }
        if (uiState.phase == SpeechPhase.ERROR) {
            SpeechHint(
                text = "Reading aloud failed. Check the relay's voice below, " +
                    "then try again.",
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.padding(horizontal = spacing.medium),
            )
        }
        if (uiState.enabled && uiState.connected && !uiState.languageSpeakable) {
            SpeechHint(
                text = "No ${speechLanguageLabel(uiState.language)} voice on " +
                    "${uiState.relayLabel}. Download it below, or install a " +
                    "system speech engine on that computer.",
                modifier = Modifier.padding(horizontal = spacing.medium),
            )
        }
        uiState.lastError?.let { error ->
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .liveRegionPolite()
                    .padding(horizontal = spacing.medium),
            ) {
                Text(
                    error,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier.weight(1f),
                )
                TextButton(onClick = onDismissError) { Text("Dismiss") }
            }
        }

        Row(
            modifier = Modifier.padding(horizontal = spacing.medium),
            horizontalArrangement = Arrangement.spacedBy(spacing.small),
        ) {
            TextButton(
                onClick = onSpeakTest,
                enabled = uiState.speaking || uiState.canSpeakTest,
            ) {
                Text(if (uiState.speaking) "Stop reading" else "Speak test")
            }
        }

        if (uiState.showCatalog) {
            VoiceCatalogCard(
                relayLabel = uiState.relayLabel,
                catalog = uiState.catalog ?: SpeechCatalogUi(),
                onInstallVoice = onInstallVoice,
                onRemoveVoice = onRemoveVoice,
                modifier = Modifier.padding(horizontal = spacing.medium),
            )
        }
    }
}

/** Same header treatment as Settings' `SectionHeader` (private there). */
@Composable
private fun SpeechSectionHeader() {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = LerdrTheme.spacing.medium)
            .padding(top = LerdrTheme.spacing.small),
    ) {
        Text(
            "SPEECH",
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            fontWeight = FontWeight.Bold,
            modifier = Modifier.weight(1f),
        )
    }
}

/** `speech-language` select — all five offered languages, oracle parity. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun LanguagePicker(
    enabled: Boolean,
    selected: String,
    onSelect: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    var expanded by remember { mutableStateOf(false) }
    Box(modifier = modifier) {
        ExposedDropdownMenuBox(
            expanded = expanded && enabled,
            onExpandedChange = { if (enabled) expanded = it },
        ) {
            OutlinedTextField(
                value = speechLanguageLabel(selected),
                onValueChange = {},
                readOnly = true,
                enabled = enabled,
                singleLine = true,
                label = { Text("Language") },
                trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded) },
                modifier = Modifier
                    .menuAnchor(ExposedDropdownMenuAnchorType.PrimaryNotEditable, enabled)
                    .fillMaxWidth(),
            )
            ExposedDropdownMenu(
                expanded = expanded && enabled,
                onDismissRequest = { expanded = false },
            ) {
                SPEECH_LANGUAGES.forEach { language ->
                    DropdownMenuItem(
                        text = { Text(language.label) },
                        onClick = {
                            expanded = false
                            onSelect(language.code)
                        },
                    )
                }
            }
        }
    }
}

/**
 * Small status/warning line under a control — the oracle's `p.hint`.
 * Polite live region: appearing/changing hints announce to screen readers.
 */
@Composable
private fun SpeechHint(
    text: String,
    modifier: Modifier = Modifier,
    color: androidx.compose.ui.graphics.Color =
        MaterialTheme.colorScheme.onSurfaceVariant,
) {
    Text(
        text,
        style = MaterialTheme.typography.bodySmall,
        color = color,
        modifier = modifier
            .liveRegionPolite()
            .padding(vertical = LerdrTheme.spacing.extraSmall),
    )
}

/**
 * `speechVoiceRelays` card — every offered language with its install state
 * and a Download/Remove action; the engine-absent notice rides on top.
 */
@Composable
private fun VoiceCatalogCard(
    relayLabel: String,
    catalog: SpeechCatalogUi,
    onInstallVoice: (String) -> Unit,
    onRemoveVoice: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val spacing = LerdrTheme.spacing
    Card(
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.surfaceContainerLow,
        ),
        shape = MaterialTheme.shapes.medium,
        modifier = modifier.fillMaxWidth(),
    ) {
        Column(modifier = Modifier.padding(spacing.medium)) {
            Text(
                "Voices on $relayLabel" +
                    if (catalog.cacheDir.isNotEmpty()) {
                        ", cached in ${catalog.cacheDir}"
                    } else {
                        ""
                    },
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (!catalog.engineInstalled) {
                Spacer(Modifier.height(spacing.extraSmall))
                Text(
                    "The speech engine is not installed on $relayLabel yet. " +
                        "The first download installs it too.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Spacer(Modifier.height(spacing.small))
            catalog.rows.forEach { row ->
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Column(Modifier.weight(1f)) {
                        Text(row.label, style = MaterialTheme.typography.titleSmall)
                        Text(
                            row.stateLabel,
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    Spacer(Modifier.width(spacing.small))
                    if (row.installed) {
                        TextButton(
                            onClick = { onRemoveVoice(row.language) },
                            enabled = !row.busy,
                        ) {
                            Text(
                                if (row.busy) "Removing…" else "Remove",
                                color = MaterialTheme.colorScheme.error,
                            )
                        }
                    } else {
                        TextButton(
                            onClick = { onInstallVoice(row.language) },
                            enabled = !row.busy,
                        ) {
                            Text(if (row.busy) "Downloading…" else "Download")
                        }
                    }
                }
            }
        }
    }
}
