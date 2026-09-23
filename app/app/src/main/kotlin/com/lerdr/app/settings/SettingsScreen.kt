package com.lerdr.app.settings

import android.app.UiModeManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.provider.Settings as AndroidSettings
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.selection.toggleable
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.filled.DarkMode
import androidx.compose.material.icons.filled.Devices
import androidx.compose.material.icons.filled.Fingerprint
import androidx.compose.material.icons.filled.Info
import androidx.compose.material.icons.filled.LightMode
import androidx.compose.material.icons.filled.Notifications
import androidx.compose.material.icons.filled.NotificationsOff
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.PreviewLightDark
import androidx.core.app.NotificationManagerCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lerdr.app.nav.LerdrNavBadges
import com.lerdr.app.nav.rememberLerdrNavBadges
import com.lerdr.app.nav.topLevelNavItems
import com.lerdr.app.security.BiometricPromptHelper
import com.lerdr.app.security.SecurityEntryPoint
import com.lerdr.core.designsystem.components.LerdrSegmentedControl
import com.lerdr.core.designsystem.components.LerdrSettingsDivider
import com.lerdr.core.designsystem.components.LerdrSettingsGroup
import com.lerdr.core.designsystem.components.LerdrShortNavigationBar
import com.lerdr.core.designsystem.theme.LerdrTheme
import com.lerdr.navigation.LerdrKey
import dagger.hilt.android.EntryPointAccessors
import lerdr.core.protocol.Protocol

/**
 * Settings (docs/04 §Settings) — grouped, Pixel-style rows: Relays
 * (chevron into the per-relay detail), Security (app lock), Appearance
 * (per-app night mode, API 31+), Notifications status + system-settings
 * link, About. Per-relay management (devices, push, speech, updates)
 * lives on [RelayDetailScreen].
 */
@Composable
fun SettingsScreen(
    onSelectTopLevel: (LerdrKey) -> Unit,
    onOpenRelay: (String) -> Unit,
) {
    // hilt-navigation-compose is absent — pull the bound singletons
    // through the screen's entry points.
    val context = LocalContext.current
    val appContext = context.applicationContext
    val entryPoint = remember(appContext) {
        EntryPointAccessors.fromApplication(appContext, SettingsEntryPoint::class.java)
    }
    val securityEntryPoint = remember(appContext) {
        EntryPointAccessors.fromApplication(appContext, SecurityEntryPoint::class.java)
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
        appLockReady = rememberAppLockReady(
            context,
            securityEntryPoint.biometricPromptHelper(),
        ),
        snackbarHostState = snackbarHostState,
        onSelectTopLevel = onSelectTopLevel,
        onOpenRelay = onOpenRelay,
        badges = rememberLerdrNavBadges(),
        onRevalidateAll = viewModel::revalidateAll,
        onThemeMode = { mode ->
            viewModel.setThemeMode(mode)
            applyThemeMode(context, mode)
        },
        onAppLockChange = viewModel::setAppLockEnabled,
        onOpenNotificationSettings = { openNotificationSettings(context) },
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsContent(
    uiState: SettingsUiState,
    appVersion: String,
    notificationsEnabled: Boolean,
    appLockReady: Boolean,
    snackbarHostState: SnackbarHostState,
    onSelectTopLevel: (LerdrKey) -> Unit,
    onOpenRelay: (String) -> Unit,
    onRevalidateAll: () -> Unit,
    onThemeMode: (ThemeMode) -> Unit,
    onAppLockChange: (Boolean) -> Unit,
    onOpenNotificationSettings: () -> Unit,
    badges: LerdrNavBadges = LerdrNavBadges(),
) {
    val spacing = LerdrTheme.spacing
    Scaffold(
        topBar = {
            TopAppBar(title = { Text("Settings") })
        },
        bottomBar = {
            LerdrShortNavigationBar(
                items = topLevelNavItems(
                    selected = LerdrKey.Settings,
                    badges = badges,
                    onSelect = onSelectTopLevel,
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
            item(key = "relays-group") {
                LerdrSettingsGroup {
                    if (uiState.relays.isEmpty()) {
                        ListItem(
                            headlineContent = { Text("No computers paired") },
                            supportingContent = {
                                Text("Pair one from the Agents tab (+) to see it here.")
                            },
                        )
                    }
                    uiState.relays.forEachIndexed { index, relay ->
                        if (index > 0) LerdrSettingsDivider()
                        RelayRow(relay = relay, onClick = { onOpenRelay(relay.relayId) })
                    }
                }
            }

            item(key = "security-header") {
                SectionHeader(title = "Security")
            }
            item(key = "security-group") {
                LerdrSettingsGroup {
                    // Whole row toggles — the canonical settings Switch
                    // pattern: the row owns the interaction (Role.Switch
                    // announces "on/off"), the Switch renders state only.
                    ListItem(
                        modifier = Modifier.toggleable(
                            value = uiState.appLockEnabled,
                            role = Role.Switch,
                            onValueChange = onAppLockChange,
                        ),
                        colors = listItemGroupColors(),
                        headlineContent = { Text("App lock") },
                        supportingContent = {
                            Text(
                                when {
                                    !uiState.appLockEnabled ->
                                        "Require biometrics or the device PIN to open Lerdr."
                                    appLockReady ->
                                        "On — verifies once every time the app opens."
                                    else ->
                                        "On, but no screen lock is set up — " +
                                            "the gate opens without verifying."
                                },
                            )
                        },
                        leadingContent = {
                            Icon(
                                Icons.Default.Fingerprint,
                                contentDescription = null,
                                tint = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        },
                        trailingContent = {
                            Switch(
                                checked = uiState.appLockEnabled,
                                onCheckedChange = null,
                            )
                        },
                    )
                }
            }

            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                item(key = "appearance-header") {
                    SectionHeader(title = "Appearance")
                }
                item(key = "appearance-group") {
                    LerdrSettingsGroup {
                        ListItem(
                            colors = listItemGroupColors(),
                            headlineContent = { Text("Theme") },
                            leadingContent = {
                                Icon(
                                    if (uiState.themeMode == ThemeMode.DARK) {
                                        Icons.Default.DarkMode
                                    } else {
                                        Icons.Default.LightMode
                                    },
                                    contentDescription = null,
                                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            },
                        )
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .padding(
                                    horizontal = spacing.medium,
                                    vertical = spacing.extraSmall,
                                ),
                        ) {
                            LerdrSegmentedControl(
                                options = ThemeMode.entries.map { it.label },
                                selectedIndex = ThemeMode.entries.indexOf(uiState.themeMode),
                                onSelect = { onThemeMode(ThemeMode.entries[it]) },
                                modifier = Modifier.fillMaxWidth(),
                            )
                        }
                        Spacer(Modifier.height(spacing.small))
                    }
                }
            }

            item(key = "notifications-header") {
                SectionHeader(title = "Notifications")
            }
            item(key = "notifications-group") {
                LerdrSettingsGroup {
                    ListItem(
                        colors = listItemGroupColors(),
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
            }

            item(key = "about-header") {
                SectionHeader(title = "About")
            }
            item(key = "about-group") {
                LerdrSettingsGroup {
                    ListItem(
                        colors = listItemGroupColors(),
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
                    LerdrSettingsDivider()
                    ListItem(
                        colors = listItemGroupColors(),
                        headlineContent = { Text("Protocol") },
                        supportingContent = {
                            Text(
                                "v${Protocol.VERSION} · " +
                                    Protocol.ENCRYPTED_WEBSOCKET_SUBPROTOCOL,
                            )
                        },
                    )
                    LerdrSettingsDivider()
                    ListItem(
                        colors = listItemGroupColors(),
                        headlineContent = { Text("Reference implementation") },
                        supportingContent = { Text("github.com/IGUNUBLUE/lerdr") },
                    )
                }
            }
        }
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

/**
 * One relay row inside the Relays group — status-tinted icon, label,
 * `origin · status` summary, chevron into the per-relay detail screen.
 */
@Composable
private fun RelayRow(
    relay: RelayRowUi,
    onClick: () -> Unit,
) {
    val statusColor = when {
        relay.connected -> LerdrTheme.extendedColors.live
        relay.authRejected -> MaterialTheme.colorScheme.error
        else -> MaterialTheme.colorScheme.onSurfaceVariant
    }
    ListItem(
        modifier = Modifier.clickable(onClick = onClick),
        colors = listItemGroupColors(),
        headlineContent = {
            Text(relay.label, maxLines = 1, overflow = TextOverflow.Ellipsis)
        },
        supportingContent = {
            Column {
                Text(
                    "${relay.origin} · ${relay.statusLabel}",
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                if (relay.detailLabel.isNotEmpty()) {
                    Text(
                        relay.detailLabel,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        },
        leadingContent = {
            Icon(
                Icons.Default.Devices,
                contentDescription = null,
                tint = statusColor,
            )
        },
        trailingContent = {
            Icon(
                Icons.AutoMirrored.Filled.KeyboardArrowRight,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        },
    )
}

/** ListItems riding inside a [LerdrSettingsGroup] drop their own fill. */
@Composable
private fun listItemGroupColors() = ListItemDefaults.colors(
    containerColor = Color.Transparent,
)

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

/**
 * Whether an authenticator can run at all — re-probed on resume because
 * the user can enrol a biometric or a lockscreen in system settings
 * between visits, and the App lock row copy follows what the gate would
 * actually do.
 */
@Composable
private fun rememberAppLockReady(
    context: Context,
    promptHelper: BiometricPromptHelper,
): Boolean {
    val lifecycleOwner = LocalLifecycleOwner.current
    val ready by produceState(
        initialValue = promptHelper.canPrompt(context),
        context,
        lifecycleOwner,
        promptHelper,
    ) {
        val observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_RESUME) {
                value = promptHelper.canPrompt(context)
            }
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        awaitDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }
    return ready
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
            appLockReady = true,
            snackbarHostState = remember { SnackbarHostState() },
            onSelectTopLevel = {},
            onOpenRelay = {},
            onRevalidateAll = {},
            onThemeMode = {},
            onAppLockChange = {},
            onOpenNotificationSettings = {},
        )
    }
}
