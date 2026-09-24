package lerdr.core.model

/**
 * Phase-5 §0 capability vocabulary — the set this app announces via
 * `client_caps` (`docs/13-phase5-wire-spec.md` §0). A capability is
 * **live** only when present on both lists: server-advertised
 * (`push_config`/`caps_update`/`herdr_status`) ∩ client-announced.
 *
 * The announced set is deliberately the families this app implements at
 * the wire layer — Track A only; Track B (`convo_sub`, `frame_zstd`,
 * `upload_binary`, inner binary codec) joins as it lands.
 */
object ClientCapabilities {

    const val FOCUS = "focus"
    const val PANE_SEARCH = "pane_search"
    const val PANE_LINKS = "pane_links"
    const val LAYOUT = "layout"

    /** Capability list emitted in `client_caps` — deterministic order. */
    val ANNOUNCED: List<String> = listOf(FOCUS, PANE_SEARCH, PANE_LINKS, LAYOUT)

    /** `preferred_inner_codec` — JSON inner frames; binary deferred (§0/Q4). */
    const val PREFERRED_INNER_CODEC = "json"

    /** `advertised ∩ announced` — the live set per docs/13 §0. */
    fun live(advertised: Collection<String>): Set<String> =
        advertised.toSet() intersect ANNOUNCED.toSet()
}
