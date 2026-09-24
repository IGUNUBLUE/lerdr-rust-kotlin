package com.lerdr.app.ui.terminal

import android.content.Intent
import android.os.Build
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.ScrollState
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.calculateZoom
import androidx.compose.foundation.gestures.detectDragGesturesAfterLongPress
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.requiredSize
import androidx.compose.foundation.magnifier
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.positionChanged
import androidx.compose.ui.platform.ClipEntry
import androidx.compose.ui.platform.LocalClipboard
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalLayoutDirection
import androidx.compose.ui.platform.LocalViewConfiguration
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLayoutResult
import androidx.compose.ui.text.drawText
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.PreviewLightDark
import androidx.compose.ui.unit.DpOffset
import androidx.compose.ui.unit.dp
import com.lerdr.app.session.SessionRepository
import com.lerdr.core.designsystem.theme.LerdrTextStyles
import com.lerdr.core.designsystem.theme.LerdrTheme
import kotlin.math.ceil
import kotlin.math.floor
import kotlin.math.max
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

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
    onTapSurface: () -> Unit = {},
    /**
     * Relay `pane_links` negotiated — enables the server hit-test item in
     * the long-press menu for links the served text cannot expose (OSC8).
     */
    paneLinksSupported: Boolean = false,
    /** `pane_link_resolve` — true when a viewport cell has link regions. */
    onResolveLink: suspend (row: Int, col: Int) -> Boolean = { _, _ -> false },
    /** `pane_link_activate` — the caller owns the result policy. */
    onActivateLink: suspend (row: Int, col: Int) -> Unit = { _, _ -> },
) {
    val density = LocalDensity.current
    val layoutDirection = LocalLayoutDirection.current
    val textMeasurer = rememberTextMeasurer()
    val clipboard = LocalClipboard.current
    val context = LocalContext.current
    val viewConfiguration = LocalViewConfiguration.current
    val menuScope = rememberCoroutineScope()
    val baseStyle: TextStyle = LerdrTextStyles.terminal.let { style ->
        // Pinch zoom rescales the font — the metrics re-probe below turns
        // it into a new grid, which re-leases the pane size.
        if (state.fontScale != 1f) style.copy(fontSize = style.fontSize * state.fontScale) else style
    }

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

    // Copy-mode freeze: while a selection is marked the grid renders the
    // rows it was marked against — live commits would shift the index
    // space under the highlight (truncated scrollback drops leading
    // lines). On clear, [renderRows] snaps back to the latest frame.
    val renderRows = state.frozenRows ?: rows
    // Find marks map to live row indices — they would paint the wrong
    // cells over a frozen buffer, so they are suppressed until unfreeze.
    val renderFindRanges = if (state.frozenRows == null) findRanges else emptyMap()

    // Row layouts — built once per committed frame, not per draw.
    val rowLayouts = remember(renderRows, textMeasurer, baseStyle, textColor) {
        val style = baseStyle.copy(color = textColor)
        renderRows.map { row ->
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
        renderRows, renderFindRanges, textMeasurer, baseStyle, textColor, findTextColor,
    ) {
        if (renderFindRanges.isEmpty()) {
            emptyMap()
        } else {
            renderFindRanges.mapNotNull { (index, ranges) ->
                val row = renderRows.getOrNull(index) ?: return@mapNotNull null
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

        var contextMenu by remember { mutableStateOf<TerminalMenuTarget?>(null) }

        // Gesture handlers must read the rows committed NOW — keying
        // pointerInput on the list would restart (and cancel) the
        // gesture mid-drag on every pane delta. Under a freeze this is
        // the frozen epoch, so selection math stays aligned with the
        // drawn grid.
        val currentRows by rememberUpdatedState(renderRows)

        val handleStemPx = with(density) { HANDLE_STEM_DP.dp.toPx() }
        val handleRadiusPx = with(density) { HANDLE_RADIUS_DP.dp.toPx() }
        val handleHitRadiusPx = with(density) { HANDLE_HIT_DP.dp.toPx() }
        val magnifierLiftPx = metrics.rowHeight * MAGNIFIER_LIFT_ROWS

        // Two coordinate spaces meet here: the drag detector sits inside
        // the scrolled content (its positions are already content-relative),
        // while openMenu is called with viewport offsets — it must add the
        // scroll deltas back to hit the same cells.
        fun contentCellAt(content: Offset): TerminalCell = TerminalCell(
            row = floor(content.y / metrics.rowHeight)
                .toInt()
                .coerceIn(0, (currentRows.size - 1).coerceAtLeast(0)),
            col = floor(content.x / metrics.cellWidth).toInt().coerceAtLeast(0),
        )

        fun cellAt(viewport: Offset): TerminalCell = contentCellAt(
            Offset(
                viewport.x + horizontalScroll.value,
                viewport.y + verticalScroll.value,
            ),
        )

        fun openMenu(point: Offset, selection: String?) {
            val cell = cellAt(point)
            val row = currentRows.getOrNull(cell.row)
            val links = row?.spans
                ?.mapNotNull { it.href }
                ?.distinct()
                .orEmpty()
            contextMenu = TerminalMenuTarget(
                offset = Offset(
                    point.x.coerceIn(0f, viewportWidth),
                    point.y.coerceIn(0f, viewportHeight),
                ),
                rowText = row?.plainText().orEmpty(),
                links = links,
                row = cell.row,
                col = cell.col,
                selection = selection,
            )
        }

        Box(
            modifier = Modifier
                .fillMaxSize()
                .padding(contentPadding)
                // Pinch zoom — consumes pointers only while two fingers are
                // down, so single-finger drags stay with the scrollers.
                .pointerInput(metrics) {
                    awaitEachGesture {
                        awaitFirstDown(requireUnconsumed = false)
                        while (true) {
                            val event = awaitPointerEvent()
                            if (event.changes.none { it.pressed }) break
                            if (event.changes.count { it.pressed } < 2) continue
                            val zoom = event.calculateZoom()
                            if (zoom != 1f) state.zoomBy(zoom)
                            event.changes.forEach { if (it.positionChanged()) it.consume() }
                        }
                    }
                }
                .pointerInput(Unit) {
                    detectTapGestures(
                        onTap = { point ->
                            // A tap inside a committed selection reopens
                            // its copy/share menu; a tap outside dismisses
                            // — either way it must not summon the keyboard.
                            val anchor = state.selectionAnchor
                            val cursor = state.selectionCursor
                            val insideText = if (anchor != null && cursor != null) {
                                val cell = cellAt(point)
                                selectionCellRanges(currentRows, anchor, cursor)[cell.row]
                                    ?.takeIf { cell.col in it }
                                    ?.let { selectedText(currentRows, anchor, cursor) }
                                    ?.takeIf { it.isNotBlank() }
                            } else {
                                null
                            }
                            when {
                                insideText != null -> openMenu(point, insideText)
                                state.hasSelection -> state.clearSelection()
                                else -> onTapSurface()
                            }
                        },
                    )
                }
                .verticalScroll(verticalScroll)
                .horizontalScroll(horizontalScroll)
                // Long-press-drag selects a cell range; a held press that
                // never moves past the touch slop opens the context menu
                // (the gesture the oracle bound to long-press). The
                // detector sits AFTER the scrollers — innermost on the
                // Main pass — so once the long-press wins, its moves are
                // consumed before the scroll drag can claim them.
                .pointerInput(metrics) {
                    detectDragGesturesAfterLongPress(
                        onDragStart = { point ->
                            // Inside the scroll container — content space.
                            // Freeze the rendered epoch: pane commits keep
                            // landing while the user drags, and the row
                            // indices they mark must keep pointing at the
                            // same text.
                            state.frozenRows = currentRows
                            state.selectionAnchor = contentCellAt(point)
                            state.selectionCursor = state.selectionAnchor
                            state.magnifierPoint = null
                        },
                        onDrag = { change, dragAmount ->
                            state.selectionCursor = contentCellAt(change.position)
                            state.magnifierPoint = change.position
                            state.selectionDistance += dragAmount.getDistance()
                            // Selecting means staring at fixed content —
                            // release follow-live once the drag is real.
                            if (state.selectionDistance > viewConfiguration.touchSlop) {
                                state.stickToBottom = false
                            }
                        },
                        onDragEnd = {
                            state.magnifierPoint = null
                            val anchor = state.selectionAnchor
                            val cursor = state.selectionCursor
                            val text = if (anchor != null && cursor != null &&
                                state.selectionDistance > viewConfiguration.touchSlop
                            ) {
                                selectedText(currentRows, anchor, cursor)
                                    .takeIf { it.isNotBlank() }
                            } else {
                                null
                            }
                            if (cursor != null && text != null) {
                                // A real range — commit, keep the
                                // highlight, and offer copy/share at the
                                // release point (the floating toolbar).
                                state.selectionCommitted = true
                                openMenu(
                                    cursor.toOffset(
                                        metrics, verticalScroll, horizontalScroll,
                                    ),
                                    text,
                                )
                            } else {
                                // Held press with no travel — the menu.
                                state.clearSelection()
                                anchor?.toOffset(
                                    metrics, verticalScroll, horizontalScroll,
                                )?.let { openMenu(it, selection = null) }
                            }
                        },
                        onDragCancel = { state.clearSelection() },
                    )
                }
                // Adjustment handles — a down on a committed teardrop is
                // consumed here, innermost, so the scroll and long-press
                // detectors never observe it. Dragging moves that end of
                // the range; the magnifier rides along.
                .pointerInput(metrics) {
                    awaitEachGesture {
                        val down = awaitFirstDown(requireUnconsumed = false)
                        if (!state.selectionCommitted) return@awaitEachGesture
                        val handles = state.selectionHandles ?: return@awaitEachGesture
                        val hitDy = Offset(0f, handleStemPx + handleRadiusPx)
                        val nearStart = (down.position - handles.first - hitDy)
                            .getDistance() <= handleHitRadiusPx
                        val nearEnd = !nearStart &&
                            (down.position - handles.second - hitDy)
                                .getDistance() <= handleHitRadiusPx
                        if (!nearStart && !nearEnd) return@awaitEachGesture
                        down.consume()
                        state.stickToBottom = false
                        // Repoint so `selectionCursor` tracks the dragged end.
                        val anchor = state.selectionAnchor ?: return@awaitEachGesture
                        val cursor = state.selectionCursor ?: return@awaitEachGesture
                        val (selStart, selEnd) = normalizeSelection(anchor, cursor)
                        state.selectionAnchor = if (nearStart) selEnd else selStart
                        state.selectionCursor = contentCellAt(down.position)
                        state.magnifierPoint = down.position
                        while (true) {
                            val event = awaitPointerEvent()
                            if (event.changes.none { it.pressed }) break
                            val change = event.changes.firstOrNull { it.id == down.id }
                                ?: continue
                            state.selectionCursor = contentCellAt(change.position)
                            state.magnifierPoint = change.position
                            change.consume()
                        }
                        state.magnifierPoint = null
                    }
                }
                // The platform lens — content coords, lifted above the
                // finger like the text-field magnifier. android.widget
                // .Magnifier has no Robolectric shadow (it NPEs inside
                // the widget) — the lens is real-device only.
                .then(
                    if (MAGNIFIER_SUPPORTED) {
                        Modifier.magnifier(
                            sourceCenter = {
                                state.magnifierPoint ?: Offset.Unspecified
                            },
                            magnifierCenter = {
                                state.magnifierPoint
                                    ?.minus(Offset(0f, magnifierLiftPx))
                                    ?: Offset.Unspecified
                            },
                        )
                    } else {
                        Modifier
                    },
                ),
        ) {
            // Selection overlay — live while dragging, held once committed.
            // Reads the state's cells so a drag repaints without recomposing
            // the committed row layouts.
            val anchor = state.selectionAnchor
            val selectionCursor = state.selectionCursor
            val selectionRanges = if (anchor != null && selectionCursor != null) {
                selectionCellRanges(renderRows, anchor, selectionCursor)
            } else {
                emptyMap()
            }
            // Teardrops only once committed — during the drag the finger
            // covers its own end. Stashed on state for the hit-test and
            // for tests reading handle positions.
            val handlePoints = if (state.selectionCommitted) {
                selectionHandlePoints(
                    selectionRanges, metrics.cellWidth, metrics.rowHeight,
                )
            } else {
                null
            }
            SideEffect { state.selectionHandles = handlePoints }
            TerminalGrid(
                rows = renderRows,
                rowLayouts = rowLayouts,
                findLayouts = findLayouts,
                findRanges = renderFindRanges,
                findMatchColor = findMatchColor,
                findActiveColor = findActiveColor,
                selectionRanges = selectionRanges,
                handlePoints = handlePoints,
                metrics = metrics,
                // The write cursor tracks live output — meaningless on a
                // frozen copy-mode buffer.
                cursor = if (state.frozenRows == null) cursor else null,
                cursorOn = cursorOn,
                cursorColor = cursorColor,
                viewportWidthPx = viewportWidth,
                verticalScroll = verticalScroll,
            )
        }

        val menu = contextMenu
        if (menu != null) {
            val transcript = renderRows.joinToString("\n") { it.plainText() }.trimEnd()
            DropdownMenu(
                expanded = true,
                onDismissRequest = { contextMenu = null },
                offset = with(density) {
                    DpOffset(menu.offset.x.toDp(), menu.offset.y.toDp())
                },
            ) {
                // A drag-committed selection offers its own copy/share —
                // the row items below still work on the tapped line.
                val selection = menu.selection
                if (selection != null) {
                    // Handles may have re-ranged the selection since the
                    // menu opened — copy/share read the live cells, with
                    // the menu's snapshot as fallback.
                    fun liveSelection(): String {
                        val a = state.selectionAnchor
                        val c = state.selectionCursor
                        return if (a != null && c != null) {
                            selectedText(currentRows, a, c).ifBlank { selection }
                        } else {
                            selection
                        }
                    }
                    DropdownMenuItem(
                        text = { Text("Copy selection") },
                        onClick = {
                            menuScope.launch {
                                clipboard.setClipEntry(
                                    ClipEntry(
                                        android.content.ClipData.newPlainText(
                                            "terminal selection",
                                            liveSelection(),
                                        ),
                                    ),
                                )
                            }
                            state.clearSelection()
                            contextMenu = null
                        },
                    )
                    DropdownMenuItem(
                        text = { Text("Share selection") },
                        onClick = {
                            val send = Intent(Intent.ACTION_SEND)
                                .setType("text/plain")
                                .putExtra(Intent.EXTRA_TEXT, liveSelection())
                            context.startActivity(Intent.createChooser(send, null))
                            state.clearSelection()
                            contextMenu = null
                        },
                    )
                }
                if (menu.rowText.isNotBlank()) {
                    DropdownMenuItem(
                        text = { Text("Copy line") },
                        onClick = {
                            menuScope.launch {
                                clipboard.setClipEntry(ClipEntry(android.content.ClipData.newPlainText("terminal line", menu.rowText)))
                            }
                            contextMenu = null
                        },
                    )
                }
                if (transcript.isNotBlank()) {
                    DropdownMenuItem(
                        text = { Text("Copy transcript") },
                        onClick = {
                            menuScope.launch {
                                clipboard.setClipEntry(ClipEntry(android.content.ClipData.newPlainText("terminal transcript", transcript)))
                            }
                            contextMenu = null
                        },
                    )
                    DropdownMenuItem(
                        text = { Text("Share transcript") },
                        onClick = {
                            val send = Intent(Intent.ACTION_SEND)
                                .setType("text/plain")
                                .putExtra(Intent.EXTRA_TEXT, transcript)
                            context.startActivity(Intent.createChooser(send, null))
                            contextMenu = null
                        },
                    )
                }
                menu.links.take(MAX_MENU_LINKS).forEach { href ->
                    DropdownMenuItem(
                        text = { Text("Open ${shortenMenuLabel(href)}") },
                        onClick = {
                            context.startActivity(
                                Intent(Intent.ACTION_VIEW, android.net.Uri.parse(href)),
                            )
                            contextMenu = null
                        },
                    )
                    DropdownMenuItem(
                        text = { Text("Copy link") },
                        onClick = {
                            menuScope.launch {
                                clipboard.setClipEntry(ClipEntry(android.content.ClipData.newPlainText("link", href)))
                            }
                            contextMenu = null
                        },
                    )
                }
                // OSC8 escape-sequence links carry no text the client can
                // regex — only the server's hit-test sees them. The item
                // appears once resolve reports cell regions; activate
                // opens on the pane host's browser.
                if (paneLinksSupported) {
                    val serverLink by produceState(false, menu.row, menu.col) {
                        value = onResolveLink(menu.row, menu.col)
                    }
                    if (serverLink) {
                        DropdownMenuItem(
                            text = { Text("Open link on desktop") },
                            onClick = {
                                menuScope.launch {
                                    onActivateLink(menu.row, menu.col)
                                }
                                contextMenu = null
                            },
                        )
                    }
                }
            }
        }
    }
}

/** The grid draw — extracted so the gesture/menu Box above stays readable. */
@Composable
private fun TerminalGrid(
    rows: List<TerminalRowUi>,
    rowLayouts: List<TextLayoutResult>,
    findLayouts: Map<Int, TextLayoutResult>,
    findRanges: Map<Int, List<TerminalFindRange>>,
    findMatchColor: Color,
    findActiveColor: Color,
    /** Row index → selected cell columns `[first..last]` — long-press-drag. */
    selectionRanges: Map<Int, IntRange>,
    /** Committed selection's teardrop edges (start, end) in content px. */
    handlePoints: Pair<Offset, Offset>?,
    metrics: CellMetrics,
    cursor: TerminalCursorUi?,
    cursorOn: Boolean,
    cursorColor: Color,
    viewportWidthPx: Float,
    verticalScroll: ScrollState,
) {
    val density = LocalDensity.current
    val maxCells = rows.maxOfOrNull { it.cells } ?: 0
    val viewportWidth = viewportWidthPx
    val contentWidth = max(viewportWidth, maxCells * metrics.cellWidth)
    val contentHeight = rows.size * metrics.rowHeight

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
            val selection = selectionRanges[index]
            if (selection != null) {
                // Cell-grid fill — unlike find marks, which ride glyph
                // extents, the selection covers whole columns.
                drawRect(
                    color = SELECTION_FILL_COLOR,
                    topLeft = Offset(
                        x = selection.first * metrics.cellWidth,
                        y = index * metrics.rowHeight,
                    ),
                    size = Size(
                        width = (selection.last - selection.first + 1) * metrics.cellWidth,
                        height = metrics.rowHeight,
                    ),
                )
            }
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
        if (handlePoints != null) {
            val stem = with(density) { HANDLE_STEM_DP.dp.toPx() }
            val radius = with(density) { HANDLE_RADIUS_DP.dp.toPx() }
            val drop = Offset(0f, stem + radius)
            for (edge in listOf(handlePoints.first, handlePoints.second)) {
                drawLine(
                    color = SELECTION_HANDLE_COLOR,
                    start = edge,
                    end = edge + Offset(0f, stem),
                    strokeWidth = HANDLE_STEM_WIDTH_PX,
                )
                drawCircle(
                    color = SELECTION_HANDLE_COLOR,
                    radius = radius,
                    center = edge + drop,
                )
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

    // Cell-range selection — long-press-drag marks anchor→cursor cells;
    // the accumulated drag distance decides select-vs-menu on release.
    var selectionAnchor by mutableStateOf<TerminalCell?>(null)
        internal set
    var selectionCursor by mutableStateOf<TerminalCell?>(null)
        internal set
    var selectionCommitted by mutableStateOf(false)
        internal set
    internal var selectionDistance = 0f

    /** Pointer position under the lens while a selection drag runs — null hides it. */
    internal var magnifierPoint by mutableStateOf<Offset?>(null)

    /**
     * Committed selection's teardrop edges in content px — refreshed by
     * the surface's composition, read by the handle-drag hit test.
     */
    internal var selectionHandles: Pair<Offset, Offset>? = null

    /**
     * The rows a live selection was marked against — pane commits keep
     * arriving while a selection is open, and in a truncated scrollback
     * every appended line shifts the index space under the highlight.
     * Freezing the rendered rows is the copy-mode contract: the view
     * holds still until the selection clears, then snaps to live.
     */
    internal var frozenRows by mutableStateOf<List<TerminalRowUi>?>(null)

    /** A selection is committed or in flight — a tap should dismiss it. */
    val hasSelection: Boolean
        get() = selectionAnchor != null && selectionCursor != null

    internal fun clearSelection() {
        selectionAnchor = null
        selectionCursor = null
        selectionCommitted = false
        selectionDistance = 0f
        magnifierPoint = null
        selectionHandles = null
        frozenRows = null
    }

    /** Follow-live pin — new commits keep the write edge in view while set. */
    var stickToBottom by mutableStateOf(true)
        internal set

    /**
     * Pinch-zoom factor on the terminal font — rescales the cell grid,
     * which re-measures the viewport and re-leases the pane size.
     */
    var fontScale by mutableFloatStateOf(1f)
        internal set

    internal fun zoomBy(factor: Float) {
        fontScale = (fontScale * factor).coerceIn(MIN_FONT_SCALE, MAX_FONT_SCALE)
    }

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

    /**
     * The "scroll to live" affordance — jump to the write edge and re-arm
     * the follow-live pin (the oracle's scrollToBottom button).
     */
    suspend fun scrollToLive() {
        scrollToBottom()
        stickToBottom = scrollState.maxValue - scrollState.value < stickThresholdPx
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

/** Selection fill — translucent blue, distinct from the find marks. */
private val SELECTION_FILL_COLOR = Color(0x553B82F6)

/** Teardrop handles — solid blue stem + knob under each selection edge. */
private val SELECTION_HANDLE_COLOR = Color(0xFF3B82F6)
private const val HANDLE_STEM_DP = 5
private const val HANDLE_RADIUS_DP = 6
private const val HANDLE_STEM_WIDTH_PX = 4f

/** Down-target around a handle knob — generous for a small visual. */
private const val HANDLE_HIT_DP = 24

/** Lens center floats this many rows above the dragging finger. */
private const val MAGNIFIER_LIFT_ROWS = 4f

/** Robolectric reports no usable android.widget.Magnifier — lens off in tests. */
private val MAGNIFIER_SUPPORTED = Build.FINGERPRINT != "robolectric"

/** Follow-live re-pin distance — the oracle's 48 px bottom edge. */
private const val STICK_THRESHOLD_DP = 48

/** Cursor block opacity — translucent so the glyph under it stays legible. */
private const val CURSOR_ALPHA = 0.35f
private const val CURSOR_BLINK_MS = 530L

/** Pinch-zoom bounds — enough range to matter without degenerate cells. */
private const val MIN_FONT_SCALE = 0.6f
private const val MAX_FONT_SCALE = 2.5f

private const val MAX_MENU_LINKS = 3
private const val MENU_LABEL_MAX = 44

private class CellMetrics(val cellWidth: Float, val rowHeight: Float)

/**
 * Long-press menu anchor — viewport offset, the row under it, its
 * client-visible links, and the cell the server hit-tests (`row`/`col`
 * are the last served frame's viewport coordinates — the `rowIndex`/`col`
 * the gestures compute).
 */
private class TerminalMenuTarget(
    val offset: Offset,
    val rowText: String,
    val links: List<String>,
    val row: Int,
    val col: Int,
    /** Text of a committed cell-range selection — null on plain long-press. */
    val selection: String? = null,
)

/** Cell → on-screen offset — the context menu anchors where the drag ended. */
private fun TerminalCell.toOffset(
    metrics: CellMetrics,
    verticalScroll: ScrollState,
    horizontalScroll: ScrollState,
): Offset = Offset(
    col * metrics.cellWidth - horizontalScroll.value,
    row * metrics.rowHeight - verticalScroll.value,
)

/** A row's printable text — trailing whitespace is draw padding, not content. */
private fun TerminalRowUi.plainText(): String =
    spans.joinToString("") { it.text }.trimEnd()

/** Long URLs crowd a menu — keep scheme + host + the path head. */
private fun shortenMenuLabel(href: String): String =
    if (href.length <= MENU_LABEL_MAX) href else href.take(MENU_LABEL_MAX - 1) + "…"

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
