package lerdr.core.terminal

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.doubleOrNull
import lerdr.core.model.Segment

/**
 * Pane delta codec — `internal/panedelta/delta.go` semantics.
 *
 * [apply] is the Go `Apply` port (relay-side verifier semantics);
 * [applyStrict] is the normative client apply — the boundary-table model
 * the released JS client implements (docs/specs/pane-delta.md §6).
 */
object PaneDelta {

    const val MINIMUM_COPY_LINES = 3
    const val MAX_CANDIDATES = 64
    const val SEGMENT_OVERHEAD_BYTES = 64

    /**
     * Go `strings.SplitAfter(s, "\n")`: each element keeps its trailing
     * `\n`, and a final `\n` leaves a trailing `""` element. Kotlin's
     * `split` drops trailing empties, so this is a manual scan.
     */
    fun splitAfterNewline(text: String): List<String> {
        val lines = ArrayList<String>()
        var start = 0
        while (true) {
            val newline = text.indexOf('\n', start)
            if (newline < 0) break
            lines.add(text.substring(start, newline + 1))
            start = newline + 1
        }
        lines.add(text.substring(start))
        return lines
    }

    /**
     * Go `Apply(previous, segments)`: `copy_lines > 0` selects a line range
     * out of `splitAfterNewline(previous)`; anything else appends `text`.
     * Returns null on a bounds violation (caller forces a `read_pane`).
     */
    fun apply(previous: String, segments: List<Segment>): String? {
        val lines = splitAfterNewline(previous)
        val output = StringBuilder()
        for (segment in segments) {
            if (segment.copyLines > 0) {
                val end = segment.copyStart + segment.copyLines
                if (segment.copyStart < 0 || end < segment.copyStart || end > lines.size) return null
                for (index in segment.copyStart until end) output.append(lines[index])
            } else {
                output.append(segment.text)
            }
        }
        return output.toString()
    }

    /**
     * The deployed client's apply (store.ts `applyPaneDelta`): indexes
     * `previous` by a newline boundary table, so `copy_lines` may legally
     * reach `count("\n")+1` — the shape the relay emits for metadata-only
     * frames. Rejects malformed segments where Go's `Apply` would treat
     * them as literals: a present `copy_lines` that is not an integer >0,
     * a negative `copy_start`, a non-string `text`, or a `null`/non-array
     * segment list.
     */
    fun applyStrict(previous: String, segments: JsonElement?): String? {
        if (segments !is JsonArray) return null
        val boundaries = ArrayList<Int>()
        boundaries.add(0)
        var index = previous.indexOf('\n')
        while (index >= 0) {
            boundaries.add(index + 1)
            index = previous.indexOf('\n', index + 1)
        }
        boundaries.add(previous.length)

        val output = StringBuilder()
        for (candidate in segments) {
            if (candidate !is JsonObject) return null
            if ("copy_lines" in candidate) {
                val copyLines = candidate.getValue("copy_lines").jsIntegerOrNull()
                    ?.takeIf { it > 0 } ?: return null
                val copyStart = candidate["copy_start"]
                    ?.let { it.jsIntegerOrNull() ?: return null }
                    ?: 0L
                if (copyStart < 0) return null
                val copyEnd = copyStart + copyLines
                if (copyEnd > boundaries.size - 1L) return null
                output.append(
                    previous.substring(boundaries[copyStart.toInt()], boundaries[copyEnd.toInt()]),
                )
            } else {
                val text = candidate["text"] as? JsonPrimitive
                if (text == null || !text.isString) return null
                output.append(text.content)
            }
        }
        return output.toString()
    }

    /**
     * Go `Build(previous, current)` — sender-side reference port, kept for
     * fixture verification (the client never builds deltas).
     */
    fun build(previous: String, current: String): List<Segment> {
        val previousLines = splitAfterNewline(previous)
        val currentLines = splitAfterNewline(current)
        val matches = HashMap<List<String>, MutableList<Int>>(previousLines.size)
        var index = 0
        while (index + MINIMUM_COPY_LINES <= previousLines.size) {
            val key = previousLines.subList(index, index + MINIMUM_COPY_LINES).toList()
            val bucket = matches.getOrPut(key) { ArrayList() }
            if (bucket.size < MAX_CANDIDATES) bucket.add(index)
            index++
        }

        val segments = ArrayList<Segment>(8)
        var literalStart = 0

        fun flushLiteral(end: Int) {
            if (end <= literalStart) return
            segments.add(
                Segment(text = currentLines.subList(literalStart, end).joinToString("")),
            )
        }

        var currentIndex = 0
        while (currentIndex < currentLines.size) {
            if (currentIndex + MINIMUM_COPY_LINES > currentLines.size) break
            val key = currentLines.subList(currentIndex, currentIndex + MINIMUM_COPY_LINES).toList()
            var bestStart = 0
            var bestLines = 0
            for (previousIndex in matches[key].orEmpty()) {
                val matched = matchingLines(previousLines, currentLines, previousIndex, currentIndex)
                if (matched > bestLines) {
                    bestStart = previousIndex
                    bestLines = matched
                }
            }
            if (bestLines < MINIMUM_COPY_LINES) {
                currentIndex++
                continue
            }
            flushLiteral(currentIndex)
            segments.add(Segment(copyStart = bestStart, copyLines = bestLines))
            currentIndex += bestLines
            literalStart = currentIndex
        }
        flushLiteral(currentLines.size)
        return segments
    }

    /** Go `Efficient` — all lengths are UTF-8 bytes. */
    fun efficient(segments: List<Segment>, current: String): Boolean {
        var literalBytes = 0
        for (segment in segments) literalBytes += segment.text.encodeToByteArray().size
        val budget = current.encodeToByteArray().size * 3 / 4
        return literalBytes + segments.size * SEGMENT_OVERHEAD_BYTES < budget
    }

    private fun matchingLines(
        previous: List<String>,
        current: List<String>,
        previousIndex: Int,
        currentIndex: Int,
    ): Int {
        var matched = 0
        while (previousIndex + matched < previous.size &&
            currentIndex + matched < current.size &&
            previous[previousIndex + matched] == current[currentIndex + matched]
        ) {
            matched++
        }
        return matched
    }

    /** `Number.isInteger` equivalent: integral numeric primitives only. */
    private fun JsonElement.jsIntegerOrNull(): Long? {
        if (this !is JsonPrimitive || isString) return null
        val value = doubleOrNull ?: return null
        if (!value.isFinite() || value % 1.0 != 0.0) return null
        if (value < Long.MIN_VALUE.toDouble() || value > Long.MAX_VALUE.toDouble()) return null
        return value.toLong()
    }
}
