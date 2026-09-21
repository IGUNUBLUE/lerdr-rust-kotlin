package com.lerdr.app.ui.terminal

import androidx.compose.foundation.Canvas
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
import androidx.compose.runtime.getValue
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
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

    // Blink rides a 530 ms toggle — a state read inside the draw pass, so
    // it repaints the cursor twice a second instead of animating per frame.
    val cursorOn by produceState(initialValue = true) {
        while (true) {
            delay(CURSOR_BLINK_MS)
            value = !value
        }
    }

    val verticalScroll = rememberScrollState()
    val horizontalScroll = rememberScrollState()

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

        // Follow-live: each commit keeps the write edge in view (the
        // pause-follow pill of docs/04 lands with scroll-back affordances).
        LaunchedEffect(revision) {
            verticalScroll.scrollTo(verticalScroll.maxValue)
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
                    drawText(
                        textLayoutResult = rowLayouts[index],
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

/** Monospace probe — ten digits average out per-glyph hinting error. */
private const val CELL_PROBE = "0123456789"

/** Cursor block opacity — translucent so the glyph under it stays legible. */
private const val CURSOR_ALPHA = 0.35f
private const val CURSOR_BLINK_MS = 530L

private class CellMetrics(val cellWidth: Float, val rowHeight: Float)

/** Row → AnnotatedString: SGR fields → [SpanStyle], links underlined. */
private fun TerminalRowUi.toAnnotatedString(defaultColor: Color): AnnotatedString {
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
