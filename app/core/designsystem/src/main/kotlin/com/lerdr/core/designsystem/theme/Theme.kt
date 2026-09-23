package com.lerdr.core.designsystem.theme

import android.os.Build
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialExpressiveTheme
import androidx.compose.material3.MotionScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.TextStyle

/**
 * Lerdr theme — Material Expressive under the hood.
 *
 * `MaterialExpressiveTheme` + `MotionScheme.expressive()` give every stable
 * component the spring-physics defaults; the M3E-only alpha surface is
 * exposed to feature code exclusively through the wrappers in
 * `core:designsystem.components` (see AGENTS.md: features never import
 * experimental M3E APIs).
 *
 * The brand palette ([LerdrDarkColorScheme] / [LerdrLightColorScheme]) is
 * the default everywhere — it carries the mission-control look from
 * docs/mockup.png regardless of wallpaper. Dynamic (Material You) color is
 * opt-in via Settings > App, since wallpaper-derived hues can wash out the
 * brand identity the product is designed around.
 */
@Composable
fun LerdrTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    dynamicColor: Boolean = false,
    spacing: LerdrSpacing = LerdrSpacing(),
    content: @Composable () -> Unit,
) {
    val colorScheme = when {
        dynamicColor && Build.VERSION.SDK_INT >= Build.VERSION_CODES.S -> {
            val context = LocalContext.current
            if (darkTheme) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
        }
        darkTheme -> LerdrDarkColorScheme
        else -> LerdrLightColorScheme
    }
    val extendedColors = if (darkTheme) LerdrDarkExtendedColors else LerdrLightExtendedColors

    CompositionLocalProvider(
        LocalLerdrSpacing provides spacing,
        LocalLerdrExtendedColors provides extendedColors,
    ) {
        MaterialExpressiveTheme(
            colorScheme = colorScheme,
            motionScheme = MotionScheme.expressive(),
            shapes = LerdrShapes,
            typography = LerdrTypography,
            content = content,
        )
    }
}

/** Token accessors — read inside composition: `LerdrTheme.spacing.medium`. */
object LerdrTheme {
    val spacing: LerdrSpacing
        @Composable get() = LocalLerdrSpacing.current

    val extendedColors: LerdrExtendedColors
        @Composable get() = LocalLerdrExtendedColors.current

    /** Monospace style for terminal/code content (see [LerdrTextStyles]). */
    val terminalStyle: TextStyle
        @Composable get() = LerdrTextStyles.terminal
}
