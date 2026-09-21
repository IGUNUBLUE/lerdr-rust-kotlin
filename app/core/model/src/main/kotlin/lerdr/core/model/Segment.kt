package lerdr.core.model

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/**
 * `panedelta.Segment` — one segment of a `pane_delta` payload. Go marks
 * all three fields `omitempty`, so `{"copy_lines":4}` copies previous
 * lines 0..4, `{"text":"x"}` appends a literal, and `{}` is the legal
 * empty literal `Build` emits for empty current content. Absent fields
 * decode to 0/"" exactly like the Go struct.
 */
@Serializable
data class Segment(
    @SerialName("copy_start") val copyStart: Int = 0,
    @SerialName("copy_lines") val copyLines: Int = 0,
    val text: String = "",
)
