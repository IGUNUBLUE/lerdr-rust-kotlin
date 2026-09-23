package com.lerdr.app.nav

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Dns
import androidx.compose.material.icons.filled.History
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.Terminal
import androidx.compose.material.icons.outlined.Dns
import androidx.compose.material.icons.outlined.History
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material.icons.outlined.Terminal
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalContext
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.lerdr.app.di.AppEntryPoint
import com.lerdr.core.designsystem.components.LerdrNavBadge
import com.lerdr.core.designsystem.components.LerdrNavItem
import com.lerdr.navigation.LerdrKey
import dagger.hilt.android.EntryPointAccessors
import kotlinx.coroutines.flow.combine
import lerdr.core.store.RelayStatus
import lerdr.core.store.agentNeedsInspection
import lerdr.core.store.agentNeedsResponse

/**
 * Live counts feeding the bottom-bar badges — the rail's "needs you" set
 * on Agents, an attention dot on Computers when a paired relay is down
 * or refused this device.
 */
data class LerdrNavBadges(
    val needsYou: Int = 0,
    val computersAlert: Boolean = false,
)

/**
 * Collects [LerdrNavBadges] from the shared session store so every
 * top-level screen renders the same marks without re-deriving them.
 */
@Composable
fun rememberLerdrNavBadges(): LerdrNavBadges {
    val appContext = LocalContext.current.applicationContext
    val sessions = remember(appContext) {
        EntryPointAccessors.fromApplication(appContext, AppEntryPoint::class.java)
            .sessionRepository()
    }
    val badges by remember(sessions) {
        combine(sessions.agents, sessions.connections) { agents, connections ->
            LerdrNavBadges(
                needsYou = agents.count {
                    agentNeedsResponse(it) || agentNeedsInspection(it)
                },
                computersAlert = connections.values.any {
                    it.authRejected || it.status == RelayStatus.DISCONNECTED
                },
            )
        }
    }.collectAsStateWithLifecycle(initialValue = LerdrNavBadges())
    return badges
}

/**
 * The four top-level destinations in bottom-bar order (docs/04): filled
 * icon while selected, outlined otherwise, badges from
 * [rememberLerdrNavBadges].
 */
fun topLevelNavItems(
    selected: LerdrKey,
    badges: LerdrNavBadges,
    onSelect: (LerdrKey) -> Unit,
): List<LerdrNavItem> = listOf(
    LerdrNavItem(
        label = "Agents",
        icon = Icons.Filled.Terminal,
        unselectedIcon = Icons.Outlined.Terminal,
        selected = selected is LerdrKey.Home,
        onClick = { onSelect(LerdrKey.Home) },
        badge = if (badges.needsYou > 0) {
            LerdrNavBadge.Count(badges.needsYou)
        } else {
            LerdrNavBadge.None
        },
    ),
    LerdrNavItem(
        label = "Computers",
        icon = Icons.Filled.Dns,
        unselectedIcon = Icons.Outlined.Dns,
        selected = selected is LerdrKey.Computers,
        onClick = { onSelect(LerdrKey.Computers) },
        badge = if (badges.computersAlert) LerdrNavBadge.Dot else LerdrNavBadge.None,
    ),
    LerdrNavItem(
        label = "Activity",
        icon = Icons.Filled.History,
        unselectedIcon = Icons.Outlined.History,
        selected = selected is LerdrKey.Activity,
        onClick = { onSelect(LerdrKey.Activity) },
    ),
    LerdrNavItem(
        label = "Settings",
        icon = Icons.Filled.Settings,
        unselectedIcon = Icons.Outlined.Settings,
        selected = selected is LerdrKey.Settings || selected is LerdrKey.RelayDetail,
        onClick = { onSelect(LerdrKey.Settings) },
    ),
)
