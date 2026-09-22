package com.lerdr.app.session

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import com.lerdr.core.designsystem.components.LerdrSegmentedControl
import com.lerdr.core.designsystem.theme.LerdrTheme

/** Agent-session render modes (docs/04 §Agent session). */
enum class SessionMode(val label: String) {
    FEED("Feed"),
    TERMINAL("Terminal"),
    FILES("Files"),
}

/**
 * Shared session chrome — agent name, workspace breadcrumb, status chip,
 * connection dot, and the `Feed | Terminal | Files` segmented switch.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SessionTopBar(
    title: String,
    breadcrumb: String,
    statusLabel: String,
    statusColor: Color,
    mode: SessionMode,
    onSelectMode: (SessionMode) -> Unit,
    onBack: () -> Unit,
    trailing: (@Composable () -> Unit)? = null,
) {
    val spacing = LerdrTheme.spacing
    Column {
        TopAppBar(
            navigationIcon = {
                IconButton(onClick = onBack) {
                    Icon(
                        Icons.AutoMirrored.Filled.ArrowBack,
                        contentDescription = "Back",
                    )
                }
            },
            title = {
                Column {
                    Text(
                        title,
                        style = MaterialTheme.typography.titleMedium,
                        maxLines = 1,
                    )
                    Text(
                        breadcrumb,
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                    )
                }
            },
            actions = {
                trailing?.invoke()
                StatusChip(label = statusLabel, color = statusColor)
                Spacer(Modifier.width(spacing.medium))
            },
        )
        LerdrSegmentedControl(
            options = SessionMode.entries.map { it.label },
            selectedIndex = mode.ordinal,
            onSelect = { onSelectMode(SessionMode.entries[it]) },
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = spacing.medium, vertical = spacing.small),
        )
    }
}

@Composable
private fun StatusChip(label: String, color: Color) {
    Surface(
        color = color.copy(alpha = 0.18f),
        contentColor = color,
        shape = CircleShape,
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.padding(
                horizontal = LerdrTheme.spacing.small,
                vertical = LerdrTheme.spacing.extraSmall,
            ),
        ) {
            Box(
                modifier = Modifier
                    .size(6.dp)
                    .clip(CircleShape)
                    .background(color),
            )
            Spacer(Modifier.width(LerdrTheme.spacing.extraSmall))
            Text(label, style = MaterialTheme.typography.labelMedium)
        }
    }
}
