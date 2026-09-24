package lerdr.core.model

import kotlinx.serialization.EncodeDefault
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement

/**
 * `protocol.Inbound` — the single flat struct every inbound client→server
 * message decodes into. Field order mirrors the Go declaration order so a
 * canonical re-serialization reproduces `decoded_json` in
 * fixtures/protocol/protocol.envelope.json byte-for-byte.
 *
 * omitempty semantics: fields emit only when non-default — except `type`
 * and `protocol`, which Go always marshals (no omitempty).
 */
@Serializable
data class Inbound(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) val type: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) val protocol: Int = 0,
    @SerialName("request_id") val requestId: String = "",
    val target: TargetRef? = null,
    @SerialName("action_id") val actionId: String = "",
    @SerialName("server_session_id") val serverSessionId: String = "",
    @SerialName("session_id") val sessionId: String = "",
    @SerialName("pane_id") val paneId: String = "",
    val text: String = "",
    val name: String = "",
    @SerialName("device_id") val deviceId: String = "",
    val role: String = "",
    val locale: String = "",
    @SerialName("profile_id") val profileId: String = "",
    val label: String = "",
    @SerialName("workspace_id") val workspaceId: String = "",
    @SerialName("workspace_ids") val workspaceIds: List<String> = emptyList(),
    @SerialName("expected_workspace_ids") val expectedWorkspaceIds: List<String> = emptyList(),
    @SerialName("close_group") val closeGroup: Boolean = false,
    @SerialName("before_workspace_id") val beforeWorkspaceId: String = "",
    val branch: String = "",
    val base: String = "",
    val force: Boolean = false,
    val cwd: String = "",
    val prompt: String = "",
    @SerialName("event_id") val eventId: String = "",
    @SerialName("approval_fingerprint") val approvalFingerprint: String = "",
    val choice: String = "",
    @SerialName("interaction_id") val interactionId: String = "",
    @SerialName("insert_index") val insertIndex: Int? = null,
    val index: Int? = null,
    val total: Int? = null,
    val keys: List<String> = emptyList(),
    @SerialName("selected_indices") val selectedIndices: List<Int> = emptyList(),
    @SerialName("other_selected") val otherSelected: Boolean = false,
    @SerialName("other_text") val otherText: String = "",
    val direction: String = "",
    val lines: Int = 0,
    val before: String = "",
    /**
     * Pagination cursor for the legacy reads — a JSON string there; the
     * Phase-5 `pane_search`/`pane_selection_read` actions carry the
     * structured `{row,col}` point instead (relay reads it from the raw
     * map, `docs/13` §1.2-1.3). JsonElement keeps both wire shapes.
     */
    val cursor: JsonElement? = null,
    val retry: Boolean = false,
    val limit: Int = 0,
    val columns: Int = 0,
    val rows: Int = 0,
    val format: String = "",
    val path: String = "",
    val filename: String = "",
    @SerialName("mime") val mime: String = "",
    val data: String = "",
    @SerialName("client_id") val clientId: String = "",
    @SerialName("replace_endpoints") val replaceEndpoints: List<String> = emptyList(),
    @SerialName("notify_finished") val notifyFinished: Boolean = false,
    val endpoints: List<String> = emptyList(),
    val origin: String = "",
    @SerialName("expected_origin") val expectedOrigin: String = "",
    @SerialName("expected_version") val expectedVersion: String = "",
    @SerialName("expected_revision") val expectedRevision: String = "",
    /** `json.RawMessage` passthrough — arbitrary JSON, verbatim. */
    val subscription: JsonElement? = null,
    /** `json.RawMessage` passthrough — arbitrary JSON, verbatim. */
    val policy: JsonElement? = null,
    @SerialName("event_ref") val eventRef: String = "",
    @SerialName("snooze_until") val snoozeUntil: String = "",
    val snoozed: Boolean = false,
    val visible: Boolean = false,
    val unlocked: Boolean = false,
    // ── Phase-5 (docs/13) — appended at the tail like the relay ────
    /** `pane_search` — the copy-engine query (§1.2). */
    val query: String = "",
    /** `client_caps`/inbound `caps_update` — announced capability list (§0). */
    val capabilities: List<String> = emptyList(),
    /** `pane_selection_read` — selection start `{row,col}` (§1.3). */
    val anchor: JsonElement? = null,
    /** `pane_search` — prior match `{start,end}` for next/previous (§1.2). */
    val previous: JsonElement? = null,
    /** `pane_link_resolve`/`pane_link_activate` — viewport cell row (§1.4). */
    val row: Int? = null,
    /** `pane_link_resolve`/`pane_link_activate` — viewport cell column. */
    val col: Int? = null,
    /** `layout_apply` — the LayoutNode tree (§1.5). */
    val root: JsonElement? = null,
    /** `layout_apply` — label for the applied tab (§1.5). */
    @SerialName("tab_label") val tabLabel: String = "",
    /** `layout_apply` — focus the applied layout (§1.5). */
    val focus: Boolean = false,
)
