package com.lerdr.app.settings

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Devices
import androidx.compose.material.icons.filled.History
import androidx.compose.material.icons.filled.RecordVoiceOver
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.Terminal
import androidx.compose.material.icons.filled.Tune
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.tooling.preview.PreviewLightDark
import com.lerdr.core.designsystem.components.LerdrNavItem
import com.lerdr.core.designsystem.components.LerdrShortNavigationBar
import com.lerdr.core.designsystem.theme.LerdrTheme
import com.lerdr.navigation.LerdrKey

/** Settings (docs/04 §Settings) — section list scaffold. */
@Composable
fun SettingsScreen(onSelectTopLevel: (LerdrKey) -> Unit) {
    SettingsContent(onSelectTopLevel = onSelectTopLevel)
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsContent(onSelectTopLevel: (LerdrKey) -> Unit) {
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
    ) { innerPadding ->
        LazyColumn(
            modifier = Modifier
                .fillMaxSize()
                .padding(innerPadding),
        ) {
            settingsSections.forEach { section ->
                item(key = section.title) {
                    SettingsRow(section)
                    HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                }
            }
        }
    }
}

private data class SettingsSectionUi(
    val title: String,
    val subtitle: String,
    val icon: ImageVector,
)

private val settingsSections = listOf(
    SettingsSectionUi("Relays", "Computers, transports, push policy", Icons.Default.Tune),
    SettingsSectionUi("Devices", "Paired devices, invitations", Icons.Default.Devices),
    SettingsSectionUi("Speech", "Voices, languages", Icons.Default.RecordVoiceOver),
    SettingsSectionUi("App", "Theme, biometric lock, diagnostics", Icons.Default.Settings),
)

@Composable
private fun SettingsRow(section: SettingsSectionUi) {
    ListItem(
        headlineContent = { Text(section.title) },
        supportingContent = { Text(section.subtitle) },
        leadingContent = {
            Icon(
                section.icon,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        },
    )
}

@PreviewLightDark
@Composable
private fun SettingsContentPreview() {
    LerdrTheme {
        SettingsContent(onSelectTopLevel = {})
    }
}
