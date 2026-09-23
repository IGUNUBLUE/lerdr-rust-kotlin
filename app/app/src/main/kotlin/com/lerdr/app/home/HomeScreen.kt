package com.lerdr.app.home

import androidx.compose.foundation.background
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
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Dns
import androidx.compose.material.icons.filled.History
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.Terminal
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.tooling.preview.PreviewLightDark
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lerdr.app.di.AppEntryPoint
import com.lerdr.app.ui.ProviderBadge
import com.lerdr.core.designsystem.components.LerdrNavItem
import dagger.hilt.android.EntryPointAccessors
import com.lerdr.core.designsystem.components.LerdrShortNavigationBar
import com.lerdr.core.designsystem.components.LerdrWavyProgressIndicator
import com.lerdr.core.designsystem.theme.LerdrTheme
import com.lerdr.navigation.LerdrKey

/**
 * Mission control (docs/04 §Home): needs-you rail, agents grouped by
 * activity, bottom nav. [HomeScreen] owns the ViewModel seam;
 * [HomeContent] is pure state → previews and Roborazzi shots stay honest.
 */
@Composable
fun HomeScreen(
    onOpenAgent: (String) -> Unit,
    onSelectTopLevel: (LerdrKey) -> Unit,
) {
    // hilt-navigation-compose is absent — pull the bound repository
    // through the singleton entry point.
    val appContext = LocalContext.current.applicationContext
    val viewModel: HomeViewModel = viewModel {
        HomeViewModel(
            EntryPointAccessors.fromApplication(appContext, AppEntryPoint::class.java)
                .homeRepository(),
        )
    }
    val uiState by viewModel.uiState.collectAsStateWithLifecycle()
    HomeContent(
        uiState = uiState,
        onOpenAgent = onOpenAgent,
        onSelectTopLevel = onSelectTopLevel,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun HomeContent(
    uiState: HomeUiState,
    onOpenAgent: (String) -> Unit,
    onSelectTopLevel: (LerdrKey) -> Unit,
) {
    val spacing = LerdrTheme.spacing
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
                items = listOf(
                    LerdrNavItem(
                        label = "Agents",
                        icon = Icons.Default.Terminal,
                        selected = true,
                        onClick = { onSelectTopLevel(LerdrKey.Home) },
                    ),
                    LerdrNavItem(
                        label = "Computers",
                        icon = Icons.Default.Dns,
                        selected = false,
                        onClick = { onSelectTopLevel(LerdrKey.Computers) },
                    ),
                    LerdrNavItem(
                        label = "Activity",
                        icon = Icons.Default.History,
                        selected = false,
                        onClick = { onSelectTopLevel(LerdrKey.Activity) },
                    ),
                    LerdrNavItem(
                        label = "Settings",
                        icon = Icons.Default.Settings,
                        selected = false,
                        onClick = { onSelectTopLevel(LerdrKey.Settings) },
                    ),
                ),
            )
        },
    ) { innerPadding ->
        LazyColumn(
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
                            )
                        }
                    }
                }
            }

            if (uiState.working.isNotEmpty()) {
                item(key = "working-header") {
                    SectionHeader(
                        label = "WORKING · ${uiState.working.size}",
                        color = LerdrTheme.extendedColors.working,
                        leading = {
                            StatusDot(color = LerdrTheme.extendedColors.working)
                        },
                        modifier = Modifier.padding(horizontal = spacing.medium),
                    )
                }
                items(uiState.working, key = { it.paneId }) { agent ->
                    AgentRow(
                        agent = agent,
                        onClick = { onOpenAgent(agent.paneId) },
                        modifier = Modifier.padding(horizontal = spacing.medium),
                    )
                }
            }

            if (uiState.idle.isNotEmpty()) {
                item(key = "idle-header") {
                    SectionHeader(
                        label = "IDLE · ${uiState.idle.size}",
                        color = LerdrTheme.extendedColors.idle,
                        modifier = Modifier.padding(horizontal = spacing.medium),
                    )
                }
                items(uiState.idle, key = { it.paneId }) { agent ->
                    AgentRow(
                        agent = agent,
                        onClick = { onOpenAgent(agent.paneId) },
                        modifier = Modifier.padding(horizontal = spacing.medium),
                    )
                }
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
private fun StatusDot(color: androidx.compose.ui.graphics.Color) {
    Box(
        modifier = Modifier
            .size(8.dp)
            .clip(CircleShape)
            .background(color),
    )
}

@Composable
private fun SectionHeader(
    label: String,
    color: androidx.compose.ui.graphics.Color,
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

@Composable
private fun AttentionCard(
    card: AttentionCardUi,
    onOpen: () -> Unit,
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
        modifier = Modifier.width(320.dp),
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
            Row(horizontalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.small)) {
                card.options.forEachIndexed { index, option ->
                    val isPrimary = index == 0 && card.kind == AttentionKind.APPROVAL
                    val isDestructive = card.kind == AttentionKind.APPROVAL &&
                        index == card.options.lastIndex
                    when {
                        isPrimary -> Button(
                            onClick = onOpen,
                            colors = ButtonDefaults.buttonColors(
                                containerColor = colors.working,
                                contentColor = colors.onWorking,
                            ),
                            modifier = Modifier.weight(1f),
                        ) { Text(option) }
                        isDestructive -> FilledTonalButton(
                            onClick = onOpen,
                            colors = ButtonDefaults.filledTonalButtonColors(
                                containerColor = MaterialTheme.colorScheme.errorContainer,
                                contentColor = MaterialTheme.colorScheme.onErrorContainer,
                            ),
                            modifier = Modifier.weight(1f),
                        ) { Text(option) }
                        else -> FilledTonalButton(
                            onClick = onOpen,
                            modifier = Modifier.weight(1f),
                        ) { Text(option) }
                    }
                }
            }
        }
    }
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
                    Text(
                        agent.title,
                        style = MaterialTheme.typography.titleSmall,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
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
            agentLabel = "claude · lerdr",
            kind = AttentionKind.APPROVAL,
            metaLabel = "approval · 40s",
            prompt = "Run go test ./internal/… ?",
            options = listOf("Allow", "Deny"),
            provider = "claude",
        ),
        AttentionCardUi(
            paneId = "sd::%2",
            agentLabel = "devin · herdr",
            kind = AttentionKind.QUESTION,
            metaLabel = "question · 3 options",
            prompt = "Which module should own the delta cache?",
            options = listOf("Answer →"),
            provider = "devin",
        ),
    ),
    working = listOf(
        AgentListItemUi(
            paneId = "sd::%3",
            title = "claude · api-server",
            statusLine = "Editing handler.go",
            activityLabel = "running tests…",
            elapsedLabel = "1:24",
            working = true,
            provider = "claude",
        ),
        AgentListItemUi(
            paneId = "sd::%4",
            title = "pi · dotfiles",
            statusLine = "Bash: git rebase",
            activityLabel = "writing migration.sql",
            elapsedLabel = "0:37",
            working = true,
            provider = "pi",
        ),
    ),
    idle = listOf(
        AgentListItemUi(
            paneId = "sd::%5",
            title = "codex · web",
            statusLine = "ready · 12m ago",
            activityLabel = null,
            elapsedLabel = "idle",
            working = false,
            provider = "codex",
        ),
    ),
    relays = listOf(
        RelayCardUi("sd", "sd", "tailscale", "12ms", 4, connected = true),
        RelayCardUi("workstation", "workstation", "gateway", "81ms", 0, connected = true),
    ),
)
