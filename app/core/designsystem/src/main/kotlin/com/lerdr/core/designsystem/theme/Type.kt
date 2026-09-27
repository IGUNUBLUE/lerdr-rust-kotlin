package com.lerdr.core.designsystem.theme

import androidx.compose.material3.Typography
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp
import com.lerdr.core.designsystem.R

/**
 * Lerdr type scale — Material defaults with product tweaks: titles read as
 * mission-control headers (slightly tighter), labels stay legible at chip
 * sizes. Monospace lives in [LerdrTextStyles], not the M3 scale.
 */
val LerdrTypography = Typography(
    headlineMedium = TextStyle(
        fontFamily = FontFamily.Default,
        fontWeight = FontWeight.SemiBold,
        fontSize = 28.sp,
        lineHeight = 34.sp,
        letterSpacing = (-0.2).sp,
    ),
    titleMedium = TextStyle(
        fontFamily = FontFamily.Default,
        fontWeight = FontWeight.SemiBold,
        fontSize = 16.sp,
        lineHeight = 22.sp,
        letterSpacing = 0.1.sp,
    ),
    labelMedium = TextStyle(
        fontFamily = FontFamily.Default,
        fontWeight = FontWeight.Medium,
        fontSize = 12.sp,
        lineHeight = 16.sp,
        letterSpacing = 0.4.sp,
    ),
)

/**
 * Bundled JetBrainsMono Nerd Font Mono — single-width coverage for the
 * box-drawing, Powerline, and private-use icon ranges agent status lines
 * draw with (OFL, see `OFL-JetBrainsMonoNerdFont.txt`). The `Mono` variant
 * pins every icon to one cell so the terminal grid stays aligned; glyphs
 * outside the font (CJK, emoji) still fall through to system fallback.
 */
private val LerdrMonoFamily = FontFamily(
    Font(R.font.jbmono_nerd_regular, FontWeight.Normal),
    Font(R.font.jbmono_nerd_bold, FontWeight.Bold),
)

/**
 * Product text styles outside the M3 scale. `terminal` is the face of the
 * terminal renderer (docs/04-app-design.md: monospace, ≥10sp effective).
 */
object LerdrTextStyles {
    val terminal = TextStyle(
        fontFamily = LerdrMonoFamily,
        fontWeight = FontWeight.Normal,
        fontSize = 12.sp,
        lineHeight = 16.sp,
        letterSpacing = 0.sp,
    )

    val code = TextStyle(
        fontFamily = LerdrMonoFamily,
        fontWeight = FontWeight.Normal,
        fontSize = 13.sp,
        lineHeight = 18.sp,
        letterSpacing = 0.sp,
    )
}
