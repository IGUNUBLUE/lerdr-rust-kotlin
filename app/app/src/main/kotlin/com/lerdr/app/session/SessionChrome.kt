package com.lerdr.app.session

import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateDpAsState
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.AccountTree
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Tune
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.lerdr.app.session.manage.ManageSheet
import com.lerdr.core.designsystem.theme.LerdrTheme
import dagger.hilt.android.EntryPointAccessors
import kotlinx.coroutines.launch

/** Agent-session render modes (docs/04 §Agent session). */
enum class SessionMode(val label: String) {
    FEED("Feed"),
    TERMINAL("Terminal"),
    FILES("Files"),
}

/**
 * Status-chip visual variant — the mockup's "morphing" statuses:
 * lease (amber), waiting (pulsing cookie), error (sharp), neutral (pill).
 */
enum class SessionStatusVariant {
    NEUTRAL,
    LEASE,
    WAITING,
    ERROR,
}

/**
 * Maps the caller-provided [statusLabel] onto a variant: terminal leases
 * render `lease 92×42`, blocked/waiting agents report `blocked`/`waiting`,
 * disconnects surface `offline`/`error`/`failed`.
 */
fun statusVariantOf(label: String): SessionStatusVariant {
    val normalized = label.trim().lowercase()
    return when {
        normalized.startsWith("lease") -> SessionStatusVariant.LEASE
        normalized.contains("blocked") ||
            normalized.contains("waiting") ||
            normalized.contains("attention") -> SessionStatusVariant.WAITING
        normalized.contains("error") ||
            normalized.contains("offline") ||
            normalized.contains("disconnected") ||
            normalized.contains("failed") ||
            normalized.contains("unauthorized") -> SessionStatusVariant.ERROR
        else -> SessionStatusVariant.NEUTRAL
    }
}

/**
 * One entry in the session ⋯ overflow menu — mode-specific actions like
 * "Find in terminal" or "Refresh" that don't fit the mockup's lean bar.
 */
data class SessionBarAction(
    val label: String,
    val icon: ImageVector,
    val onClick: () -> Unit,
)

/**
 * Shared session chrome — agent name (tap-to-rename via `agent_rename`
 * for controllers), workspace breadcrumb, morphing status chip, connection
 * dot, the `Feed | Terminal | Files` segmented switch, and a single ⋯
 * overflow menu carrying the mode actions plus worktrees/manage entries.
 *
 * The bar stays close to the mockup — title + chip — so the session name
 * and rename affordance keep their space even on narrow panes; everything
 * else lives under the overflow menu.
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
    provider: String? = null,
    /** Pulses the avatar ring while the agent works / the pane is live. */
    active: Boolean = false,
    actions: List<SessionBarAction> = emptyList(),
    /**
     * When set, the workspace tab strip renders under the mode switch, the
     * title becomes renameable for controllers, and the ⋯ menu gains the
     * worktrees + manage entries. The strip self-hides when the pane's
     * workspace has a single tab.
     */
    tabsPaneId: String? = null,
    onSelectTab: (lerdr.core.store.Agent) -> Unit = {},
    /** Chip variant — derived from [statusLabel] by default. */
    statusVariant: SessionStatusVariant = statusVariantOf(statusLabel),
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
                Row(verticalAlignment = Alignment.CenterVertically) {
                    com.lerdr.app.ui.ProviderBadge(
                        provider = provider,
                        label = title,
                        size = 40.dp,
                        prominent = true,
                        active = active,
                    )
                    Spacer(Modifier.width(spacing.small))
                    SessionTitle(
                        paneId = tabsPaneId,
                        title = title,
                        breadcrumb = breadcrumb,
                        modifier = Modifier.weight(1f),
                    )
                }
            },
            actions = {
                SessionOverflowMenu(paneId = tabsPaneId, actions = actions)
                StatusChip(
                    label = statusLabel,
                    color = statusColor,
                    variant = statusVariant,
                )
                Spacer(Modifier.width(spacing.medium))
            },
        )
        SessionModeSwitch(
            mode = mode,
            onSelectMode = onSelectMode,
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
 * Editable session title — controllers tap the name (or pencil) to swap in
 * an inline field wired to `agent_rename` (the oracle's `renameTab`).
 * Readers and the no-agent case render plain text. The breadcrumb stays
 * put either way.
 */
@Composable
private fun SessionTitle(
    paneId: String?,
    title: String,
    breadcrumb: String,
    modifier: Modifier = Modifier,
) {
    if (paneId == null) {
        TitleColumn(title = title, breadcrumb = breadcrumb, modifier = modifier)
        return
    }
    val appContext = LocalContext.current.applicationContext
    val entryPoint = remember(appContext) {
        EntryPointAccessors.fromApplication(
            appContext,
            WorktreesEntryPoint::class.java,
        )
    }
    val repository = remember(entryPoint) { entryPoint.sessionRepository() }
    val relayId = paneId.substringBefore("::")
    val connection by repository.connection(relayId)
        .collectAsStateWithLifecycle(initialValue = null)
    val canControl = remember(connection) { repository.canControl(relayId) }
    if (!canControl) {
        TitleColumn(title = title, breadcrumb = breadcrumb, modifier = modifier)
        return
    }

    var editing by remember { mutableStateOf(false) }
    var draft by remember { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    var renameError by remember { mutableStateOf<String?>(null) }
    val scope = rememberCoroutineScope()

    if (editing) {
        Dialog(
            onDismissRequest = {
                if (!busy) {
                    editing = false
                    renameError = null
                }
            },
        ) {
            SessionTitleEditor(
                draft = draft,
                busy = busy,
                error = renameError,
                onDraftChange = { draft = it; renameError = null },
                onConfirm = {
                    val name = draft.trim()
                    if (name.isEmpty()) {
                        renameError = "Enter a new name."
                        return@SessionTitleEditor
                    }
                    busy = true
                    scope.launch {
                        try {
                            repository.renameAgent(paneId, name)
                            editing = false
                            renameError = null
                        } catch (failure: Exception) {
                            renameError = failure.message
                                ?: "The rename could not be sent"
                        } finally {
                            busy = false
                        }
                    }
                },
                onCancel = {
                    editing = false
                    renameError = null
                },
            )
        }
    }

    TitleColumn(
        title = title,
        breadcrumb = breadcrumb,
        editable = true,
        onEdit = {
            draft = title
            renameError = null
            editing = true
        },
        modifier = modifier,
    )
}

/** Title + breadcrumb; `editable` adds the pencil affordance + click. */
@Composable
private fun TitleColumn(
    title: String,
    breadcrumb: String,
    modifier: Modifier = Modifier,
    editable: Boolean = false,
    onEdit: () -> Unit = {},
) {
    Column(
        modifier = if (editable) {
            modifier.clickable(
                onClickLabel = "Rename session",
                role = Role.Button,
                onClick = onEdit,
            )
        } else {
            modifier
        },
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                title,
                style = MaterialTheme.typography.titleMedium,
                maxLines = 1,
            )
            if (editable) {
                Spacer(Modifier.width(LerdrTheme.spacing.extraSmall))
                Icon(
                    Icons.Default.Edit,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.size(14.dp),
                )
            }
        }
        Text(
            breadcrumb,
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            maxLines = 1,
        )
    }
}

/**
 * Rename dialog content — `agent_rename{name}` on Done/Save. Stateless so
 * screenshot tests render it directly; the caller wraps it in a `Dialog`.
 */
@Composable
fun SessionTitleEditor(
    draft: String,
    busy: Boolean,
    error: String?,
    onDraftChange: (String) -> Unit,
    onConfirm: () -> Unit,
    onCancel: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val focusRequester = remember { FocusRequester() }
    Surface(
        shape = MaterialTheme.shapes.extraLarge,
        tonalElevation = 6.dp,
        modifier = modifier.testTag("session-title:dialog"),
    ) {
        Column(
            modifier = Modifier.padding(LerdrTheme.spacing.large),
            verticalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.medium),
        ) {
            Text(
                "Rename session",
                style = MaterialTheme.typography.titleLarge,
            )
            OutlinedTextField(
                value = draft,
                onValueChange = onDraftChange,
                modifier = Modifier
                    .fillMaxWidth()
                    .focusRequester(focusRequester)
                    .testTag("session-title:field"),
                textStyle = MaterialTheme.typography.titleMedium,
                singleLine = true,
                enabled = !busy,
                isError = error != null,
                supportingText = error?.let { { Text(it) } },
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
                keyboardActions = KeyboardActions(onDone = { onConfirm() }),
            )
            Row(
                horizontalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.extraSmall),
                modifier = Modifier.align(Alignment.End),
            ) {
                TextButton(
                    onClick = onCancel,
                    enabled = !busy,
                    modifier = Modifier.testTag("session-title:cancel"),
                ) {
                    Text("Cancel")
                }
                TextButton(
                    onClick = onConfirm,
                    enabled = !busy,
                    modifier = Modifier.testTag("session-title:confirm"),
                ) {
                    Text("Save")
                }
            }
        }
    }
    LaunchedEffect(Unit) { focusRequester.requestFocus() }
}

/**
 * ⋯ overflow menu for the session bar — the mockup keeps the bar lean
 * (title + status chip), so mode actions ([actions]) and the pane
 * affordances (worktrees, manage) live here.
 *
 * "Manage worktrees" resolves the pane's agent for its relay + workspace
 * ids and hides while they are absent (e.g. inventory loading).
 * "Manage session" opens [ManageSheet] — the oracle's `ManageDialog` port
 * (`agent_rename` / `agent_restart` / `agent_clear` / `agent_stop` /
 * `copy_agent_response` + pane metadata). The sheet hides mutations for
 * readers itself, so the entry renders for every role.
 *
 * Renders nothing when there is no pane and no caller action.
 */
@Composable
private fun SessionOverflowMenu(
    paneId: String?,
    actions: List<SessionBarAction>,
) {
    var menuOpen by remember { mutableStateOf(false) }
    var showManage by remember { mutableStateOf(false) }
    var showWorktrees by remember { mutableStateOf(false) }

    val worktreesTarget = if (paneId != null) {
        val appContext = LocalContext.current.applicationContext
        val entryPoint = remember(appContext) {
            EntryPointAccessors.fromApplication(
                appContext,
                WorktreesEntryPoint::class.java,
            )
        }
        val agent by entryPoint.sessionRepository().agent(paneId)
            .collectAsStateWithLifecycle(initialValue = null)
        agent?.let {
            val workspaceId = it.workspaceId.takeIf(String::isNotEmpty)
            val relayId = it.relayId.takeIf(String::isNotEmpty)
            if (workspaceId != null && relayId != null) relayId to workspaceId else null
        }
    } else {
        null
    }

    if (actions.isEmpty() && paneId == null) return

    IconButton(
        onClick = { menuOpen = true },
        modifier = Modifier.testTag("session-bar:overflow"),
    ) {
        Icon(
            Icons.Default.MoreVert,
            contentDescription = "Session actions",
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
    DropdownMenu(
        expanded = menuOpen,
        onDismissRequest = { menuOpen = false },
    ) {
        actions.forEach { action ->
            DropdownMenuItem(
                text = { Text(action.label) },
                leadingIcon = {
                    Icon(action.icon, contentDescription = null)
                },
                onClick = {
                    menuOpen = false
                    action.onClick()
                },
            )
        }
        if (paneId != null) {
            if (actions.isNotEmpty()) HorizontalDivider()
            worktreesTarget?.let {
                DropdownMenuItem(
                    text = { Text("Manage worktrees") },
                    leadingIcon = {
                        Icon(Icons.Default.AccountTree, contentDescription = null)
                    },
                    onClick = {
                        menuOpen = false
                        showWorktrees = true
                    },
                    modifier = Modifier.testTag("session-bar:worktrees"),
                )
            }
            DropdownMenuItem(
                text = { Text("Manage session") },
                leadingIcon = {
                    Icon(Icons.Default.Tune, contentDescription = null)
                },
                onClick = {
                    menuOpen = false
                    showManage = true
                },
                modifier = Modifier.testTag("session-bar:manage"),
            )
        }
    }
    if (showManage && paneId != null) {
        ManageSheet(paneId = paneId, onDismiss = { showManage = false })
    }
    worktreesTarget?.let { (relayId, workspaceId) ->
        if (showWorktrees) {
            WorktreesSheet(
                relayId = relayId,
                workspaceId = workspaceId,
                onDismiss = { showWorktrees = false },
            )
        }
    }
}

/**
 * Feed/Terminal/Files switch — the mockup's light-blue pill with dark text
 * for the selected segment. Uses `primary`/`onPrimary` so the flagship dark
 * theme renders the #A9C6F8 pill; light theme falls back to a filled
 * medium-blue segment (standard M3 selected treatment).
 */
@Composable
private fun SessionModeSwitch(
    mode: SessionMode,
    onSelectMode: (SessionMode) -> Unit,
    modifier: Modifier = Modifier,
) {
    SingleChoiceSegmentedButtonRow(modifier = modifier) {
        SessionMode.entries.forEachIndexed { index, option ->
            SegmentedButton(
                selected = index == mode.ordinal,
                onClick = { onSelectMode(option) },
                shape = SegmentedButtonDefaults.itemShape(
                    index = index,
                    count = SessionMode.entries.size,
                ),
                colors = SegmentedButtonDefaults.colors(
                    activeContainerColor = MaterialTheme.colorScheme.primary,
                    activeContentColor = MaterialTheme.colorScheme.onPrimary,
                    activeBorderColor = MaterialTheme.colorScheme.primary,
                ),
                modifier = Modifier.testTag("session-mode:${option.label.lowercase()}"),
            ) {
                Text(option.label)
            }
        }
    }
}

/**
 * Status pill — morphs per [SessionStatusVariant]: neutral keeps the
 * caller's tint on a full pill, `lease`/`waiting` go amber, `error` goes
 * danger with a sharp corner; `waiting` also pulses the dot.
 */
@Composable
private fun StatusChip(
    label: String,
    color: Color,
    variant: SessionStatusVariant,
) {
    val colors = LerdrTheme.extendedColors
    val accent = when (variant) {
        SessionStatusVariant.LEASE,
        SessionStatusVariant.WAITING -> colors.attention
        SessionStatusVariant.ERROR -> colors.danger
        SessionStatusVariant.NEUTRAL -> color
    }
    val containerColor by animateColorAsState(
        targetValue = accent.copy(alpha = 0.18f),
        animationSpec = tween(350),
        label = "chip-container",
    )
    val contentColor by animateColorAsState(
        targetValue = accent,
        animationSpec = tween(350),
        label = "chip-content",
    )
    val corner by animateDpAsState(
        targetValue = when (variant) {
            SessionStatusVariant.WAITING -> 10.dp
            SessionStatusVariant.ERROR -> 3.dp
            else -> 24.dp
        },
        animationSpec = tween(350),
        label = "chip-corner",
    )
    val dotAlpha = if (variant == SessionStatusVariant.WAITING) {
        val transition = rememberInfiniteTransition(label = "chip-pulse")
        transition.animateFloat(
            initialValue = 1f,
            targetValue = 0.25f,
            animationSpec = infiniteRepeatable(tween(700), RepeatMode.Reverse),
            label = "chip-dot",
        ).value
    } else {
        1f
    }
    val shape: Shape = RoundedCornerShape(corner)
    Surface(
        color = containerColor,
        contentColor = contentColor,
        shape = shape,
        modifier = Modifier
            .semantics { liveRegion = LiveRegionMode.Polite }
            .testTag("session-status:$label"),
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
                    .background(contentColor.copy(alpha = dotAlpha)),
            )
            Spacer(Modifier.width(LerdrTheme.spacing.extraSmall))
            Text(label, style = MaterialTheme.typography.labelMedium)
        }
    }
}
