package lerdr.core.conversation

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.longOrNull
import lerdr.core.model.CommandResultMessage

/** The relay's `get_conversation_history` payload failed wire validation. */
class ConversationProjectionException(message: String) :
    IllegalArgumentException(message)

/**
 * Transport → domain seam for `get_conversation_history` pages.
 *
 * The relay answers the action with `command_result.data` carrying a
 * serialized `conversation.BrowsePage` (`internal/conversation/browser.go`).
 * [project] normalizes that payload into [ConversationPage] with the same
 * rules the released web client applies in `normalizeConversationPage`
 * (`frontend/src/lib/store.ts`) — the behavioral oracle for this module:
 *
 * - required page fields (`available`, `has_more`, `entries`, `state`,
 *   `mode`) must be present and well-typed, page caps enforced
 *   (≤ [MAX_ENTRIES] entries);
 * - malformed *entries* are dropped, not fatal: non-object rows, roles
 *   outside user/assistant, empty ids, and rows with neither text nor tools;
 * - page-level invariants: `has_more` and `state = preparing` require a
 *   `next_cursor`; `state = failed` requires `error`; `progress` requires
 *   `scanned_bytes ≤ source_bytes`; `continuation_reason` is legal only with
 *   `continuation_incomplete` and must be a known reason.
 *
 * Anything violating the contract throws [ConversationProjectionException] —
 * callers surface it as a page-level error state, like the web client's
 * `CommandError`.
 */
object ConversationProjector {

    // Caps mirror the released client's normalization limits.
    private const val MAX_ENTRIES = 200
    private const val MAX_TOOLS = 128
    private const val MAX_CURSOR_CHARS = 2048
    private const val MAX_ENTRY_ID_CHARS = 256
    private const val MAX_TIMESTAMP_CHARS = 128
    private const val MAX_TEXT_CHARS = 1_048_576
    private const val MAX_TOOL_ID_CHARS = 256
    private const val MAX_TOOL_NAME_CHARS = 160
    private const val MAX_TOOL_IO_CHARS = 1_048_576
    private const val MAX_ENUM_CHARS = 32
    private const val MAX_PROGRESS_PHASE_CHARS = 32
    private const val MAX_ERROR_CODE_CHARS = 64
    private const val MAX_ERROR_MESSAGE_CHARS = 512
    private const val MAX_REASON_CHARS = 512
    private const val MAX_REASON_CODE_CHARS = 64
    private const val MAX_REVISION_CHARS = 256
    private const val MAX_CONTINUATION_REASON_CHARS = 64
    private const val MAX_OMO_PHASES = 128
    private const val MAX_OMO_TASKS = 1000
    private const val MAX_OMO_STRING_CHARS = 4096
    private const val MAX_OMO_REASON_CHARS = 80
    private const val MAX_OMO_SESSION_CHARS = 160
    private const val MAX_OMO_TASK_ID_CHARS = 160
    private const val MAX_OMO_UPDATED_CHARS = 80

    /**
     * Projects the `data` payload of a `command_result` for
     * [ConversationPageRequest.ACTION]. Callers route by `request_id`; the
     * `ok`/`error` failure surface is handled at the command layer — this
     * reads [CommandResultMessage.data] only.
     */
    fun project(result: CommandResultMessage): ConversationPage =
        project(result.data)

    /** Projects a raw `BrowsePage` payload into the domain page. */
    fun project(data: JsonElement?): ConversationPage {
        val page = data as? JsonObject ?: invalid("page payload is not an object")
        val available = strictBool(page["available"]) ?: invalid("'available' must be a boolean")
        val hasMore = strictBool(page["has_more"]) ?: invalid("'has_more' must be a boolean")
        val rawEntries = page["entries"] as? JsonArray
            ?: invalid("'entries' must be an array")
        if (rawEntries.size > MAX_ENTRIES) invalid("'entries' exceeds $MAX_ENTRIES")
        val entries = rawEntries.mapNotNull(::projectEntry)

        val nextCursor = optionalString(page["next_cursor"], MAX_CURSOR_CHARS) ?: ""
        val state = ConversationBrowseState.fromWire(
            requiredString(page["state"], MAX_ENUM_CHARS, "state"),
        ) ?: invalid("'state' is not a known browse state")
        val mode = ConversationBrowseMode.fromWire(
            requiredString(page["mode"], MAX_ENUM_CHARS, "mode"),
        ) ?: invalid("'mode' is not a known browse mode")
        if (hasMore && nextCursor.isEmpty()) {
            invalid("'has_more' without 'next_cursor'")
        }
        if (state == ConversationBrowseState.PREPARING && nextCursor.isEmpty()) {
            invalid("'preparing' page without 'next_cursor'")
        }
        val progress = page["progress"]?.takeIf { it !is JsonNull }?.let(::projectProgress)
        val diagnostics = projectDiagnostics(page["diagnostics"])
        val error = page["error"]?.takeIf { it !is JsonNull }?.let(::projectError)
        if (state == ConversationBrowseState.FAILED && error == null) {
            invalid("'failed' page without 'error'")
        }
        return ConversationPage(
            available = available,
            reasonCode = text(page["reason_code"], MAX_REASON_CODE_CHARS),
            reason = optionalString(page["reason"], MAX_REASON_CHARS) ?: "",
            entries = entries,
            nextCursor = nextCursor,
            hasMore = hasMore,
            total = optionalCount(page["total"]),
            state = state,
            mode = mode,
            sourceRevision = optionalString(page["source_revision"], MAX_REVISION_CHARS) ?: "",
            snapshotId = optionalString(page["snapshot_id"], MAX_REVISION_CHARS) ?: "",
            progress = progress,
            diagnostics = diagnostics,
            error = error,
            omoPlan = page["omo_plan"]?.let(::projectOmoPlan),
        )
    }

    private fun projectEntry(element: JsonElement): ConversationEntry? {
        val entry = element as? JsonObject ?: return null
        val role = ConversationRole.fromWire(
            (entry["role"] as? JsonPrimitive)?.takeIf { it.isString }?.content,
        ) ?: return null
        val text = text(entry["text"], MAX_TEXT_CHARS)
        val tools = (entry["tools"] as? JsonArray)
            ?.asSequence()
            ?.take(MAX_TOOLS)
            ?.mapNotNull(::projectTool)
            ?.toList()
            .orEmpty()
        val id = identity(entry["id"], MAX_ENTRY_ID_CHARS)
        if (id.isEmpty() || (text.isEmpty() && tools.isEmpty())) return null
        return ConversationEntry(
            id = id,
            timestamp = text(entry["timestamp"], MAX_TIMESTAMP_CHARS),
            role = role,
            text = text,
            tools = tools,
            truncated = strictBool(entry["truncated"]) == true,
        )
    }

    private fun projectTool(element: JsonElement): ConversationTool? {
        val tool = element as? JsonObject ?: return null
        return ConversationTool(
            id = identity(tool["id"], MAX_TOOL_ID_CHARS),
            name = text(tool["name"], MAX_TOOL_NAME_CHARS).ifEmpty { "Tool" },
            input = text(tool["input"], MAX_TOOL_IO_CHARS),
            output = text(tool["output"], MAX_TOOL_IO_CHARS),
            error = strictBool(tool["error"]) == true,
            truncated = strictBool(tool["truncated"]) == true,
        )
    }

    private fun projectProgress(element: JsonElement): ConversationBrowseProgress {
        val progress = element as? JsonObject ?: invalid("'progress' must be an object")
        val scanned = count(progress["scanned_bytes"], "scanned_bytes")
        val source = count(progress["source_bytes"], "source_bytes")
        if (scanned > source) invalid("'progress.scanned_bytes' exceeds 'source_bytes'")
        return ConversationBrowseProgress(
            phase = requiredString(progress["phase"], MAX_PROGRESS_PHASE_CHARS, "progress.phase"),
            scannedBytes = scanned,
            sourceBytes = source,
        )
    }

    private fun projectDiagnostics(element: JsonElement?): ConversationDiagnostics {
        if (element == null || element is JsonNull) return ConversationDiagnostics()
        val diagnostics = element as? JsonObject
            ?: invalid("'diagnostics' must be an object")
        val incomplete = optionalBool(diagnostics["continuation_incomplete"], "continuation_incomplete")
        // "" is falsy in the oracle — normalize to absent before the checks.
        val continuationReason =
            optionalString(diagnostics["continuation_reason"], MAX_CONTINUATION_REASON_CHARS)
                ?.takeIf { it.isNotEmpty() }
        if (incomplete && continuationReason !in ConversationDiagnostics.CONTINUATION_REASONS) {
            invalid("'continuation_reason' is not a known reason")
        }
        if (!incomplete && continuationReason != null) {
            invalid("'continuation_reason' without 'continuation_incomplete'")
        }
        return ConversationDiagnostics(
            oversizedRecords = countInt(diagnostics["oversized_records"], "oversized_records"),
            corruptRecords = countInt(diagnostics["corrupt_records"], "corrupt_records"),
            omittedTools = countInt(diagnostics["omitted_tools"], "omitted_tools"),
            omittedPayloads = countInt(diagnostics["omitted_payloads"], "omitted_payloads"),
            // browseBoolean(optional): absent → false, non-boolean → invalid.
            planCorrupt = optionalBool(diagnostics["plan_corrupt"], "plan_corrupt"),
            sourceTruncated = optionalBool(diagnostics["source_truncated"], "source_truncated"),
            continuationIncomplete = incomplete,
            continuationReason = continuationReason,
        )
    }

    private fun projectError(element: JsonElement): ConversationBrowseError {
        val error = element as? JsonObject ?: invalid("'error' must be an object")
        return ConversationBrowseError(
            code = requiredString(error["code"], MAX_ERROR_CODE_CHARS, "error.code"),
            message = requiredString(error["message"], MAX_ERROR_MESSAGE_CHARS, "error.message"),
            retryable = strictBool(error["retryable"]) ?: invalid("'error.retryable' must be a boolean"),
        )
    }

    private fun projectOmoPlan(element: JsonElement): OmoTodoState? {
        val state = element as? JsonObject ?: return null
        var remainingTasks = MAX_OMO_TASKS
        val phases = (state["phases"] as? JsonArray).orEmpty()
            .take(MAX_OMO_PHASES)
            .mapNotNull { candidate ->
                val phase = candidate as? JsonObject ?: return@mapNotNull null
                val tasks = (phase["tasks"] as? JsonArray).orEmpty()
                    .take(remainingTasks)
                    .mapNotNull { item ->
                        val task = item as? JsonObject ?: return@mapNotNull null
                        val status = OmoTaskStatus.fromWire(
                            (task["status"] as? JsonPrimitive)?.takeIf { it.isString }?.content ?: "",
                        )
                        val content = text(task["content"], MAX_OMO_STRING_CHARS)
                        if (status == null || content.isEmpty()) return@mapNotNull null
                        OmoTodoTask(
                            id = text(task["id"], MAX_OMO_TASK_ID_CHARS).ifEmpty { null },
                            content = content,
                            status = status,
                        )
                    }
                remainingTasks -= tasks.size
                OmoTodoPhase(
                    name = text(phase["name"], MAX_OMO_STRING_CHARS).ifEmpty { "Plan" },
                    tasks = tasks,
                )
            }
        return OmoTodoState(
            available = strictBool(state["available"]) == true,
            reasonCode = text(state["reason_code"], MAX_OMO_REASON_CHARS).ifEmpty { null },
            sessionId = text(state["session_id"], MAX_OMO_SESSION_CHARS).ifEmpty { null },
            version = (state["version"] as? JsonPrimitive)
                ?.takeIf { !it.isString }?.longOrNull
                ?.takeIf { it in Int.MIN_VALUE..Int.MAX_VALUE }?.toInt(),
            updatedAt = text(state["updated_at"], MAX_OMO_UPDATED_CHARS).ifEmpty { null },
            phases = phases,
            truncated = strictBool(state["truncated"]) == true,
        )
    }

    // ── field primitives (store.ts browse* helpers) ─────────────────────

    /** `browseRecord`-style object-or-invalid for required fields. */
    private fun invalid(what: String): Nothing =
        throw ConversationProjectionException("relay returned invalid conversation history ($what)")

    /** `typeof === 'boolean'`: rejects string "true"/"false" like the oracle. */
    private fun strictBool(element: JsonElement?): Boolean? =
        (element as? JsonPrimitive)?.takeIf { !it.isString }?.booleanOrNull

    /** `browseBoolean(value, optional = true)`: absent/null → false, non-boolean → invalid. */
    private fun optionalBool(element: JsonElement?, field: String): Boolean {
        if (element == null || element is JsonNull) return false
        return strictBool(element) ?: invalid("'$field' must be a boolean")
    }

    /**
     * `browseString(value, max)` optional form: absent/null → null; present
     * must be a string within the cap, else invalid.
     */
    private fun optionalString(element: JsonElement?, max: Int): String? {
        if (element == null || element is JsonNull) return null
        val value = (element as? JsonPrimitive)?.takeIf { it.isString }?.content
            ?: invalid("expected a string field")
        if (value.length > max) invalid("string field exceeds $max chars")
        return value
    }

    /** `browseString(value, max, required = true)`: must be a non-empty string. */
    private fun requiredString(element: JsonElement?, max: Int, field: String): String {
        val value = optionalString(element, max)
        if (value.isNullOrEmpty()) invalid("'$field' must be a non-empty string")
        return value
    }

    /** `browseText`: non-string → "", string → capped (never invalid). */
    private fun text(element: JsonElement?, max: Int): String =
        (element as? JsonPrimitive)?.takeIf { it.isString }?.content?.take(max) ?: ""

    /** `browseIdentity`: a string within the cap, else "". */
    private fun identity(element: JsonElement?, max: Int): String {
        val value = (element as? JsonPrimitive)?.takeIf { it.isString }?.content ?: return ""
        return if (value.length <= max) value else ""
    }

    /** `browseCount`: absent/null → 0; otherwise a non-negative integer. */
    private fun count(element: JsonElement?, field: String): Long {
        if (element == null || element is JsonNull) return 0
        val value = (element as? JsonPrimitive)?.takeIf { !it.isString }?.longOrNull
        if (value == null || value < 0) invalid("'$field' must be a non-negative integer")
        return value
    }

    /** `browseCount` bounded to the domain's Int counters. */
    private fun countInt(element: JsonElement?, field: String): Int {
        val value = count(element, field)
        if (value > Int.MAX_VALUE) invalid("'$field' exceeds Int range")
        return value.toInt()
    }

    /** `total` handling: absent/null → null; otherwise `browseCount`. */
    private fun optionalCount(element: JsonElement?): Int? {
        if (element == null || element is JsonNull) return null
        val value = count(element, "total")
        if (value > Int.MAX_VALUE) invalid("'total' exceeds Int range")
        return value.toInt()
    }
}
