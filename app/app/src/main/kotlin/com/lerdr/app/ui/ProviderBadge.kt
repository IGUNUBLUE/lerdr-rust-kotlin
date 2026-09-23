package com.lerdr.app.ui

import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.lerp
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import com.lerdr.app.R
import kotlin.math.abs

/**
 * Provider avatar — official CLI mark (simple-icons, CC0; devin mark via
 * devin.ai) when the wire `agent` identity is known, a brand-hued
 * monogram tile for the rest of the supported vocabulary (qoder/omo/
 * hermes have no public mark — each gets a deterministic hue so every
 * CLI is still visually distinct). Null/empty providers keep the neutral
 * letter monogram.
 *
 * [prominent] renders the session-chrome variant: gradient-filled disc
 * with a luminous ring keyed to the provider hue. The ring pulses while
 * [active] — i.e. the agent is working or the pane is live.
 */
@Composable
fun ProviderBadge(
    provider: String?,
    label: String,
    size: Dp = 36.dp,
    prominent: Boolean = false,
    active: Boolean = false,
    modifier: Modifier = Modifier,
) {
    val logo = providerLogoRes(provider)
    val (container, content) = when {
        logo != null -> MaterialTheme.colorScheme.primaryContainer to
            MaterialTheme.colorScheme.onPrimaryContainer
        provider != null -> providerTileColors(provider)
        else -> MaterialTheme.colorScheme.primaryContainer to
            MaterialTheme.colorScheme.onPrimaryContainer
    }
    val fill = if (prominent) {
        Modifier.background(
            Brush.linearGradient(
                listOf(
                    lerp(container, Color.White, 0.16f),
                    lerp(container, Color.Black, 0.24f),
                ),
            ),
        )
    } else {
        Modifier.background(container)
    }
    val ring = if (prominent) {
        val hue = provider?.let(::providerHue)
        val base = if (hue != null) {
            Color.hsv(hue, 0.6f, 0.95f)
        } else {
            MaterialTheme.colorScheme.outlineVariant
        }
        val pulse by if (active) {
            rememberInfiniteTransition(label = "providerRing").animateFloat(
                initialValue = 0.35f,
                targetValue = 1f,
                animationSpec = infiniteRepeatable(
                    animation = tween(1100, easing = FastOutSlowInEasing),
                    repeatMode = RepeatMode.Reverse,
                ),
                label = "providerRingAlpha",
            )
        } else {
            rememberUpdatedState(1f)
        }
        val ringColor = base.copy(alpha = base.alpha * pulse)
        Modifier.border(
            1.5.dp,
            Brush.sweepGradient(
                listOf(ringColor, ringColor.copy(alpha = ringColor.alpha * 0.2f), ringColor),
            ),
            CircleShape,
        )
    } else {
        Modifier
    }
    Box(
        contentAlignment = Alignment.Center,
        modifier = modifier
            .size(size)
            .then(ring)
            .clip(CircleShape)
            .then(fill),
    ) {
        if (logo != null) {
            Icon(
                painter = painterResource(logo),
                contentDescription = provider,
                tint = content,
                modifier = Modifier.size(size * 0.55f),
            )
        } else {
            Text(
                providerGlyph(provider)
                    ?: provider?.trim()?.firstOrNull()?.uppercase()
                    ?: label.firstOrNull()?.uppercase() ?: "?",
                style = MaterialTheme.typography.labelMedium,
                color = content,
            )
        }
    }
}

/**
 * Official marks keyed by the normalized `agent` identity the wire
 * carries (`docs/03` agent vocabulary). Null → monogram path.
 */
fun providerLogoRes(provider: String?): Int? = when (provider?.lowercase()?.trim()) {
    "claude", "claudecode" -> R.drawable.ic_provider_claude
    "codex", "openaicodex" -> R.drawable.ic_provider_openai
    "gemini", "geminicli" -> R.drawable.ic_provider_gemini
    "opencode" -> R.drawable.ic_provider_opencode
    "copilot", "githubcopilot" -> R.drawable.ic_provider_copilot
    "cursor" -> R.drawable.ic_provider_cursor
    "devin", "devincli", "devinai" -> R.drawable.ic_provider_devin
    else -> null
}

/** Override glyph for providers better known by a symbol than a letter. */
private fun providerGlyph(provider: String?): String? = when (provider?.lowercase()?.trim()) {
    "pi", "picodingagent", "omp", "ohmypi" -> "π"
    "sh", "bash", "zsh", "fish", "shell", "pwsh", "powershell", "cmd" -> "❯"
    else -> null
}

/** Stable hue for a provider identity — drives tiles and rings. */
private fun providerHue(provider: String): Float =
    abs(provider.lowercase().trim().hashCode()) % 360f

/**
 * Deterministic pastel tile for logo-less providers — HSV rotation of the
 * name keeps every CLI distinguishable across sessions. Colors are drawn
 * in a band that stays readable on both light and dark surfaces.
 */
private fun providerTileColors(provider: String): Pair<Color, Color> {
    val hue = providerHue(provider)
    val container = Color.hsv(hue, 0.45f, 0.72f)
    val content = if (container.luminance() > 0.5f) {
        Color.hsv(hue, 0.75f, 0.28f)
    } else {
        Color.hsv(hue, 0.35f, 0.95f)
    }
    return container to content
}
