package com.lerdr.app.home

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.CreateNewFolder
import androidx.compose.material.icons.filled.Dns
import androidx.compose.material.icons.filled.History
import androidx.compose.material.icons.filled.PhoneAndroid
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.SmartToy
import androidx.compose.material.icons.filled.Stop
import androidx.compose.material.icons.filled.Terminal
import androidx.compose.material.icons.filled.Visibility
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.AssistChip
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Surface
import androidx.compose.material3.SwipeToDismissBox
import androidx.compose.material3.SwipeToDismissBoxValue
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.material3.pulltorefresh.PullToRefreshDefaults
import androidx.compose.material3.pulltorefresh.rememberPullToRefreshState
import androidx.compose.material3.rememberSwipeToDismissBoxState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.PreviewLightDark
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lerdr.app.di.AppEntryPoint
import com.lerdr.app.ui.ProviderBadge
import com.lerdr.app.nav.LerdrNavBadges
import com.lerdr.app.nav.rememberLerdrNavBadges
import com.lerdr.app.nav.topLevelNavItems
import com.lerdr.core.designsystem.components.LerdrShortNavigationBar
import com.lerdr.core.designsystem.components.LerdrWavyProgressIndicator
import com.lerdr.core.designsystem.theme.LerdrTheme
import com.lerdr.navigation.LerdrKey
import dagger.hilt.android.EntryPointAccessors
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.emptyFlow
import kotlinx.coroutines.flow.merge
import lerdr.core.model.Interaction
import lerdr.core.model.Option

/**
 * Mission control (docs/04 §Home): needs-you rail with inline answers,
 * agents grouped by `relay ▸ workspace`, an expandable "New" FAB hosting
 * the launch sheets, bottom nav. [HomeScreen] owns the ViewModel seam;
 * [HomeContent] is pure state → previews and Roborazzi shots stay honest.
 */
@Composable
fun HomeScreen(
    onOpenAgent: (String) -> Unit,
    onSelectTopLevel: (LerdrKey) -> Unit,
) {
    // hilt-navigation-compose is absent — pull the bound repositories
    // through the singleton entry point.
    val appContext = LocalContext.current.applicationContext
    val entryPoint = remember {
        EntryPointAccessors.fromApplication(appContext, AppEntryPoint::class.java)
    }
    val viewModel: HomeViewModel = viewModel {
        HomeViewModel(entryPoint.homeRepository(), entryPoint.sessionRepository())
    }
    val launchViewModel: LaunchViewModel = viewModel(key = "home-launch") {
        val launch = EntryPointAccessors.fromApplication(
            appContext,
            LaunchEntryPoint::class.java,
        )
        LaunchViewModel(launch.sessionRepository(), launch.workspaceStore())
    }
    val uiState by viewModel.uiState.collectAsStateWithLifecycle()
    val refreshing by viewModel.inventoryRefreshing.collectAsStateWithLifecycle()
    var sheet by rememberSaveable { mutableStateOf<HomeSheet?>(null) }
    var homeReselects by remember { mutableStateOf(0) }
    val messages = remember(viewModel, launchViewModel) {
        merge(viewModel.messages, launchViewModel.messages)
    }
    LaunchedEffect(launchViewModel) {
        launchViewModel.events.collect { event ->
            when (event) {
                is LaunchViewModel.LaunchEvent.Launched -> {
                    sheet = null
                    onOpenAgent(event.paneId)
                }
                LaunchViewModel.LaunchEvent.Dismissed -> sheet = null
            }
        }
    }
    HomeContent(
        uiState = uiState,
        onOpenAgent = onOpenAgent,
        onSelectTopLevel = { key ->
            // Re-tapping Agents while on Home scrolls the list back to top.
            if (key == LerdrKey.Home) homeReselects++
            onSelectTopLevel(key)
        },
        homeReselects = homeReselects,
        badges = rememberLerdrNavBadges(),
        onRespond = viewModel::respond,
        onAnswerOption = viewModel::answerQuestion,
        onStopAgent = viewModel::stopAgent,
        onNewAgent = {
            launchViewModel.beginAgent()
            sheet = HomeSheet.AGENT
        },
        onNewWorkspace = {
            launchViewModel.beginWorkspace()
            sheet = HomeSheet.WORKSPACE
        },
        refreshing = refreshing,
        onRefreshInventory = viewModel::refreshInventory,
        messages = messages,
    )
    when (sheet) {
        HomeSheet.AGENT -> NewAgentSheet(
            viewModel = launchViewModel,
            onDismiss = { sheet = null },
        )
        HomeSheet.WORKSPACE -> NewWorkspaceSheet(
            viewModel = launchViewModel,
            onDismiss = { sheet = null },
        )
        null -> Unit
    }
}

/** Which launch sheet the Home FAB opened. */
private enum class HomeSheet { AGENT, WORKSPACE }

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun HomeContent(
    uiState: HomeUiState,
    onOpenAgent: (String) -> Unit,
    onSelectTopLevel: (LerdrKey) -> Unit,
    homeReselects: Int = 0,
    onRespond: (AttentionCardUi, Int) -> Unit = { _, _ -> },
    onAnswerOption: (AttentionCardUi, Int) -> Unit = { _, _ -> },
    onStopAgent: (AgentListItemUi) -> Unit = {},
    onNewAgent: () -> Unit = {},
    onNewWorkspace: () -> Unit = {},
    refreshing: Boolean = false,
    onRefreshInventory: () -> Unit = {},
    messages: Flow<String> = emptyFlow(),
    badges: LerdrNavBadges = LerdrNavBadges(),
) {
    val spacing = LerdrTheme.spacing
    val snackbarHostState = remember { SnackbarHostState() }
    var pendingStop by remember { mutableStateOf<AgentListItemUi?>(null) }
    var fabExpanded by rememberSaveable { mutableStateOf(false) }

    LaunchedEffect(messages) {
        messages.collect { snackbarHostState.showSnackbar(it) }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Column {
                        Text("Agents", style = MaterialTheme.typography.headlineMedium)
                        if (uiState.relaySummary.isNotEmpty()) {
                            Text(
                                uiState.relaySummary,
                                style = MaterialTheme.typography.bodyMedium,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                },
                actions = {
                    if (uiState.live) {
                        LiveChip(modifier = Modifier.padding(end = spacing.medium))
                    }
                },
            )
        },
        bottomBar = {
            LerdrShortNavigationBar(
                items = topLevelNavItems(
                    selected = LerdrKey.Home,
                    badges = badges,
                    onSelect = onSelectTopLevel,
                ),
            )
        },
        floatingActionButton = {
            HomeFabMenu(
                expanded = fabExpanded,
                onExpandedChange = { fabExpanded = it },
                onNewAgent = onNewAgent,
                onNewWorkspace = onNewWorkspace,
            )
        },
        snackbarHost = { SnackbarHost(snackbarHostState) },
    ) { innerPadding ->
        // The oracle's pull-to-refresh (AgentList.svelte): the gesture arms
        // only at scroll top — PullToRefreshBox's nested scroll gives that
        // for free — releases past threshold fire `requestInventoryRefresh`
        // plus the trigger haptic. `refreshing` is the ~900 ms hold window.
        val pullState = rememberPullToRefreshState()
        val haptic = LocalHapticFeedback.current
        PullToRefreshBox(
            isRefreshing = refreshing,
            onRefresh = {
                if (!refreshing) {
                    haptic.performHapticFeedback(HapticFeedbackType.LongPress)
                    onRefreshInventory()
                }
            },
            state = pullState,
            modifier = Modifier.fillMaxSize(),
            indicator = {
                PullToRefreshDefaults.Indicator(
                    state = pullState,
                    isRefreshing = refreshing,
                    modifier = Modifier
                        .align(Alignment.TopCenter)
                        // The box underlaps the top app bar — drop the cue
                        // into the list's own top inset.
                        .padding(top = innerPadding.calculateTopPadding()),
                )
            },
        ) {
            val listState = rememberLazyListState()
            // A newly-blocked agent must surface even when the user scrolled
            // the rail out of view — scroll back to the top on new arrivals
            // and when the Agents tab is re-selected.
            var seenAttention by remember { mutableStateOf(setOf<String>()) }
            LaunchedEffect(uiState.needsYou) {
                val current = uiState.needsYou.mapTo(HashSet()) { it.paneId }
                if (current.any { it !in seenAttention }) {
                    listState.animateScrollToItem(0)
                }
                seenAttention = current
            }
            LaunchedEffect(homeReselects) {
                if (homeReselects > 0) listState.animateScrollToItem(0)
            }
            LazyColumn(
                state = listState,
                modifier = Modifier.fillMaxSize(),
                contentPadding = PaddingValues(
                    top = innerPadding.calculateTopPadding(),
                    bottom = innerPadding.calculateBottomPadding() + spacing.medium,
                ),
                verticalArrangement = Arrangement.spacedBy(spacing.small),
            ) {
                if (uiState.needsYou.isNotEmpty()) {
                    item(key = "needs-you-header") {
                        SectionHeader(
                            label = "NEEDS YOU · ${uiState.needsYou.size}",
                            color = LerdrTheme.extendedColors.attention,
                            leading = {
                                Icon(
                                    Icons.Default.Warning,
                                    contentDescription = null,
                                    tint = LerdrTheme.extendedColors.attention,
                                    modifier = Modifier.size(16.dp),
                                )
                            },
                            modifier = Modifier.padding(horizontal = spacing.medium),
                        )
                    }
                    item(key = "needs-you-rail") {
                        LazyRow(
                            contentPadding = PaddingValues(horizontal = spacing.medium),
                            horizontalArrangement = Arrangement.spacedBy(spacing.small),
                        ) {
                            items(uiState.needsYou, key = { it.paneId }) { card ->
                                AttentionCard(
                                    card = card,
                                    onOpen = { onOpenAgent(card.paneId) },
                                    onRespond = { index -> onRespond(card, index) },
                                    onAnswerOption = { index ->
                                        onAnswerOption(card, index)
                                    },
                                )
                            }
                        }
                    }
                }

                if (uiState.working.isNotEmpty()) {
                    item(key = "working-header") {
                        SectionHeader(
                            label = "WORKING · ${uiState.working.sumOf { it.agents.size }}",
                            color = LerdrTheme.extendedColors.working,
                            leading = {
                                StatusDot(color = LerdrTheme.extendedColors.working)
                            },
                            modifier = Modifier.padding(horizontal = spacing.medium),
                        )
                    }
                    uiState.working.forEach { group ->
                        item(key = "working-group:${group.key}") {
                            GroupHeader(
                                group = group,
                                modifier = Modifier.padding(horizontal = spacing.medium),
                            )
                        }
                        items(group.agents, key = { "working:${it.paneId}" }) { agent ->
                            SwipeableAgentRow(
                                agent = agent,
                                onOpen = { onOpenAgent(agent.paneId) },
                                onRequestStop = { pendingStop = agent },
                                modifier = Modifier.padding(horizontal = spacing.medium),
                            )
                        }
                    }
                }

                if (uiState.idle.isNotEmpty()) {
                    item(key = "idle-header") {
                        SectionHeader(
                            label = "IDLE · ${uiState.idle.sumOf { it.agents.size }}",
                            color = LerdrTheme.extendedColors.idle,
                            modifier = Modifier.padding(horizontal = spacing.medium),
                        )
                    }
                    uiState.idle.forEach { group ->
                        item(key = "idle-group:${group.key}") {
                            GroupHeader(
                                group = group,
                                modifier = Modifier.padding(horizontal = spacing.medium),
                            )
                        }
                        items(group.agents, key = { "idle:${it.paneId}" }) { agent ->
                            SwipeableAgentRow(
                                agent = agent,
                                onOpen = { onOpenAgent(agent.paneId) },
                                onRequestStop = { pendingStop = agent },
                                modifier = Modifier.padding(horizontal = spacing.medium),
                            )
                        }
                    }
                }

                if (uiState.needsYou.isEmpty() &&
                    uiState.working.isEmpty() &&
                    uiState.idle.isEmpty()
                ) {
                    item(key = "empty-state") {
                        EmptyState(
                            hasRelays = uiState.relays.isNotEmpty(),
                            modifier = Modifier
                                .fillParentMaxSize()
                                .padding(horizontal = spacing.large),
                        )
                    }
                }
            }
        }
    }

    pendingStop?.let { agent ->
        StopAgentDialog(
            agent = agent,
            onConfirm = {
                pendingStop = null
                onStopAgent(agent)
            },
            onDismiss = { pendingStop = null },
        )
    }
}

/**
 * The expanding "New" FAB — docs/04's `FloatingActionButtonMenu` pattern
 * (labeled "New agent"/"New workspace" mini-actions over a toggle FAB);
 * the M3E widget itself lives only on core:designsystem's expressive
 * artifact, so Home composes the same speed-dial shape by hand.
 */
@Composable
fun HomeFabMenu(
    expanded: Boolean,
    onExpandedChange: (Boolean) -> Unit,
    onNewAgent: () -> Unit,
    onNewWorkspace: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val spacing = LerdrTheme.spacing
    Column(
        horizontalAlignment = Alignment.End,
        verticalArrangement = Arrangement.spacedBy(spacing.small),
        modifier = modifier,
    ) {
        AnimatedVisibility(visible = expanded) {
            Column(
                horizontalAlignment = Alignment.End,
                verticalArrangement = Arrangement.spacedBy(spacing.small),
            ) {
                FabMenuItem(
                    label = "New workspace",
                    icon = {
                        Icon(Icons.Default.CreateNewFolder, contentDescription = null)
                    },
                    onClick = {
                        onExpandedChange(false)
                        onNewWorkspace()
                    },
                )
                FabMenuItem(
                    label = "New agent",
                    icon = { Icon(Icons.Default.SmartToy, contentDescription = null) },
                    onClick = {
                        onExpandedChange(false)
                        onNewAgent()
                    },
                )
            }
        }
        FloatingActionButton(
            onClick = { onExpandedChange(!expanded) },
        ) {
            Icon(
                imageVector = if (expanded) Icons.Default.Close else Icons.Default.Add,
                contentDescription = if (expanded) {
                    "Close actions"
                } else {
                    "New agent or workspace"
                },
            )
        }
    }
}

@Composable
private fun FabMenuItem(
    label: String,
    icon: @Composable () -> Unit,
    onClick: () -> Unit,
) {
    // One click target for label + circle — a real SmallFloatingActionButton
    // nested in a clickable row would double the semantics.
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.small),
        modifier = Modifier
            .clip(MaterialTheme.shapes.medium)
            .clickable(onClickLabel = label, role = Role.Button, onClick = onClick),
    ) {
        Surface(
            color = MaterialTheme.colorScheme.surfaceContainerHigh,
            contentColor = MaterialTheme.colorScheme.onSurface,
            shape = MaterialTheme.shapes.medium,
        ) {
            Text(
                label,
                style = MaterialTheme.typography.labelLarge,
                modifier = Modifier.padding(
                    horizontal = LerdrTheme.spacing.small,
                    vertical = LerdrTheme.spacing.extraSmall,
                ),
            )
        }
        Surface(
            color = MaterialTheme.colorScheme.primaryContainer,
            contentColor = MaterialTheme.colorScheme.onPrimaryContainer,
            shape = CircleShape,
        ) {
            Box(
                contentAlignment = Alignment.Center,
                modifier = Modifier.size(40.dp),
            ) {
                icon()
            }
        }
    }
}

@Composable
private fun LiveChip(modifier: Modifier = Modifier) {
    val colors = LerdrTheme.extendedColors
    Surface(
        color = colors.workingContainer,
        contentColor = colors.onWorkingContainer,
        shape = CircleShape,
        modifier = modifier,
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.padding(
                horizontal = LerdrTheme.spacing.small,
                vertical = LerdrTheme.spacing.extraSmall,
            ),
        ) {
            StatusDot(color = colors.working)
            Spacer(Modifier.width(LerdrTheme.spacing.extraSmall))
            Text("live", style = MaterialTheme.typography.labelMedium)
        }
    }
}

@Composable
private fun StatusDot(color: Color) {
    Box(
        modifier = Modifier
            .size(8.dp)
            .clip(CircleShape)
            .background(color),
    )
}

/**
 * Empty Agents state — a quiet pointer to the launch FAB when computers are
 * paired, or to the Computers tab when nothing is. `fillParentMaxSize`
 * inside the LazyColumn centers it in the remaining viewport.
 */
@Composable
private fun EmptyState(
    hasRelays: Boolean,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier,
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Icon(
            Icons.Default.Terminal,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.6f),
            modifier = Modifier.size(56.dp),
        )
        Spacer(Modifier.height(LerdrTheme.spacing.medium))
        Text(
            "No agents running",
            style = MaterialTheme.typography.titleMedium,
            color = MaterialTheme.colorScheme.onSurface,
        )
        Spacer(Modifier.height(LerdrTheme.spacing.extraSmall))
        Text(
            text = if (hasRelays) {
                "Launch an agent or workspace with the + button."
            } else {
                "Pair a computer from the Computers tab to get started."
            },
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.Center,
        )
    }
}

@Composable
private fun SectionHeader(
    label: String,
    color: Color,
    modifier: Modifier = Modifier,
    leading: (@Composable () -> Unit)? = null,
) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = modifier.padding(vertical = LerdrTheme.spacing.small),
    ) {
        leading?.let {
            it()
            Spacer(Modifier.width(LerdrTheme.spacing.extraSmall))
        }
        Text(
            label,
            style = MaterialTheme.typography.labelMedium,
            color = color,
            fontWeight = FontWeight.Bold,
        )
    }
}

/** "relay ▸ workspace" — one group's subheader inside a status section. */
@Composable
private fun GroupHeader(
    group: AgentGroupUi,
    modifier: Modifier = Modifier,
) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = modifier.padding(top = LerdrTheme.spacing.extraSmall),
    ) {
        Text(
            "${group.relayLabel} ▸ ${group.label}",
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f, fill = false),
        )
        group.watchingDevices?.let { devices ->
            Spacer(Modifier.width(LerdrTheme.spacing.extraSmall))
            Icon(
                Icons.Filled.PhoneAndroid,
                contentDescription = "$devices device(s) on this workspace",
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.size(14.dp),
            )
            Text(
                "$devices",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

private enum class ApprovalTone { APPROVE, TRUST, DENY }

/**
 * The oracle's `approvalButtonTone` — the last option or a deny-ish label is
 * destructive; "always/trust/configure" choices get the trust tint.
 */
private fun approvalTone(option: String, index: Int, total: Int): ApprovalTone {
    val value = option.trim().lowercase()
    if (index == total - 1 || DENY_WORDS.containsMatchIn(value)) return ApprovalTone.DENY
    if (TRUST_WORDS.containsMatchIn(value)) return ApprovalTone.TRUST
    return ApprovalTone.APPROVE
}

private val DENY_WORDS = Regex("\\b(no|deny|reject|cancel|exit)\\b")
private val TRUST_WORDS = Regex("\\b(always|trust|don't ask|dont ask|configure|edit|amend)\\b")

/** The oracle truncates option labels past 48 chars. */
private fun optionLabel(option: String): String =
    if (option.length > 48) option.take(45) + "…" else option

/**
 * A needs-you card — a blocked agent with its answer affordances inline.
 * The card is a polite live region so a newly-blocked agent announces
 * itself; [responding] swaps buttons for the oracle's "Waiting for agent…"
 * status; [AttentionCardUi.controllable] hides mutating buttons for
 * readers while keeping card navigation.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun AttentionCard(
    card: AttentionCardUi,
    onOpen: () -> Unit,
    onRespond: (Int) -> Unit,
    onAnswerOption: (Int) -> Unit,
) {
    val colors = LerdrTheme.extendedColors
    val container = when (card.kind) {
        AttentionKind.APPROVAL -> colors.attentionContainer
        AttentionKind.QUESTION -> MaterialTheme.colorScheme.surfaceVariant
        AttentionKind.CHAT -> colors.chatContainer
    }
    Card(
        onClick = onOpen,
        colors = CardDefaults.cardColors(containerColor = container),
        shape = MaterialTheme.shapes.large,
        modifier = Modifier
            .width(320.dp)
            .semantics { liveRegion = LiveRegionMode.Polite },
    ) {
        Column(
            modifier = Modifier.padding(LerdrTheme.spacing.medium),
            verticalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.small),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                AgentAvatar(provider = card.provider, label = card.agentLabel)
                Spacer(Modifier.width(LerdrTheme.spacing.small))
                Column {
                    Text(
                        card.agentLabel,
                        style = MaterialTheme.typography.titleSmall,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Text(
                        card.metaLabel,
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            Text(
                card.prompt,
                style = MaterialTheme.typography.bodyMedium,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
            when {
                card.responding -> Text(
                    "Waiting for agent…",
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                card.kind == AttentionKind.APPROVAL && card.controllable &&
                    card.options.isNotEmpty() -> Row(
                    horizontalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.small),
                ) {
                    card.options.take(MAX_INLINE_OPTIONS).forEachIndexed { index, option ->
                        ApprovalButton(
                            option = optionLabel(option),
                            tone = approvalTone(option, index, card.options.size),
                            onClick = { onRespond(index) },
                            modifier = Modifier.weight(1f),
                        )
                    }
                }
                card.controllable && card.quickOptions.isNotEmpty() -> FlowRow(
                    horizontalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.small),
                    verticalArrangement = Arrangement.spacedBy(
                        LerdrTheme.spacing.extraSmall,
                    ),
                ) {
                    card.quickOptions.forEach { option ->
                        AssistChip(
                            onClick = { onAnswerOption(option.index) },
                            label = {
                                Text(
                                    optionLabel(option.label),
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                            },
                        )
                    }
                }
                card.chooseLabel != null -> FilledTonalButton(onClick = onOpen) {
                    Text(card.chooseLabel.orEmpty())
                }
            }
        }
    }
}

@Composable
private fun ApprovalButton(
    option: String,
    tone: ApprovalTone,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = LerdrTheme.extendedColors
    when (tone) {
        ApprovalTone.APPROVE -> Button(
            onClick = onClick,
            colors = ButtonDefaults.buttonColors(
                containerColor = colors.working,
                contentColor = colors.onWorking,
            ),
            modifier = modifier,
        ) { Text(option, maxLines = 1, overflow = TextOverflow.Ellipsis) }
        ApprovalTone.TRUST -> FilledTonalButton(
            onClick = onClick,
            colors = ButtonDefaults.filledTonalButtonColors(
                containerColor = MaterialTheme.colorScheme.secondaryContainer,
                contentColor = MaterialTheme.colorScheme.onSecondaryContainer,
            ),
            modifier = modifier,
        ) { Text(option, maxLines = 1, overflow = TextOverflow.Ellipsis) }
        ApprovalTone.DENY -> FilledTonalButton(
            onClick = onClick,
            colors = ButtonDefaults.filledTonalButtonColors(
                containerColor = colors.dangerContainer,
                contentColor = colors.onDangerContainer,
            ),
            modifier = modifier,
        ) { Text(option, maxLines = 1, overflow = TextOverflow.Ellipsis) }
    }
}

/**
 * Agent row with the swipe affordances: start→end opens the session,
 * end→start asks for the stop confirmation (readers never see the stop
 * side). The row never actually dismisses — the store owns membership.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SwipeableAgentRow(
    agent: AgentListItemUi,
    onOpen: () -> Unit,
    onRequestStop: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val dismissState = rememberSwipeToDismissBoxState(
        confirmValueChange = { value ->
            when (value) {
                SwipeToDismissBoxValue.StartToEnd -> onOpen()
                SwipeToDismissBoxValue.EndToStart -> onRequestStop()
                SwipeToDismissBoxValue.Settled -> {}
            }
            false
        },
    )
    SwipeToDismissBox(
        state = dismissState,
        modifier = modifier,
        enableDismissFromEndToStart = agent.controllable,
        backgroundContent = {
            // `dismissDirection` is Settled when idle — fall back to the
            // anchor the drag would land on so the tint previews early.
            val direction = dismissState.dismissDirection
                .takeIf { it != SwipeToDismissBoxValue.Settled }
                ?: dismissState.targetValue
            SwipeBackground(direction = direction)
        },
    ) {
        AgentRow(agent = agent, onClick = onOpen)
    }
}

@Composable
private fun SwipeBackground(direction: SwipeToDismissBoxValue) {
    val colors = LerdrTheme.extendedColors
    when (direction) {
        SwipeToDismissBoxValue.StartToEnd -> Box(
            contentAlignment = Alignment.CenterStart,
            modifier = Modifier
                .fillMaxSize()
                .clip(MaterialTheme.shapes.medium)
                .background(MaterialTheme.colorScheme.primaryContainer)
                .padding(start = LerdrTheme.spacing.large),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    Icons.Default.Terminal,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onPrimaryContainer,
                )
                Spacer(Modifier.width(LerdrTheme.spacing.small))
                Text(
                    "Open",
                    style = MaterialTheme.typography.labelLarge,
                    color = MaterialTheme.colorScheme.onPrimaryContainer,
                )
            }
        }
        SwipeToDismissBoxValue.EndToStart -> Box(
            contentAlignment = Alignment.CenterEnd,
            modifier = Modifier
                .fillMaxSize()
                .clip(MaterialTheme.shapes.medium)
                .background(colors.dangerContainer)
                .padding(end = LerdrTheme.spacing.large),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    Icons.Default.Stop,
                    contentDescription = null,
                    tint = colors.onDangerContainer,
                )
                Spacer(Modifier.width(LerdrTheme.spacing.small))
                Text(
                    "Stop",
                    style = MaterialTheme.typography.labelLarge,
                    color = colors.onDangerContainer,
                )
            }
        }
        else -> Box(Modifier.fillMaxSize())
    }
}

/**
 * The oracle's ManageDialog stop confirmation — "Stop this agent? Its pane
 * closes on the computer." with a danger confirm.
 */
@Composable
fun StopAgentDialog(
    agent: AgentListItemUi,
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Stop agent") },
        text = {
            Text("Stop this agent? Its pane closes on the computer.")
        },
        confirmButton = {
            Button(
                onClick = onConfirm,
                colors = ButtonDefaults.buttonColors(
                    containerColor = LerdrTheme.extendedColors.danger,
                    contentColor = LerdrTheme.extendedColors.onDanger,
                ),
            ) {
                Text("Confirm Stop")
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) { Text("Cancel") }
        },
    )
}

@Composable
private fun AgentRow(
    agent: AgentListItemUi,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Card(
        onClick = onClick,
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.surfaceContainerLow,
        ),
        shape = MaterialTheme.shapes.medium,
        modifier = modifier.fillMaxWidth(),
    ) {
        Column(modifier = Modifier.padding(LerdrTheme.spacing.medium)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                AgentAvatar(provider = agent.provider, label = agent.title)
                Spacer(Modifier.width(LerdrTheme.spacing.small))
                Column(Modifier.weight(1f)) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(
                            agent.title,
                            style = MaterialTheme.typography.titleSmall,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                            modifier = Modifier.weight(1f, fill = false),
                        )
                        if (agent.watching) {
                            Spacer(Modifier.width(LerdrTheme.spacing.extraSmall))
                            Icon(
                                Icons.Filled.Visibility,
                                contentDescription = "Watched by a device",
                                tint = MaterialTheme.colorScheme.onSurfaceVariant,
                                modifier = Modifier.size(14.dp),
                            )
                        }
                    }
                    Text(
                        agent.statusLine,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                Spacer(Modifier.width(LerdrTheme.spacing.small))
                ElapsedChip(agent = agent)
            }
            if (agent.stateLabels.isNotEmpty()) {
                Spacer(Modifier.height(LerdrTheme.spacing.extraSmall))
                Row(horizontalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.extraSmall)) {
                    agent.stateLabels.forEach { label ->
                        Surface(
                            color = MaterialTheme.colorScheme.surfaceContainerHighest,
                            contentColor = MaterialTheme.colorScheme.onSurfaceVariant,
                            shape = MaterialTheme.shapes.small,
                        ) {
                            Text(
                                label,
                                style = MaterialTheme.typography.labelSmall,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                                modifier = Modifier.padding(
                                    horizontal = LerdrTheme.spacing.extraSmall,
                                    vertical = 2.dp,
                                ),
                            )
                        }
                    }
                }
            }
            if (agent.working) {
                Spacer(Modifier.height(LerdrTheme.spacing.small))
                LerdrWavyProgressIndicator(
                    modifier = Modifier.fillMaxWidth(),
                    color = LerdrTheme.extendedColors.working,
                )
                agent.activityLabel?.let {
                    Text(
                        it,
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(top = LerdrTheme.spacing.extraSmall),
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        }
    }
}

@Composable
private fun ElapsedChip(agent: AgentListItemUi) {
    val colors = LerdrTheme.extendedColors
    val (container, content) = if (agent.working) {
        colors.workingContainer to colors.onWorkingContainer
    } else {
        MaterialTheme.colorScheme.surfaceContainerHighest to
            MaterialTheme.colorScheme.onSurfaceVariant
    }
    Surface(color = container, contentColor = content, shape = CircleShape) {
        Text(
            agent.elapsedLabel,
            style = MaterialTheme.typography.labelSmall,
            modifier = Modifier.padding(
                horizontal = LerdrTheme.spacing.small,
                vertical = LerdrTheme.spacing.extraSmall,
            ),
        )
    }
}

@Composable
private fun AgentAvatar(provider: String?, label: String) {
    ProviderBadge(provider = provider, label = label)
}

private const val MAX_INLINE_OPTIONS = 3

@PreviewLightDark
@Composable
private fun HomeContentPreview() {
    LerdrTheme {
        HomeContent(
            uiState = previewHomeUiState,
            onOpenAgent = {},
            onSelectTopLevel = {},
        )
    }
}

@PreviewLightDark
@Composable
private fun HomeContentEmptyPreview() {
    LerdrTheme {
        HomeContent(
            uiState = HomeUiState(),
            onOpenAgent = {},
            onSelectTopLevel = {},
        )
    }
}

private val previewHomeUiState = HomeUiState(
    live = true,
    relaySummary = "2 computers · tailscale",
    needsYou = listOf(
        AttentionCardUi(
            paneId = "sd::%1",
            relayId = "sd",
            agentLabel = "claude · lerdr",
            kind = AttentionKind.APPROVAL,
            metaLabel = "approval · 40s",
            prompt = "Run go test ./internal/… ?",
            options = listOf("Allow", "Deny"),
            controllable = true,
            provider = "claude",
        ),
        AttentionCardUi(
            paneId = "sd::%2",
            relayId = "sd",
            agentLabel = "devin · herdr",
            kind = AttentionKind.QUESTION,
            metaLabel = "question · 3 options",
            prompt = "Which module should own the delta cache?",
            interaction = Interaction(
                id = "q1",
                kind = "single_select",
                question = "Which module should own the delta cache?",
                options = listOf(
                    Option(index = 0, label = "core:store"),
                    Option(index = 1, label = "session"),
                    Option(index = 2, label = "relay"),
                ),
                other = lerdr.core.model.Other(hidden = true),
                questionTotal = 1,
            ),
            controllable = true,
            provider = "devin",
        ),
    ),
    working = listOf(
        AgentGroupUi(
            key = "sd\u0000lerdr",
            relayLabel = "sd",
            label = "lerdr",
            agents = listOf(
                AgentListItemUi(
                    paneId = "sd::%3",
                    relayId = "sd",
                    title = "claude · api-server",
                    statusLine = "Editing handler.go",
                    activityLabel = "running tests…",
                    elapsedLabel = "1:24",
                    working = true,
                    controllable = true,
                    provider = "claude",
                ),
                AgentListItemUi(
                    paneId = "sd::%4",
                    relayId = "sd",
                    title = "pi · dotfiles",
                    statusLine = "Bash: git rebase",
                    activityLabel = "writing migration.sql",
                    elapsedLabel = "0:37",
                    working = true,
                    controllable = true,
                    provider = "pi",
                ),
            ),
        ),
        AgentGroupUi(
            key = "workstation\u0000herdr",
            relayLabel = "workstation",
            label = "herdr",
            agents = listOf(
                AgentListItemUi(
                    paneId = "workstation::%1",
                    relayId = "workstation",
                    title = "devin · herdr",
                    statusLine = "Watching relay logs",
                    activityLabel = "tail -f relay.log",
                    elapsedLabel = "0:12",
                    working = true,
                    controllable = true,
                    provider = "devin",
                ),
            ),
        ),
    ),
    idle = listOf(
        AgentGroupUi(
            key = "sd\u0000web",
            relayLabel = "sd",
            label = "web",
            agents = listOf(
                AgentListItemUi(
                    paneId = "sd::%5",
                    relayId = "sd",
                    title = "codex · web",
                    statusLine = "ready · 12m ago",
                    activityLabel = null,
                    elapsedLabel = "idle",
                    working = false,
                    controllable = true,
                    provider = "codex",
                ),
            ),
        ),
    ),
    relays = listOf(
        RelayCardUi("sd", "sd", "tailscale", "12ms", 4, connected = true, rttMs = 12),
        RelayCardUi("workstation", "workstation", "gateway", "81ms", 0, connected = true, rttMs = 81),
    ),
)
