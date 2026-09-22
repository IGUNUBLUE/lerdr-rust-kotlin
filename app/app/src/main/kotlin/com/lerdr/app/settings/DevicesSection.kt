package com.lerdr.app.settings

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Devices
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.ClipEntry
import androidx.compose.ui.platform.LocalClipboard
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.PreviewLightDark
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lerdr.core.designsystem.theme.LerdrTheme
import dagger.hilt.android.EntryPointAccessors
import java.text.DateFormat
import java.util.Date
import kotlinx.coroutines.launch
import lerdr.core.data.DeviceRole

/**
 * Devices settings section — the `DeviceSettings.svelte` port. Rendered once
 * per connected relay by the orchestrator; builds its own [DevicesViewModel]
 * through the screen's Hilt entry point, exactly like `SettingsScreen`.
 */
@Composable
fun DevicesSection(relayId: String, modifier: Modifier = Modifier) {
    val context = LocalContext.current
    val appContext = context.applicationContext
    val viewModel: DevicesViewModel = viewModel(key = "devices-$relayId") {
        DevicesViewModel(
            relayId = relayId,
            sessions = EntryPointAccessors
                .fromApplication(appContext, SettingsEntryPoint::class.java)
                .sessionRepository(),
        )
    }
    val uiState by viewModel.uiState.collectAsStateWithLifecycle()
    DevicesContent(
        uiState = uiState,
        onRefresh = viewModel::refresh,
        onRename = viewModel::renameDevice,
        onRevoke = viewModel::revokeDevice,
        onInvite = viewModel::createInvitation,
        onForgetCurrent = viewModel::forgetCurrentDevice,
        onReset = viewModel::resetDevices,
        onInvitationCopied = viewModel::invitationCopied,
        onInvitationCopyFailed = viewModel::invitationCopyFailed,
        onDismissInvitation = viewModel::dismissInvitation,
        onDismissStatus = viewModel::dismissStatus,
        modifier = modifier,
    )
}

/**
 * Stateless body — the screenshot tests drive it with canned state.
 * Mirrors the oracle's card: heading + Invite affordance, storage hint,
 * status line, invitation block (link, QR, copy), current-device summary,
 * the paired list, then the danger actions.
 */
@Composable
fun DevicesContent(
    uiState: DevicesUiState,
    onRefresh: () -> Unit,
    onRename: (deviceId: String, name: String) -> Unit,
    onRevoke: (DeviceUi) -> Unit,
    onInvite: (name: String, role: DeviceRole) -> Unit,
    onForgetCurrent: () -> Unit,
    onReset: () -> Unit,
    onInvitationCopied: () -> Unit,
    onInvitationCopyFailed: () -> Unit,
    onDismissInvitation: () -> Unit,
    onDismissStatus: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val spacing = LerdrTheme.spacing
    val disabled = uiState.actionBusy
    var renameTarget by remember { mutableStateOf<DeviceUi?>(null) }
    var revokeTarget by remember { mutableStateOf<DeviceUi?>(null) }
    var inviteOpen by rememberSaveable { mutableStateOf(false) }
    var resetOpen by rememberSaveable { mutableStateOf(false) }
    var forgetOpen by rememberSaveable { mutableStateOf(false) }

    val dateFormat = remember {
        DateFormat.getDateTimeInstance(DateFormat.MEDIUM, DateFormat.SHORT)
    }
    fun formatDate(epochMs: Long?): String =
        if (epochMs == null || epochMs <= 0L) "Never" else dateFormat.format(Date(epochMs))

    Card(
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.surfaceContainerLow,
        ),
        shape = MaterialTheme.shapes.medium,
        modifier = modifier.fillMaxWidth(),
    ) {
        Column(
            modifier = Modifier.padding(spacing.medium),
            verticalArrangement = Arrangement.spacedBy(spacing.small),
        ) {
            // ── header: title + relay label, refresh, invite ──────────
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    Icons.Default.Devices,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Spacer(Modifier.width(spacing.small))
                Column(Modifier.weight(1f)) {
                    Text("Devices", style = MaterialTheme.typography.titleSmall)
                    Text(
                        uiState.relayLabel,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                IconButton(
                    onClick = onRefresh,
                    enabled = uiState.connected && !uiState.refreshing && !disabled,
                ) {
                    Icon(
                        Icons.Default.Refresh,
                        contentDescription = "Refresh the device list",
                    )
                }
                if (uiState.canAdminister) {
                    TextButton(
                        onClick = { inviteOpen = true },
                        enabled = !disabled && uiState.connected && uiState.canInvite,
                    ) {
                        Text("Invite device")
                    }
                }
            }

            Text(
                "Device credentials are stored on this device. They are " +
                    "revocable, including from other paired devices.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            // ── oracle `status` line — success or failure text ────────
            uiState.status?.let { status ->
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        status,
                        style = MaterialTheme.typography.bodySmall,
                        color = if (uiState.statusIsError) {
                            MaterialTheme.colorScheme.error
                        } else {
                            MaterialTheme.colorScheme.onSurfaceVariant
                        },
                        modifier = Modifier.weight(1f),
                    )
                    TextButton(onClick = onDismissStatus) {
                        Text("Dismiss", style = MaterialTheme.typography.labelMedium)
                    }
                }
            }

            // ── one-use invitation link (+ QR when the relay draws it) ─
            uiState.invitation?.let { invitation ->
                InvitationBlock(
                    invitation = invitation,
                    onCopied = onInvitationCopied,
                    onCopyFailed = onInvitationCopyFailed,
                    onDismiss = onDismissInvitation,
                )
            }

            // ── current-device summary ────────────────────────────────
            uiState.devices
                .firstOrNull { it.current || it.deviceId == uiState.currentDeviceId }
                ?.let { current ->
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(spacing.small),
                        modifier = Modifier
                            .fillMaxWidth()
                            .clip(MaterialTheme.shapes.small)
                            .background(MaterialTheme.colorScheme.surfaceContainerHigh)
                            .padding(horizontal = spacing.medium, vertical = spacing.small),
                    ) {
                        Text(
                            "This device",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                        Text(
                            current.name,
                            style = MaterialTheme.typography.bodyMedium,
                            fontWeight = FontWeight.SemiBold,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                            modifier = Modifier.weight(1f),
                        )
                        RolePill(current.role)
                    }
                }

            // ── paired devices ────────────────────────────────────────
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    "PAIRED DEVICES",
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    fontWeight = FontWeight.Bold,
                    modifier = Modifier.weight(1f),
                )
                Text(
                    "${uiState.devices.size}",
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }

            when {
                uiState.loading && uiState.devices.isEmpty() -> {
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(spacing.small),
                    ) {
                        CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
                        Text(
                            "Loading devices…",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
                uiState.devices.isEmpty() -> {
                    Text(
                        if (uiState.fetched) {
                            "No paired devices were returned by this relay."
                        } else {
                            "Connect to this relay to load its paired devices."
                        },
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                else -> Column {
                    uiState.devices.forEach { device ->
                        DeviceRow(
                            device = device,
                            isCurrent = device.current ||
                                device.deviceId == uiState.currentDeviceId,
                            canAdminister = uiState.canAdminister,
                            enabled = !disabled && uiState.connected,
                            formatDate = ::formatDate,
                            onRename = { renameTarget = device },
                            onRevoke = { revokeTarget = device },
                        )
                    }
                }
            }

            // ── danger zone ───────────────────────────────────────────
            Row(horizontalArrangement = Arrangement.spacedBy(spacing.small)) {
                if (uiState.currentDeviceId.isNotEmpty()) {
                    TextButton(
                        onClick = { forgetOpen = true },
                        enabled = !disabled && uiState.connected,
                    ) {
                        Text("Forget this device")
                    }
                }
                if (uiState.canAdminister) {
                    Spacer(Modifier.weight(1f))
                    TextButton(
                        onClick = { resetOpen = true },
                        enabled = !disabled && uiState.connected &&
                            uiState.devices.isNotEmpty(),
                    ) {
                        Text("Reset all devices", color = MaterialTheme.colorScheme.error)
                    }
                }
            }
        }
    }

    // ── dialogs ───────────────────────────────────────────────────────

    renameTarget?.let { target ->
        RenameDeviceDialog(
            device = target,
            busy = disabled,
            onConfirm = { name ->
                onRename(target.deviceId, name)
                renameTarget = null
            },
            onDismiss = { renameTarget = null },
        )
    }

    if (inviteOpen) {
        InviteDeviceDialog(
            busy = disabled,
            connected = uiState.connected,
            onConfirm = { name, role ->
                onInvite(name, role)
                inviteOpen = false
            },
            onDismiss = { inviteOpen = false },
        )
    }

    revokeTarget?.let { target ->
        AlertDialog(
            onDismissRequest = { revokeTarget = null },
            title = { Text("Revoke ${target.name}?") },
            text = {
                Text(
                    "This immediately closes that device's relay connection " +
                        "and removes its push endpoints and pending authorization.",
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        onRevoke(target)
                        revokeTarget = null
                    },
                    enabled = !disabled && uiState.connected,
                ) {
                    Text("Revoke", color = MaterialTheme.colorScheme.error)
                }
            },
            dismissButton = {
                TextButton(onClick = { revokeTarget = null }) {
                    Text("Cancel")
                }
            },
        )
    }

    if (resetOpen) {
        ResetDevicesDialog(
            busy = disabled,
            connected = uiState.connected,
            onConfirm = {
                onReset()
                resetOpen = false
            },
            onDismiss = { resetOpen = false },
        )
    }

    if (forgetOpen) {
        AlertDialog(
            onDismissRequest = { forgetOpen = false },
            title = { Text("Forget this device") },
            text = {
                Text(
                    "Revokes this device's credential at the relay — it can " +
                        "never authenticate again, and this session ends. " +
                        "Pair again to reconnect.",
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        onForgetCurrent()
                        forgetOpen = false
                    },
                    enabled = !disabled && uiState.connected,
                ) {
                    Text("Forget", color = MaterialTheme.colorScheme.error)
                }
            },
            dismissButton = {
                TextButton(onClick = { forgetOpen = false }) {
                    Text("Cancel")
                }
            },
        )
    }
}

// ── pieces ────────────────────────────────────────────────────────────

@Composable
private fun RolePill(role: DeviceRole) {
    val isController = role == DeviceRole.CONTROLLER
    Surface(
        shape = MaterialTheme.shapes.extraLarge,
        border = BorderStroke(
            1.dp,
            if (isController) {
                MaterialTheme.colorScheme.primary
            } else {
                MaterialTheme.colorScheme.outline
            },
        ),
        color = Color.Transparent,
    ) {
        Text(
            if (isController) "Controller" else "Reader",
            style = MaterialTheme.typography.labelSmall,
            modifier = Modifier.padding(horizontal = 8.dp, vertical = 2.dp),
        )
    }
}

@Composable
private fun DeviceRow(
    device: DeviceUi,
    isCurrent: Boolean,
    canAdminister: Boolean,
    enabled: Boolean,
    formatDate: (Long?) -> String,
    onRename: () -> Unit,
    onRevoke: () -> Unit,
) {
    val spacing = LerdrTheme.spacing
    HorizontalDivider()
    Row(
        verticalAlignment = Alignment.Top,
        horizontalArrangement = Arrangement.spacedBy(spacing.small),
        modifier = Modifier
            .fillMaxWidth()
            // .device-row: padding .8rem 0
            .padding(vertical = spacing.small + spacing.extraSmall),
    ) {
        Column(Modifier.weight(1f)) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(spacing.small),
            ) {
                Text(
                    device.name,
                    style = MaterialTheme.typography.bodyMedium,
                    fontWeight = FontWeight.SemiBold,
                )
                if (isCurrent) Pill("This device")
                if (device.revoked) {
                    Pill("Revoked", color = MaterialTheme.colorScheme.error)
                }
            }
            // dl margin: .55rem 0 0
            Spacer(Modifier.height(spacing.small))
            Row(horizontalArrangement = Arrangement.spacedBy(spacing.medium)) {
                MetaItem("Role", if (device.role == DeviceRole.CONTROLLER) "Controller" else "Reader")
                MetaItem("Paired", formatDate(device.pairedAtEpochMs))
                MetaItem("Last seen", formatDate(device.lastSeenAtEpochMs))
            }
        }
        if (canAdminister) {
            Column {
                TextButton(onClick = onRename, enabled = enabled) {
                    Text("Rename")
                }
                TextButton(onClick = onRevoke, enabled = enabled) {
                    Text("Revoke", color = MaterialTheme.colorScheme.error)
                }
            }
        }
    }
}

@Composable
private fun MetaItem(label: String, value: String) {
    Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(
            label,
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text(value, style = MaterialTheme.typography.labelSmall)
    }
}

@Composable
private fun Pill(text: String, color: Color = MaterialTheme.colorScheme.onSurfaceVariant) {
    Surface(
        shape = MaterialTheme.shapes.extraLarge,
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outline),
        color = Color.Transparent,
    ) {
        Text(
            text,
            style = MaterialTheme.typography.labelSmall,
            color = color,
            modifier = Modifier.padding(horizontal = 8.dp, vertical = 2.dp),
        )
    }
}

// ── invitation ────────────────────────────────────────────────────────

@Composable
private fun InvitationBlock(
    invitation: InvitationUi,
    onCopied: () -> Unit,
    onCopyFailed: () -> Unit,
    onDismiss: () -> Unit,
) {
    val spacing = LerdrTheme.spacing
    val clipboard = LocalClipboard.current
    val clipboardScope = rememberCoroutineScope()
    Column(
        verticalArrangement = Arrangement.spacedBy(spacing.small),
        modifier = Modifier
            .fillMaxWidth()
            .clip(MaterialTheme.shapes.small)
            .background(MaterialTheme.colorScheme.surfaceContainerHigh)
            .padding(spacing.medium),
    ) {
        Text(
            "One-use invitation link",
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        SelectionContainer {
            Text(
                invitation.link,
                style = MaterialTheme.typography.bodySmall,
                fontFamily = androidx.compose.ui.text.font.FontFamily.Monospace,
            )
        }
        invitation.qr?.let { qr ->
            QrCanvas(
                qr = qr,
                modifier = Modifier
                    .align(Alignment.CenterHorizontally)
                    .widthIn(max = 224.dp)
                    .fillMaxWidth(),
            )
            Text(
                "Scan it with the other device, or copy the link.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Row(horizontalArrangement = Arrangement.spacedBy(spacing.small)) {
            TextButton(
                onClick = {
                    clipboardScope.launch {
                        try {
                            clipboard.setClipEntry(
                                ClipEntry(
                                    android.content.ClipData.newPlainText(
                                        "invitation link",
                                        invitation.link,
                                    ),
                                ),
                            )
                            onCopied()
                        } catch (failure: Exception) {
                            onCopyFailed()
                        }
                    }
                },
            ) {
                Text("Copy link")
            }
            TextButton(onClick = onDismiss) {
                Text("Dismiss")
            }
        }
    }
}

/** `qrBitmap`'s `<svg>` — a quiet-zone-padded module grid. */
@Composable
private fun QrCanvas(qr: QrBitmapUi, modifier: Modifier = Modifier) {
    Canvas(
        modifier = modifier
            .fillMaxWidth()
            .aspectRatio(1f),
    ) {
        // Oracle: viewBox="-2 -2 size+4 size+4" — a 2-module quiet zone.
        val quiet = 2
        val cells = qr.size + quiet * 2
        val cell = size.width / cells
        drawRect(Color.White, size = size)
        qr.darkModules.forEachIndexed { index, dark ->
            if (dark) {
                val column = index % qr.size + quiet
                val row = index / qr.size + quiet
                drawRect(
                    Color.Black,
                    topLeft = Offset(column * cell, row * cell),
                    size = Size(cell, cell),
                )
            }
        }
    }
}

// ── dialogs ───────────────────────────────────────────────────────────

/** Oracle `rename-device-*` — names identify paired devices on this relay. */
@Composable
private fun RenameDeviceDialog(
    device: DeviceUi,
    busy: Boolean,
    onConfirm: (String) -> Unit,
    onDismiss: () -> Unit,
) {
    var name by rememberSaveable { mutableStateOf(device.name) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Rename device") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.small)) {
                Text(
                    "Names identify paired devices on this relay.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                OutlinedTextField(
                    value = name,
                    onValueChange = { name = it.take(64) },
                    label = { Text("Device name") },
                    singleLine = true,
                )
            }
        },
        confirmButton = {
            TextButton(
                onClick = { onConfirm(name) },
                enabled = !busy && name.isNotBlank(),
            ) {
                Text("Save")
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss, enabled = !busy) {
                Text("Cancel")
            }
        },
    )
}

/** Oracle `invite-device-*` — name + role + the one-use-secret warning. */
@Composable
private fun InviteDeviceDialog(
    busy: Boolean,
    connected: Boolean,
    onConfirm: (String, DeviceRole) -> Unit,
    onDismiss: () -> Unit,
) {
    var name by rememberSaveable { mutableStateOf("") }
    var role by rememberSaveable { mutableStateOf(DeviceRole.READER) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Invite device") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.small)) {
                Text(
                    "Create a short-lived, one-use enrollment invitation.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                OutlinedTextField(
                    value = name,
                    onValueChange = { name = it.take(64) },
                    label = { Text("Device name") },
                    singleLine = true,
                )
                Row(horizontalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.small)) {
                    DeviceRole.entries.forEach { option ->
                        FilterChip(
                            selected = role == option,
                            onClick = { role = option },
                            label = {
                                Text(
                                    if (option == DeviceRole.CONTROLLER) {
                                        "Controller"
                                    } else {
                                        "Reader"
                                    },
                                )
                            },
                        )
                    }
                }
                Text(
                    "The generated link carries the one-use secret in its URL " +
                        "fragment. Share it only with the intended device.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        },
        confirmButton = {
            TextButton(
                onClick = { onConfirm(name, role) },
                enabled = !busy && connected && name.isNotBlank(),
            ) {
                Text("Create invitation")
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss, enabled = !busy) {
                Text("Cancel")
            }
        },
    )
}

/** Oracle `reset-devices-*` — gated on typing RESET. */
@Composable
private fun ResetDevicesDialog(
    busy: Boolean,
    connected: Boolean,
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
) {
    var confirmation by rememberSaveable { mutableStateOf("") }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Reset all devices") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.small)) {
                Text(
                    "Revoke every issued device credential for this relay.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Text(
                    "Every paired device, including this one, will lose " +
                        "access. This cannot be undone.",
                    color = MaterialTheme.colorScheme.error,
                )
                OutlinedTextField(
                    value = confirmation,
                    onValueChange = { confirmation = it },
                    label = { Text("Type RESET to continue") },
                    singleLine = true,
                )
            }
        },
        confirmButton = {
            TextButton(
                onClick = onConfirm,
                enabled = !busy && connected && confirmation == "RESET",
            ) {
                Text("Confirm reset", color = MaterialTheme.colorScheme.error)
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss, enabled = !busy) {
                Text("Cancel")
            }
        },
    )
}

// ── previews ──────────────────────────────────────────────────────────

@PreviewLightDark
@Composable
private fun DevicesContentPreview() {
    LerdrTheme {
        DevicesContent(
            uiState = DevicesUiState(
                relayLabel = "workstation",
                connected = true,
                canAdminister = true,
                canInvite = true,
                fetched = true,
                currentDeviceId = "dev-1",
                devices = listOf(
                    DeviceUi(
                        deviceId = "dev-1",
                        credentialId = "cred-1",
                        name = "This phone",
                        role = DeviceRole.CONTROLLER,
                        pairedAtEpochMs = 1_767_225_600_000L,
                        lastSeenAtEpochMs = 1_772_445_600_000L,
                        current = true,
                        revoked = false,
                    ),
                    DeviceUi(
                        deviceId = "dev-2",
                        credentialId = "cred-2",
                        name = "Kitchen tablet",
                        role = DeviceRole.READER,
                        pairedAtEpochMs = 1_767_225_600_000L,
                        lastSeenAtEpochMs = 1_772_532_000_000L,
                        current = false,
                        revoked = false,
                    ),
                ),
            ),
            onRefresh = {},
            onRename = { _, _ -> },
            onRevoke = {},
            onInvite = { _, _ -> },
            onForgetCurrent = {},
            onReset = {},
            onInvitationCopied = {},
            onInvitationCopyFailed = {},
            onDismissInvitation = {},
            onDismissStatus = {},
        )
    }
}
