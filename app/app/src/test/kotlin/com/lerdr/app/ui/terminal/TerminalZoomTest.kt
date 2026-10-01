package com.lerdr.app.ui.terminal

import androidx.compose.foundation.ScrollState
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Pinch zoom + fit-width scale plumbing — `applyFontScale` is the shared
 * path both gestures and the toolbar's fit-width action use: it clamps
 * to the zoom bounds and reports the applied value exactly once.
 */
class TerminalZoomTest {

    private fun state(): Pair<TerminalSurfaceState, MutableList<Float>> {
        val applied = mutableListOf<Float>()
        return TerminalSurfaceState(
            ScrollState(0),
            onFontScaleChanged = applied::add,
        ) to applied
    }

    @Test
    fun `applyFontScale applies and reports the value`() {
        val (state, applied) = state()
        state.applyFontScale(0.35f)
        assertEquals(0.35f, state.fontScale, 0.0001f)
        assertEquals(listOf(0.35f), applied)
    }

    @Test
    fun `applyFontScale clamps into the zoom bounds`() {
        val (state, applied) = state()
        state.applyFontScale(0.05f)
        assertEquals(0.25f, state.fontScale, 0.0001f)
        state.applyFontScale(9f)
        assertEquals(2.5f, state.fontScale, 0.0001f)
        assertEquals(listOf(0.25f, 2.5f), applied)
    }

    @Test
    fun `applyFontScale fires no callback for a no-op`() {
        val (state, applied) = state()
        state.applyFontScale(1f)
        assertTrue(applied.isEmpty())
    }

    @Test
    fun `zoomBy multiplies the current scale`() {
        val (state, _) = state()
        state.applyFontScale(0.5f)
        state.zoomBy(2f)
        assertEquals(1f, state.fontScale, 0.0001f)
        state.zoomBy(10f)
        assertEquals(2.5f, state.fontScale, 0.0001f)
    }

    @Test
    fun `fitWidthScale defaults to actual size`() {
        val (state, _) = state()
        assertEquals(1f, state.fitWidthScale, 0.0001f)
    }
}
