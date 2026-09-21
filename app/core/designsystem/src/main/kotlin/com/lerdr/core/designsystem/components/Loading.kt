package com.lerdr.core.designsystem.components

import androidx.compose.material3.ContainedLoadingIndicator
import androidx.compose.material3.ExperimentalMaterial3ExpressiveApi
import androidx.compose.material3.LoadingIndicator
import androidx.compose.material3.LoadingIndicatorDefaults
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.isSpecified
import androidx.compose.ui.tooling.preview.PreviewLightDark
import com.lerdr.core.designsystem.theme.LerdrTheme

/**
 * M3E morphing loading indicator — the "thinking"/connecting affordance
 * (working row on Feed, relay card while connecting). Stable signature over
 * the experimental [LoadingIndicator].
 */

/** Indeterminate morphing loader. [contained] adds the container plate. */
@OptIn(ExperimentalMaterial3ExpressiveApi::class)
@Composable
fun LerdrLoadingIndicator(
    modifier: Modifier = Modifier,
    color: Color = Color.Unspecified,
    contained: Boolean = false,
) {
    if (contained) {
        ContainedLoadingIndicator(
            modifier = modifier,
            indicatorColor = if (color.isSpecified) {
                color
            } else {
                LoadingIndicatorDefaults.containedIndicatorColor
            },
        )
    } else {
        LoadingIndicator(
            modifier = modifier,
            color = if (color.isSpecified) color else LoadingIndicatorDefaults.indicatorColor,
        )
    }
}

/** Determinate morphing loader — [progress] reports a fraction in 0f..1f. */
@OptIn(ExperimentalMaterial3ExpressiveApi::class)
@Composable
fun LerdrLoadingIndicator(
    progress: () -> Float,
    modifier: Modifier = Modifier,
    color: Color = Color.Unspecified,
    contained: Boolean = false,
) {
    if (contained) {
        ContainedLoadingIndicator(
            progress = progress,
            modifier = modifier,
            indicatorColor = if (color.isSpecified) {
                color
            } else {
                LoadingIndicatorDefaults.containedIndicatorColor
            },
        )
    } else {
        LoadingIndicator(
            progress = progress,
            modifier = modifier,
            color = if (color.isSpecified) color else LoadingIndicatorDefaults.indicatorColor,
        )
    }
}

@PreviewLightDark
@Composable
private fun LerdrLoadingIndicatorPreview() {
    LerdrTheme {
        LerdrLoadingIndicator()
    }
}

@PreviewLightDark
@Composable
private fun LerdrLoadingIndicatorContainedPreview() {
    LerdrTheme {
        LerdrLoadingIndicator(contained = true)
    }
}
