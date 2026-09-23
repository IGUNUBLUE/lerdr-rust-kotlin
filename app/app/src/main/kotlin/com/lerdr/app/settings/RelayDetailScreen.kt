package com.lerdr.app.settings

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Devices
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
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
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lerdr.app.di.AppEntryPoint
import com.lerdr.core.designsystem.components.LerdrSettingsGroup
import com.lerdr.core.designsystem.theme.LerdrTheme
import dagger.hilt.android.EntryPointAccessors

/**
 * Per-relay detail — the second level of the Settings hierarchy. The
 * header card carries the live status + lifecycle actions (reconnect,
 * forget); the management sections (push policy, devices, speech) stack
 * below, each self-sufficient through its own entry point.
 */
@Composable
fun RelayDetailScreen(
    relayId: String,
    onBack: () -> Unit,
) {
    val appContext = LocalContext.current.applicationContext
    val sessions = remember(appContext) {
        EntryPointAccessors.fromApplication(appContext, AppEntryPoint::class.java)
            .sessionRepository()
    }
    val viewModel: RelayDetailViewModel = viewModel(key = relayId) {
        RelayDetailViewModel(relayId, sessions)
    }
    val uiState by viewModel.uiState.collectAsStateWithLifecycle()

    val snackbarHostState = remember { SnackbarHostState() }
    LaunchedEffect(uiState.lastError) {
        uiState.lastError?.let {
            snackbarHostState.showSnackbar(it)
            viewModel.dismissError()
        }
    }

    RelayDetailContent(
        relay = uiState.relay,
        snackbarHostState = snackbarHostState,
        onBack = onBack,
        onReconnect = viewModel::reconnect,
        onForget = { viewModel.forget(onForgotten = onBack) },
        sections = {
            PushPolicySection(relayId)
            DevicesSection(relayId)
            SpeechSection(relayId)
        },
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun RelayDetailContent(
    relay: RelayRowUi?,
    snackbarHostState: SnackbarHostState,
    onBack: () -> Unit,
    onReconnect: () -> Unit,
    onForget: () -> Unit,
    sections: @Composable () -> Unit = {},
) {
    val spacing = LerdrTheme.spacing
    var confirmForget by remember { mutableStateOf(false) }
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        relay?.label ?: "Relay",
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(
                            Icons.AutoMirrored.Filled.ArrowBack,
                            contentDescription = "Back",
                        )
                    }
                },
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
            item(key = "status") {
                LerdrSettingsGroup {
                    ListItem(
                        headlineContent = { Text(relay?.label ?: "Unknown relay") },
                        supportingContent = {
                            Column {
                                Text(
                                    listOfNotNull(
                                        relay?.origin,
                                        relay?.statusLabel,
                                    ).joinToString(" · "),
                                )
                                relay?.detailLabel
                                    ?.takeIf(String::isNotEmpty)
                                    ?.let { Text(it) }
                            }
                        },
                        leadingContent = {
                            Icon(
                                Icons.Default.Devices,
                                contentDescription = null,
                                tint = when {
                                    relay?.connected == true ->
                                        LerdrTheme.extendedColors.live
                                    relay?.authRejected == true ->
                                        MaterialTheme.colorScheme.error
                                    else -> MaterialTheme.colorScheme.onSurfaceVariant
                                },
                            )
                        },
                    )
                    Row(
                        horizontalArrangement = Arrangement.spacedBy(spacing.small),
                        modifier = Modifier
                            .fillMaxWidth()
                            .padding(
                                horizontal = spacing.medium,
                                vertical = spacing.small,
                            ),
                    ) {
                        if (relay?.canReconnect == true) {
                            TextButton(onClick = onReconnect) {
                                Text("Reconnect")
                            }
                        }
                        Spacer(Modifier.weight(1f))
                        TextButton(onClick = { confirmForget = true }) {
                            Text("Forget", color = MaterialTheme.colorScheme.error)
                        }
                    }
                }
            }

            item(key = "sections") {
                Column(verticalArrangement = Arrangement.spacedBy(spacing.small)) {
                    sections()
                }
            }
        }
    }

    if (confirmForget) {
        AlertDialog(
            onDismissRequest = { confirmForget = false },
            title = { Text("Forget ${relay?.label ?: "relay"}?") },
            text = {
                Text(
                    "Removes the relay, this device's credential, and the live " +
                        "session. Pair again to reconnect.",
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        confirmForget = false
                        onForget()
                    },
                ) {
                    Text("Forget", color = MaterialTheme.colorScheme.error)
                }
            },
            dismissButton = {
                TextButton(onClick = { confirmForget = false }) {
                    Text("Cancel")
                }
            },
        )
    }
}
