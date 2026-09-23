package com.lerdr.core.designsystem.components

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Home
import androidx.compose.material.icons.filled.Notifications
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.outlined.Home
import androidx.compose.material.icons.outlined.Notifications
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material3.Badge
import androidx.compose.material3.BadgedBox
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationItemIconPosition
import androidx.compose.material3.ShortNavigationBar
import androidx.compose.material3.ShortNavigationBarItem
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.tooling.preview.PreviewLightDark
import com.lerdr.core.designsystem.theme.LerdrTheme

/**
 * Optional marker on a [LerdrNavItem] icon — the M3 badge pair: a small
 * dot for "something changed" and a count for "N things need you".
 */
@Immutable
sealed interface LerdrNavBadge {
    data object None : LerdrNavBadge

    /** Small 6dp dot — status signal (e.g. a relay is down). */
    data object Dot : LerdrNavBadge

    /** Counted badge — clamped to `99+` per the M3 large-badge spec. */
    @Immutable
    data class Count(val value: Int) : LerdrNavBadge
}

/**
 * One destination in [LerdrShortNavigationBar]. [icon] is the filled mark
 * used while selected; [unselectedIcon] falls back to [icon] when the
 * family has no outlined variant (M3 then prescribes a heavier weight —
 * the filled glyph reads as "active" already).
 */
@Immutable
data class LerdrNavItem(
    val label: String,
    val icon: ImageVector,
    val selected: Boolean,
    val onClick: () -> Unit,
    val enabled: Boolean = true,
    val unselectedIcon: ImageVector? = null,
    val badge: LerdrNavBadge = LerdrNavBadge.None,
)

/**
 * M3E short navigation bar — the `Agents · Computers · Activity ·
 * Settings` rail from docs/04. Wraps [ShortNavigationBar]/
 * [ShortNavigationBarItem] so the alpha types never escape into feature
 * code; renders [LerdrNavItem.badge] through [BadgedBox].
 */
@Composable
fun LerdrShortNavigationBar(
    items: List<LerdrNavItem>,
    modifier: Modifier = Modifier,
) {
    ShortNavigationBar(modifier = modifier) {
        items.forEach { item ->
            ShortNavigationBarItem(
                selected = item.selected,
                onClick = item.onClick,
                icon = { NavItemIcon(item) },
                label = { Text(item.label) },
                enabled = item.enabled,
                iconPosition = NavigationItemIconPosition.Top,
            )
        }
    }
}

@Composable
private fun NavItemIcon(item: LerdrNavItem) {
    val glyph = if (item.selected) item.icon else item.unselectedIcon ?: item.icon
    when (val badge = item.badge) {
        LerdrNavBadge.None -> Icon(imageVector = glyph, contentDescription = item.label)
        else -> BadgedBox(
            badge = {
                when (badge) {
                    LerdrNavBadge.Dot -> Badge(
                        modifier = Modifier.semantics {
                            contentDescription = "${item.label}: attention required"
                        },
                    )
                    is LerdrNavBadge.Count -> Badge {
                        Text(
                            if (badge.value > 99) "99+" else badge.value.toString(),
                            modifier = Modifier.semantics {
                                contentDescription = "${item.label}: ${badge.value} need you"
                            },
                        )
                    }
                    LerdrNavBadge.None -> Unit
                }
            },
        ) {
            Icon(imageVector = glyph, contentDescription = item.label)
        }
    }
}

@PreviewLightDark
@Composable
private fun LerdrShortNavigationBarPreview() {
    LerdrTheme {
        LerdrShortNavigationBar(
            items = listOf(
                LerdrNavItem(
                    "Agents",
                    Icons.Filled.Home,
                    selected = true,
                    onClick = {},
                    unselectedIcon = Icons.Outlined.Home,
                    badge = LerdrNavBadge.Count(3),
                ),
                LerdrNavItem(
                    "Activity",
                    Icons.Filled.Notifications,
                    selected = false,
                    onClick = {},
                    unselectedIcon = Icons.Outlined.Notifications,
                    badge = LerdrNavBadge.Dot,
                ),
                LerdrNavItem(
                    "Settings",
                    Icons.Filled.Settings,
                    selected = false,
                    onClick = {},
                    unselectedIcon = Icons.Outlined.Settings,
                ),
            ),
        )
    }
}
