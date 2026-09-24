package lerdr.core.model

/**
 * Phase-5 §0 capability vocabulary — the set this app announces via
 * `client_caps` (`docs/13-phase5-wire-spec.md` §0). A capability is
 * **live** only when present on both lists: server-advertised
 * (`push_config`/`caps_update`/`herdr_status`) ∩ client-announced.
 *
 * The announced set is deliberately the families this app implements at
 * the wire layer — Track A + Track B; the inner binary codec was
 * dropped from the plan (§2.1 — `frame_zstd` captured the win).
 */
object ClientCapabilities {

    const val FOCUS = "focus"
    const val PANE_SEARCH = "pane_search"
    const val PANE_LINKS = "pane_links"
    const val LAYOUT = "layout"
    const val CONVO_SUB = "convo_sub"
    const val FRAME_ZSTD = "frame_zstd"
    const val UPLOAD_BINARY = "upload_binary"

    /** Capability list emitted in `client_caps` — deterministic order. */
    val ANNOUNCED: List<String> = listOf(
        FOCUS, PANE_SEARCH, PANE_LINKS, LAYOUT,
        CONVO_SUB, FRAME_ZSTD, UPLOAD_BINARY,
    )

    /** `advertised ∩ announced` — the live set per docs/13 §0. */
    fun live(advertised: Collection<String>): Set<String> =
        advertised.toSet() intersect ANNOUNCED.toSet()
}
