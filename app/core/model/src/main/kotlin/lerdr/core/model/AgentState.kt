package lerdr.core.model

import kotlinx.serialization.EncodeDefault
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/**
 * `coordinator.AgentState` — one agent row in an `agents` snapshot.
 * Field order and omitempty mirror the Go struct (`state.go:18-58`).
 */
@Serializable
data class AgentState(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) @SerialName("pane_id") val paneId: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) @SerialName("raw_pane_id") val rawPaneId: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) @SerialName("terminal_id") val terminalId: String = "",
    @SerialName("server_session_id") val serverSessionId: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) val generation: Long = 0,
    @SerialName("agent_session_id") val agentSessionId: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) @SerialName("tab_id") val tabId: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) @SerialName("tab_label") val tabLabel: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) @SerialName("tab_number") val tabNumber: Int = 0,
    @SerialName("tab_order") val tabOrder: Int = 0,
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) @SerialName("workspace_id") val workspaceId: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) val agent: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) val name: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) val status: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) @SerialName("_focused") val focused: Boolean = false,
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) val cwd: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) val project: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) val host: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) val session: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) @SerialName("session_name") val sessionName: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) @SerialName("updated_at") val updatedAt: Long = 0,
    @SerialName("last_active_at") val lastActiveAt: Long = 0,
    @SerialName("last_seen_at") val lastSeenAt: Long = 0,
    @SerialName("activity_seq") val activitySeq: Long = 0,
    @SerialName("event_id") val blockedEventId: String = "",
    @SerialName("attention_kind") val attentionKind: String = "",
    val prompt: String = "",
    val command: String = "",
    val options: List<String> = emptyList(),
    @SerialName("approval_fingerprint") val approvalFingerprint: String = "",
    val interaction: Interaction? = null,
    @SerialName("question_layout") val questionLayout: Boolean = false,
    @SerialName("conversation_history_available") val conversationHistoryAvailable: Boolean = false,
    @SerialName("pane_revision") val paneRevision: Long = 0,
) {
    companion object {
        const val STATUS_WORKING = "working"
        const val STATUS_BLOCKED = "blocked"
        const val STATUS_DONE = "done"
        const val STATUS_IDLE = "idle"
    }
}

/** `activity.Entry` — one journal entry in `activity`/`activity_history`. */
@Serializable
data class ActivityEntry(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) val id: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) val timestamp: Long = 0,
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) val kind: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) val status: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) val summary: String = "",
    val host: String = "",
    @SerialName("pane_id") val paneId: String = "",
    val agent: String = "",
    val project: String = "",
    @SerialName("request_id") val requestId: String = "",
    val extract: String = "",
    val session: String = "",
    val details: Map<String, kotlinx.serialization.json.JsonElement>? = null,
)
