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
 * Dynamic color is the default on API 31+; below that — or when the user
 * disables it — the brand palette ([LerdrDarkColorScheme] /
 * [LerdrLightColorScheme]) keeps the mission-control look.
 */
@Composable
fun LerdrTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    dynamicColor: Boolean = true,
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
