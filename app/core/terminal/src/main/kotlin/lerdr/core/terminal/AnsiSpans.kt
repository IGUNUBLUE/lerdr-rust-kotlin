package lerdr.core.terminal

/**
 * `ansiToHtml` ported to a leaf-span emitter. Only SGR (`ESC [ params m`)
 * sequences are parsed — every other CSI/OSC byte sequence stays in the
 * text verbatim, exactly like the JS split regex.
 */
object AnsiSpans {

    private val SGR_PATTERN = Regex("\\x1b\\[([0-9;]*)m")

    /** Mutable mirror of the JS `styles` record, keyed semantically. */
    private class SgrStyle {
        var color: String? = null
        var backgroundColor: String? = null
        var bold = false
        var dim = false
        var italic = false
        var underline = false

        fun clear() {
            color = null
            backgroundColor = null
            bold = false
            dim = false
            italic = false
            underline = false
        }

        /** The `effective` copy: headingAccent when italic+bold carry no color. */
        fun effectiveFg(scheme: TerminalScheme): String? =
            color ?: if (italic && bold) scheme.headingAccent else null
    }

    fun parse(
        text: String,
        scheme: TerminalScheme = TerminalScheme.DARK,
        normalizeNearWhiteBackground: Boolean = false,
        normalizeNearBlackForeground: Boolean = false,
        preserveTerminalCells: Boolean = false,
    ): List<TerminalSpan> {
        val spans = ArrayList<TerminalSpan>()
        val styles = SgrStyle()
        var column = 0

        fun emitText(part: String) {
            val fg = styles.effectiveFg(scheme)
            if (preserveTerminalCells) {
                val (leaves, nextColumn) = TerminalCells.cells(part, column)
                column = nextColumn
                for (leaf in leaves) {
                    spans.add(
                        TerminalSpan(
                            text = leaf.text,
                            fg = fg,
                            bg = styles.backgroundColor,
                            bold = styles.bold,
                            italic = styles.italic,
                            underline = styles.underline,
                            dim = styles.dim,
                            className = leaf.className,
                            href = leaf.href,
                            widthCells = leaf.widthCells,
                            styles = leaf.styles,
                        ),
                    )
                }
                return
            }
            for (chunk in TerminalLinkify.linkify(part)) {
                when (chunk) {
                    is TerminalLinkify.Chunk.Plain -> spans.add(
                        TerminalSpan(
                            text = chunk.text,
                            fg = fg,
                            bg = styles.backgroundColor,
                            bold = styles.bold,
                            italic = styles.italic,
                            underline = styles.underline,
                            dim = styles.dim,
                        ),
                    )
                    is TerminalLinkify.Chunk.Link -> spans.add(
                        TerminalSpan(
                            text = chunk.text,
                            fg = fg,
                            bg = styles.backgroundColor,
                            bold = styles.bold,
                            italic = styles.italic,
                            underline = styles.underline,
                            dim = styles.dim,
                            className = "terminal-link",
                            href = chunk.href,
                        ),
                    )
                }
            }
        }

        var cursor = 0
        for (match in SGR_PATTERN.findAll(text)) {
            emitText(text.substring(cursor, match.range.first))
            applySgr(
                match.groupValues[1],
                styles,
                scheme,
                normalizeNearWhiteBackground,
                normalizeNearBlackForeground,
            )
            cursor = match.range.last + 1
        }
        emitText(text.substring(cursor))
        return spans
    }

    private fun applySgr(
        params: String,
        styles: SgrStyle,
        scheme: TerminalScheme,
        normalizeNearWhiteBackground: Boolean,
        normalizeNearBlackForeground: Boolean,
    ) {
        // `parts[index].split(';').map(Number)` — NaN/empty parse as 0-class
        // values; digits-only params keep them integral doubles.
        val codes = if (params.isEmpty()) {
            doubleArrayOf(0.0)
        } else {
            params.split(';').map { it.toDoubleOrNull() ?: 0.0 }.toDoubleArray()
        }
        if (codes.any { it == 0.0 }) styles.clear()
        var position = 0
        while (position < codes.size) {
            val code = codes[position]
            when {
                code == 1.0 -> styles.bold = true
                code == 2.0 -> styles.dim = true
                code == 3.0 -> styles.italic = true
                code == 4.0 -> styles.underline = true
                code == 22.0 -> {
                    styles.bold = false
                    styles.dim = false
                }
                code == 23.0 -> styles.italic = false
                code == 24.0 -> styles.underline = false
                code == 39.0 -> styles.color = null
                code == 49.0 -> styles.backgroundColor = null
                code == 38.0 || code == 48.0 -> {
                    val extended = ansiExtendedColor(codes, position, scheme)
                    if (extended.color != null) {
                        if (code == 38.0) {
                            styles.color = normalizedAnsiForeground(
                                extended.color,
                                normalizeNearBlackForeground,
                                scheme,
                            )
                        } else {
                            styles.backgroundColor = normalizedAnsiBackground(
                                extended.color,
                                normalizeNearWhiteBackground,
                                scheme,
                            )
                        }
                    }
                    position += extended.consumed
                }
                scheme.colors[code.toInt()] != null -> styles.color =
                    normalizedAnsiForeground(
                        scheme.colors.getValue(code.toInt()),
                        normalizeNearBlackForeground,
                        scheme,
                    )
                scheme.colors[code.toInt() - 10] != null -> styles.backgroundColor =
                    normalizedAnsiBackground(
                        scheme.colors.getValue(code.toInt() - 10),
                        normalizeNearWhiteBackground,
                        scheme,
                    )
            }
            position++
        }
    }

    private class ExtendedColor(val color: String?, val consumed: Int)

    private fun ansiExtendedColor(
        codes: DoubleArray,
        position: Int,
        scheme: TerminalScheme,
    ): ExtendedColor {
        if (codes.getOrNull(position + 1) == 2.0 && codes.size > position + 4) {
            return ExtendedColor(
                "rgb(${codes[position + 2].toInt()},${codes[position + 3].toInt()},${codes[position + 4].toInt()})",
                4,
            )
        }
        if (codes.getOrNull(position + 1) == 5.0 && codes.size > position + 2) {
            return ExtendedColor(ansi256Color(codes[position + 2], scheme), 2)
        }
        return ExtendedColor(null, 0)
    }

    /** `ansi256Color` — index <16 maps through the scheme palette. */
    fun ansi256Color(index: Double, scheme: TerminalScheme): String? {
        if (index % 1.0 != 0.0 || index < 0 || index > 255) return null
        val value = index.toInt()
        if (value < 8) return scheme.colors[30 + value]
        if (value < 16) return scheme.colors[90 + value - 8]
        if (value < 232) {
            val offset = value - 16
            val levels = intArrayOf(0, 95, 135, 175, 215, 255)
            return "rgb(${levels[offset / 36]},${levels[(offset % 36) / 6]},${levels[offset % 6]})"
        }
        val gray = 8 + (value - 232) * 10
        return "rgb($gray,$gray,$gray)"
    }

    fun normalizedAnsiBackground(
        color: String,
        normalize: Boolean,
        scheme: TerminalScheme,
    ): String = if (normalize && scheme.opposingBackground(color)) scheme.rowFallback else color

    fun normalizedAnsiForeground(
        color: String,
        normalize: Boolean,
        scheme: TerminalScheme,
    ): String {
        if (!normalize) return color
        val channels = ansiColorChannels(color) ?: return color
        val luminance = ansiRelativeLuminance(channels)
        val spread = channels.max() - channels.min()
        if (scheme.vanishingText(channels, spread)) return "var(--terminal-text)"
        if (scheme.faintText(luminance)) {
            return "color-mix(in srgb, $color 35%, var(--terminal-text))"
        }
        return color
    }

    private val HEX_COLOR = Regex("^#([0-9a-f]{3}|[0-9a-f]{6})$", RegexOption.IGNORE_CASE)
    private val RGB_COLOR =
        Regex("^rgb\\(\\s*(\\d+)\\s*,\\s*(\\d+)\\s*,\\s*(\\d+)\\s*\\)$", RegexOption.IGNORE_CASE)

    fun ansiColorChannels(color: String): IntArray? {
        val value = color.trim()
        val hex = HEX_COLOR.matchEntire(value)
        if (hex != null) {
            val digits = hex.groupValues[1]
            val expanded = if (digits.length == 3) {
                digits.map { "$it$it" }.joinToString("")
            } else {
                digits
            }
            return intArrayOf(
                expanded.substring(0, 2).toInt(16),
                expanded.substring(2, 4).toInt(16),
                expanded.substring(4, 6).toInt(16),
            )
        }
        val rgb = RGB_COLOR.matchEntire(value) ?: return null
        return intArrayOf(
            rgb.groupValues[1].toInt(),
            rgb.groupValues[2].toInt(),
            rgb.groupValues[3].toInt(),
        )
    }

    private fun ansiRelativeLuminance(channels: IntArray): Double {
        val linear = channels.map { channel ->
            val value = channel / 255.0
            if (value <= 0.04045) value / 12.92 else Math.pow((value + 0.055) / 1.055, 2.4)
        }
        return 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2]
    }

    fun isNearWhiteAnsiColor(color: String): Boolean {
        val channels = ansiColorChannels(color) ?: return false
        return channels.min() >= 220 && channels.max() - channels.min() <= 40
    }

    fun isNearBlackAnsiColor(color: String): Boolean {
        val channels = ansiColorChannels(color) ?: return false
        return channels.max() <= 48 && channels.max() - channels.min() <= 24
    }
}
