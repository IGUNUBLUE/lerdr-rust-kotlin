package com.lerdr.core.designsystem.theme

import androidx.compose.runtime.Immutable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

/**
 * Lerdr spacing scale — 4dp grid, named for intent. Read via
 * [LerdrTheme.spacing]; screens never scatter raw dp paddings.
 */
@Immutable
data class LerdrSpacing(
    /** Inline gaps inside chips/badges. */
    val extraSmall: Dp = 4.dp,
    /** Between related elements (icon↔label, stacked rows). */
    val small: Dp = 8.dp,
    /** Card padding, list item gaps — the default unit. */
    val medium: Dp = 16.dp,
    /** Section breaks, rail separators. */
    val large: Dp = 24.dp,
    /** Screen-level breathing room, hero gaps. */
    val extraLarge: Dp = 32.dp,
)

val LocalLerdrSpacing = staticCompositionLocalOf { LerdrSpacing() }
