package lerdr.core.conversation

/**
 * Page-level model for `get_conversation_history` — the wire `BrowsePage`
 * (`internal/conversation/browser.go`) normalized for the feed, plus the
 * request side. The fixtures under `fixtures/conversation/` pin the
 * reader-level contract this projection preserves.
 */

/** Page lifecycle — Go `BrowseState`. */
enum class ConversationBrowseState(val wire: String) {
    /** Entries are final for this read. */
    READY("ready"),

    /**
     * The relay is indexing older history in the background; the page carries
     * a [ConversationPage.progress] snapshot and a [ConversationPage.nextCursor]
     * that resolves once preparation finishes.
     */
    PREPARING("preparing"),

    /** The read failed; [ConversationPage.error] carries the failure. */
    FAILED("failed"),
    ;

    companion object {
        fun fromWire(value: String): ConversationBrowseState? = when (value) {
            "ready" -> READY
            "preparing" -> PREPARING
            "failed" -> FAILED
            else -> null
        }
    }
}

/** Read path the relay served the page from — Go `BrowseMode`. */
enum class ConversationBrowseMode(val wire: String) {
    /** Tail window of the live transcript source (JSONL readers). */
    RECENT("recent"),

    /** Prepared snapshot for deep history (snapshot/chain browsing). */
    SNAPSHOT("snapshot"),

    /** Direct sqlite-backed readers (opencode, hermes). */
    NATIVE("native"),
    ;

    companion object {
        fun fromWire(value: String): ConversationBrowseMode? = when (value) {
            "recent" -> RECENT
            "snapshot" -> SNAPSHOT
            "native" -> NATIVE
            else -> null
        }
    }
}

/** `BrowseProgress` — preparation progress while state = PREPARING. */
data class ConversationBrowseProgress(
    val phase: String,
    val scannedBytes: Long,
    val sourceBytes: Long,
)

/** `BrowseError` — structured failure on `state = failed` pages. */
data class ConversationBrowseError(
    val code: String,
    val message: String,
    val retryable: Boolean,
)

/**
 * `BrowseDiagnostics` — reader/browser self-report on the page.
 *
 * [sourceTruncated] corresponds to the reader's `file_truncated`: the page is
 * a tail window of a source larger than the read cap, not the full history.
 * [corruptRecords]/[planCorrupt] correspond to the reader's `source_corrupt`:
 * source rows were skipped as malformed (see [ConversationPage.sourceCorrupt]).
 */
data class ConversationDiagnostics(
    val oversizedRecords: Int = 0,
    val corruptRecords: Int = 0,
    val omittedTools: Int = 0,
    val omittedPayloads: Int = 0,
    val planCorrupt: Boolean = false,
    val sourceTruncated: Boolean = false,
    val continuationIncomplete: Boolean = false,
    val continuationReason: String? = null,
) {
    companion object {
        /** Legal `continuation_reason` values (claude chain browsing). */
        val CONTINUATION_REASONS: Set<String> = setOf(
            "missing_source", "invalid_link", "ambiguous_link",
            "cycle", "resolution_limit", "partial_link",
        )
    }
}

/**
 * One `get_conversation_history` response page.
 *
 * ## Tail-first pagination contract
 *
 * [entries] are chronological (oldest → newest) *within* a page, and pages
 * resolve from the tail: a request with `cursor = ""` returns the newest
 * `limit` entries. When [hasMore] is true, [nextCursor] is the opaque token
 * to pass back as `cursor` to fetch the next-older page; on the wire it is
 * the page's first entry id wrapped in the relay's signed `hb1.` envelope
 * (the fixtures model the native form: `next_cursor` = first entry id of the
 * page — the `before`/`cursor` that returns the next-older page).
 * `hasMore = false` means the page starts at the oldest entry; [nextCursor]
 * is empty then.
 *
 * Live tail: re-requesting with `cursor = ""` re-reads the tail — appended
 * entries grow the newest page while existing entry ids stay stable, so the
 * feed can merge by id.
 *
 * Cursors are scope-bound and short-lived (relay-side TTL ~15min): a stale or
 * foreign cursor answers `state = failed` + `error.code = invalid_cursor`/
 * `cursor_expired`; a source that changed under an open cursor answers
 * `source_changed` — restart from `cursor = ""`.
 *
 * @param available false when the conversation cannot be served at all
 *   ([reasonCode] explains: `invalid_provider`, `invalid_session` …).
 *   Mid-read failures keep `available = true` and surface via [state]/[error].
 * @param total full visible entry count when known; null on unavailable and
 *   failed pages and on tail windows of clipped sources.
 */
data class ConversationPage(
    val available: Boolean,
    val reasonCode: String = "",
    val reason: String = "",
    val entries: List<ConversationEntry> = emptyList(),
    val nextCursor: String = "",
    val hasMore: Boolean = false,
    val total: Int? = null,
    val state: ConversationBrowseState = ConversationBrowseState.READY,
    val mode: ConversationBrowseMode = ConversationBrowseMode.RECENT,
    val sourceRevision: String = "",
    val snapshotId: String = "",
    val progress: ConversationBrowseProgress? = null,
    val diagnostics: ConversationDiagnostics = ConversationDiagnostics(),
    val error: ConversationBrowseError? = null,
    val omoPlan: OmoTodoState? = null,
) {
    /** The relay skipped malformed source rows while building this page. */
    val sourceCorrupt: Boolean
        get() = diagnostics.corruptRecords > 0 || diagnostics.planCorrupt

    /** This page is a tail window of a source larger than the read cap. */
    val fileTruncated: Boolean
        get() = diagnostics.sourceTruncated
}

/** Machine codes carried in `reason_code` and `error.code`. Open set — the
 * relay may add codes; these are the ones the Go oracle emits today. */
object ConversationReason {
    const val INVALID_PROVIDER = "invalid_provider"
    const val INVALID_SESSION = "invalid_session"
    const val INVALID_CURSOR = "invalid_cursor"
    const val CURSOR_EXPIRED = "cursor_expired"
    const val SOURCE_CHANGED = "source_changed"
    const val SOURCE_CORRUPT = "source_corrupt"
    const val SOURCE_UNAVAILABLE = "source_unavailable"
    const val OUTPUT_LIMIT = "output_limit"
    const val QUERY_FAILED = "query_failed"
    const val PATH_UNCONTAINED = "path_uncontained"
    const val INDEX_FAILED = "index_failed"
    const val INDEX_BUSY = "index_busy"
    const val INDEX_CAPACITY_EXCEEDED = "index_capacity_exceeded"
    const val INDEX_STORAGE_UNAVAILABLE = "index_storage_unavailable"
    const val REQUEST_CANCELLED = "request_cancelled"
    const val VALIDATION_PENDING = "validation_pending"
}

/**
 * Parameters of one `get_conversation_history` call — applied to the inbound
 * `cursor`, `limit`, and `retry` fields.
 *
 * @param cursor [ConversationPage.nextCursor] verbatim; "" asks for the
 *   newest page.
 * @param limit requested page size; [effectiveLimit] is what the relay
 *   applies (clamped to `1..MAX_PAGE_SIZE`, `DEFAULT_PAGE_SIZE` when < 1).
 * @param retry re-issue after a `cursor_expired`/`source_changed` failure.
 */
data class ConversationPageRequest(
    val cursor: String = "",
    val limit: Int = DEFAULT_PAGE_SIZE,
    val retry: Boolean = false,
) {
    /** Page size the relay applies for this request. */
    val effectiveLimit: Int =
        if (limit < 1) DEFAULT_PAGE_SIZE else minOf(limit, MAX_PAGE_SIZE)

    companion object {
        const val ACTION = "get_conversation_history"
        const val DEFAULT_PAGE_SIZE = 80
        const val MAX_PAGE_SIZE = 200
    }
}
