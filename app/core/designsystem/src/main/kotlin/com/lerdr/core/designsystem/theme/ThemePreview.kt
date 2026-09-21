package com.lerdr.core.designsystem.theme

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.tooling.preview.PreviewLightDark
import androidx.compose.ui.unit.dp

/** Theme sanity preview — palette, type scale, extended agent colors. */
@PreviewLightDark
@Composable
private fun LerdrThemePreview() {
    LerdrTheme {
        Surface {
            Column(
                modifier = Modifier.padding(LerdrTheme.spacing.medium),
                verticalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.small),
            ) {
                Text("Agents", style = MaterialTheme.typography.headlineMedium)
                Text(
                    "2 computers · tailscale",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Row(horizontalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.small)) {
                    Swatch("working", LerdrTheme.extendedColors.workingContainer)
                    Swatch("needs you", LerdrTheme.extendedColors.attentionContainer)
                    Swatch("primary", MaterialTheme.colorScheme.primaryContainer)
                }
                Text(
                    "lerdr git:(main) cargo test",
                    style = LerdrTheme.terminalStyle,
                    color = LerdrTheme.extendedColors.terminalText,
                    modifier = Modifier
                        .fillMaxWidth()
                        .background(LerdrTheme.extendedColors.terminalSurface)
                        .padding(LerdrTheme.spacing.medium),
                )
            }
        }
    }
}

@Composable
private fun Swatch(label: String, color: androidx.compose.ui.graphics.Color) {
    Column {
        Box(
            modifier = Modifier
                .width(72.dp)
                .height(40.dp)
                .background(color, MaterialTheme.shapes.small),
        )
        Text(label, style = MaterialTheme.typography.labelSmall)
    }
}
