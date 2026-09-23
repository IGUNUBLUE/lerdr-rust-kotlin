package com.lerdr.app.activity

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Bolt
import androidx.compose.material.icons.filled.CloudOff
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Dns
import androidx.compose.material.icons.filled.History
import androidx.compose.material.icons.filled.Link
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.Sync
import androidx.compose.material.icons.filled.Terminal
import androidx.compose.material.icons.filled.VpnKey
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.PreviewLightDark
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lerdr.core.designsystem.components.LerdrNavItem
import com.lerdr.core.designsystem.components.LerdrShortNavigationBar
import com.lerdr.core.designsystem.theme.LerdrTheme
import com.lerdr.navigation.LerdrKey
import dagger.hilt.android.EntryPointAccessors

/**
 * Activity journal (docs/04 §Activity) — one newest-first list merging
 * this device's session events ([ActivityJournal]) with each relay's
 * action journal ([SessionRepository.activities]). Per-relay filter chips
 * narrow it; the refresh action re-pulls relay history.
 */
@Composable
fun ActivityScreen(
    onSelectTopLevel: (LerdrKey) -> Unit,
) {
    // hilt-navigation-compose is absent — pull the bound singletons
    // through the screen's entry point.
    val appContext = LocalContext.current.applicationContext
    val entryPoint = remember(appContext) {
        EntryPointAccessors.fromApplication(appContext, ActivityEntryPoint::class.java)
    }
    val viewModel: ActivityViewModel = viewModel {
        ActivityViewModel(entryPoint.sessionRepository(), entryPoint.activityJournal())
    }
    val uiState by viewModel.uiState.collectAsStateWithLifecycle()
    ActivityContent(
        uiState = uiState,
        onSelectTopLevel = onSelectTopLevel,
        onSelectFilter = viewModel::selectRelayFilter,
        onRefresh = viewModel::refresh,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ActivityContent(
    uiState: ActivityUiState,
    onSelectTopLevel: (LerdrKey) -> Unit,
    onSelectFilter: (String?) -> Unit,
    onRefresh: () -> Unit,
) {
    val spacing = LerdrTheme.spacing
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Activity") },
                actions = {
                    IconButton(onClick = onRefresh) {
                        Icon(
                            Icons.Default.Refresh,
                            contentDescription = "Refresh journal",
                        )
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
                        selected = false,
                        onClick = { onSelectTopLevel(LerdrKey.Computers) },
                    ),
                    LerdrNavItem(
                        label = "Activity",
                        icon = Icons.Default.History,
                        selected = true,
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
    ) { innerPadding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(innerPadding),
        ) {
            if (uiState.filters.size > 1) {
                Row(
                    horizontalArrangement = Arrangement.spacedBy(spacing.small),
                    modifier = Modifier
                        .fillMaxWidth()
                        .horizontalScroll(rememberScrollState())
                        .padding(horizontal = spacing.medium),
                ) {
                    FilterChip(
                        selected = uiState.selectedFilter == null,
                        onClick = { onSelectFilter(null) },
                        label = { Text("All") },
                    )
                    uiState.filters.forEach { filter ->
                        FilterChip(
                            selected = filter.selected,
                            onClick = { onSelectFilter(filter.relayId) },
                            label = { Text(filter.label) },
                        )
                    }
                }
            }
            if (uiState.items.isEmpty()) {
                Box(
                    contentAlignment = Alignment.Center,
                    modifier = Modifier.fillMaxSize(),
                ) {
                    Text(
                        if (uiState.selectedFilter == null) {
                            "No activity yet — session events and relay actions land here."
                        } else {
                            "Nothing recorded for this relay yet."
                        },
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(spacing.medium),
                    )
                }
            } else {
                LazyColumn(
                    modifier = Modifier.fillMaxSize(),
                    contentPadding = PaddingValues(vertical = spacing.small),
                ) {
                    items(uiState.items, key = { it.key }) { item ->
                        ActivityRow(item)
                    }
                }
            }
        }
    }
}

@Composable
private fun ActivityRow(item: ActivityItemUi) {
    ListItem(
        headlineContent = {
            Text(item.headline, maxLines = 1, overflow = TextOverflow.Ellipsis)
        },
        supportingContent = {
            Text(
                listOfNotNull(
                    item.relayLabel.takeIf { it.isNotEmpty() },
                    item.detail.takeIf { it.isNotEmpty() },
                    item.timestampLabel,
                ).joinToString(" · "),
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        },
        leadingContent = {
            Icon(
                imageVector = item.kind.icon(),
                contentDescription = null,
                tint = item.kind.tint(),
            )
        },
        trailingContent = {
            Text(
                item.ageLabel,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        },
    )
}

private fun ActivityItemKind.icon(): ImageVector = when (this) {
    ActivityItemKind.RELAY_ADDED -> Icons.Default.Add
    ActivityItemKind.RELAY_REMOVED -> Icons.Default.Delete
    ActivityItemKind.CONNECTING -> Icons.Default.Sync
    ActivityItemKind.CONNECTED -> Icons.Default.Link
    ActivityItemKind.DISCONNECTED -> Icons.Default.CloudOff
    ActivityItemKind.AUTH_REJECTED -> Icons.Default.Lock
    ActivityItemKind.PAIRING_REQUIRED -> Icons.Default.VpnKey
    ActivityItemKind.ACTION -> Icons.Default.Bolt
}

@Composable
private fun ActivityItemKind.tint(): Color = when (this) {
    ActivityItemKind.CONNECTED, ActivityItemKind.RELAY_ADDED -> LerdrTheme.extendedColors.live
    ActivityItemKind.AUTH_REJECTED -> MaterialTheme.colorScheme.error
    ActivityItemKind.PAIRING_REQUIRED -> LerdrTheme.extendedColors.attention
    else -> MaterialTheme.colorScheme.onSurfaceVariant
}

@PreviewLightDark
@Composable
private fun ActivityContentPreview() {
    LerdrTheme {
        ActivityContent(
            uiState = ActivityUiState(
                items = listOf(
                    ActivityItemUi(
                        key = "k1",
                        relayId = "sd",
                        relayLabel = "workstation",
                        headline = "Submitted terminal text",
                        detail = "send_input · claude",
                        timestampEpochMs = 0,
                        timestampLabel = "14:02",
                        ageLabel = "40s ago",
                        kind = ActivityItemKind.ACTION,
                    ),
                    ActivityItemUi(
                        key = "k2",
                        relayId = "sd",
                        relayLabel = "workstation",
                        headline = "Connected",
                        detail = "websocket · relay 0.4.2",
                        timestampEpochMs = 0,
                        timestampLabel = "14:01",
                        ageLabel = "1m ago",
                        kind = ActivityItemKind.CONNECTED,
                    ),
                ),
                filters = listOf(RelayFilterUi("sd", "workstation", selected = false)),
            ),
            onSelectTopLevel = {},
            onSelectFilter = {},
            onRefresh = {},
        )
    }
}
