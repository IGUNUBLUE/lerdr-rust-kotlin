package lerdr.core.terminal

import java.text.BreakIterator

/**
 * `terminalCellsHtml` ported to a leaf emitter — the fixed-grid rendering of
 * one text part under `preserveTerminalCells`: plain ASCII runs fold into
 * `terminal-cell-run` spans carrying `width_cells`, wide/mark graphemes and
 * box drawing become individual `terminal-cell` spans, `─ ━ ═` runs become
 * `terminal-cell-horizontal` spans.
 */
internal object TerminalCells {

    internal data class Leaf(
        val text: String,
        val className: String? = null,
        val widthCells: Int? = null,
        val href: String? = null,
        val styles: Map<String, String>? = null,
    )

    fun cells(text: String, startingColumn: Int): Pair<List<Leaf>, Int> {
        var column = startingColumn
        val leaves = ArrayList<Leaf>()
        val plain = StringBuilder()
        var plainCells = 0
        val horizontal = StringBuilder()
        var horizontalCells = 0

        fun flushPlain() {
            if (plain.isEmpty()) return
            val cells = plainCells
            for (chunk in TerminalLinkify.linkify(plain.toString())) {
                when (chunk) {
                    is TerminalLinkify.Chunk.Plain ->
                        leaves.add(Leaf(chunk.text, "terminal-cell-run", cells))
                    is TerminalLinkify.Chunk.Link ->
                        leaves.add(Leaf(chunk.text, "terminal-cell-run terminal-link", cells, chunk.href))
                }
            }
            plain.clear()
            plainCells = 0
        }

        fun flushHorizontal() {
            if (horizontal.isEmpty()) return
            val kind = when (horizontal[0]) {
                '━' -> "heavy"
                '═' -> "double"
                else -> "single"
            }
            leaves.add(
                Leaf(
                    horizontal.toString(),
                    "terminal-cell-horizontal terminal-cell-horizontal-$kind",
                    horizontalCells,
                ),
            )
            horizontal.clear()
            horizontalCells = 0
        }

        for (segment in graphemes(text)) {
            if (segment == "\t") {
                flushHorizontal()
                val spaces = 8 - (column % 8)
                plain.append(" ".repeat(spaces))
                plainCells += spaces
                column += spaces
                continue
            }
            val width = graphemeWidth(segment)
            val box = BOX_RENDERINGS[segment]
            if (box != null) {
                flushHorizontal()
                flushPlain()
                leaves.add(
                    Leaf(
                        segment,
                        "terminal-cell ${box.className}",
                        styles = box.style?.let { mapOf("background" to it) },
                    ),
                )
                column += width
                continue
            }
            if (segment in HORIZONTAL_CELLS) {
                flushPlain()
                if (horizontal.isNotEmpty() && horizontal[0] != segment[0]) flushHorizontal()
                horizontal.append(segment)
                horizontalCells += width
                column += width
                continue
            }
            flushHorizontal()
            if (segment.length == 1 && segment[0] in '\u0020'..'\u007E') {
                plain.append(segment)
                plainCells += width
            } else {
                flushPlain()
                val wide = if (width == 2) " terminal-cell-wide" else ""
                leaves.add(Leaf(segment, "terminal-cell$wide"))
            }
            column += width
        }
        flushHorizontal()
        flushPlain()
        return leaves to column
    }

    private val HORIZONTAL_CELLS = setOf("─", "━", "═")

    /**
     * Extended grapheme clusters. `BreakIterator.getCharacterInstance`
     * covers marks/modifiers/variation selectors but not ZWJ chains or
     * regional-indicator pairs, which `Intl.Segmenter` joins — merge those.
     */
    internal fun graphemes(text: String): List<String> {
        if (text.isEmpty()) return emptyList()
        val iterator = BreakIterator.getCharacterInstance()
        iterator.setText(text)
        val clusters = ArrayList<String>()
        var start = iterator.first()
        var end = iterator.next()
        while (end != BreakIterator.DONE) {
            clusters.add(text.substring(start, end))
            start = end
            end = iterator.next()
        }
        // Join grapheme + ZWJ + grapheme chains (GB11).
        val zwjMerged = ArrayList<String>(clusters.size)
        var index = 0
        while (index < clusters.size) {
            var cluster = clusters[index]
            while (index + 2 < clusters.size && clusters[index + 1] == "\u200D") {
                cluster += clusters[index + 1] + clusters[index + 2]
                index += 2
            }
            zwjMerged.add(cluster)
            index++
        }
        // Join regional-indicator pairs (flags, GB12/GB13).
        val result = ArrayList<String>(zwjMerged.size)
        index = 0
        while (index < zwjMerged.size) {
            if (index + 1 < zwjMerged.size &&
                isRegionalIndicator(zwjMerged[index]) &&
                isRegionalIndicator(zwjMerged[index + 1])
            ) {
                result.add(zwjMerged[index] + zwjMerged[index + 1])
                index += 2
            } else {
                result.add(zwjMerged[index])
                index++
            }
        }
        return result
    }

    private fun isRegionalIndicator(cluster: String): Boolean {
        if (cluster.codePointCount(0, cluster.length) != 1) return false
        val codePoint = cluster.codePointAt(0)
        return codePoint in 0x1F1E6..0x1F1FF
    }

    /** `terminalGraphemeWidth` — marks 0, emoji/VS16/wide 2, else 1. */
    fun graphemeWidth(grapheme: String): Int {
        if (grapheme.isEmpty()) return 0
        if (grapheme.codePoints().allMatch { isMark(it) }) return 0
        if ('\uFE0F' in grapheme || grapheme.codePoints().anyMatch { isEmojiPresentation(it) }) return 2
        return if (isWideTerminalCodePoint(grapheme.codePointAt(0))) 2 else 1
    }

    private fun isMark(codePoint: Int): Boolean = when (Character.getType(codePoint)) {
        Character.NON_SPACING_MARK.toInt(),
        Character.COMBINING_SPACING_MARK.toInt(),
        Character.ENCLOSING_MARK.toInt(),
        -> true
        else -> false
    }

    /** `isWideTerminalCodePoint` — the JS range list verbatim. */
    fun isWideTerminalCodePoint(codePoint: Int): Boolean =
        codePoint >= 0x1100 && (
            codePoint <= 0x115f ||
                codePoint == 0x2329 ||
                codePoint == 0x232a ||
                (codePoint in 0x2e80..0xa4cf && codePoint != 0x303f) ||
                codePoint in 0xac00..0xd7a3 ||
                codePoint in 0xf900..0xfaff ||
                codePoint in 0xfe10..0xfe19 ||
                codePoint in 0xfe30..0xfe6f ||
                codePoint in 0xff00..0xff60 ||
                codePoint in 0xffe0..0xffe6 ||
                codePoint in 0x20000..0x3fffd
            )

    /**
     * `\p{Emoji_Presentation}` — JDK 17 has no emoji binary properties, so
     * this is the emoji-data.txt range table (Unicode 15.x), packed as
     * sorted `[start, end]` pairs.
     */
    fun isEmojiPresentation(codePoint: Int): Boolean {
        var index = 0
        while (index < EMOJI_PRESENTATION_RANGES.size) {
            if (codePoint < EMOJI_PRESENTATION_RANGES[index]) return false
            if (codePoint <= EMOJI_PRESENTATION_RANGES[index + 1]) return true
            index += 2
        }
        return false
    }

    private val EMOJI_PRESENTATION_RANGES = intArrayOf(
        0x231A, 0x231B, 0x23E9, 0x23EC, 0x23F0, 0x23F0, 0x23F3, 0x23F3,
        0x25FD, 0x25FE, 0x2614, 0x2615, 0x2648, 0x2653, 0x267F, 0x267F,
        0x2693, 0x2693, 0x26A1, 0x26A1, 0x26AA, 0x26AB, 0x26BD, 0x26BE,
        0x26C4, 0x26C5, 0x26CE, 0x26CE, 0x26D4, 0x26D4, 0x26EA, 0x26EA,
        0x26F2, 0x26F3, 0x26F5, 0x26F5, 0x26FA, 0x26FA, 0x26FD, 0x26FD,
        0x2705, 0x2705, 0x270A, 0x270B, 0x2728, 0x2728, 0x274C, 0x274C,
        0x274E, 0x274E, 0x2753, 0x2755, 0x2757, 0x2757, 0x2795, 0x2797,
        0x27B0, 0x27B0, 0x27BF, 0x27BF, 0x2B1B, 0x2B1C, 0x2B50, 0x2B50,
        0x2B55, 0x2B55, 0x1F004, 0x1F004, 0x1F0CF, 0x1F0CF, 0x1F18E, 0x1F18E,
        0x1F191, 0x1F19A, 0x1F1E6, 0x1F1FF, 0x1F201, 0x1F201, 0x1F21A, 0x1F21A,
        0x1F22F, 0x1F22F, 0x1F232, 0x1F236, 0x1F238, 0x1F23A, 0x1F23C, 0x1F23F,
        0x1F249, 0x1F3FA, 0x1F400, 0x1F53D, 0x1F540, 0x1F643, 0x1F650, 0x1F67F,
        0x1F6C5, 0x1F6CB, 0x1F6CD, 0x1F6CF, 0x1F6E0, 0x1F6EA, 0x1F6F0, 0x1F6FC,
        0x1F7E0, 0x1F7EB, 0x1F7F0, 0x1F7F0, 0x1F90C, 0x1F93A, 0x1F93C, 0x1F945,
        0x1F947, 0x1F9FF, 0x1FA70, 0x1FA7C, 0x1FA80, 0x1FA88, 0x1FA90, 0x1FABD,
        0x1FABF, 0x1FAC5, 0x1FACE, 0x1FADB, 0x1FAE0, 0x1FAE8, 0x1FAF0, 0x1FAF8,
    )

    // ── Box drawing ─────────────────────────────────────────────────

    private enum class Stroke { NONE, LIGHT, HEAVY, DOUBLE }
    private enum class Arc { DOWN_RIGHT, DOWN_LEFT, UP_RIGHT, UP_LEFT }
    private enum class Direction { UP, RIGHT, DOWN, LEFT, VERTICAL, HORIZONTAL }

    private class BoxRendering(val className: String, val style: String?)

    private fun boxArmLayers(direction: Direction, stroke: Stroke): List<String> {
        if (stroke == Stroke.NONE) return emptyList()
        val vertical = direction == Direction.UP || direction == Direction.DOWN ||
            direction == Direction.VERTICAL
        val wholeAxis = direction == Direction.VERTICAL || direction == Direction.HORIZONTAL
        val edge = when {
            wholeAxis -> "50%"
            direction == Direction.UP -> "top"
            direction == Direction.DOWN -> "bottom"
            direction == Direction.RIGHT -> "right"
            else -> "left"
        }
        val length = if (wholeAxis) "100%" else "50%"
        val extent = if (vertical) ".75px $length" else "$length 1px"
        fun layer(position: String, size: String = extent) =
            "linear-gradient(currentColor,currentColor) $position/$size no-repeat"
        if (stroke == Stroke.DOUBLE) {
            return if (vertical) {
                listOf(layer("calc(50% - .15em) $edge"), layer("calc(50% + .15em) $edge"))
            } else {
                listOf(layer("$edge calc(50% - .15em)"), layer("$edge calc(50% + .15em)"))
            }
        }
        val position = if (vertical) "50% $edge" else "$edge 50%"
        val size = if (stroke == Stroke.HEAVY) {
            if (vertical) "2px $length" else "$length 2px"
        } else {
            extent
        }
        return listOf(layer(position, size))
    }

    /** `[up, right, down, left, arc?]` per TERMINAL_BOX_CELLS. */
    private class BoxCell(
        val up: Stroke,
        val right: Stroke,
        val down: Stroke,
        val left: Stroke,
        val arc: Arc? = null,
    )

    private val BOX_CELLS: Map<String, BoxCell> = buildMap {
        fun n(s: String) = when (s) {
            "light" -> Stroke.LIGHT
            "heavy" -> Stroke.HEAVY
            "double" -> Stroke.DOUBLE
            else -> Stroke.NONE
        }

        fun cell(c: String, up: String, right: String, down: String, left: String, arc: Arc? = null) {
            put(c, BoxCell(n(up), n(right), n(down), n(left), arc))
        }
        cell("│", "light", "", "light", "")
        cell("┃", "heavy", "", "heavy", "")
        cell("┌", "", "light", "light", "")
        cell("┐", "", "", "light", "light")
        cell("└", "light", "light", "", "")
        cell("┘", "light", "", "", "light")
        cell("├", "light", "light", "light", "")
        cell("┤", "light", "", "light", "light")
        cell("┬", "", "light", "light", "light")
        cell("┴", "light", "light", "", "light")
        cell("┼", "light", "light", "light", "light")
        cell("┏", "", "heavy", "heavy", "")
        cell("┓", "", "", "heavy", "heavy")
        cell("┗", "heavy", "heavy", "", "")
        cell("┛", "heavy", "", "", "heavy")
        cell("┣", "heavy", "heavy", "heavy", "")
        cell("┫", "heavy", "", "heavy", "heavy")
        cell("┳", "", "heavy", "heavy", "heavy")
        cell("┻", "heavy", "heavy", "", "heavy")
        cell("╋", "heavy", "heavy", "heavy", "heavy")
        cell("╒", "", "double", "light", "")
        cell("╓", "", "light", "double", "")
        cell("╔", "", "double", "double", "")
        cell("╕", "", "", "light", "double")
        cell("╖", "", "", "double", "light")
        cell("╗", "", "", "double", "double")
        cell("╘", "light", "double", "", "")
        cell("╙", "double", "light", "", "")
        cell("╚", "double", "double", "", "")
        cell("╛", "light", "", "", "double")
        cell("╜", "double", "", "", "light")
        cell("╝", "double", "", "", "double")
        cell("╞", "light", "double", "light", "")
        cell("╟", "double", "light", "double", "")
        cell("╠", "double", "double", "double", "")
        cell("╡", "light", "", "light", "double")
        cell("╢", "double", "", "double", "light")
        cell("╣", "double", "", "double", "double")
        cell("╤", "", "double", "light", "double")
        cell("╥", "", "light", "double", "light")
        cell("╦", "", "double", "double", "double")
        cell("╧", "light", "double", "", "double")
        cell("╨", "double", "light", "", "light")
        cell("╩", "double", "double", "", "double")
        cell("╪", "light", "double", "light", "double")
        cell("╫", "double", "light", "double", "light")
        cell("╬", "double", "double", "double", "double")
        cell("╭", "", "light", "light", "", Arc.DOWN_RIGHT)
        cell("╮", "", "", "light", "light", Arc.DOWN_LEFT)
        cell("╯", "light", "", "", "light", Arc.UP_LEFT)
        cell("╰", "light", "light", "", "", Arc.UP_RIGHT)
        cell("╴", "", "", "", "light")
        cell("╵", "light", "", "", "")
        cell("╶", "", "light", "", "")
        cell("╷", "", "", "light", "")
        cell("╸", "", "", "", "heavy")
        cell("╹", "heavy", "", "", "")
        cell("╺", "", "heavy", "", "")
        cell("╻", "", "", "heavy", "")
        cell("╼", "", "heavy", "", "light")
        cell("╽", "light", "", "heavy", "")
        cell("╾", "", "light", "", "heavy")
        cell("╿", "heavy", "", "light", "")
        cell("║", "double", "", "double", "")
    }

    private val BOX_RENDERINGS: Map<String, BoxRendering> = BOX_CELLS.mapValues { (_, cell) ->
        if (cell.arc != null) {
            val arc = when (cell.arc) {
                Arc.DOWN_RIGHT -> "down-right"
                Arc.DOWN_LEFT -> "down-left"
                Arc.UP_RIGHT -> "up-right"
                Arc.UP_LEFT -> "up-left"
            }
            return@mapValues BoxRendering(
                "terminal-cell-box terminal-cell-arc terminal-cell-arc-$arc",
                null,
            )
        }
        val verticalLayers = if (cell.up != Stroke.NONE && cell.up == cell.down) {
            boxArmLayers(Direction.VERTICAL, cell.up)
        } else {
            boxArmLayers(Direction.UP, cell.up) + boxArmLayers(Direction.DOWN, cell.down)
        }
        val horizontalLayers = if (cell.right != Stroke.NONE && cell.right == cell.left) {
            boxArmLayers(Direction.HORIZONTAL, cell.right)
        } else {
            boxArmLayers(Direction.RIGHT, cell.right) + boxArmLayers(Direction.LEFT, cell.left)
        }
        BoxRendering("terminal-cell-box", (verticalLayers + horizontalLayers).joinToString(","))
    }
}
