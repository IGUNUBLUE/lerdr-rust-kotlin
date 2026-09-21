package com.lerdr.core.designsystem.components

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Home
import androidx.compose.material.icons.filled.Notifications
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.Icon
import androidx.compose.material3.NavigationItemIconPosition
import androidx.compose.material3.ShortNavigationBar
import androidx.compose.material3.ShortNavigationBarItem
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.tooling.preview.PreviewLightDark
import com.lerdr.core.designsystem.theme.LerdrTheme

/**
 * One destination in [LerdrShortNavigationBar]. [icon] stays an
 * [ImageVector] so features never touch the item composable API.
 */
@Immutable
data class LerdrNavItem(
    val label: String,
    val icon: ImageVector,
    val selected: Boolean,
    val onClick: () -> Unit,
    val enabled: Boolean = true,
)

/**
 * M3E short navigation bar — the `Agents · Activity · Settings` rail from
 * docs/04. Wraps [ShortNavigationBar]/[ShortNavigationBarItem] so the alpha
 * types never escape into feature code.
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
                icon = {
                    Icon(imageVector = item.icon, contentDescription = item.label)
                },
                label = { Text(item.label) },
                enabled = item.enabled,
                iconPosition = NavigationItemIconPosition.Top,
            )
        }
    }
}

@PreviewLightDark
@Composable
private fun LerdrShortNavigationBarPreview() {
    LerdrTheme {
        LerdrShortNavigationBar(
            items = listOf(
                LerdrNavItem("Agents", Icons.Default.Home, selected = true, onClick = {}),
                LerdrNavItem(
                    "Activity",
                    Icons.Default.Notifications,
                    selected = false,
                    onClick = {},
                ),
                LerdrNavItem("Settings", Icons.Default.Settings, selected = false, onClick = {}),
            ),
        )
    }
}
