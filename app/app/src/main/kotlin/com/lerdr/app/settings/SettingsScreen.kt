package com.lerdr.app.settings

import android.app.UiModeManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.provider.Settings as AndroidSettings
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Devices
import androidx.compose.material.icons.filled.History
import androidx.compose.material.icons.filled.Info
import androidx.compose.material.icons.filled.Notifications
import androidx.compose.material.icons.filled.NotificationsOff
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.Terminal
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.PreviewLightDark
import androidx.core.app.NotificationManagerCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lerdr.core.designsystem.components.LerdrNavItem
import com.lerdr.core.designsystem.components.LerdrShortNavigationBar
import com.lerdr.core.designsystem.theme.LerdrTheme
import com.lerdr.navigation.LerdrKey
import dagger.hilt.android.EntryPointAccessors
import lerdr.core.protocol.Protocol

/**
 * Settings (docs/04 §Settings) — Relays with working lifecycle actions,
 * Appearance (per-app night mode, API 31+), Notifications status +
 * system-settings link, About. Every visible control does something; rows
 * without a backend (devices, speech, updates) stay absent on purpose.
 */
@Composable
fun SettingsScreen(
    onSelectTopLevel: (LerdrKey) -> Unit,
) {
    // hilt-navigation-compose is absent — pull the bound singletons
    // through the screen's entry point.
    val context = LocalContext.current
    val appContext = context.applicationContext
    val entryPoint = remember(appContext) {
        EntryPointAccessors.fromApplication(appContext, SettingsEntryPoint::class.java)
    }
    val viewModel: SettingsViewModel = viewModel {
        SettingsViewModel(entryPoint.sessionRepository(), entryPoint.appPreferences())
    }
    val uiState by viewModel.uiState.collectAsStateWithLifecycle()

    val snackbarHostState = remember { SnackbarHostState() }
    LaunchedEffect(uiState.lastError) {
        uiState.lastError?.let {
            snackbarHostState.showSnackbar(it)
            viewModel.dismissError()
        }
    }

    SettingsContent(
        uiState = uiState,
        appVersion = remember { appVersionName(context) },
        notificationsEnabled = rememberNotificationsEnabled(context),
        snackbarHostState = snackbarHostState,
        onSelectTopLevel = onSelectTopLevel,
        onReconnectRelay = viewModel::reconnectRelay,
        onForgetRelay = viewModel::forgetRelay,
        onRevalidateAll = viewModel::revalidateAll,
        onThemeMode = { mode ->
            viewModel.setThemeMode(mode)
            applyThemeMode(context, mode)
        },
        onOpenNotificationSettings = { openNotificationSettings(context) },
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsContent(
    uiState: SettingsUiState,
    appVersion: String,
    notificationsEnabled: Boolean,
    snackbarHostState: SnackbarHostState,
    onSelectTopLevel: (LerdrKey) -> Unit,
    onReconnectRelay: (String) -> Unit,
    onForgetRelay: (String) -> Unit,
    onRevalidateAll: () -> Unit,
    onThemeMode: (ThemeMode) -> Unit,
    onOpenNotificationSettings: () -> Unit,
) {
    val spacing = LerdrTheme.spacing
    var forgetTarget by remember { mutableStateOf<RelayRowUi?>(null) }
    Scaffold(
        topBar = {
            TopAppBar(title = { Text("Settings") })
        },
        bottomBar = {
            LerdrShortNavigationBar(
                items = listOf(
                    LerdrNavItem(
                        label = "Agents",
                        icon = Icons.Default.Terminal,
                        selected = false,
                        onClick = { onSelectTopLevel(LerdrKey.Home) },
                    ),
                    LerdrNavItem(
                        label = "Activity",
                        icon = Icons.Default.History,
                        selected = false,
                        onClick = { onSelectTopLevel(LerdrKey.Activity) },
                    ),
                    LerdrNavItem(
                        label = "Settings",
                        icon = Icons.Default.Settings,
                        selected = true,
                        onClick = { onSelectTopLevel(LerdrKey.Settings) },
                    ),
                ),
            )
        },
        snackbarHost = { SnackbarHost(snackbarHostState) },
    ) { innerPadding ->
        LazyColumn(
            modifier = Modifier
                .fillMaxSize()
                .padding(innerPadding),
            contentPadding = PaddingValues(vertical = spacing.small),
            verticalArrangement = Arrangement.spacedBy(spacing.small),
        ) {
            item(key = "relays-header") {
                SectionHeader(
                    title = "Relays",
                    actionLabel = if (uiState.relays.isNotEmpty()) "Revalidate all" else null,
                    onAction = onRevalidateAll,
                )
            }
            if (uiState.relays.isEmpty()) {
                item(key = "relays-empty") {
                    ListItem(
                        headlineContent = { Text("No computers paired") },
                        supportingContent = {
                            Text("Pair one from the Agents tab (+) to see it here.")
                        },
                    )
                }
            }
            items(uiState.relays, key = { it.relayId }) { relay ->
                RelayCard(
                    relay = relay,
                    onReconnect = { onReconnectRelay(relay.relayId) },
                    onForget = { forgetTarget = relay },
                    modifier = Modifier.padding(horizontal = spacing.medium),
                )
            }

            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                item(key = "appearance-header") {
                    SectionHeader(title = "Appearance")
                }
                item(key = "appearance-theme") {
                    ListItem(
                        headlineContent = { Text("Theme") },
                        supportingContent = {
                            Row(horizontalArrangement = Arrangement.spacedBy(spacing.small)) {
                                ThemeMode.entries.forEach { mode ->
                                    FilterChip(
                                        selected = uiState.themeMode == mode,
                                        onClick = { onThemeMode(mode) },
                                        label = { Text(mode.label) },
                                    )
                                }
                            }
                        },
                    )
                }
            }

            item(key = "notifications-header") {
                SectionHeader(title = "Notifications")
            }
            item(key = "notifications-status") {
                ListItem(
                    headlineContent = { Text("Notifications") },
                    supportingContent = {
                        Text(
                            if (notificationsEnabled) {
                                "Allowed — agent alerts can reach the shade."
                            } else {
                                "Blocked — enable them to get agent alerts."
                            },
                        )
                    },
                    leadingContent = {
                        Icon(
                            if (notificationsEnabled) {
                                Icons.Default.Notifications
                            } else {
                                Icons.Default.NotificationsOff
                            },
                            contentDescription = null,
                            tint = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    },
                    trailingContent = {
                        TextButton(onClick = onOpenNotificationSettings) {
                            Text("System settings")
                        }
                    },
                )
            }

            item(key = "about-header") {
                SectionHeader(title = "About")
            }
            item(key = "about-version") {
                ListItem(
                    headlineContent = { Text("Lerdr for Android") },
                    supportingContent = { Text("Version $appVersion") },
                    leadingContent = {
                        Icon(
                            Icons.Default.Info,
                            contentDescription = null,
                            tint = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    },
                )
            }
            item(key = "about-protocol") {
                ListItem(
                    headlineContent = { Text("Protocol") },
                    supportingContent = {
                        Text("v${Protocol.VERSION} · ${Protocol.ENCRYPTED_WEBSOCKET_SUBPROTOCOL}")
                    },
                )
            }
            item(key = "about-reference") {
                ListItem(
                    headlineContent = { Text("Reference implementation") },
                    supportingContent = { Text("github.com/IGUNUBLUE/lerdr") },
                )
            }
        }
    }

    forgetTarget?.let { relay ->
        AlertDialog(
            onDismissRequest = { forgetTarget = null },
            title = { Text("Forget ${relay.label}?") },
            text = {
                Text(
                    "Removes the relay, this device's credential, and the live " +
                        "session. Pair again to reconnect.",
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        onForgetRelay(relay.relayId)
                        forgetTarget = null
                    },
                ) {
                    Text("Forget", color = MaterialTheme.colorScheme.error)
                }
            },
            dismissButton = {
                TextButton(onClick = { forgetTarget = null }) {
                    Text("Cancel")
                }
            },
        )
    }
}

@Composable
private fun SectionHeader(
    title: String,
    actionLabel: String? = null,
    onAction: () -> Unit = {},
) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = LerdrTheme.spacing.medium)
            .padding(top = LerdrTheme.spacing.small),
    ) {
        Text(
            title.uppercase(),
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            fontWeight = FontWeight.Bold,
            modifier = Modifier.weight(1f),
        )
        if (actionLabel != null) {
            TextButton(onClick = onAction) {
                Text(actionLabel, style = MaterialTheme.typography.labelMedium)
            }
        }
    }
}

@Composable
private fun RelayCard(
    relay: RelayRowUi,
    onReconnect: () -> Unit,
    onForget: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val spacing = LerdrTheme.spacing
    val statusColor = when {
        relay.connected -> LerdrTheme.extendedColors.live
        relay.authRejected -> MaterialTheme.colorScheme.error
        else -> MaterialTheme.colorScheme.onSurfaceVariant
    }
    Card(
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.surfaceContainerLow,
        ),
        shape = MaterialTheme.shapes.medium,
        modifier = modifier.fillMaxWidth(),
    ) {
        Column(modifier = Modifier.padding(spacing.medium)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    Icons.Default.Devices,
                    contentDescription = null,
                    tint = statusColor,
                )
                Spacer(Modifier.width(spacing.small))
                Column(Modifier.weight(1f)) {
                    Text(
                        relay.label,
                        style = MaterialTheme.typography.titleSmall,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Text(
                        "${relay.origin} · ${relay.statusLabel}",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    if (relay.detailLabel.isNotEmpty()) {
                        Text(
                            relay.detailLabel,
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                }
            }
            Spacer(Modifier.height(spacing.small))
            Row(horizontalArrangement = Arrangement.spacedBy(spacing.small)) {
                if (relay.canReconnect) {
                    TextButton(onClick = onReconnect) {
                        Text("Reconnect")
                    }
                }
                TextButton(onClick = onForget) {
                    Text("Forget", color = MaterialTheme.colorScheme.error)
                }
            }
        }
    }
}

// ── platform seams (screen-owned; VMs never see a Context) ────────────

/** Re-reads the system grant whenever the screen resumes. */
@Composable
private fun rememberNotificationsEnabled(context: Context): Boolean {
    val lifecycleOwner = LocalLifecycleOwner.current
    val enabled by produceState(
        initialValue = NotificationManagerCompat.from(context).areNotificationsEnabled(),
        context,
        lifecycleOwner,
    ) {
        val observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_RESUME) {
                value = NotificationManagerCompat.from(context).areNotificationsEnabled()
            }
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        awaitDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }
    return enabled
}

/** `ACTION_APP_NOTIFICATION_SETTINGS`, falling back to the app-info page. */
private fun openNotificationSettings(context: Context) {
    val direct = Intent(AndroidSettings.ACTION_APP_NOTIFICATION_SETTINGS)
        .putExtra(AndroidSettings.EXTRA_APP_PACKAGE, context.packageName)
    runCatching { context.startActivity(direct) }.onFailure {
        runCatching {
            context.startActivity(
                Intent(AndroidSettings.ACTION_APPLICATION_DETAILS_SETTINGS)
                    .setData(Uri.parse("package:${context.packageName}")),
            )
        }
    }
}

/**
 * Applies the persisted [ThemeMode] via the platform per-app night mode
 * (API 31+, persisted by the system — no appcompat on the classpath).
 * The activity recreates and `isSystemInDarkTheme` follows.
 */
private fun applyThemeMode(context: Context, mode: ThemeMode) {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S) return
    val nightMode = when (mode) {
        ThemeMode.SYSTEM -> UiModeManager.MODE_NIGHT_AUTO
        ThemeMode.LIGHT -> UiModeManager.MODE_NIGHT_NO
        ThemeMode.DARK -> UiModeManager.MODE_NIGHT_YES
    }
    runCatching {
        context.getSystemService(UiModeManager::class.java)
            ?.setApplicationNightMode(nightMode)
    }
}

@Suppress("DEPRECATION")
private fun appVersionName(context: Context): String = runCatching {
    val manager = context.packageManager
    val info = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
        manager.getPackageInfo(
            context.packageName,
            PackageManager.PackageInfoFlags.of(0L),
        )
    } else {
        manager.getPackageInfo(context.packageName, 0)
    }
    info.versionName
}.getOrNull().orEmpty().ifEmpty { "unknown" }

private val ThemeMode.label: String
    get() = when (this) {
        ThemeMode.SYSTEM -> "System"
        ThemeMode.LIGHT -> "Light"
        ThemeMode.DARK -> "Dark"
    }

@PreviewLightDark
@Composable
private fun SettingsContentPreview() {
    LerdrTheme {
        SettingsContent(
            uiState = SettingsUiState(
                relays = listOf(
                    RelayRowUi(
                        relayId = "sd",
                        label = "workstation",
                        origin = "wss://sd.example.com",
                        statusLabel = "connected",
                        detailLabel = "websocket · relay 0.4.2 · protocol 3",
                        connected = true,
                        authRejected = false,
                        canReconnect = true,
                    ),
                    RelayRowUi(
                        relayId = "old",
                        label = "old laptop",
                        origin = "wss://old.example.com",
                        statusLabel = "authorization rejected — re-pair",
                        detailLabel = "",
                        connected = false,
                        authRejected = true,
                        canReconnect = false,
                    ),
                ),
            ),
            appVersion = "0.1.0",
            notificationsEnabled = true,
            snackbarHostState = remember { SnackbarHostState() },
            onSelectTopLevel = {},
            onReconnectRelay = {},
            onForgetRelay = {},
            onRevalidateAll = {},
            onThemeMode = {},
            onOpenNotificationSettings = {},
        )
    }
}
