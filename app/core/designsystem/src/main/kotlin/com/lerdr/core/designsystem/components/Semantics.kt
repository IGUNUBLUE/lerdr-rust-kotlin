package com.lerdr.core.designsystem.components

import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics

/**
 * Accessibility conventions (docs/04 §Accessibility).
 *
 * Content descriptions: icon-only controls always carry an explicit
 * `contentDescription` on the `Icon`/`IconButton` — decorative icons inside
 * labeled controls use `contentDescription = null`. These helpers cover the
 * cases the control APIs can't express: announce-on-change regions.
 */

/**
 * Marks the subtree a polite live region — assistive tech announces content
 * changes when the user is idle (status lines, feed rows, error text).
 * Prefer `Modifier.semantics(mergeDescendants = true) { liveRegion = … }`
 * on rows whose children should read as one announcement.
 */
fun Modifier.liveRegionPolite(): Modifier =
    semantics { liveRegion = LiveRegionMode.Polite }

/**
 * Assertive live region — interrupts the current announcement. Reserve for
 * genuinely urgent changes (a blocking approval appearing); default to
 * [liveRegionPolite] everywhere else.
 */
fun Modifier.liveRegionAssertive(): Modifier =
    semantics { liveRegion = LiveRegionMode.Assertive }
