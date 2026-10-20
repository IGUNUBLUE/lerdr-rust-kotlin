package com.lerdr.app.settings

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.selection.selectable
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.foundation.layout.Row
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import com.lerdr.app.push.PushStage
import com.lerdr.app.push.PushUiState

internal fun pushDistributorName(packageName: String): String = when (packageName) {
    "org.unifiedpush.distributor.sunup" -> "Sunup"
    "io.heckel.ntfy" -> "ntfy"
    "org.unifiedpush.distributor.nextpush" -> "NextPush"
    else -> packageName.substringAfterLast('.')
}

/** Registration state and explicit provider choice; selecting never happens just by opening it. */
@Composable
internal fun PushDeliveryRow(
    push: PushUiState,
    relayId: String,
    connected: Boolean,
    onPickDistributor: (String) -> Unit,
    onChangeDistributor: () -> Unit,
    onDismissDistributor: () -> Unit,
    onSunupSetup: () -> Unit,
) {
    val via = listOfNotNull(push.distributor?.let(::pushDistributorName), push.endpointHost)
        .joinToString(" · ").ifEmpty { "notification provider" }
    val subscribedHere = relayId in push.subscribedRelays
    val supporting = when (push.stage) {
        PushStage.NO_DISTRIBUTOR -> "Install Sunup to receive notifications while Lerdr is closed."
        PushStage.NEEDS_PICK -> "Choose an installed notification provider."
        PushStage.REGISTERING -> "Registering with $via…"
        PushStage.ENDPOINT_READY -> if (connected) "Connecting $via to this computer…"
            else "Ready via $via — connect this computer to finish setup."
        PushStage.SUBSCRIBED -> if (subscribedHere) "Active via $via" else "Connecting via $via…"
        PushStage.FAILED -> "Unavailable — ${push.error ?: "registration failed"}"
    }
    ListItem(
        headlineContent = { Text("Delivery") },
        supportingContent = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(supporting)
                if (subscribedHere) {
                    Text("Background notifications are registered. Send a test to check this phone.")
                } else if (connected) {
                    Text("Until setup finishes, background delivery depends on Lerdr staying connected.")
                }
                if (push.choosingDistributor || push.stage == PushStage.NEEDS_PICK) {
                    push.distributors.sortedBy { if (it == "org.unifiedpush.distributor.sunup") 0 else 1 }
                        .forEach { distributor ->
                            Row(
                                modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp)
                                    .selectable(selected = distributor == push.distributor,
                                        role = Role.RadioButton,
                                        onClick = { onPickDistributor(distributor) })
                                    .padding(vertical = 4.dp),
                                verticalAlignment = Alignment.CenterVertically,
                            ) {
                                RadioButton(selected = distributor == push.distributor, onClick = null)
                                Text(pushDistributorName(distributor), modifier = Modifier.padding(start = 12.dp))
                            }
                        }
                    if (push.distributors.isEmpty()) Text("No notification provider is installed yet.")
                    if (push.distributor != null) {
                        TextButton(onClick = onDismissDistributor) { Text("Cancel") }
                    }
                } else {
                    TextButton(onClick = onChangeDistributor) { Text("Change provider") }
                }
                TextButton(onClick = onSunupSetup) {
                    Text(if (push.stage == PushStage.NO_DISTRIBUTOR) "Set up Sunup" else "Sunup setup guide")
                }
                if (push.stage == PushStage.NO_DISTRIBUTOR || push.choosingDistributor) {
                    Text("Sunup needs no account or Google Play Services. It uses Mozilla’s service by default. " +
                        "Open Sunup and allow notifications and background battery use.",
                        style = MaterialTheme.typography.bodySmall)
                }
            }
        },
        colors = cardItemColors(),
    )
}
