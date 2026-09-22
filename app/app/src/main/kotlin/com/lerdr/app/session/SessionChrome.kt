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
import androidx.compose.material.icons.filled.AccountTree
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
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
    /**
     * When set, the workspace tab strip renders under the mode switch and
     * a worktrees entry joins the bar actions. The strip self-hides when
     * the pane's workspace has a single tab.
     */
    tabsPaneId: String? = null,
    onSelectTab: (lerdr.core.store.Agent) -> Unit = {},
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
                if (tabsPaneId != null) {
                    WorktreesEntryButton(tabsPaneId)
                }
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
        if (tabsPaneId != null) {
            WorkspaceTabsStrip(paneId = tabsPaneId, onSelectTab = onSelectTab)
        }
    }
}

/**
 * Worktrees entry for the session bar — resolves the pane's agent for
 * its relay + workspace ids, then opens [WorktreesSheet]. Hidden when
 * the agent row or its workspace is absent (e.g. inventory loading).
 */
@Composable
private fun WorktreesEntryButton(paneId: String) {
    val appContext = LocalContext.current.applicationContext
    val entryPoint = androidx.compose.runtime.remember(appContext) {
        dagger.hilt.android.EntryPointAccessors.fromApplication(
            appContext,
            WorktreesEntryPoint::class.java,
        )
    }
    val agent by entryPoint.sessionRepository().agent(paneId)
        .collectAsStateWithLifecycle(initialValue = null)
    val workspaceId = agent?.workspaceId?.takeIf { it.isNotEmpty() } ?: return
    val relayId = agent?.relayId ?: return
    var showSheet by androidx.compose.runtime.remember { androidx.compose.runtime.mutableStateOf(false) }
    IconButton(onClick = { showSheet = true }) {
        Icon(
            Icons.Default.AccountTree,
            contentDescription = "Manage worktrees",
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
    if (showSheet) {
        WorktreesSheet(
            relayId = relayId,
            workspaceId = workspaceId,
            onDismiss = { showSheet = false },
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
