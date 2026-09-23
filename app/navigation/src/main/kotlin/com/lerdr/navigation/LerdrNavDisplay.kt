package com.lerdr.navigation

import androidx.compose.animation.AnimatedContentTransitionScope
import androidx.compose.animation.ContentTransform
import androidx.compose.animation.core.CubicBezierEasing
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.scaleIn
import androidx.compose.animation.slideInHorizontally
import androidx.compose.animation.slideOutHorizontally
import androidx.compose.animation.togetherWith
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.dp
import androidx.lifecycle.viewmodel.navigation3.rememberViewModelStoreNavEntryDecorator
import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavEntry
import androidx.navigation3.runtime.entryProvider
import androidx.navigation3.runtime.rememberSaveableStateHolderNavEntryDecorator
import androidx.navigation3.scene.Scene
import androidx.navigation3.ui.NavDisplay

/**
 * The app's [NavDisplay] with Lerdr defaults wired:
 * - `onBack` pops the typed back stack (predictive back comes free —
 *   the manifest sets `enableOnBackInvokedCallback` and NavDisplay's
 *   default predictive-pop spec drives the gesture preview).
 * - `entryDecorators` adds per-entry `ViewModelStore` scoping
 *   ([rememberViewModelStoreNavEntryDecorator]) on top of the saveable-state
 *   holder — entries get ViewModels keyed by nav key.
 * - Transitions follow docs/04 §Motion: a Material shared-axis-X slide for
 *   forward/back list→detail pushes (peer session modes slide in tab
 *   order), and a fade-through for bottom-bar tab switches. Predictive
 *   back keeps the platform default (fade in under a scaling-down top).
 *
 * Feature screens contribute destinations through [entryBuilder] — the
 * `EntryProviderScope<LerdrKey>` receiver keeps everything on the sealed key
 * type.
 */
@Composable
fun LerdrNavDisplay(
    navigator: LerdrNavigator,
    modifier: Modifier = Modifier,
    entryBuilder: EntryProviderScope<LerdrKey>.() -> Unit,
) {
    // Material shared-axis slide distance (mtrl_transition_shared_axis_slide_distance).
    val slideDistancePx = with(LocalDensity.current) { SharedAxisSlideDistance.roundToPx() }
    NavDisplay(
        backStack = navigator.backStack,
        modifier = modifier,
        onBack = navigator::goBack,
        entryDecorators = listOf(
            rememberSaveableStateHolderNavEntryDecorator(),
            rememberViewModelStoreNavEntryDecorator(),
        ),
        transitionSpec = { forwardSpec(slideDistancePx) },
        popTransitionSpec = { popSpec(slideDistancePx) },
        entryProvider = entryProvider<LerdrKey>(
            fallback = { key ->
                NavEntry(key) {
                    error("No navigation entry registered for $key")
                }
            },
            builder = entryBuilder,
        ),
    )
}

/** The key the scene surfaces — the top entry of its back-stack slice. */
private fun Scene<LerdrKey>.topKey(): LerdrKey? =
    entries.lastOrNull()?.contentKey as? LerdrKey

/** Both ends on the bottom bar → fade-through, never a lateral slide. */
private fun AnimatedContentTransitionScope<Scene<LerdrKey>>.isTopLevelSwitch(): Boolean =
    initialState.topKey() in LerdrKey.topLevel && targetState.topKey() in LerdrKey.topLevel

/**
 * Feed/Terminal/Files are ordered modes of one session — switching modes
 * slides in the direction the segmented control implies.
 */
private fun sessionModeIndex(key: LerdrKey?): Int = when (key) {
    is LerdrKey.AgentFeed -> 0
    is LerdrKey.Terminal -> 1
    is LerdrKey.Files -> 2
    else -> -1
}

/** Push/place transition — fade-through between tabs, shared-axis otherwise. */
private fun AnimatedContentTransitionScope<Scene<LerdrKey>>.forwardSpec(
    slidePx: Int,
): ContentTransform {
    if (isTopLevelSwitch()) return fadeThrough()
    val from = sessionModeIndex(initialState.topKey())
    val to = sessionModeIndex(targetState.topKey())
    val forward = from < 0 || to < 0 || to >= from
    return sharedAxisX(forward = forward, slidePx = slidePx)
}

/** Back transition — fade-through between tabs, reversed shared-axis else. */
private fun AnimatedContentTransitionScope<Scene<LerdrKey>>.popSpec(
    slidePx: Int,
): ContentTransform {
    if (isTopLevelSwitch()) return fadeThrough()
    return sharedAxisX(forward = false, slidePx = slidePx)
}

/**
 * Material shared-axis X: both surfaces slide 30dp along the travel
 * direction while cross-fading — the outgoing fade completes in the first
 * 35% of the transition, the incoming fade starts there (MDC
 * `MaterialSharedAxis` recipe under the M3 emphasized easing).
 */
private fun AnimatedContentTransitionScope<Scene<LerdrKey>>.sharedAxisX(
    forward: Boolean,
    slidePx: Int,
): ContentTransform {
    val sign = if (forward) 1 else -1
    return slideInHorizontally(
        animationSpec = tween(SharedAxisDurationMillis, easing = EmphasizedDecelerate),
        initialOffsetX = { sign * slidePx },
    ) + fadeIn(
        tween(SharedAxisFadeInMillis, delayMillis = SharedAxisFadeOutMillis, easing = LinearEasing),
    ) togetherWith slideOutHorizontally(
        animationSpec = tween(SharedAxisDurationMillis, easing = EmphasizedAccelerate),
        targetOffsetX = { -sign * slidePx },
    ) + fadeOut(tween(SharedAxisFadeOutMillis, easing = LinearEasing))
}

/**
 * Material fade-through (bottom-bar tab switches): the outgoing surface
 * fades out first, then the incoming scales 0.92→1 while fading in.
 */
private fun fadeThrough(): ContentTransform =
    fadeIn(
        tween(FadeThroughInMillis, delayMillis = FadeThroughOutMillis, easing = LinearEasing),
    ) + scaleIn(
        initialScale = FadeThroughScaleFrom,
        animationSpec = tween(FadeThroughDurationMillis, easing = EmphasizedDecelerate),
    ) togetherWith fadeOut(tween(FadeThroughOutMillis, easing = LinearEasing))

// M3 emphasized motion tokens (motionEasingEmphasized*).
private val EmphasizedDecelerate = CubicBezierEasing(0.05f, 0.7f, 0.1f, 1f)
private val EmphasizedAccelerate = CubicBezierEasing(0.3f, 0f, 0.8f, 0.15f)

// MDC shared-axis recipe: 300ms slide, fades meet at the 35% mark.
private const val SharedAxisDurationMillis = 300
private const val SharedAxisFadeOutMillis = 105
private const val SharedAxisFadeInMillis = 195
private val SharedAxisSlideDistance = 30.dp

// MDC fade-through recipe: 90ms out, 210ms in, incoming grows from 0.92.
private const val FadeThroughDurationMillis = 300
private const val FadeThroughOutMillis = 90
private const val FadeThroughInMillis = 210
private const val FadeThroughScaleFrom = 0.92f
