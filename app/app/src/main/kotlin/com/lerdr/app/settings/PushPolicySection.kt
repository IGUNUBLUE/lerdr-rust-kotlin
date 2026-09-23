package com.lerdr.app.settings

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.selection.toggleable
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Notifications
import androidx.compose.material.icons.filled.NotificationsOff
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.PreviewLightDark
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lerdr.core.designsystem.components.liveRegionPolite
import com.lerdr.core.designsystem.theme.LerdrTheme
import dagger.hilt.android.EntryPointAccessors
import java.time.OffsetDateTime
import java.time.format.DateTimeFormatter
import java.util.Locale

/**
 * Per-relay "Push notifications" card — the oracle's
 * `NotificationSettings.svelte` (docs/04 §Settings: "per-relay push policy
 * editor"). Wired into `SettingsScreen` per connected relay.
 *
 * [PushPolicySection] owns the ViewModel seam (entry-point lookup +
 * `viewModel {}`); [PushPolicyContent] is pure state so previews and
 * Roborazzi tests render it without Hilt.
 */
@Composable
fun PushPolicySection(
    relayId: String,
    modifier: Modifier = Modifier,
) {
    val appContext = LocalContext.current.applicationContext
    val viewModel: PushPolicyViewModel = viewModel(key = "push-policy:$relayId") {
        val entryPoint = EntryPointAccessors.fromApplication(
            appContext,
            SettingsEntryPoint::class.java,
        )
        PushPolicyViewModel(entryPoint.sessionRepository())
    }
    LaunchedEffect(relayId) { viewModel.bind(relayId) }
    val uiState by viewModel.uiState.collectAsStateWithLifecycle()
    PushPolicyContent(
        uiState = uiState,
        onCategoryChange = viewModel::setCategory,
        onSettleMs = viewModel::setSettleMs,
        onCooldownMs = viewModel::setCooldownMs,
        onSnoozeOff = viewModel::clearSnooze,
        onSnoozeFor = viewModel::snoozeFor,
        onSnoozeIndefinitely = viewModel::snoozeIndefinitely,
        onUpdateOnce = viewModel::setUpdateOnce,
        onSendTest = viewModel::sendTest,
        onRetry = viewModel::refreshPolicy,
        modifier = modifier,
    )
}

@Immutable
private data class CategoryUi(val key: String, val label: String, val detail: String)

/** Oracle `CATEGORY_LABELS`/`CONFIGURABLE_CATEGORIES` — the four editable categories. */
private val CONFIGURABLE_CATEGORIES = listOf(
    CategoryUi("attention", "Approval needed", "An agent needs your review."),
    CategoryUi("question", "Questions", "An agent needs your answer."),
    CategoryUi("finished", "Finished", "An agent finished."),
    CategoryUi("test", "Test notifications", "Delivery checks for this device."),
)

/** Oracle settle `<select>`: 0 / 2 s / 5 s / 15 s. */
private val SETTLE_OPTIONS = listOf(
    0L to "Immediately",
    2_000L to "2 seconds",
    5_000L to "5 seconds",
    15_000L to "15 seconds",
)

/** Oracle cooldown `<select>`: none / 30 s / 1 min / 5 min. */
private val COOLDOWN_OPTIONS = listOf(
    0L to "None",
    30_000L to "30 seconds",
    60_000L to "1 minute",
    300_000L to "5 minutes",
)

/** Oracle snooze `<select>` timed options: 1 h / 8 h / 24 h. */
private val SNOOZE_DURATIONS = listOf(
    3_600_000L to "1 hour",
    28_800_000L to "8 hours",
    86_400_000L to "24 hours",
)

@Composable
fun PushPolicyContent(
    uiState: PushPolicyUiState,
    onCategoryChange: (String, Boolean) -> Unit,
    onSettleMs: (Long) -> Unit,
    onCooldownMs: (Long) -> Unit,
    onSnoozeOff: () -> Unit,
    onSnoozeFor: (Long) -> Unit,
    onSnoozeIndefinitely: () -> Unit,
    onUpdateOnce: (Boolean) -> Unit,
    onSendTest: () -> Unit,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val spacing = LerdrTheme.spacing
    Card(
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.surfaceContainerLow,
        ),
        shape = MaterialTheme.shapes.medium,
        modifier = modifier.fillMaxWidth(),
    ) {
        Column(modifier = Modifier.padding(vertical = spacing.medium)) {
            // Card header — the section title rides inside the card like
            // RelayCard carries its relay identity.
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = spacing.medium),
            ) {
                Icon(
                    if (uiState.connected) {
                        Icons.Default.Notifications
                    } else {
                        Icons.Default.NotificationsOff
                    },
                    contentDescription = null,
                    tint = if (uiState.connected) {
                        LerdrTheme.extendedColors.live
                    } else {
                        MaterialTheme.colorScheme.onSurfaceVariant
                    },
                )
                Spacer(Modifier.width(spacing.small))
                Column(Modifier.weight(1f)) {
                    Text("Push notifications", style = MaterialTheme.typography.titleSmall)
                    Text(
                        listOfNotNull(
                            uiState.relayLabel.ifEmpty { uiState.relayId }
                                .takeIf { it.isNotEmpty() },
                            when {
                                uiState.connected -> "connected"
                                uiState.connecting -> "connecting…"
                                else -> "offline"
                            },
                        ).joinToString(" · "),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }

            val policy = uiState.policy
            val controlsEnabled = uiState.connected && uiState.supported && !uiState.saving
            when {
                // Capability verdict is only trustworthy once push_config landed.
                uiState.connected && uiState.capabilitiesKnown && !uiState.supported ->
                    CardHint("This relay does not expose a notification policy.")
                policy != null -> {
                    if (!uiState.connected) {
                        CardHint(
                            "This relay is offline — reconnect to change " +
                                "its notification policy.",
                        )
                    }
                    PolicyControls(
                        uiState = uiState,
                        policy = policy,
                        enabled = controlsEnabled,
                        onCategoryChange = onCategoryChange,
                        onSettleMs = onSettleMs,
                        onCooldownMs = onCooldownMs,
                        onSnoozeOff = onSnoozeOff,
                        onSnoozeFor = onSnoozeFor,
                        onSnoozeIndefinitely = onSnoozeIndefinitely,
                        onUpdateOnce = onUpdateOnce,
                        onSendTest = onSendTest,
                    )
                }
                else -> when {
                    !uiState.connected -> CardHint(
                        if (uiState.connecting) {
                            "Connecting — the notification policy loads once the relay answers."
                        } else {
                            "Offline — the notification policy appears once this relay connects."
                        },
                    )
                    uiState.loadFailed -> Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier
                            .fillMaxWidth()
                            .padding(horizontal = spacing.medium),
                    ) {
                        Text(
                            "Could not load the notification policy.",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.error,
                            modifier = Modifier.weight(1f),
                        )
                        TextButton(onClick = onRetry) { Text("Retry") }
                    }
                    else -> CardHint("Loading the notification policy…")
                }
            }
        }
    }
}

/** The editable policy — categories, timings, snooze, updates, and the test row. */
@Composable
private fun PolicyControls(
    uiState: PushPolicyUiState,
    policy: PushPolicyUi,
    enabled: Boolean,
    onCategoryChange: (String, Boolean) -> Unit,
    onSettleMs: (Long) -> Unit,
    onCooldownMs: (Long) -> Unit,
    onSnoozeOff: () -> Unit,
    onSnoozeFor: (Long) -> Unit,
    onSnoozeIndefinitely: () -> Unit,
    onUpdateOnce: (Boolean) -> Unit,
    onSendTest: () -> Unit,
) {
    val spacing = LerdrTheme.spacing
    CONFIGURABLE_CATEGORIES.forEach { category ->
        // Row-owned toggle — the whole row is the touch target and reads
        // as one "on/off" Switch; the trailing Switch is display-only.
        ListItem(
            modifier = Modifier.toggleable(
                value = policy.categories[category.key] == true,
                enabled = enabled,
                role = Role.Switch,
                onValueChange = { onCategoryChange(category.key, it) },
            ),
            headlineContent = { Text(category.label) },
            supportingContent = { Text(category.detail) },
            trailingContent = {
                Switch(
                    checked = policy.categories[category.key] == true,
                    onCheckedChange = null,
                    enabled = enabled,
                )
            },
            colors = cardItemColors(),
        )
    }

    ListItem(
        headlineContent = { Text("Settle delay") },
        supportingContent = {
            PolicyChips(
                options = optionsWithCurrent(SETTLE_OPTIONS, policy.settleMs),
                selected = policy.settleMs,
                enabled = enabled,
                onSelect = onSettleMs,
            )
        },
        colors = cardItemColors(),
    )
    ListItem(
        headlineContent = { Text("Cooldown") },
        supportingContent = {
            PolicyChips(
                options = optionsWithCurrent(COOLDOWN_OPTIONS, policy.cooldownMs),
                selected = policy.cooldownMs,
                enabled = enabled,
                onSelect = onCooldownMs,
            )
        },
        colors = cardItemColors(),
    )
    ListItem(
        headlineContent = { Text("Snooze") },
        supportingContent = {
            SnoozeChips(
                policy = policy,
                enabled = enabled,
                onSnoozeOff = onSnoozeOff,
                onSnoozeFor = onSnoozeFor,
                onSnoozeIndefinitely = onSnoozeIndefinitely,
            )
        },
        colors = cardItemColors(),
    )
    ListItem(
        modifier = Modifier.toggleable(
            value = policy.updateOnce,
            enabled = enabled,
            role = Role.Switch,
            onValueChange = onUpdateOnce,
        ),
        headlineContent = { Text("Update alerts") },
        supportingContent = {
            Text("Only the first notification per relay version reaches this device.")
        },
        trailingContent = {
            Switch(
                checked = policy.updateOnce,
                onCheckedChange = null,
                enabled = enabled,
            )
        },
        colors = cardItemColors(),
    )

    uiState.policyError?.let { error ->
        Text(
            error,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.error,
            modifier = Modifier
                .liveRegionPolite()
                .padding(horizontal = spacing.medium),
        )
    }

    Column(modifier = Modifier.padding(horizontal = spacing.medium)) {
        Spacer(Modifier.height(spacing.small))
        FilledTonalButton(
            onClick = onSendTest,
            enabled = enabled && uiState.test != PushTestUi.Sending,
        ) {
            Text("Send test")
        }
        Spacer(Modifier.height(spacing.extraSmall))
        Text(
            testMessage(uiState.test),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

/** One wrapping row of FilterChips — the Compose answer to the oracle's `<select>`. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun PolicyChips(
    options: List<Pair<Long, String>>,
    selected: Long,
    enabled: Boolean,
    onSelect: (Long) -> Unit,
) {
    val spacing = LerdrTheme.spacing
    FlowRow(
        horizontalArrangement = Arrangement.spacedBy(spacing.small),
        verticalArrangement = Arrangement.spacedBy(spacing.extraSmall),
        modifier = Modifier.fillMaxWidth(),
    ) {
        options.forEach { (value, label) ->
            FilterChip(
                selected = value == selected,
                onClick = { onSelect(value) },
                label = { Text(label) },
                enabled = enabled,
            )
        }
    }
}

/**
 * The oracle's snooze `<select>` as a chip group: Not snoozed / a display-only
 * "Until …" chip while a timed snooze is armed / the fixed durations /
 * "Until I turn it back on".
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun SnoozeChips(
    policy: PushPolicyUi,
    enabled: Boolean,
    onSnoozeOff: () -> Unit,
    onSnoozeFor: (Long) -> Unit,
    onSnoozeIndefinitely: () -> Unit,
) {
    val spacing = LerdrTheme.spacing
    FlowRow(
        horizontalArrangement = Arrangement.spacedBy(spacing.small),
        verticalArrangement = Arrangement.spacedBy(spacing.extraSmall),
        modifier = Modifier.fillMaxWidth(),
    ) {
        FilterChip(
            selected = !policy.snoozed,
            onClick = onSnoozeOff,
            label = { Text("Not snoozed") },
            enabled = enabled,
        )
        if (policy.snoozed && policy.snoozeUntil != null) {
            FilterChip(
                selected = true,
                onClick = {},
                label = { Text("Until ${formatSnoozeUntil(policy.snoozeUntil)}") },
                enabled = false,
            )
        }
        SNOOZE_DURATIONS.forEach { (durationMs, label) ->
            FilterChip(
                selected = false,
                onClick = { onSnoozeFor(durationMs) },
                label = { Text(label) },
                enabled = enabled,
            )
        }
        FilterChip(
            selected = policy.snoozed && policy.snoozeUntil == null,
            onClick = onSnoozeIndefinitely,
            label = { Text("Until I turn it back on") },
            enabled = enabled,
        )
    }
}

@Composable
private fun CardHint(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = LerdrTheme.spacing.medium)
            .padding(top = LerdrTheme.spacing.small),
    )
}

/** ListItems inside a card read flat — drop the default surface tint. */
@Composable
private fun cardItemColors() = ListItemDefaults.colors(
    containerColor = Color.Transparent,
)

/** Keep a foreign wire value selectable — the oracle's select would blank it. */
private fun optionsWithCurrent(
    options: List<Pair<Long, String>>,
    current: Long,
): List<Pair<Long, String>> =
    if (options.any { it.first == current }) {
        options
    } else {
        options + (current to "$current ms")
    }

/** `new Date(snooze_until).toLocaleString()` — fixed, locale-stable format. */
private fun formatSnoozeUntil(rfc3339: String): String = try {
    OffsetDateTime.parse(rfc3339)
        .format(DateTimeFormatter.ofPattern("MMM d yyyy, HH:mm", Locale.ENGLISH))
} catch (invalid: Exception) {
    rfc3339
}

/** Oracle `testMessage` — relay-side acceptance only, verbatim copy. */
private fun testMessage(test: PushTestUi): String = when (test) {
    PushTestUi.Sending -> "Asking the relay service to send a neutral test…"
    is PushTestUi.Accepted -> {
        val verb = if (test.result == "queued") "queued" else "accepted"
        "The relay service $verb the test. This does not confirm that " +
            "the phone displayed it."
    }
    is PushTestUi.Rejected -> "The relay could not accept the test (${test.code})."
    PushTestUi.Idle ->
        "A test reports relay service acceptance only; it cannot confirm " +
            "handset display or human delivery."
}

@PreviewLightDark
@Composable
private fun PushPolicyContentPreview() {
    LerdrTheme {
        Column(modifier = Modifier.padding(LerdrTheme.spacing.medium)) {
            PushPolicyContent(
                uiState = PushPolicyUiState(
                    relayId = "r1",
                    relayLabel = "workstation",
                    connected = true,
                    capabilitiesKnown = true,
                    supported = true,
                    policy = PushPolicyUi(
                        deviceId = "dev-1",
                        categories = PushPolicyUi.DEFAULT_CATEGORIES +
                            ("finished" to true),
                        snoozed = true,
                        snoozeUntil = "2030-01-01T00:00:00Z",
                    ),
                ),
                onCategoryChange = { _, _ -> },
                onSettleMs = {},
                onCooldownMs = {},
                onSnoozeOff = {},
                onSnoozeFor = {},
                onSnoozeIndefinitely = {},
                onUpdateOnce = {},
                onSendTest = {},
                onRetry = {},
            )
        }
    }
}
