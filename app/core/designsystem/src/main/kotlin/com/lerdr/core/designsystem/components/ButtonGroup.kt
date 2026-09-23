package com.lerdr.core.designsystem.components

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.ButtonGroup
import androidx.compose.material3.ButtonGroupDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.tooling.preview.PreviewLightDark
import com.lerdr.core.designsystem.theme.LerdrTheme

/**
 * M3E [ButtonGroup] wrapper — the inline choice row on attention/question
 * cards ("Allow · Always · Deny"). Items overflow into a menu automatically
 * when they don't fit; [ButtonGroupScope]-specific behavior stays inside
 * this module.
 */
@Immutable
data class LerdrButtonGroupItem(
    val label: String,
    val onClick: () -> Unit,
    val enabled: Boolean = true,
    /** Optional icon composable, e.g. `{ Icon(...) }`. */
    val icon: (@Composable () -> Unit)? = null,
    /** Relative width share; `Float.NaN` (default) lets the group decide. */
    val weight: Float = Float.NaN,
)

/**
 * Horizontal group of action buttons with M3E squeeze/overflow behavior.
 *
 * @param items actions in display order
 * @param expandedRatio M3E press expansion ratio — see ButtonGroupDefaults
 */
@Composable
fun LerdrButtonGroup(
    items: List<LerdrButtonGroupItem>,
    modifier: Modifier = Modifier,
    expandedRatio: Float = ButtonGroupDefaults.ExpandedRatio,
    horizontalArrangement: Arrangement.Horizontal = ButtonGroupDefaults.HorizontalArrangement,
    verticalAlignment: Alignment.Vertical = Alignment.Top,
) {
    ButtonGroup(
        overflowIndicator = { menuState ->
            ButtonGroupDefaults.OverflowIndicator(menuState = menuState)
        },
        modifier = modifier,
        expandedRatio = expandedRatio,
        horizontalArrangement = horizontalArrangement,
        verticalAlignment = verticalAlignment,
    ) {
        items.forEach { item ->
            clickableItem(
                onClick = item.onClick,
                label = item.label,
                icon = item.icon,
                weight = item.weight,
                enabled = item.enabled,
            )
        }
    }
}

/**
 * Segmented single-select control — the `Feed | Terminal | Files` switch on
 * the agent session top bar. Wraps the stable single-choice segmented row.
 * The selected segment carries the mockup's light-blue pill on dark text —
 * `primary`/`onPrimary` — with the segment border blended into the fill.
 */
@Composable
fun LerdrSegmentedControl(
    options: List<String>,
    selectedIndex: Int,
    onSelect: (Int) -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    enabledOptions: List<Boolean> = List(options.size) { true },
) {
    val selectedColors = SegmentedButtonDefaults.colors(
        activeContainerColor = MaterialTheme.colorScheme.primary,
        activeContentColor = MaterialTheme.colorScheme.onPrimary,
        activeBorderColor = MaterialTheme.colorScheme.primary,
    )
    SingleChoiceSegmentedButtonRow(modifier = modifier) {
        options.forEachIndexed { index, label ->
            SegmentedButton(
                selected = index == selectedIndex,
                onClick = { onSelect(index) },
                shape = SegmentedButtonDefaults.itemShape(
                    index = index,
                    count = options.size,
                ),
                colors = selectedColors,
                enabled = enabled && enabledOptions.getOrElse(index) { true },
                label = { Text(label) },
            )
        }
    }
}

@PreviewLightDark
@Composable
private fun LerdrButtonGroupPreview() {
    LerdrTheme {
        Column(verticalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.medium)) {
            LerdrButtonGroup(
                items = listOf(
                    LerdrButtonGroupItem(label = "Allow", onClick = {}),
                    LerdrButtonGroupItem(label = "Always", onClick = {}),
                    LerdrButtonGroupItem(label = "Deny", onClick = {}),
                ),
            )
            LerdrSegmentedControl(
                options = listOf("Feed", "Terminal", "Files"),
                selectedIndex = 0,
                onSelect = {},
                modifier = Modifier.fillMaxWidth(),
            )
        }
    }
}
