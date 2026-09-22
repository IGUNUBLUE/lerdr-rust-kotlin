package com.lerdr.app.ui.terminal

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.ScrollState
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.requiredSize
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalLayoutDirection
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.drawText
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.PreviewLightDark
import androidx.compose.ui.unit.dp
import com.lerdr.app.session.SessionRepository
import com.lerdr.core.designsystem.theme.LerdrTextStyles
import com.lerdr.core.designsystem.theme.LerdrTheme
import kotlin.math.ceil
import kotlin.math.floor
import kotlin.math.max
import kotlinx.coroutines.delay

/**
 * The pane grid — renders committed [TerminalRowUi]s as a monospace cell
 * matrix plus the write cursor.
 *
 * Why a `Canvas` and not a per-row `LazyColumn` (the skill's terminal rule):
 * pane deltas commit at interactive rates and the row model is a
 * fixed-pitch grid — row i always sits at `y = i * rowHeight` — so one
 * Canvas can draw just the visible window (manual virtualization) with
 * zero per-row composition or layout passes. Measured rows rebuild only
 * when [rows] changes instance (the ViewModel reuses the same list across
 * metadata-only commits, so unchanged content keeps its draw cache), and
 * the scroll offset is read inside the draw pass — scrolling the
 * scrollback never recomposes. `LazyColumn` would pay an item composition
 * + layout per visible row per commit for nothing the grid needs.
 *
 * Find-in-buffer: [findRanges] is the `mark[data-terminal-find]` layer —
 * row index → character ranges to highlight, each tagged active. Only
 * matched rows get a re-measured overlay layout, and the highlight state
 * is a separate remember key so query/active changes never touch the
 * committed row layouts (the fingerprint/delta reuse is unaffected).
 * [state] exposes [TerminalSurfaceState.revealRow] for match reveal and
 * carries the follow-live pin the reveal releases.
 */
@Composable
fun TerminalSurface(
    rows: List<TerminalRowUi>,
    cursor: TerminalCursorUi?,
    revision: Long,
    modifier: Modifier = Modifier,
    textColor: Color = LerdrTheme.extendedColors.terminalText,
    cursorColor: Color = LerdrTheme.extendedColors.terminalAccent,
    contentPadding: PaddingValues = PaddingValues(0.dp),
    state: TerminalSurfaceState = rememberTerminalSurfaceState(),
    findRanges: Map<Int, List<TerminalFindRange>> = emptyMap(),
    findMatchColor: Color = FIND_MATCH_COLOR,
    findActiveColor: Color = FIND_ACTIVE_COLOR,
    findTextColor: Color = FIND_TEXT_COLOR,
    onViewportMeasured: (columns: Int, rows: Int) -> Unit = { _, _ -> },
) {
    val density = LocalDensity.current
    val layoutDirection = LocalLayoutDirection.current
    val textMeasurer = rememberTextMeasurer()
    val baseStyle: TextStyle = LerdrTextStyles.terminal

    // Monospace grid metrics — one probe defines every cell. Re-probe when
    // density/font scale or the style changes; never per frame.
    val metrics = remember(textMeasurer, baseStyle, density) {
        val probe = textMeasurer.measure(
            text = AnnotatedString(CELL_PROBE),
            style = baseStyle,
            softWrap = false,
            maxLines = 1,
        )
        CellMetrics(
            cellWidth = probe.size.width / CELL_PROBE.length.toFloat(),
            rowHeight = probe.size.height.toFloat(),
        )
    }

    // Row layouts — built once per committed frame, not per draw.
    val rowLayouts = remember(rows, textMeasurer, baseStyle, textColor) {
        val style = baseStyle.copy(color = textColor)
        rows.map { row ->
            textMeasurer.measure(
                text = row.toAnnotatedString(textColor),
                style = style,
                overflow = TextOverflow.Clip,
                softWrap = false,
                maxLines = 1,
            )
        }
    }

    // Find overlays — only rows carrying a match re-measure, with the
    // oracle's dark mark fg over the SGR color. Keyed on the range map, so
    // query/active changes leave the committed row layouts untouched.
    val findLayouts = remember(
        rows, findRanges, textMeasurer, baseStyle, textColor, findTextColor,
    ) {
        if (findRanges.isEmpty()) {
            emptyMap()
        } else {
            findRanges.mapNotNull { (index, ranges) ->
                val row = rows.getOrNull(index) ?: return@mapNotNull null
                index to textMeasurer.measure(
                    text = row.toAnnotatedString(textColor, ranges, findTextColor),
                    style = baseStyle.copy(color = textColor),
                    overflow = TextOverflow.Clip,
                    softWrap = false,
                    maxLines = 1,
                )
            }.toMap()
        }
    }

    // Blink rides a 530 ms toggle — a state read inside the draw pass, so
    // it repaints the cursor twice a second instead of animating per frame.
    val cursorOn by produceState(initialValue = true) {
        while (true) {
            delay(CURSOR_BLINK_MS)
            value = !value
        }
    }

    val verticalScroll = state.scrollState
    val horizontalScroll = rememberScrollState()

    SideEffect {
        state.rowHeightPx = metrics.rowHeight
        state.stickThresholdPx = with(density) { STICK_THRESHOLD_DP.dp.toPx() }
    }

    // The oracle's handleScroll: scrolling up into history releases the
    // follow-live pin; landing at the bottom edge re-pins it. Programmatic
    // scrolls (follow-live, find reveal) are excluded from the read.
    LaunchedEffect(state) {
        var previous = state.scrollState.value
        snapshotFlow { state.scrollState.value }.collect { value ->
            if (state.scrollState.isScrollInProgress && state.programmaticScrolls == 0) {
                val bottomDistance = state.scrollState.maxValue - value
                if (value < previous - 1 && bottomDistance > 1) {
                    state.stickToBottom = false
                } else if (bottomDistance < state.stickThresholdPx) {
                    state.stickToBottom = true
                }
            }
            previous = value
        }
    }

    BoxWithConstraints(modifier = modifier) {
        val padHorizontal = with(density) {
            (
                contentPadding.calculateLeftPadding(layoutDirection) +
                    contentPadding.calculateRightPadding(layoutDirection)
                ).toPx()
        }
        val padVertical = with(density) {
            (contentPadding.calculateTopPadding() + contentPadding.calculateBottomPadding())
                .toPx()
        }
        val viewportWidth = constraints.maxWidth.toFloat() - padHorizontal
        val viewportHeight = constraints.maxHeight.toFloat() - padVertical

        // The measured cell grid — clamped into the wire lease envelope so
        // a narrow viewport negotiates a legal size instead of erroring.
        val gridColumns = floor(viewportWidth / metrics.cellWidth).toInt()
            .coerceIn(SessionRepository.MIN_PANE_COLUMNS, SessionRepository.MAX_PANE_COLUMNS)
        val gridRows = floor(viewportHeight / metrics.rowHeight).toInt()
            .coerceIn(SessionRepository.MIN_PANE_ROWS, SessionRepository.MAX_PANE_ROWS)
        LaunchedEffect(gridColumns, gridRows) {
            onViewportMeasured(gridColumns, gridRows)
        }

        // Follow-live: each commit keeps the write edge in view while the
        // pin holds — a find reveal or a scroll into history releases it.
        LaunchedEffect(revision) {
            if (state.stickToBottom) state.scrollToBottom()
        }

        val maxCells = rows.maxOfOrNull { it.cells } ?: 0
        val contentWidth = max(viewportWidth, maxCells * metrics.cellWidth)
        val contentHeight = rows.size * metrics.rowHeight

        Box(
            modifier = Modifier
                .fillMaxSize()
                .padding(contentPadding)
                .verticalScroll(verticalScroll)
                .horizontalScroll(horizontalScroll),
        ) {
            Canvas(
                modifier = Modifier.requiredSize(
                    width = with(density) { contentWidth.toDp() },
                    height = with(density) { contentHeight.toDp() },
                ),
            ) {
                val scrollY = verticalScroll.value.toFloat()
                val viewport = verticalScroll.viewportSize.toFloat()
                // Manual virtualization: only the rows intersecting the
                // viewport emit draw calls.
                val firstRow = floor(scrollY / metrics.rowHeight).toInt()
                    .coerceIn(0, rowLayouts.size)
                val lastRow = ceil((scrollY + viewport) / metrics.rowHeight).toInt()
                    .coerceIn(firstRow, rowLayouts.size)
                for (index in firstRow until lastRow) {
                    val layout = findLayouts[index] ?: rowLayouts[index]
                    val ranges = findRanges[index]
                    if (ranges != null) {
                        // The mark fill — getPathForRange resolves the exact
                        // glyph extent, so wide cells and tab-expanded runs
                        // highlight at their rendered width.
                        val rowTop = index * metrics.rowHeight
                        for (range in ranges) {
                            val bounds = layout.getPathForRange(range.start, range.end)
                                .getBounds()
                            drawRect(
                                color = if (range.active) findActiveColor else findMatchColor,
                                topLeft = Offset(bounds.left, rowTop),
                                size = Size(bounds.width, metrics.rowHeight),
                            )
                            if (range.active) {
                                // The oracle's .active box-shadow ring.
                                drawRect(
                                    color = FIND_ACTIVE_RING_COLOR,
                                    topLeft = Offset(bounds.left, rowTop),
                                    size = Size(bounds.width, metrics.rowHeight),
                                    style = Stroke(width = ACTIVE_RING_WIDTH),
                                )
                            }
                        }
                    }
                    drawText(
                        textLayoutResult = layout,
                        topLeft = Offset(0f, index * metrics.rowHeight),
                    )
                }
                if (cursor != null && cursorOn && cursor.row in firstRow until lastRow) {
                    drawRect(
                        color = cursorColor,
                        topLeft = Offset(
                            x = cursor.column * metrics.cellWidth,
                            y = cursor.row * metrics.rowHeight,
                        ),
                        size = Size(metrics.cellWidth, metrics.rowHeight),
                        alpha = CURSOR_ALPHA,
                    )
                }
            }
        }
    }
}

/**
 * Scroll/highlight handle for [TerminalSurface] — owns the vertical
 * [scrollState] so callers can reveal a find match, and carries the
 * follow-live pin (`virtualStickToBottom` in the oracle): true while the
 * view rides the write edge, released by scrolling into history or by a
 * find reveal, re-armed within [stickThresholdPx] of the bottom.
 */
@Stable
class TerminalSurfaceState internal constructor(
    internal val scrollState: ScrollState,
) {
    internal var rowHeightPx = 0f
    internal var stickThresholdPx = 0f
    internal var programmaticScrolls = 0

    /** Follow-live pin — new commits keep the write edge in view while set. */
    var stickToBottom by mutableStateOf(true)
        internal set

    /**
     * `revealFindMatch`'s scroll — center [row] in the viewport, release
     * the pin, then re-derive it from the landed position (a match already
     * at the bottom edge keeps follow-live).
     */
    suspend fun revealRow(row: Int) {
        val rowHeight = rowHeightPx
        if (row < 0 || rowHeight <= 0f) return
        programmaticScrolls += 1
        stickToBottom = false
        try {
            val target = (row * rowHeight - (scrollState.viewportSize - rowHeight) / 2f)
                .coerceIn(0f, scrollState.maxValue.toFloat())
            // Instant like the oracle's `scrollTop =` assignment — stepping
            // through matches shouldn't animate between jumps.
            scrollState.scrollTo(target.toInt())
            stickToBottom = scrollState.maxValue - scrollState.value < stickThresholdPx
        } finally {
            programmaticScrolls -= 1
        }
    }

    /** Follow-live tick — the revision effect pins to the write edge. */
    internal suspend fun scrollToBottom() {
        programmaticScrolls += 1
        try {
            scrollState.scrollTo(scrollState.maxValue)
        } finally {
            programmaticScrolls -= 1
        }
    }
}

@Composable
fun rememberTerminalSurfaceState(
    scrollState: ScrollState = rememberScrollState(),
): TerminalSurfaceState = remember(scrollState) { TerminalSurfaceState(scrollState) }

/** Monospace probe — ten digits average out per-glyph hinting error. */
private const val CELL_PROBE = "0123456789"

/** Find mark colors — `mark.terminal-find-match` / `.active` verbatim. */
private val FIND_MATCH_COLOR = Color(0xFFF2D66D)
private val FIND_ACTIVE_COLOR = Color(0xFFFF8A4C)
private val FIND_TEXT_COLOR = Color(0xFF17120A)

/** `.active`'s `box-shadow: 0 0 0 1px #fff6`. */
private val FIND_ACTIVE_RING_COLOR = Color(0x66FFFFFF)
private const val ACTIVE_RING_WIDTH = 2f

/** Follow-live re-pin distance — the oracle's 48 px bottom edge. */
private const val STICK_THRESHOLD_DP = 48

/** Cursor block opacity — translucent so the glyph under it stays legible. */
private const val CURSOR_ALPHA = 0.35f
private const val CURSOR_BLINK_MS = 530L

private class CellMetrics(val cellWidth: Float, val rowHeight: Float)

/**
 * Row → AnnotatedString: SGR fields → [SpanStyle], links underlined.
 * [findRanges] overlay the mark fg — added last, they win over the SGR
 * colors underneath (the oracle's mark color swap keeps matched glyphs
 * legible on the highlight fill).
 */
private fun TerminalRowUi.toAnnotatedString(
    defaultColor: Color,
    findRanges: List<TerminalFindRange> = emptyList(),
    findTextColor: Color = Color.Unspecified,
): AnnotatedString {
    val builder = AnnotatedString.Builder()
    for (span in spans) {
        if (span.text.isEmpty()) continue
        val start = builder.length
        builder.append(span.text)
        val fg = (span.fg ?: defaultColor).let { color ->
            if (span.dim) color.copy(alpha = color.alpha * DIM_ALPHA) else color
        }
        builder.addStyle(
            SpanStyle(
                color = fg,
                background = span.bg ?: Color.Unspecified,
                fontWeight = if (span.bold) FontWeight.Bold else null,
                fontStyle = if (span.italic) FontStyle.Italic else null,
                textDecoration = if (span.underline || span.href != null) {
                    TextDecoration.Underline
                } else {
                    null
                },
            ),
            start,
            builder.length,
        )
    }
    for (range in findRanges) {
        if (range.end <= range.start) continue
        builder.addStyle(
            SpanStyle(color = findTextColor),
            range.start,
            range.end.coerceAtMost(builder.length),
        )
    }
    return builder.toAnnotatedString()
}

/** SGR 2 (`dim`) — the oracle's `opacity: .7`. */
private const val DIM_ALPHA = 0.7f

@PreviewLightDark
@Composable
private fun TerminalSurfacePreview() {
    LerdrTheme {
        Surface(color = LerdrTheme.extendedColors.terminalSurface) {
            val rows = remember {
                parseTerminalRows(
                    listOf(
                        "lerdr git:(main) \u001b[32mcargo test\u001b[0m -p lerdr-e2ee",
                        "running 14 tests  test handshake_credential … \u001b[32mok\u001b[0m",
                        "\u001b[1mtest result: ok.\u001b[0m 14 passed; 0 failed",
                        "$ ",
                    ),
                    format = TERMINAL_FORMAT_ANSI,
                )
            }
            TerminalSurface(
                rows = rows,
                cursor = TerminalCursorUi(row = 3, column = 2),
                revision = 1,
                modifier = Modifier.fillMaxSize(),
                contentPadding = PaddingValues(12.dp),
            )
        }
    }
}
