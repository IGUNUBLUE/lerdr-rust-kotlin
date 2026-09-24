package lerdr.core.model

import kotlinx.serialization.EncodeDefault
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/**
 * Phase-5 §1.2-1.5 pane-content vocabulary — the copy-engine point/range
 * shapes shared by `pane_search`, `pane_selection_read`, and the link
 * actions, plus their `command_result.data` payload DTOs
 * (`docs/13-phase5-wire-spec.md`).
 *
 * Request-side the app emits these inside the flat `Inbound` raw fields
 * (`cursor`/`anchor`/`previous`/`row`/`col`/`root`); response-side they
 * decode out of [CommandResultMessage.data].
 */

/** Copy-engine cell — `{row,col}` (herdr `PaneTextPoint`). */
@Serializable
data class PaneTextPoint(
    val row: Long = 0,
    val col: Long = 0,
)

/** `{start,end}` cell pair — search-match and selection shape. */
@Serializable
data class PaneTextRange(
    val start: PaneTextPoint = PaneTextPoint(),
    val end: PaneTextPoint = PaneTextPoint(),
)

/** `pane_search` result — matches plus upstream match-position metadata. */
@Serializable
data class PaneSearchResult(
    val matches: List<PaneTextRange> = emptyList(),
    @SerialName("content_revision") val contentRevision: Long = 0,
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) val total: Long = 0,
    /** Hit-cursor position when herdr tracks it; `null` when it does not. */
    val current: Long? = null,
    @SerialName("current_global") val currentGlobal: Long? = null,
)

/** `pane_selection_read` result — the range's text + the fence revision. */
@Serializable
data class PaneSelectionResult(
    val text: String = "",
    @SerialName("content_revision") val contentRevision: Long = 0,
)

/** One link's inclusive display-cell bounds — `{row,start_col,end_col}`. */
@Serializable
data class PaneLinkRegion(
    val row: Long = 0,
    @SerialName("start_col") val startCol: Long = 0,
    @SerialName("end_col") val endCol: Long = 0,
)

/**
 * `pane_link_resolve` result — the hit-tested link's cell **regions**.
 * herdr 0.9.1 exposes bounds only; the URL surfaces on activate, never
 * here (docs/13 §1.4 correction).
 */
@Serializable
data class PaneLinkResolvedResult(
    val regions: List<PaneLinkRegion> = emptyList(),
)

/** `pane_link_activate` result — `url` present even when not `handled`. */
@Serializable
data class PaneLinkActivatedResult(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) val handled: Boolean = false,
    val url: String? = null,
)

/** `layout_export` result — the verbatim herdr `LayoutNode` tree. */
@Serializable
data class LayoutExportResult(
    val root: kotlinx.serialization.json.JsonElement? = null,
)
