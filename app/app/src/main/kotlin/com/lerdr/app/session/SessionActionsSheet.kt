package com.lerdr.app.session

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import com.lerdr.core.designsystem.theme.LerdrTheme

/** One scrollable action surface; large text never truncates action labels. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun SessionActionsSheet(
    actions: List<SessionBarAction>,
    onDismiss: () -> Unit,
    title: String = "Session actions",
) {
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
    ) {
        Column(
            modifier = Modifier
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp)
                .padding(bottom = 24.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Text(title, style = MaterialTheme.typography.headlineSmall,
                modifier = Modifier.semantics { heading() })
            actions.groupBy { it.group }.forEach { (group, entries) ->
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(
                        group,
                        style = MaterialTheme.typography.labelLarge,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(horizontal = 8.dp).semantics { heading() },
                    )
                    Surface(
                        shape = MaterialTheme.shapes.large,
                        color = MaterialTheme.colorScheme.surfaceContainerLow,
                    ) {
                        Column {
                            entries.forEachIndexed { index, action ->
                                if (index > 0) HorizontalDivider(
                                    modifier = Modifier.padding(start = 56.dp, end = 16.dp),
                                    color = MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.4f),
                                )
                                SessionActionRow(action) {
                                    onDismiss()
                                    action.onClick()
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

@Composable
internal fun SessionActionRow(action: SessionBarAction, onClick: () -> Unit = action.onClick) {
    val tint = if (action.destructive) LerdrTheme.extendedColors.danger
        else MaterialTheme.colorScheme.onSurface
    ListItem(
        headlineContent = { Text(action.label, color = tint) },
        supportingContent = action.description?.let { description -> { Text(description) } },
        leadingContent = { Icon(action.icon, contentDescription = null, tint = tint) },
        colors = ListItemDefaults.colors(containerColor = Color.Transparent),
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = 56.dp)
            .then(action.tag?.let { Modifier.testTag(it) } ?: Modifier)
            .clickable(role = Role.Button, onClick = onClick),
    )
}
