package com.lerdr.app.activity

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.History
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.Terminal
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.PreviewLightDark
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lerdr.app.di.AppEntryPoint
import com.lerdr.core.designsystem.components.LerdrNavItem
import com.lerdr.core.designsystem.components.LerdrShortNavigationBar
import com.lerdr.core.designsystem.theme.LerdrTheme
import com.lerdr.navigation.LerdrKey
import dagger.hilt.android.EntryPointAccessors

/** Activity journal (docs/04 §Activity) — the cross-relay action log. */
@Composable
fun ActivityScreen(
    onSelectTopLevel: (LerdrKey) -> Unit,
) {
    // hilt-navigation-compose is absent — pull the bound repository
    // through the singleton entry point.
    val appContext = LocalContext.current.applicationContext
    val viewModel: ActivityViewModel = viewModel {
        ActivityViewModel(
            EntryPointAccessors.fromApplication(appContext, AppEntryPoint::class.java)
                .sessionRepository(),
        )
    }
    val uiState by viewModel.uiState.collectAsStateWithLifecycle()
    ActivityContent(uiState = uiState, onSelectTopLevel = onSelectTopLevel)
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ActivityContent(
    uiState: ActivityUiState,
    onSelectTopLevel: (LerdrKey) -> Unit,
) {
    Scaffold(
        topBar = {
            TopAppBar(title = { Text("Activity") })
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
        if (uiState.items.isEmpty()) {
            Box(
                contentAlignment = Alignment.Center,
                modifier = Modifier
                    .fillMaxSize()
                    .padding(innerPadding),
            ) {
                Text(
                    "No activity yet — actions you take on agents land here.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        } else {
            LazyColumn(
                modifier = Modifier.fillMaxSize(),
                contentPadding = PaddingValues(
                    top = innerPadding.calculateTopPadding(),
                    bottom = innerPadding.calculateBottomPadding(),
                ),
                verticalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.extraSmall),
            ) {
                items(uiState.items, key = { it.key }) { item ->
                    ListItem(
                        headlineContent = {
                            Text(item.label, maxLines = 1, overflow = TextOverflow.Ellipsis)
                        },
                        supportingContent = {
                            Text(
                                listOfNotNull(
                                    item.relayLabel.takeIf { it.isNotEmpty() },
                                    item.detail.takeIf { it.isNotEmpty() },
                                    item.ageLabel,
                                ).joinToString(" · "),
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                        },
                    )
                }
            }
        }
    }
}

@PreviewLightDark
@Composable
private fun ActivityContentPreview() {
    LerdrTheme {
        ActivityContent(
            uiState = ActivityUiState(
                items = listOf(
                    ActivityItemUi("k1", "sd", "Submitted terminal text", "send_input", "40s ago"),
                    ActivityItemUi("k2", "sd", "responded Allow", "respond", "3m ago"),
                ),
            ),
            onSelectTopLevel = {},
        )
    }
}
