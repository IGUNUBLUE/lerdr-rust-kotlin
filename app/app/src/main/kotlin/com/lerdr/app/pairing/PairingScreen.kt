package com.lerdr.app.pairing

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.QrCodeScanner
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.PreviewLightDark
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lerdr.app.di.AppEntryPoint
import com.lerdr.core.designsystem.components.LerdrLoadingIndicator
import com.lerdr.core.designsystem.theme.LerdrTextStyles
import com.lerdr.core.designsystem.theme.LerdrTheme
import com.lerdr.navigation.LerdrDeepLinks
import com.lerdr.navigation.SetupLink
import dagger.hilt.android.EntryPointAccessors

/**
 * Pairing — accepts a `lerdr://pair?…` deep link (prefilled via [setupLink])
 * or a pasted setup link. [PairingViewModel] owns the redemption; the
 * content below is a pure function of [PairingUiState].
 */
@Composable
fun PairingScreen(
    setupLink: SetupLink?,
    onPaired: () -> Unit,
    onBack: () -> Unit,
) {
    val appContext = LocalContext.current.applicationContext
    val viewModel: PairingViewModel = viewModel {
        PairingViewModel(
            EntryPointAccessors.fromApplication(appContext, AppEntryPoint::class.java)
                .pairingManager(),
        )
    }
    val uiState by viewModel.uiState.collectAsStateWithLifecycle()
    LaunchedEffect(uiState.phase) {
        if (uiState.phase == PairingUiState.Phase.SUCCESS) onPaired()
    }
    PairingContent(
        setupLink = setupLink,
        uiState = uiState,
        onConnectLink = viewModel::connect,
        onConnectPasted = viewModel::connectPasted,
        onBack = onBack,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PairingContent(
    setupLink: SetupLink?,
    uiState: PairingUiState,
    onConnectLink: (SetupLink) -> Unit,
    onConnectPasted: (String) -> Unit,
    onBack: () -> Unit,
) {
    val spacing = LerdrTheme.spacing
    var pastedLink by rememberSaveable { mutableStateOf("") }
    val parsed = setupLink ?: LerdrDeepLinks.parseSetupLink(pastedLink)
    val connecting = uiState.phase == PairingUiState.Phase.CONNECTING

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Pair a computer") },
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
    ) { innerPadding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(innerPadding)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = spacing.medium),
            verticalArrangement = Arrangement.spacedBy(spacing.medium),
        ) {
            Text(
                "Scan the QR on your relay's setup screen, or paste the setup link.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            OutlinedButton(
                onClick = { /* QR scanner lands with the pairing feature round */ },
                enabled = !connecting,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Icon(
                    Icons.Default.QrCodeScanner,
                    contentDescription = null,
                    modifier = Modifier.padding(end = spacing.small),
                )
                Text("Scan QR code")
            }

            OutlinedTextField(
                value = pastedLink,
                onValueChange = { pastedLink = it },
                label = { Text("Setup link") },
                placeholder = { Text("lerdr://pair?setup=…", style = LerdrTextStyles.code) },
                textStyle = LerdrTextStyles.code,
                singleLine = true,
                enabled = !connecting,
                isError = pastedLink.isNotEmpty() && parsed == null,
                supportingText = {
                    if (pastedLink.isNotEmpty() && parsed == null) {
                        Text("Not a valid setup link")
                    }
                },
                modifier = Modifier.fillMaxWidth(),
            )

            parsed?.let { link ->
                SetupLinkCard(link)
            }

            uiState.error?.let { error ->
                Text(
                    error.message,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.error,
                )
            }

            Spacer(Modifier.height(spacing.small))

            Button(
                onClick = {
                    if (setupLink != null) {
                        onConnectLink(setupLink)
                    } else {
                        onConnectPasted(pastedLink)
                    }
                },
                enabled = parsed != null && !connecting,
                modifier = Modifier.fillMaxWidth(),
            ) {
                if (connecting) {
                    LerdrLoadingIndicator(modifier = Modifier.size(18.dp))
                    Spacer(Modifier.size(spacing.small))
                }
                Text(if (connecting) "Pairing…" else "Connect")
            }
        }
    }
}

@Composable
private fun SetupLinkCard(link: SetupLink) {
    val spacing = LerdrTheme.spacing
    val colors = LerdrTheme.extendedColors
    Card(
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.surfaceContainerLow,
        ),
        shape = MaterialTheme.shapes.medium,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(
            modifier = Modifier.padding(spacing.medium),
            verticalArrangement = Arrangement.spacedBy(spacing.extraSmall),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    link.displayName,
                    style = MaterialTheme.typography.titleMedium,
                    modifier = Modifier.weight(1f),
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                Surface(
                    color = if (link.isInvitation) {
                        colors.chatContainer
                    } else {
                        colors.workingContainer
                    },
                    contentColor = if (link.isInvitation) {
                        colors.onChatContainer
                    } else {
                        colors.onWorkingContainer
                    },
                    shape = MaterialTheme.shapes.small,
                ) {
                    Text(
                        if (link.isInvitation) "invitation" else "bootstrap",
                        style = MaterialTheme.typography.labelSmall,
                        modifier = Modifier.padding(
                            horizontal = spacing.small,
                            vertical = spacing.extraSmall,
                        ),
                    )
                }
            }
            link.relay?.let {
                Text(
                    it,
                    style = LerdrTextStyles.code,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
    }
}

@PreviewLightDark
@Composable
private fun PairingContentEmptyPreview() {
    LerdrTheme {
        PairingContent(
            setupLink = null,
            uiState = PairingUiState(),
            onConnectLink = {},
            onConnectPasted = {},
            onBack = {},
        )
    }
}

@PreviewLightDark
@Composable
private fun PairingContentLinkedPreview() {
    LerdrTheme {
        PairingContent(
            setupLink = SetupLink(
                setup = "abc123",
                label = "workstation",
                relay = "wss://relay.example.com",
                invite = "inv_9f2k",
            ),
            uiState = PairingUiState(),
            onConnectLink = {},
            onConnectPasted = {},
            onBack = {},
        )
    }
}

@PreviewLightDark
@Composable
private fun PairingContentErrorPreview() {
    LerdrTheme {
        PairingContent(
            setupLink = null,
            uiState = PairingUiState(error = PairingUiState.Error.INVITATION_EXPIRED),
            onConnectLink = {},
            onConnectPasted = {},
            onBack = {},
        )
    }
}
