package com.lerdr.app.computers

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Dns
import androidx.compose.material.icons.filled.History
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.Terminal
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.PreviewLightDark
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lerdr.app.di.AppEntryPoint
import com.lerdr.app.home.HomeViewModel
import com.lerdr.app.home.RelayCardUi
import com.lerdr.core.designsystem.components.LerdrNavItem
import com.lerdr.core.designsystem.components.LerdrShortNavigationBar
import com.lerdr.core.designsystem.theme.LerdrTheme
import com.lerdr.navigation.LerdrKey
import dagger.hilt.android.EntryPointAccessors

/**
 * Computers tab — the connected Herdr relays as a first-class destination
 * (the mission-control carousel promoted to a real tab; see docs/04
 * §Navigation model). Read-only glance: fine management (reconnect /
 * forget / rename / revoke) stays on Settings → Devices.
 */
@Composable
fun ComputersScreen(
    onSelectTopLevel: (LerdrKey) -> Unit,
    onPairDevice: () -> Unit,
    onManageDevices: () -> Unit,
) {
    val appContext = LocalContext.current.applicationContext
    val viewModel: HomeViewModel = viewModel {
        HomeViewModel(
            EntryPointAccessors.fromApplication(appContext, AppEntryPoint::class.java)
                .homeRepository(),
        )
    }
    val uiState by viewModel.uiState.collectAsStateWithLifecycle()
    ComputersContent(
        relays = uiState.relays,
        relaySummary = uiState.relaySummary,
        onSelectTopLevel = onSelectTopLevel,
        onPairDevice = onPairDevice,
        onManageDevices = onManageDevices,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ComputersContent(
    relays: List<RelayCardUi>,
    relaySummary: String,
    onSelectTopLevel: (LerdrKey) -> Unit,
    onPairDevice: () -> Unit,
    onManageDevices: () -> Unit,
) {
    val spacing = LerdrTheme.spacing
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Column {
                        Text("Computers", style = MaterialTheme.typography.headlineMedium)
                        if (relaySummary.isNotEmpty()) {
                            Text(
                                relaySummary,
                                style = MaterialTheme.typography.bodyMedium,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                },
            )
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
                        label = "Computers",
                        icon = Icons.Default.Dns,
                        selected = true,
                        onClick = { onSelectTopLevel(LerdrKey.Computers) },
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
                        selected = false,
                        onClick = { onSelectTopLevel(LerdrKey.Settings) },
                    ),
                ),
            )
        },
        floatingActionButton = {
            FloatingActionButton(onClick = onPairDevice) {
                Icon(Icons.Default.Add, contentDescription = "Pair device")
            }
        },
    ) { innerPadding ->
        LazyColumn(
            modifier = Modifier.fillMaxSize(),
            contentPadding = PaddingValues(
                top = innerPadding.calculateTopPadding(),
                bottom = innerPadding.calculateBottomPadding() + spacing.medium,
            ),
            verticalArrangement = Arrangement.spacedBy(spacing.small),
        ) {
            items(relays, key = { it.relayId }) { relay ->
                ComputerRow(
                    relay = relay,
                    onClick = onManageDevices,
                    modifier = Modifier.padding(horizontal = spacing.medium),
                )
            }
        }
    }
}

@Composable
private fun ComputerRow(
    relay: RelayCardUi,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Card(
        onClick = onClick,
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.surfaceContainerLow,
        ),
        shape = MaterialTheme.shapes.medium,
        modifier = modifier.fillMaxWidth(),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.padding(LerdrTheme.spacing.medium),
        ) {
            Box(
                contentAlignment = Alignment.Center,
                modifier = Modifier
                    .size(36.dp)
                    .clip(CircleShape),
            ) {
                Icon(
                    imageVector = Icons.Default.Dns,
                    contentDescription = null,
                    tint = if (relay.connected) {
                        LerdrTheme.extendedColors.live
                    } else {
                        MaterialTheme.colorScheme.onSurfaceVariant
                    },
                )
            }
            Spacer(Modifier.width(LerdrTheme.spacing.small))
            Column(Modifier.weight(1f)) {
                Text(
                    relay.label,
                    style = MaterialTheme.typography.titleSmall,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                Text(
                    "${relay.transport} · ${relay.statusLabel}",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            Spacer(Modifier.width(LerdrTheme.spacing.small))
            val count = relay.agentCount
            if (count > 0) {
                Text(
                    if (count == 1) "1 agent" else "$count agents",
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

@PreviewLightDark
@Composable
private fun ComputersContentPreview() {
    LerdrTheme {
        ComputersContent(
            relays = listOf(
                RelayCardUi("sd", "sd", "tailscale", "12ms", 4, connected = true),
                RelayCardUi("workstation", "workstation", "gateway", "81ms", 0, connected = false),
            ),
            relaySummary = "2 computers · tailscale",
            onSelectTopLevel = {},
            onPairDevice = {},
            onManageDevices = {},
        )
    }
}
