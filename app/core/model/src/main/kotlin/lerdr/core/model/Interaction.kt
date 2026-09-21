package lerdr.core.model

import kotlinx.serialization.EncodeDefault
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/**
 * `Interaction` — the structured-question schema from
 * docs/specs/questions.md §1 (internal/question/parser.go).
 *
 * `kind` is modeled as a raw string: the spec lists only
 * `single_select`/`multi_select`, but the protocol.envelope fixture carries
 * a `"choice"` value — a tolerant string keeps decode lossless while
 * [kindOrNull] exposes the spec'd set.
 */
@Serializable
data class Interaction(
    val id: String,
    val kind: String,
    val question: String,
    val options: List<Option>,
    val other: Other,
    @EncodeDefault(EncodeDefault.Mode.ALWAYS)
    @SerialName("submit_label") val submitLabel: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS)
    @SerialName("can_chat") val canChat: Boolean = false,
    @EncodeDefault(EncodeDefault.Mode.ALWAYS)
    @SerialName("can_go_back") val canGoBack: Boolean = false,
    @SerialName("question_index") val questionIndex: Int = 0,
    @SerialName("question_total") val questionTotal: Int = 0,
) {
    /** Spec'd interaction kinds — the only values the parser emits. */
    enum class Kind(val wire: String) {
        SINGLE_SELECT("single_select"),
        MULTI_SELECT("multi_select"),
    }

    /** The kind as a spec'd [Kind], or null for unrecognized values. */
    val kindOrNull: Kind?
        get() = Kind.entries.firstOrNull { it.wire == kind }
}

@Serializable
data class Option(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) val index: Int = 0,
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) val label: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) val description: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) val selected: Boolean = false,
    val summary: List<SummaryEntry> = emptyList(),
)

/** One answered question on a review-screen option (`{q, a}` pairs). */
@Serializable
data class SummaryEntry(
    @SerialName("q") val question: String,
    @SerialName("a") val answer: String,
)

@Serializable
data class Other(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) val selected: Boolean = false,
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) val text: String = "",
    val label: String = "",
    val placeholder: String = "",
    @SerialName("allow_empty") val allowEmpty: Boolean = false,
    val hidden: Boolean = false,
)
