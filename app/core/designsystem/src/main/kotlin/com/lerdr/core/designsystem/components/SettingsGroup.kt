package com.lerdr.core.designsystem.components

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.tooling.preview.PreviewLightDark
import com.lerdr.core.designsystem.theme.LerdrTheme

/**
 * Grouped-settings container — the Pixel/M3E pattern where related rows
 * sit inside one rounded surface instead of floating on the backdrop.
 * Children are laid out in order; [LerdrSettingsDivider] draws the
 * hairline between rows.
 */
@Composable
fun LerdrSettingsGroup(
    modifier: Modifier = Modifier,
    content: @Composable ColumnScope.() -> Unit,
) {
    val spacing = LerdrTheme.spacing
    Surface(
        color = MaterialTheme.colorScheme.surfaceContainerLow,
        shape = MaterialTheme.shapes.large,
        modifier = modifier
            .fillMaxWidth()
            .padding(horizontal = spacing.medium),
    ) {
        Column(content = content)
    }
}

/** Hairline separator between rows inside a [LerdrSettingsGroup]. */
@Composable
fun LerdrSettingsDivider(modifier: Modifier = Modifier) {
    HorizontalDivider(
        modifier = modifier.padding(start = LerdrTheme.spacing.large * 2),
        color = MaterialTheme.colorScheme.outlineVariant,
    )
}

@PreviewLightDark
@Composable
private fun LerdrSettingsGroupPreview() {
    LerdrTheme {
        LerdrSettingsGroup {
            Text("Row one", modifier = Modifier.padding(LerdrTheme.spacing.medium))
            LerdrSettingsDivider()
            Text("Row two", modifier = Modifier.padding(LerdrTheme.spacing.medium))
        }
    }
}
