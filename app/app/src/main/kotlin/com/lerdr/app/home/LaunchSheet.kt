package com.lerdr.app.home

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ArrowUpward
import androidx.compose.material.icons.filled.Folder
import androidx.compose.material.icons.filled.FolderOpen
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExposedDropdownMenuAnchorType
import androidx.compose.material3.ExposedDropdownMenuBox
import androidx.compose.material3.ExposedDropdownMenuDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.tooling.preview.PreviewLightDark
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.lerdr.app.session.DirectoryEntry
import com.lerdr.app.session.DirectoryListing
import com.lerdr.app.session.SessionRepository
import com.lerdr.core.designsystem.components.LerdrLoadingIndicator
import com.lerdr.core.designsystem.theme.LerdrTheme
import dagger.hilt.EntryPoint
import dagger.hilt.InstallIn
import dagger.hilt.components.SingletonComponent
import lerdr.core.model.AgentProfile
import lerdr.core.store.WorkspaceStore

/**
 * Singleton seam for the launch sheets — mirrors `WorktreesEntryPoint`
 * (AppEntryPoint does not expose the workspace store).
 */
@EntryPoint
@InstallIn(SingletonComponent::class)
interface LaunchEntryPoint {
    fun sessionRepository(): SessionRepository
    fun workspaceStore(): WorkspaceStore
}

/**
 * "New agent" sheet — the oracle's `LaunchView` (`agent_start`): relay and
 * profile pickers, the directory-browser cwd field, a suggested name, an
 * optional first prompt, and the workspace target when the relay has
 * workspaces. Submit is gated by `canControl` — the read-only warning text
 * is the oracle's verbatim.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun NewAgentSheet(
    viewModel: LaunchViewModel,
    onDismiss: () -> Unit,
) {
    val uiState by viewModel.uiState.collectAsStateWithLifecycle()
    ModalBottomSheet(onDismissRequest = onDismiss) {
        NewAgentSheetContent(
            uiState = uiState,
            onRelaySelect = viewModel::selectRelay,
            onProfileSelect = viewModel::selectProfile,
            onWorkspaceSelect = viewModel::selectWorkspace,
            onNameChange = viewModel::onNameChange,
            onPromptChange = viewModel::onPromptChange,
            onBrowseDirectories = viewModel::openDirectoryBrowser,
            onSubmit = viewModel::submitAgent,
        )
    }
    DirectoryBrowserDialog(
        directory = uiState.directory,
        onBrowse = viewModel::loadDirectory,
        onDismiss = viewModel::closeDirectoryBrowser,
    )
}

/** Stateless form body — previews and Roborazzi shots render this tree. */
@Composable
fun NewAgentSheetContent(
    uiState: LaunchUiState,
    onRelaySelect: (String) -> Unit,
    onProfileSelect: (String) -> Unit,
    onWorkspaceSelect: (String) -> Unit,
    onNameChange: (String) -> Unit,
    onPromptChange: (String) -> Unit,
    onBrowseDirectories: () -> Unit,
    onSubmit: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val spacing = LerdrTheme.spacing
    Column(
        modifier = modifier
            .fillMaxWidth()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = spacing.large)
            .padding(bottom = spacing.large),
        verticalArrangement = Arrangement.spacedBy(spacing.small),
    ) {
        Text("Start Agent", style = MaterialTheme.typography.titleLarge)
        Spacer(Modifier.height(spacing.extraSmall))

        LaunchFieldLabel("Computer")
        RelayPicker(
            relays = uiState.relays,
            selected = uiState.relayId,
            enabled = !uiState.submitting,
            onSelect = onRelaySelect,
        )
        LaunchWarnings(uiState)

        LaunchFieldLabel("Agent")
        ProfilePicker(
            profiles = uiState.profiles,
            selected = uiState.profileId,
            enabled = !uiState.submitting,
            onSelect = onProfileSelect,
        )

        if (uiState.workspaces.isNotEmpty()) {
            LaunchFieldLabel("Workspace")
            WorkspacePicker(
                workspaces = uiState.workspaces,
                selected = uiState.workspaceId,
                enabled = !uiState.submitting,
                onSelect = onWorkspaceSelect,
            )
            if (uiState.workspaceTargetLabel.isNotEmpty()) {
                // The oracle's hint — "New tab in workspace <b>X</b>. …"
                Text(
                    buildAnnotatedString {
                        append("New tab in workspace ")
                        withStyle(SpanStyle(fontWeight = FontWeight.Bold)) {
                            append(uiState.workspaceTargetLabel)
                        }
                        append(". The desktop keeps its current focus.")
                    },
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }

        LaunchFieldLabel("Working Directory")
        DirectoryField(
            label = uiState.cwdLabel.ifEmpty { uiState.cwd },
            loading = uiState.directory.loading,
            supported = uiState.directory.supported,
            enabled = !uiState.submitting,
            onClick = onBrowseDirectories,
        )
        LaunchHint("The folder shown above is selected — tap it to browse.")

        LaunchFieldLabel("Name")
        OutlinedTextField(
            value = uiState.name,
            onValueChange = onNameChange,
            enabled = !uiState.submitting,
            singleLine = true,
            isError = uiState.name.isNotEmpty() && !validAgentName(uiState.name),
            placeholder = { Text("project-codex") },
            supportingText = {
                if (uiState.name.isNotEmpty() && !validAgentName(uiState.name)) {
                    Text(
                        "Start with a lowercase letter; use lowercase letters, " +
                            "numbers, underscores, or dashes.",
                    )
                }
            },
            modifier = Modifier.fillMaxWidth(),
        )

        LaunchFieldLabel("Initial task (optional)")
        OutlinedTextField(
            value = uiState.prompt,
            onValueChange = onPromptChange,
            enabled = !uiState.submitting,
            minLines = 2,
            maxLines = 4,
            placeholder = { Text("Describe the task to start…") },
            modifier = Modifier.fillMaxWidth(),
        )
        LaunchHint("Sent to the agent as its first prompt after it starts.")

        Spacer(Modifier.height(spacing.extraSmall))
        val canSubmit = !uiState.submitting && !uiState.readOnly &&
            !uiState.directory.loading && uiState.directoryReady &&
            uiState.relayId.isNotEmpty() && uiState.profileId.isNotEmpty() &&
            uiState.cwd.isNotEmpty() && validAgentName(uiState.name)
        Button(
            onClick = onSubmit,
            enabled = canSubmit,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text(if (uiState.submitting) "Starting agent…" else "Start Agent")
        }
        LaunchStatus(uiState)
    }
}

/**
 * "New workspace" sheet — the oracle's WorkspaceManager create dialog
 * (`workspace_create`): relay picker, directory browser, label.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun NewWorkspaceSheet(
    viewModel: LaunchViewModel,
    onDismiss: () -> Unit,
) {
    val uiState by viewModel.uiState.collectAsStateWithLifecycle()
    ModalBottomSheet(onDismissRequest = onDismiss) {
        NewWorkspaceSheetContent(
            uiState = uiState,
            onRelaySelect = viewModel::selectRelay,
            onLabelChange = viewModel::onWorkspaceLabelChange,
            onBrowseDirectories = viewModel::openDirectoryBrowser,
            onSubmit = viewModel::submitWorkspace,
        )
    }
    DirectoryBrowserDialog(
        directory = uiState.directory,
        onBrowse = viewModel::loadDirectory,
        onDismiss = viewModel::closeDirectoryBrowser,
    )
}

/** Stateless workspace form body. */
@Composable
fun NewWorkspaceSheetContent(
    uiState: LaunchUiState,
    onRelaySelect: (String) -> Unit,
    onLabelChange: (String) -> Unit,
    onBrowseDirectories: () -> Unit,
    onSubmit: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val spacing = LerdrTheme.spacing
    Column(
        modifier = modifier
            .fillMaxWidth()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = spacing.large)
            .padding(bottom = spacing.large),
        verticalArrangement = Arrangement.spacedBy(spacing.small),
    ) {
        Text("New Workspace", style = MaterialTheme.typography.titleLarge)
        Spacer(Modifier.height(spacing.extraSmall))

        LaunchFieldLabel("Computer")
        RelayPicker(
            relays = uiState.relays,
            selected = uiState.relayId,
            enabled = !uiState.submitting,
            onSelect = onRelaySelect,
        )
        LaunchWarnings(uiState)

        LaunchFieldLabel("Working Directory")
        DirectoryField(
            label = uiState.cwdLabel.ifEmpty { uiState.cwd },
            loading = uiState.directory.loading,
            supported = uiState.directory.supported,
            enabled = !uiState.submitting,
            onClick = onBrowseDirectories,
        )
        LaunchHint("The folder shown above is selected — tap it to browse.")

        LaunchFieldLabel("Label")
        OutlinedTextField(
            value = uiState.workspaceLabel,
            onValueChange = onLabelChange,
            enabled = !uiState.submitting,
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )

        Spacer(Modifier.height(spacing.extraSmall))
        val canSubmit = !uiState.submitting && !uiState.readOnly &&
            uiState.relayId.isNotEmpty() && uiState.cwd.isNotEmpty() &&
            uiState.workspaceLabel.isNotBlank()
        Button(
            onClick = onSubmit,
            enabled = canSubmit,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text(if (uiState.submitting) "Creating…" else "Create Workspace")
        }
        LaunchStatus(uiState)
    }
}

/**
 * The `list_directories` browser as a dialog — oracle parity: current
 * folder header, a "Parent folder" row, directory rows, plus the loading /
 * error / capability-missing / empty states.
 */
@Composable
fun DirectoryBrowserDialog(
    directory: DirectoryBrowserUi,
    onBrowse: (String) -> Unit,
    onDismiss: () -> Unit,
) {
    if (!directory.open) return
    val listing = directory.listing
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Working Directory") },
        text = {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .heightIn(max = 360.dp)
                    .verticalScroll(rememberScrollState()),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Icon(
                        Icons.Default.FolderOpen,
                        contentDescription = null,
                        tint = MaterialTheme.colorScheme.primary,
                        modifier = Modifier.size(20.dp),
                    )
                    Spacer(Modifier.width(LerdrTheme.spacing.small))
                    Column(Modifier.weight(1f)) {
                        Text(
                            listing?.currentLabel?.ifEmpty { null }
                                ?: listing?.currentPath
                                ?: "…",
                            style = MaterialTheme.typography.titleSmall,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                        listing?.currentPath?.takeIf { it.isNotEmpty() }?.let {
                            Text(
                                it,
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                        }
                    }
                }
                Spacer(Modifier.height(LerdrTheme.spacing.small))
                when {
                    !directory.supported -> Text(
                        "Update and restart this computer's relay to browse directories.",
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    directory.loading -> Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier.padding(vertical = LerdrTheme.spacing.small),
                    ) {
                        LerdrLoadingIndicator(modifier = Modifier.size(20.dp))
                        Spacer(Modifier.width(LerdrTheme.spacing.small))
                        Text(
                            "Loading folders…",
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    directory.error != null -> Text(
                        directory.error,
                        style = MaterialTheme.typography.bodyMedium,
                        color = LerdrTheme.extendedColors.danger,
                    )
                    else -> {
                        if (!listing?.parent.isNullOrEmpty()) {
                            DirectoryRow(
                                icon = {
                                    Icon(
                                        Icons.Default.ArrowUpward,
                                        contentDescription = null,
                                        tint = MaterialTheme.colorScheme.onSurfaceVariant,
                                        modifier = Modifier.size(18.dp),
                                    )
                                },
                                label = "Parent folder",
                                onClick = { onBrowse(listing!!.parent) },
                            )
                        }
                        listing?.directories?.forEach { entry ->
                            DirectoryRow(
                                icon = {
                                    Icon(
                                        Icons.Default.Folder,
                                        contentDescription = null,
                                        tint = MaterialTheme.colorScheme.onSurfaceVariant,
                                        modifier = Modifier.size(18.dp),
                                    )
                                },
                                label = entry.name,
                                onClick = { onBrowse(entry.path) },
                            )
                        }
                        if (listing != null && listing.directories.isEmpty()) {
                            Text(
                                "This folder has no subdirectories. It remains selected.",
                                style = MaterialTheme.typography.bodyMedium,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                modifier = Modifier.padding(
                                    vertical = LerdrTheme.spacing.small,
                                ),
                            )
                        }
                    }
                }
            }
        },
        confirmButton = {
            TextButton(onClick = onDismiss) { Text("Done") }
        },
    )
}

@Composable
private fun DirectoryRow(
    icon: @Composable () -> Unit,
    label: String,
    onClick: () -> Unit,
) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = 40.dp)
            .clickable(role = Role.Button, onClick = onClick)
            .padding(vertical = LerdrTheme.spacing.extraSmall),
    ) {
        icon()
        Spacer(Modifier.width(LerdrTheme.spacing.small))
        Text(
            label,
            style = MaterialTheme.typography.bodyMedium,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

/** Small form-section caption — the oracle's `<label>`/`.field-label`. */
@Composable
private fun LaunchFieldLabel(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.labelMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

/** The oracle's `p.hint` — quiet supporting text under a field. */
@Composable
private fun LaunchHint(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

/** Warning lines under the relay picker — oracle `p.warning` parity. */
@Composable
private fun LaunchWarnings(uiState: LaunchUiState) {
    val warningColor = MaterialTheme.colorScheme.error
    if (uiState.unavailableRelayLabels.isNotEmpty()) {
        Text(
            "Agent inventory is unavailable on " +
                uiState.unavailableRelayLabels.joinToString(", ") + ".",
            style = MaterialTheme.typography.bodySmall,
            color = warningColor,
        )
    }
    if (uiState.readOnly) {
        Text(
            "This paired device has read-only access to the selected relay.",
            style = MaterialTheme.typography.bodySmall,
            color = warningColor,
        )
    }
}

/** The oracle's `p.form-status` — post-submit status line. */
@Composable
private fun LaunchStatus(uiState: LaunchUiState) {
    uiState.status?.let {
        Text(
            it,
            style = MaterialTheme.typography.bodySmall,
            color = if (uiState.statusError) {
                MaterialTheme.colorScheme.error
            } else {
                MaterialTheme.colorScheme.onSurfaceVariant
            },
        )
    }
}

/** "Computer" picker — connected + inventory-ready relays only. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun RelayPicker(
    relays: List<LaunchRelayOption>,
    selected: String,
    enabled: Boolean,
    onSelect: (String) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    ExposedDropdownMenuBox(
        expanded = expanded && enabled,
        onExpandedChange = { if (enabled) expanded = it },
    ) {
        OutlinedTextField(
            value = relays.firstOrNull { it.id == selected }?.label
                ?: if (relays.isEmpty()) "No ready relays" else "",
            onValueChange = {},
            readOnly = true,
            enabled = enabled,
            singleLine = true,
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded) },
            modifier = Modifier
                .menuAnchor(ExposedDropdownMenuAnchorType.PrimaryNotEditable, enabled)
                .fillMaxWidth(),
        )
        ExposedDropdownMenu(
            expanded = expanded && enabled,
            onDismissRequest = { expanded = false },
        ) {
            relays.forEach { relay ->
                DropdownMenuItem(
                    text = { Text(relay.label) },
                    onClick = {
                        expanded = false
                        onSelect(relay.id)
                    },
                )
            }
        }
    }
}

/** "Agent" picker — `connection.agentProfiles` rows (label, else id). */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ProfilePicker(
    profiles: List<AgentProfile>,
    selected: String,
    enabled: Boolean,
    onSelect: (String) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    ExposedDropdownMenuBox(
        expanded = expanded && enabled,
        onExpandedChange = { if (enabled) expanded = it },
    ) {
        OutlinedTextField(
            value = profiles.firstOrNull { it.id == selected }
                ?.let { it.label ?: it.id.orEmpty() }
                ?: if (profiles.isEmpty()) "No agent profiles available" else "",
            onValueChange = {},
            readOnly = true,
            enabled = enabled,
            singleLine = true,
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded) },
            modifier = Modifier
                .menuAnchor(ExposedDropdownMenuAnchorType.PrimaryNotEditable, enabled)
                .fillMaxWidth(),
        )
        ExposedDropdownMenu(
            expanded = expanded && enabled,
            onDismissRequest = { expanded = false },
        ) {
            profiles.forEach { profile ->
                DropdownMenuItem(
                    text = { Text(profile.label ?: profile.id.orEmpty()) },
                    onClick = {
                        expanded = false
                        onSelect(profile.id.orEmpty())
                    },
                )
            }
        }
    }
}

/** "Workspace" picker — first entry is the default "New workspace". */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun WorkspacePicker(
    workspaces: List<LaunchWorkspaceOption>,
    selected: String,
    enabled: Boolean,
    onSelect: (String) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    ExposedDropdownMenuBox(
        expanded = expanded && enabled,
        onExpandedChange = { if (enabled) expanded = it },
    ) {
        OutlinedTextField(
            value = workspaces.firstOrNull { it.id == selected }?.label
                ?: "New workspace",
            onValueChange = {},
            readOnly = true,
            enabled = enabled,
            singleLine = true,
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded) },
            modifier = Modifier
                .menuAnchor(ExposedDropdownMenuAnchorType.PrimaryNotEditable, enabled)
                .fillMaxWidth(),
        )
        ExposedDropdownMenu(
            expanded = expanded && enabled,
            onDismissRequest = { expanded = false },
        ) {
            DropdownMenuItem(
                text = { Text("New workspace") },
                onClick = {
                    expanded = false
                    onSelect("")
                },
            )
            workspaces.forEach { workspace ->
                DropdownMenuItem(
                    text = { Text(workspace.label) },
                    onClick = {
                        expanded = false
                        onSelect(workspace.id)
                    },
                )
            }
        }
    }
}

/**
 * The cwd field — oracle parity is a button, not free text: the selected
 * folder is always the last listing's `current`, so tapping opens the
 * browser dialog rather than an editor.
 */
@Composable
private fun DirectoryField(
    label: String,
    loading: Boolean,
    supported: Boolean,
    enabled: Boolean,
    onClick: () -> Unit,
) {
    val text = label.ifEmpty {
        when {
            loading -> "Loading…"
            !supported -> "Unavailable"
            else -> "Choose a folder"
        }
    }
    Surface(
        onClick = onClick,
        enabled = enabled,
        shape = MaterialTheme.shapes.small,
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outline),
        color = MaterialTheme.colorScheme.surface,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier
                .heightIn(min = 56.dp)
                .padding(
                    horizontal = LerdrTheme.spacing.medium,
                    vertical = LerdrTheme.spacing.small,
                ),
        ) {
            Icon(
                Icons.Default.Folder,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.size(20.dp),
            )
            Spacer(Modifier.width(LerdrTheme.spacing.small))
            Text(
                text,
                style = MaterialTheme.typography.bodyMedium,
                color = if (label.isEmpty()) {
                    MaterialTheme.colorScheme.onSurfaceVariant
                } else {
                    MaterialTheme.colorScheme.onSurface
                },
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f),
            )
            Icon(
                Icons.Default.KeyboardArrowDown,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@PreviewLightDark
@Composable
private fun NewAgentSheetPreview() {
    LerdrTheme {
        NewAgentSheetContent(
            uiState = LaunchUiState(
                relays = listOf(LaunchRelayOption("sd", "sd")),
                relayId = "sd",
                profiles = listOf(
                    AgentProfile(id = "claude", label = "Claude"),
                    AgentProfile(id = "codex", label = "Codex"),
                ),
                profileId = "claude",
                name = "lerdr-claude",
                cwd = "/home/u/lerdr",
                cwdLabel = "lerdr",
                directoryReady = true,
                workspaces = listOf(LaunchWorkspaceOption("w1", "lerdr")),
            ),
            onRelaySelect = {},
            onProfileSelect = {},
            onWorkspaceSelect = {},
            onNameChange = {},
            onPromptChange = {},
            onBrowseDirectories = {},
            onSubmit = {},
        )
    }
}

@PreviewLightDark
@Composable
private fun NewWorkspaceSheetPreview() {
    LerdrTheme {
        NewWorkspaceSheetContent(
            uiState = LaunchUiState(
                relays = listOf(LaunchRelayOption("sd", "sd")),
                relayId = "sd",
                cwd = "/home/u/lerdr",
                cwdLabel = "lerdr",
                workspaceLabel = "lerdr",
            ),
            onRelaySelect = {},
            onLabelChange = {},
            onBrowseDirectories = {},
            onSubmit = {},
        )
    }
}

@PreviewLightDark
@Composable
private fun DirectoryBrowserPreview() {
    LerdrTheme {
        DirectoryBrowserDialog(
            directory = DirectoryBrowserUi(
                open = true,
                listing = DirectoryListing(
                    currentPath = "/home/u/lerdr",
                    currentLabel = "lerdr",
                    parent = "/home/u",
                    directories = listOf(
                        DirectoryEntry("app", "/home/u/lerdr/app"),
                        DirectoryEntry("relay", "/home/u/lerdr/relay"),
                    ),
                ),
            ),
            onBrowse = {},
            onDismiss = {},
        )
    }
}
