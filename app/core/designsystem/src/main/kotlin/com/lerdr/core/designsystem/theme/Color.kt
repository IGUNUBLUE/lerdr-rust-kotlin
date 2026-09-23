package com.lerdr.core.designsystem.theme

import androidx.compose.material3.ColorScheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color

/**
 * Lerdr brand palette — the fallback when dynamic color is unavailable or
 * disabled (API < 31, sideloaded contexts, user toggle). Dark-first: the
 * product spends its life next to terminals, so the dark scheme carries the
 * design and light is a tonal inversion of the same hues.
 */

// Dark scheme — tuned against docs/mockup.png (near-black navy surfaces,
// periwinkle primary, green "working", amber "needs you").
private val LerdrDarkPrimary = Color(0xFFA9C6F8)
private val LerdrDarkOnPrimary = Color(0xFF0D2244)
private val LerdrDarkPrimaryContainer = Color(0xFF274777)
private val LerdrDarkOnPrimaryContainer = Color(0xFFD7E3FF)
private val LerdrDarkSecondary = Color(0xFFBBC7DB)
private val LerdrDarkOnSecondary = Color(0xFF253140)
private val LerdrDarkSecondaryContainer = Color(0xFF3B4758)
private val LerdrDarkOnSecondaryContainer = Color(0xFFD7E3F7)
private val LerdrDarkTertiary = Color(0xFF7BD9A8)
private val LerdrDarkOnTertiary = Color(0xFF003822)
private val LerdrDarkTertiaryContainer = Color(0xFF195238)
private val LerdrDarkOnTertiaryContainer = Color(0xFF97F6C4)
private val LerdrDarkBackground = Color(0xFF0B0E14)
private val LerdrDarkOnBackground = Color(0xFFDEE3EE)
private val LerdrDarkSurface = Color(0xFF0B0E14)
private val LerdrDarkOnSurface = Color(0xFFDEE3EE)
private val LerdrDarkSurfaceVariant = Color(0xFF1B2130)
private val LerdrDarkOnSurfaceVariant = Color(0xFF9FA9BF)
private val LerdrDarkOutline = Color(0xFF4A5568)
private val LerdrDarkOutlineVariant = Color(0xFF2A3142)
private val LerdrDarkError = Color(0xFFF2A09B)
private val LerdrDarkOnError = Color(0xFF601410)
private val LerdrDarkErrorContainer = Color(0xFF8C1D18)
private val LerdrDarkOnErrorContainer = Color(0xFFF9DEDC)
private val LerdrDarkInverseSurface = Color(0xFFDEE3EE)
private val LerdrDarkInverseOnSurface = Color(0xFF2A2F3A)
private val LerdrDarkInversePrimary = Color(0xFF3F5F9E)
private val LerdrDarkScrim = Color(0xFF000000)
private val LerdrDarkSurfaceBright = Color(0xFF31363F)
private val LerdrDarkSurfaceDim = Color(0xFF0B0E14)
private val LerdrDarkSurfaceContainerLowest = Color(0xFF060810)
private val LerdrDarkSurfaceContainerLow = Color(0xFF131720)
private val LerdrDarkSurfaceContainer = Color(0xFF171B26)
private val LerdrDarkSurfaceContainerHigh = Color(0xFF1E2431)
private val LerdrDarkSurfaceContainerHighest = Color(0xFF262D3C)

// Light scheme — tonal inversion; brand hues stay, surfaces lift.
private val LerdrLightPrimary = Color(0xFF355CA6)
private val LerdrLightOnPrimary = Color(0xFFFFFFFF)
private val LerdrLightPrimaryContainer = Color(0xFFD9E2FF)
private val LerdrLightOnPrimaryContainer = Color(0xFF001945)
private val LerdrLightSecondary = Color(0xFF555F71)
private val LerdrLightOnSecondary = Color(0xFFFFFFFF)
private val LerdrLightSecondaryContainer = Color(0xFFD9E3F8)
private val LerdrLightOnSecondaryContainer = Color(0xFF121C2B)
private val LerdrLightTertiary = Color(0xFF156B47)
private val LerdrLightOnTertiary = Color(0xFFFFFFFF)
private val LerdrLightTertiaryContainer = Color(0xFFAAF2C7)
private val LerdrLightOnTertiaryContainer = Color(0xFF002112)
private val LerdrLightBackground = Color(0xFFF8F9FE)
private val LerdrLightOnBackground = Color(0xFF191C22)
private val LerdrLightSurface = Color(0xFFF8F9FE)
private val LerdrLightOnSurface = Color(0xFF191C22)
private val LerdrLightSurfaceVariant = Color(0xFFDFE2EF)
private val LerdrLightOnSurfaceVariant = Color(0xFF434851)
private val LerdrLightOutline = Color(0xFF737882)
private val LerdrLightOutlineVariant = Color(0xFFC3C6D3)
private val LerdrLightError = Color(0xFFBA1A1A)
private val LerdrLightOnError = Color(0xFFFFFFFF)
private val LerdrLightErrorContainer = Color(0xFFFFDAD6)
private val LerdrLightOnErrorContainer = Color(0xFF410002)
private val LerdrLightInverseSurface = Color(0xFF2E3037)
private val LerdrLightInverseOnSurface = Color(0xFFEFF0F6)
private val LerdrLightInversePrimary = Color(0xFFA9C6F8)
private val LerdrLightScrim = Color(0xFF000000)
private val LerdrLightSurfaceBright = Color(0xFFF8F9FE)
private val LerdrLightSurfaceDim = Color(0xFFD8DAE2)
private val LerdrLightSurfaceContainerLowest = Color(0xFFFFFFFF)
private val LerdrLightSurfaceContainerLow = Color(0xFFF2F4F8)
private val LerdrLightSurfaceContainer = Color(0xFFECEEF4)
private val LerdrLightSurfaceContainerHigh = Color(0xFFE7E8EE)
private val LerdrLightSurfaceContainerHighest = Color(0xFFE1E3E9)

/** Brand light scheme — used when dynamic color is off. */
val LerdrLightColorScheme: ColorScheme = lightColorScheme(
    primary = LerdrLightPrimary,
    onPrimary = LerdrLightOnPrimary,
    primaryContainer = LerdrLightPrimaryContainer,
    onPrimaryContainer = LerdrLightOnPrimaryContainer,
    secondary = LerdrLightSecondary,
    onSecondary = LerdrLightOnSecondary,
    secondaryContainer = LerdrLightSecondaryContainer,
    onSecondaryContainer = LerdrLightOnSecondaryContainer,
    tertiary = LerdrLightTertiary,
    onTertiary = LerdrLightOnTertiary,
    tertiaryContainer = LerdrLightTertiaryContainer,
    onTertiaryContainer = LerdrLightOnTertiaryContainer,
    background = LerdrLightBackground,
    onBackground = LerdrLightOnBackground,
    surface = LerdrLightSurface,
    onSurface = LerdrLightOnSurface,
    surfaceVariant = LerdrLightSurfaceVariant,
    onSurfaceVariant = LerdrLightOnSurfaceVariant,
    surfaceTint = LerdrLightPrimary,
    inverseSurface = LerdrLightInverseSurface,
    inverseOnSurface = LerdrLightInverseOnSurface,
    inversePrimary = LerdrLightInversePrimary,
    error = LerdrLightError,
    onError = LerdrLightOnError,
    errorContainer = LerdrLightErrorContainer,
    onErrorContainer = LerdrLightOnErrorContainer,
    outline = LerdrLightOutline,
    outlineVariant = LerdrLightOutlineVariant,
    scrim = LerdrLightScrim,
    surfaceBright = LerdrLightSurfaceBright,
    surfaceDim = LerdrLightSurfaceDim,
    surfaceContainer = LerdrLightSurfaceContainer,
    surfaceContainerHigh = LerdrLightSurfaceContainerHigh,
    surfaceContainerHighest = LerdrLightSurfaceContainerHighest,
    surfaceContainerLow = LerdrLightSurfaceContainerLow,
    surfaceContainerLowest = LerdrLightSurfaceContainerLowest,
)

/** Brand dark scheme — the flagship look (see docs/mockup.png). */
val LerdrDarkColorScheme: ColorScheme = darkColorScheme(
    primary = LerdrDarkPrimary,
    onPrimary = LerdrDarkOnPrimary,
    primaryContainer = LerdrDarkPrimaryContainer,
    onPrimaryContainer = LerdrDarkOnPrimaryContainer,
    secondary = LerdrDarkSecondary,
    onSecondary = LerdrDarkOnSecondary,
    secondaryContainer = LerdrDarkSecondaryContainer,
    onSecondaryContainer = LerdrDarkOnSecondaryContainer,
    tertiary = LerdrDarkTertiary,
    onTertiary = LerdrDarkOnTertiary,
    tertiaryContainer = LerdrDarkTertiaryContainer,
    onTertiaryContainer = LerdrDarkOnTertiaryContainer,
    background = LerdrDarkBackground,
    onBackground = LerdrDarkOnBackground,
    surface = LerdrDarkSurface,
    onSurface = LerdrDarkOnSurface,
    surfaceVariant = LerdrDarkSurfaceVariant,
    onSurfaceVariant = LerdrDarkOnSurfaceVariant,
    surfaceTint = LerdrDarkPrimary,
    inverseSurface = LerdrDarkInverseSurface,
    inverseOnSurface = LerdrDarkInverseOnSurface,
    inversePrimary = LerdrDarkInversePrimary,
    error = LerdrDarkError,
    onError = LerdrDarkOnError,
    errorContainer = LerdrDarkErrorContainer,
    onErrorContainer = LerdrDarkOnErrorContainer,
    outline = LerdrDarkOutline,
    outlineVariant = LerdrDarkOutlineVariant,
    scrim = LerdrDarkScrim,
    surfaceBright = LerdrDarkSurfaceBright,
    surfaceDim = LerdrDarkSurfaceDim,
    surfaceContainer = LerdrDarkSurfaceContainer,
    surfaceContainerHigh = LerdrDarkSurfaceContainerHigh,
    surfaceContainerHighest = LerdrDarkSurfaceContainerHighest,
    surfaceContainerLow = LerdrDarkSurfaceContainerLow,
    surfaceContainerLowest = LerdrDarkSurfaceContainerLowest,
)

/**
 * Semantic agent colors — roles the Material [ColorScheme] has no slot for:
 * the "working" green, "needs you" amber, and muted idle grey from the
 * mission-control mockup. Consumed via [LerdrTheme.extendedColors].
 */
@Immutable
data class LerdrExtendedColors(
    /** Agent actively producing output (progress strips, status chips). */
    val working: Color,
    val onWorking: Color,
    val workingContainer: Color,
    val onWorkingContainer: Color,
    /** Agent blocked on approval/question — the "needs you" accent. */
    val attention: Color,
    val onAttention: Color,
    val attentionContainer: Color,
    val onAttentionContainer: Color,
    /** Agent waiting on chat input. */
    val chat: Color,
    val chatContainer: Color,
    val onChatContainer: Color,
    /** Idle agent / dimmed affordance. */
    val idle: Color,
    /**
     * Muted destructive/deny accent — triage "Deny", stop/close actions.
     * Deliberately quieter than [ColorScheme.error]: the mockup's deny chip
     * is maroon, not the saturated alert red.
     */
    val danger: Color,
    val onDanger: Color,
    val dangerContainer: Color,
    val onDangerContainer: Color,
    /** Live-connection dot and chip. */
    val live: Color,
    /** Terminal surface — darker than [ColorScheme.surface]. */
    val terminalSurface: Color,
    val terminalText: Color,
    val terminalAccent: Color,
)

val LerdrDarkExtendedColors = LerdrExtendedColors(
    working = Color(0xFF5BD99A),
    onWorking = Color(0xFF00391F),
    workingContainer = Color(0xFF143426),
    onWorkingContainer = Color(0xFF9EEBC0),
    attention = Color(0xFFE8A04C),
    onAttention = Color(0xFF422700),
    attentionContainer = Color(0xFF3A2812),
    onAttentionContainer = Color(0xFFF0C896),
    chat = Color(0xFF9BA8FF),
    chatContainer = Color(0xFF232B52),
    onChatContainer = Color(0xFFC9D2FF),
    idle = Color(0xFF8A94A6),
    danger = Color(0xFFE88C8C),
    onDanger = Color(0xFF4A0B0B),
    dangerContainer = Color(0xFF3A1414),
    onDangerContainer = Color(0xFFF5B8B8),
    live = Color(0xFF5BD99A),
    terminalSurface = Color(0xFF06080D),
    terminalText = Color(0xFFA8D8B9),
    terminalAccent = Color(0xFF6FC3DF),
)

val LerdrLightExtendedColors = LerdrExtendedColors(
    working = Color(0xFF18794C),
    onWorking = Color(0xFFFFFFFF),
    workingContainer = Color(0xFFA7F0C9),
    onWorkingContainer = Color(0xFF002112),
    attention = Color(0xFF825500),
    onAttention = Color(0xFFFFFFFF),
    attentionContainer = Color(0xFFFFDDB3),
    onAttentionContainer = Color(0xFF291800),
    chat = Color(0xFF4356B4),
    chatContainer = Color(0xFFDCE1FF),
    onChatContainer = Color(0xFF00105C),
    idle = Color(0xFF5B6170),
    danger = Color(0xFFA33B3B),
    onDanger = Color(0xFFFFFFFF),
    dangerContainer = Color(0xFFF5D5D5),
    onDangerContainer = Color(0xFF3E0B0B),
    live = Color(0xFF18794C),
    terminalSurface = Color(0xFF10131C),
    terminalText = Color(0xFFA8D8B9),
    terminalAccent = Color(0xFF6FC3DF),
)

val LocalLerdrExtendedColors = staticCompositionLocalOf { LerdrDarkExtendedColors }
