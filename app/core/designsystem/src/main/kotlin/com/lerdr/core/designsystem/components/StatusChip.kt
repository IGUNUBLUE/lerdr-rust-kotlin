package com.lerdr.core.designsystem.components

import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.material3.ExperimentalMaterial3ExpressiveApi
import androidx.compose.material3.MaterialShapes
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.toPath
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.drawscope.withTransform
import androidx.compose.ui.graphics.isSpecified
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.tooling.preview.PreviewLightDark
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.graphics.shapes.CornerRounding
import androidx.graphics.shapes.Morph
import androidx.graphics.shapes.RoundedPolygon
import com.lerdr.core.designsystem.theme.LerdrTheme

/**
 * Agent/session status vocabulary for the morphing indicators
 * (docs/04 §Motion — "morphing shapes on status indicators").
 * [stateLabel] doubles as the accessibility state description.
 */
enum class LerdrStatus(val stateLabel: String) {
    /** Rounded square — quiet, at rest. */
    Idle("Idle"),

    /** Wavy sun — actively producing output. */
    Working("Working"),

    /** Pulsing scalloped cookie — blocked on the user ("needs you"). */
    Attention("Needs attention"),

    /** Sharp notched square in danger tones. */
    Error("Error"),
}

/**
 * Morphing status glyph — a small polygon that shape-shifts between
 * [LerdrStatus] values with the M3E spatial spring. Attention pulses on top
 * of its cookie shape (the spec's "pulsing cookie" for waiting agents).
 *
 * Colors come from [LerdrTheme.extendedColors]; [color] overrides the fill
 * for callers that already sit on a status container.
 */
@OptIn(ExperimentalMaterial3ExpressiveApi::class)
@Composable
fun LerdrStatusDot(
    status: LerdrStatus,
    modifier: Modifier = Modifier,
    size: Dp = 12.dp,
    color: Color = Color.Unspecified,
) {
    val accent = if (color.isSpecified) color else statusAccent(status)
    var displayed by remember { mutableStateOf(statusPolygon(status)) }
    var morph by remember { mutableStateOf(Morph(displayed, displayed)) }
    val progress = remember { Animatable(1f) }
    val morphSpec = MaterialTheme.motionScheme.defaultSpatialSpec<Float>()

    LaunchedEffect(status) {
        val target = statusPolygon(status)
        if (target == displayed) return@LaunchedEffect
        morph = Morph(displayed, target)
        displayed = target
        progress.snapTo(0f)
        progress.animateTo(1f, morphSpec)
    }

    // The "waiting" cue — a slow breathe on the cookie's scale.
    val pulse by if (status == LerdrStatus.Attention) {
        rememberInfiniteTransition(label = "statusPulse").animateFloat(
            initialValue = 0.85f,
            targetValue = 1.05f,
            animationSpec = infiniteRepeatable(
                tween(700, easing = FastOutSlowInEasing),
                repeatMode = RepeatMode.Reverse,
            ),
            label = "statusPulseScale",
        )
    } else {
        rememberUpdatedState(1f)
    }

    // Union bounds across the morph — the fit never jitters mid-transition.
    val bounds = remember(morph) { morph.calculateMaxBounds(FloatArray(4)) }
    val path = remember { Path() }
    Canvas(modifier = modifier.size(size)) {
        val drawSize = this.size // DrawScope.size — the Dp `size` param shadows it otherwise.
        val bw = (bounds[2] - bounds[0]).coerceAtLeast(0.001f)
        val bh = (bounds[3] - bounds[1]).coerceAtLeast(0.001f)
        val fit = minOf(drawSize.width / bw, drawSize.height / bh) * pulse * ShapeInset
        morph.toPath(progress.value, path)
        withTransform({
            translate(drawSize.width / 2f, drawSize.height / 2f)
            scale(fit, fit, Offset.Zero)
            translate(-(bounds[0] + bounds[2]) / 2f, -(bounds[1] + bounds[3]) / 2f)
        }) {
            drawPath(path, color = accent)
        }
    }
}

/**
 * Status pill — the morphing [LerdrStatusDot] plus a label on the matching
 * `extendedColors` container. Merged into one semantics node in a polite
 * live region so status flips announce as a single utterance
 * ("<label>, working"). Not interactive — wrap in a clickable parent when
 * the chip itself should do something.
 */
@Composable
fun LerdrStatusChip(
    status: LerdrStatus,
    label: String,
    modifier: Modifier = Modifier,
    dotSize: Dp = 10.dp,
) {
    val spacing = LerdrTheme.spacing
    Surface(
        shape = MaterialTheme.shapes.small,
        color = statusContainer(status),
        contentColor = statusOnContainer(status),
        modifier = modifier.semantics(mergeDescendants = true) {
            liveRegion = LiveRegionMode.Polite
            stateDescription = status.stateLabel
        },
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.padding(
                horizontal = spacing.small,
                vertical = spacing.extraSmall,
            ),
        ) {
            LerdrStatusDot(status = status, size = dotSize)
            Spacer(Modifier.width(spacing.extraSmall))
            Text(label, style = MaterialTheme.typography.labelMedium, maxLines = 1)
        }
    }
}

// ── internals ─────────────────────────────────────────────────────────

/** Solid accent per status — the dot's fill. */
@Composable
private fun statusAccent(status: LerdrStatus): Color = with(LerdrTheme.extendedColors) {
    when (status) {
        LerdrStatus.Idle -> idle
        LerdrStatus.Working -> working
        LerdrStatus.Attention -> attention
        LerdrStatus.Error -> danger
    }
}

@Composable
private fun statusContainer(status: LerdrStatus): Color = with(LerdrTheme.extendedColors) {
    when (status) {
        LerdrStatus.Idle -> MaterialTheme.colorScheme.surfaceContainerHighest
        LerdrStatus.Working -> workingContainer
        LerdrStatus.Attention -> attentionContainer
        LerdrStatus.Error -> dangerContainer
    }
}

@Composable
private fun statusOnContainer(status: LerdrStatus): Color = with(LerdrTheme.extendedColors) {
    when (status) {
        LerdrStatus.Idle -> MaterialTheme.colorScheme.onSurfaceVariant
        LerdrStatus.Working -> onWorkingContainer
        LerdrStatus.Attention -> onAttentionContainer
        LerdrStatus.Error -> onDangerContainer
    }
}

/** docs/04 mapping: wavy working / pulsing-cookie attention / sharp error. */
@OptIn(ExperimentalMaterial3ExpressiveApi::class)
private fun statusPolygon(status: LerdrStatus): RoundedPolygon = when (status) {
    LerdrStatus.Idle -> MaterialShapes.Square
    LerdrStatus.Working -> MaterialShapes.Sunny
    LerdrStatus.Attention -> MaterialShapes.Cookie9Sided
    LerdrStatus.Error -> ErrorPolygon
}

/** Chamfered square — reads as a corner-notched tile next to the rounder set. */
private val ErrorPolygon = RoundedPolygon(
    floatArrayOf(
        -0.55f, -1f, 0.55f, -1f,
        1f, -0.55f, 1f, 0.55f,
        0.55f, 1f, -0.55f, 1f,
        -1f, 0.55f, -1f, -0.55f,
    ),
    CornerRounding.Unrounded,
)

/** Leave a whisper of padding inside the dot's box so lobes never clip. */
private const val ShapeInset = 0.9f

@PreviewLightDark
@Composable
private fun LerdrStatusChipPreview() {
    LerdrTheme {
        Column(
            verticalArrangement = Arrangement.spacedBy(LerdrTheme.spacing.small),
            modifier = Modifier.padding(LerdrTheme.spacing.medium),
        ) {
            LerdrStatus.entries.forEach { status ->
                LerdrStatusChip(status = status, label = status.stateLabel)
            }
        }
    }
}
