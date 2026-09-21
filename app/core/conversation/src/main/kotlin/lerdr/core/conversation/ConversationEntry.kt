package lerdr.core.conversation

/**
 * Domain model for the semantic feed — "what the agent is doing" — as the
 * relay reports it through `get_conversation_history` pages.
 *
 * Shapes mirror the Go oracle `internal/conversation/reader.go`:
 * `Entry{ID, Timestamp, Role, Text, Tools, Truncated}` and
 * `ToolActivity{ID, Name, Input, Output, Error, Truncated}`. The relay owns
 * record filtering (sidechains, meta rows, reasoning blocks, system reminders
 * never reach the wire); every entry here is user-visible.
 */

/** Entry authors the feed renders. Other wire roles are dropped upstream. */
enum class ConversationRole(val wire: String) {
    USER("user"),
    ASSISTANT("assistant"),
    ;

    companion object {
        /** Maps a wire `role` string; returns null for non-renderable roles. */
        fun fromWire(value: String?): ConversationRole? = when (value) {
            "user" -> USER
            "assistant" -> ASSISTANT
            else -> null
        }
    }
}

/**
 * One tool call attached to an [ConversationEntry] — Go `ToolActivity`.
 *
 * The relay associates results back to calls across records (claude/codex/
 * pi-style `tool_use`/`tool_result` pairs, opencode part state, hermes
 * `tool_calls` + `role=tool` rows), so [output] is the joined result text —
 * empty while the call has no result yet.
 *
 * @param id association id the agent emitted (`toolu_…`, `call_…`, `ses_…`
 *   internal ids). Opaque; may be empty for agents that emit nameless calls.
 * @param name display name; never empty — the relay substitutes "Tool".
 * @param input verbatim argument payload as emitted: JSON text for structured
 *   calls (`{"command":"…"}`), raw text for patch-style tools.
 * @param output joined tool-result text, "" when none arrived (yet).
 * @param error true when the call failed (`is_error` / state.status="error").
 * @param truncated true when input or output hit a relay size clamp.
 */
data class ConversationTool(
    val id: String = "",
    val name: String = "Tool",
    val input: String = "",
    val output: String = "",
    val error: Boolean = false,
    val truncated: Boolean = false,
)

/**
 * One visible conversation row — Go `conversation.Entry`.
 *
 * @param id opaque, stable per source record: `sha256(raw jsonl line)[:24]`
 *   for JSONL readers (`-N` suffix disambiguates byte-identical lines), the
 *   message id for opencode, the sqlite rowid for hermes. Claude continuation
 *   segments prefix it with an inode-derived namespace
 *   (`sha256(sessionID+"\x00"+fileRevision)[:12]+"-"+rawID`). UI uses it as a
 *   list key and cursor anchor only — never parsed.
 * @param timestamp source record timestamp (RFC3339), "" when the record
 *   carried none.
 * @param text visible body after relay-side filtering (command envelopes
 *   unwrapped to `/name args`, hidden blocks stripped).
 * @param tools tool calls on this row; empty for plain messages.
 * @param truncated true when text or a tool payload hit a relay size clamp.
 */
data class ConversationEntry(
    val id: String,
    val timestamp: String = "",
    val role: ConversationRole,
    val text: String = "",
    val tools: List<ConversationTool> = emptyList(),
    val truncated: Boolean = false,
)
