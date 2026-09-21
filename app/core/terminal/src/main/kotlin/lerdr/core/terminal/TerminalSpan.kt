package lerdr.core.terminal

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/**
 * Flattened terminal span — a leaf of the DOM tree `ansiToHtml` produces,
 * with ancestor `<span style>` attributes merged down (the shape
 * `fixtures/ansi/ansi.spans.json` `expected_spans` uses).
 *
 * [styles] carries CSS declarations that are not color/weight fields —
 * currently only the box-drawing `background:` layer stacks. `width:calc()`
 * on cell runs flattens to [widthCells].
 */
@Serializable
data class TerminalSpan(
    val text: String,
    val fg: String? = null,
    val bg: String? = null,
    val bold: Boolean = false,
    val italic: Boolean = false,
    val underline: Boolean = false,
    val dim: Boolean = false,
    @SerialName("class") val className: String? = null,
    val href: String? = null,
    @SerialName("width_cells") val widthCells: Int? = null,
    val styles: Map<String, String>? = null,
)
