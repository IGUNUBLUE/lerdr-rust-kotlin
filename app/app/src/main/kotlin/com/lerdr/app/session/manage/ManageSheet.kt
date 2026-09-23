package com.lerdr.app.session.manage

import android.content.ClipData
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Check
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.platform.ClipEntry
import androidx.compose.ui.platform.LocalClipboard
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lerdr.app.session.WorktreesEntryPoint
import com.lerdr.core.designsystem.theme.LerdrTextStyles
import com.lerdr.core.designsystem.theme.LerdrTheme
import dagger.hilt.android.EntryPointAccessors

/** Test tags for the manage sheet — see `ManageSheetScreenshotTest`. */
object ManageSheetTags {
    const val CONTENT = "manage-sheet:content"
    const val NAME_FIELD = "manage-sheet:name-field"
    const val SAVE_NAME = "manage-sheet:save-name"
    const val COPY_RESPONSE = "manage-sheet:copy-response"
    const val RESTART = "manage-sheet:restart"
    const val CLEAR = "manage-sheet:clear"
    const val STOP = "manage-sheet:stop"
    const val CONFIRM = "manage-sheet:confirm"
    const val CANCEL_CONFIRM = "manage-sheet:cancel-confirm"
    const val STATUS = "manage-sheet:status"
    const val META = "manage-sheet:meta"
    const val READER_NOTE = "manage-sheet:reader-note"
}

/**
 * The session ⋯ surface — a modal bottom sheet mirroring the oracle's
 * `ManageDialog`: rename (`agent_rename`), restart (`agent_restart`),
 * clear (`agent_clear`), stop (`agent_stop`), copy the last response
 * (`copy_agent_response`), plus a metadata block (pane id, cwd, workspace,
 * agent identity, relay label).
 *
 * Reader devices get the metadata block only — every mutation affordance is
 * hidden, matching `docs/04-app-design.md` (reader mode hides mutations
 * entirely).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ManageSheet(
    paneId: String,
    onDismiss: () -> Unit,
) {
    val app = LocalContext.current.applicationContext
    val viewModel: ManageViewModel = viewModel(key = "manage:$paneId") {
        val entryPoint = EntryPointAccessors.fromApplication(
            app,
            WorktreesEntryPoint::class.java,
        )
        ManageViewModel(
            paneId = paneId,
            sessions = entryPoint.sessionRepository(),
            workspaces = entryPoint.workspaceStore(),
        )
    }
    val uiState by viewModel.uiState.collectAsStateWithLifecycle()

    LaunchedEffect(uiState.shouldDismiss) {
        if (uiState.shouldDismiss) onDismiss()
    }
    val clipboard = LocalClipboard.current
    LaunchedEffect(uiState.clipboardText) {
        val text = uiState.clipboardText ?: return@LaunchedEffect
        clipboard.setClipEntry(
            ClipEntry(ClipData.newPlainText("Agent response", text)),
        )
        viewModel.consumeClipboard()
    }

    ModalBottomSheet(onDismissRequest = onDismiss) {
        ManageSheetContent(
            uiState = uiState,
            onNameDraftChange = viewModel::onNameDraftChange,
            onSaveName = viewModel::saveRename,
            onCopyResponse = viewModel::copyResponse,
            onRestart = viewModel::restart,
            onBeginConfirm = viewModel::beginConfirm,
            onCancelConfirm = viewModel::cancelConfirm,
            onConfirmAction = viewModel::confirmAction,
        )
    }
}

/** Stateless sheet body — the piece screenshot tests render directly. */
@Composable
fun ManageSheetContent(
    uiState: ManageUiState,
    onNameDraftChange: (String) -> Unit,
    onSaveName: () -> Unit,
    onCopyResponse: () -> Unit,
    onRestart: () -> Unit,
    onBeginConfirm: (ManageConfirm) -> Unit,
    onCancelConfirm: () -> Unit,
    onConfirmAction: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = LerdrTheme.extendedColors
    Column(
        modifier = modifier
            .fillMaxWidth()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = LerdrTheme.spacing.large)
            .padding(bottom = LerdrTheme.spacing.extraLarge)
            .testTag(ManageSheetTags.CONTENT),
        verticalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.medium),
    ) {
        Column(verticalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.extraSmall)) {
            Text("Manage agent", style = MaterialTheme.typography.titleLarge)
            Text(
                text = listOf(uiState.title, uiState.relayLabel)
                    .filter { it.isNotBlank() }
                    .joinToString(" · "),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }

        when {
            uiState.confirming != null -> ManageConfirmPanel(
                confirming = uiState.confirming,
                busy = uiState.busy,
                onConfirm = onConfirmAction,
                onCancel = onCancelConfirm,
            )
            uiState.canControl -> ManageActions(
                uiState = uiState,
                onNameDraftChange = onNameDraftChange,
                onSaveName = onSaveName,
                onCopyResponse = onCopyResponse,
                onRestart = onRestart,
                onBeginConfirm = onBeginConfirm,
            )
            else -> Text(
                text = "This device is paired as a reader — session actions " +
                    "are unavailable.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(ManageSheetTags.READER_NOTE),
            )
        }

        HorizontalDivider()
        ManageMetadata(uiState)

        uiState.status?.let { status ->
            Text(
                text = status,
                style = MaterialTheme.typography.bodySmall,
                color = if (uiState.statusError) colors.danger else colors.idle,
                modifier = Modifier
                    .semantics { liveRegion = LiveRegionMode.Polite }
                    .testTag(ManageSheetTags.STATUS),
            )
        }
    }
}

/** Rename field + the action list — controller-only per `docs/04`. */
@Composable
private fun ManageActions(
    uiState: ManageUiState,
    onNameDraftChange: (String) -> Unit,
    onSaveName: () -> Unit,
    onCopyResponse: () -> Unit,
    onRestart: () -> Unit,
    onBeginConfirm: (ManageConfirm) -> Unit,
) {
    val colors = LerdrTheme.extendedColors
    Column(verticalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.small)) {
        Text(
            text = "Session name",
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.extraSmall),
        ) {
            OutlinedTextField(
                value = uiState.nameDraft,
                onValueChange = onNameDraftChange,
                modifier = Modifier
                    .weight(1f)
                    .testTag(ManageSheetTags.NAME_FIELD),
                textStyle = MaterialTheme.typography.bodyLarge,
                singleLine = true,
                enabled = !uiState.busy,
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
                keyboardActions = KeyboardActions(
                    onDone = { onSaveName() },
                ),
            )
            IconButton(
                onClick = onSaveName,
                enabled = !uiState.busy && uiState.nameDirty,
                modifier = Modifier.testTag(ManageSheetTags.SAVE_NAME),
            ) {
                Icon(Icons.Default.Check, contentDescription = "Save name")
            }
        }

        Spacer(Modifier.height(LerdrTheme.spacing.extraSmall))

        FilledTonalButton(
            onClick = onCopyResponse,
            enabled = !uiState.busy,
            modifier = Modifier
                .fillMaxWidth()
                .testTag(ManageSheetTags.COPY_RESPONSE),
        ) {
            Text("Copy last response")
        }
        FilledTonalButton(
            onClick = onRestart,
            enabled = !uiState.busy,
            modifier = Modifier
                .fillMaxWidth()
                .testTag(ManageSheetTags.RESTART),
        ) {
            Text("Restart agent")
        }
        OutlinedButton(
            onClick = { onBeginConfirm(ManageConfirm.CLEAR) },
            enabled = !uiState.busy,
            modifier = Modifier
                .fillMaxWidth()
                .testTag(ManageSheetTags.CLEAR),
        ) {
            Text("Clear transcript")
        }
        Button(
            onClick = { onBeginConfirm(ManageConfirm.STOP) },
            enabled = !uiState.busy,
            colors = ButtonDefaults.buttonColors(
                containerColor = colors.danger,
                contentColor = colors.onDanger,
            ),
            modifier = Modifier
                .fillMaxWidth()
                .testTag(ManageSheetTags.STOP),
        ) {
            Text("Stop agent")
        }
    }
}

/**
 * The oracle's confirm step — the panel *replaces* the action list so the
 * destructive tap can never be an accident. Focus starts on Cancel.
 */
@Composable
private fun ManageConfirmPanel(
    confirming: ManageConfirm,
    busy: Boolean,
    onConfirm: () -> Unit,
    onCancel: () -> Unit,
) {
    val colors = LerdrTheme.extendedColors
    val cancelFocus = remember { FocusRequester() }
    LaunchedEffect(confirming) { cancelFocus.requestFocus() }
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .semantics { liveRegion = LiveRegionMode.Assertive }
            .selectableGroup(),
        verticalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.small),
    ) {
        Text(
            text = when (confirming) {
                ManageConfirm.CLEAR ->
                    "Clear this agent's transcript and restart it fresh? " +
                        "The conversation history is discarded."
                ManageConfirm.STOP ->
                    "Stop this agent? Its process exits and the workspace " +
                        "slot is released."
            },
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurface,
        )
        Row(horizontalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.small)) {
            OutlinedButton(
                onClick = onCancel,
                enabled = !busy,
                modifier = Modifier
                    .weight(1f)
                    .focusRequester(cancelFocus)
                    .testTag(ManageSheetTags.CANCEL_CONFIRM),
            ) {
                Text("Cancel")
            }
            val confirmLabel = when (confirming) {
                ManageConfirm.CLEAR -> "Confirm clear"
                ManageConfirm.STOP -> "Confirm stop"
            }
            val confirmColors = when (confirming) {
                ManageConfirm.CLEAR -> ButtonDefaults.buttonColors()
                ManageConfirm.STOP -> ButtonDefaults.buttonColors(
                    containerColor = colors.danger,
                    contentColor = colors.onDanger,
                )
            }
            Button(
                onClick = onConfirm,
                enabled = !busy,
                colors = confirmColors,
                modifier = Modifier
                    .weight(1f)
                    .testTag(ManageSheetTags.CONFIRM),
            ) {
                Text(confirmLabel)
            }
        }
    }
}

/** Pane id / cwd / workspace / agent identity / relay — selectable. */
@Composable
private fun ManageMetadata(uiState: ManageUiState) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .testTag(ManageSheetTags.META),
        verticalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.extraSmall),
    ) {
        Text(
            text = "Details",
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        SelectionContainer {
            Column(verticalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.extraSmall)) {
                MetadataRow("Pane", uiState.rawPaneId.ifBlank { uiState.paneId })
                MetadataRow("Directory", uiState.cwd)
                MetadataRow("Workspace", uiState.workspaceLabel)
                MetadataRow(
                    "Agent",
                    listOfNotNull(
                        uiState.provider,
                        uiState.sessionName.takeIf { it.isNotBlank() },
                    ).joinToString(" · ").ifBlank { "—" },
                )
                MetadataRow("Computer", uiState.relayLabel)
                MetadataRow(
                    "Role",
                    if (uiState.canControl) "Controller" else "Reader",
                )
            }
        }
    }
}

@Composable
private fun MetadataRow(label: String, value: String) {
    if (value.isBlank()) return
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.small),
    ) {
        Text(
            text = label,
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(top = 2.dp),
        )
        Text(
            text = value,
            style = LerdrTextStyles.code,
            color = MaterialTheme.colorScheme.onSurface,
        )
    }
}

@Preview(showBackground = true)
@Composable
private fun ManageSheetContentPreview() {
    LerdrTheme {
        ManageSheetContent(
            uiState = ManageUiState(
                paneId = "relay::%12",
                title = "lerdr",
                provider = "claude",
                relayLabel = "workstation",
                rawPaneId = "%12",
                cwd = "/home/u/Projects/lerdr",
                workspaceLabel = "lerdr",
                sessionName = "Fix the login bug",
                canControl = true,
                nameDraft = "lerdr",
            ),
            onNameDraftChange = {},
            onSaveName = {},
            onCopyResponse = {},
            onRestart = {},
            onBeginConfirm = {},
            onCancelConfirm = {},
            onConfirmAction = {},
        )
    }
}

@Preview(showBackground = true)
@Composable
private fun ManageSheetContentReaderPreview() {
    LerdrTheme {
        ManageSheetContent(
            uiState = ManageUiState(
                paneId = "relay::%12",
                title = "lerdr",
                provider = "claude",
                relayLabel = "workstation",
                rawPaneId = "%12",
                cwd = "/home/u/Projects/lerdr",
                workspaceLabel = "lerdr",
                canControl = false,
            ),
            onNameDraftChange = {},
            onSaveName = {},
            onCopyResponse = {},
            onRestart = {},
            onBeginConfirm = {},
            onCancelConfirm = {},
            onConfirmAction = {},
        )
    }
}
