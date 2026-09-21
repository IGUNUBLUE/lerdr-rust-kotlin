package com.lerdr.app.pairing

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
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
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.PreviewLightDark
import androidx.compose.ui.unit.dp
import com.lerdr.core.designsystem.theme.LerdrTextStyles
import com.lerdr.core.designsystem.theme.LerdrTheme
import com.lerdr.navigation.LerdrDeepLinks
import com.lerdr.navigation.SetupLink

/**
 * Pairing — accepts a `lerdr://pair?…` deep link (prefilled via [setupLink])
 * or a pasted setup link. The real flow (QR scan, handshake, credential
 * store) lands with `core:data`; this stub validates the link shape and
 * routes to Home.
 */
@Composable
fun PairingScreen(
    setupLink: SetupLink?,
    onPaired: () -> Unit,
    onBack: () -> Unit,
) {
    PairingContent(
        setupLink = setupLink,
        onPaired = onPaired,
        onBack = onBack,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PairingContent(
    setupLink: SetupLink?,
    onPaired: () -> Unit,
    onBack: () -> Unit,
) {
    val spacing = LerdrTheme.spacing
    var pastedLink by rememberSaveable { mutableStateOf("") }
    val parsed = setupLink ?: LerdrDeepLinks.parseSetupLink(pastedLink)

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

            Spacer(Modifier.height(spacing.small))

            Button(
                onClick = onPaired,
                enabled = parsed != null,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text("Connect")
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
            if (link.gateways.isNotEmpty()) {
                Text(
                    "${link.gateways.size} gateway(s)",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

@PreviewLightDark
@Composable
private fun PairingContentEmptyPreview() {
    LerdrTheme {
        PairingContent(setupLink = null, onPaired = {}, onBack = {})
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
            onPaired = {},
            onBack = {},
        )
    }
}
