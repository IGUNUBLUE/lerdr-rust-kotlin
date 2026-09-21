package lerdr.core.terminal

/**
 * `TERMINAL_SCHEMES` from terminal.ts — the dark palette plus the light
 * (Catppuccin Latte terminal) palette, each with the predicates that decide
 * which colors vanish against the pane and how they are normalized.
 */
enum class TerminalScheme(
    val colors: Map<Int, String>,
    val headingAccent: String,
    val rowFallback: String,
) {
    DARK(
        colors = mapOf(
            30 to "#555", 31 to "#ff5f5f", 32 to "#5fd75f", 33 to "#ffd75f",
            34 to "#5fafff", 35 to "#d75fff", 36 to "#1abc9c", 37 to "#e5e5e5",
            90 to "#777", 91 to "#ff8080", 92 to "#80ff80", 93 to "#ffff80",
            94 to "#80bfff", 95 to "#ff80ff", 96 to "#80ffff", 97 to "#fff",
        ),
        headingAccent = "#3daee9",
        rowFallback = "rgb(61,64,64)",
    ),
    LIGHT(
        colors = mapOf(
            30 to "#5c5f77", 31 to "#d20f39", 32 to "#40a02b", 33 to "#df8e1d",
            34 to "#1e66f5", 35 to "#ea76cb", 36 to "#179299", 37 to "#acb0be",
            90 to "#6c6f85", 91 to "#de293e", 92 to "#49af3d", 93 to "#eea02d",
            94 to "#456eff", 95 to "#fe85d8", 96 to "#2d9fa8", 97 to "#bcc0cc",
        ),
        headingAccent = "#1e66f5",
        rowFallback = "rgb(204,208,218)",
    ),
    ;

    /** A row background painted for the opposite scheme. */
    fun opposingBackground(color: String): Boolean = when (this) {
        DARK -> AnsiSpans.isNearWhiteAnsiColor(color)
        LIGHT -> AnsiSpans.isNearBlackAnsiColor(color)
    }

    /** Neutral text that disappears entirely takes the pane's text color. */
    fun vanishingText(channels: IntArray, spread: Int): Boolean = when (this) {
        DARK -> channels.max() <= 96 && spread <= 30
        LIGHT -> channels.min() >= 160 && spread <= 30
    }

    /** Tinted text that merely fades is mixed toward the pane's text color. */
    fun faintText(luminance: Double): Boolean = when (this) {
        DARK -> luminance < 0.14
        LIGHT -> luminance > 0.6
    }
}
