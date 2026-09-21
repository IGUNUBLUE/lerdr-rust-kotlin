package com.lerdr.core.designsystem.components

import androidx.compose.material3.CircularWavyProgressIndicator
import androidx.compose.material3.LinearWavyProgressIndicator
import androidx.compose.material3.WavyProgressIndicatorDefaults
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.isSpecified
import androidx.compose.ui.tooling.preview.PreviewLightDark
import com.lerdr.core.designsystem.theme.LerdrTheme

/**
 * M3E wavy progress — the signature "working" affordance (docs/04 §Home:
 * pinned working agents get the wavy strip). These wrappers own the alpha
 * dependency; features see stable signatures only.
 */

/** Determinate linear wavy strip (known progress fraction in `progress`). */
@Composable
fun LerdrWavyProgressIndicator(
    progress: () -> Float,
    modifier: Modifier = Modifier,
    color: Color = Color.Unspecified,
    trackColor: Color = Color.Unspecified,
) {
    LinearWavyProgressIndicator(
        progress = progress,
        modifier = modifier,
        color = if (color.isSpecified) color else WavyProgressIndicatorDefaults.indicatorColor,
        trackColor = if (trackColor.isSpecified) {
            trackColor
        } else {
            WavyProgressIndicatorDefaults.trackColor
        },
    )
}

/** Indeterminate linear wavy strip — activity with no measurable fraction. */
@Composable
fun LerdrWavyProgressIndicator(
    modifier: Modifier = Modifier,
    color: Color = Color.Unspecified,
    trackColor: Color = Color.Unspecified,
) {
    LinearWavyProgressIndicator(
        modifier = modifier,
        color = if (color.isSpecified) color else WavyProgressIndicatorDefaults.indicatorColor,
        trackColor = if (trackColor.isSpecified) {
            trackColor
        } else {
            WavyProgressIndicatorDefaults.trackColor
        },
    )
}

/** Determinate circular wavy indicator. */
@Composable
fun LerdrCircularWavyProgressIndicator(
    progress: () -> Float,
    modifier: Modifier = Modifier,
    color: Color = Color.Unspecified,
    trackColor: Color = Color.Unspecified,
) {
    CircularWavyProgressIndicator(
        progress = progress,
        modifier = modifier,
        color = if (color.isSpecified) color else WavyProgressIndicatorDefaults.indicatorColor,
        trackColor = if (trackColor.isSpecified) {
            trackColor
        } else {
            WavyProgressIndicatorDefaults.trackColor
        },
    )
}

/** Indeterminate circular wavy indicator. */
@Composable
fun LerdrCircularWavyProgressIndicator(
    modifier: Modifier = Modifier,
    color: Color = Color.Unspecified,
    trackColor: Color = Color.Unspecified,
) {
    CircularWavyProgressIndicator(
        modifier = modifier,
        color = if (color.isSpecified) color else WavyProgressIndicatorDefaults.indicatorColor,
        trackColor = if (trackColor.isSpecified) {
            trackColor
        } else {
            WavyProgressIndicatorDefaults.trackColor
        },
    )
}

@PreviewLightDark
@Composable
private fun LerdrWavyProgressIndicatorPreview() {
    LerdrTheme {
        LerdrWavyProgressIndicator(progress = { 0.6f })
    }
}

@PreviewLightDark
@Composable
private fun LerdrWavyProgressIndicatorIndeterminatePreview() {
    LerdrTheme {
        LerdrWavyProgressIndicator()
    }
}

@PreviewLightDark
@Composable
private fun LerdrCircularWavyProgressIndicatorPreview() {
    LerdrTheme {
        LerdrCircularWavyProgressIndicator()
    }
}
