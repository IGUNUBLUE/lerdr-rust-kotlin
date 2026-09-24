package lerdr.core.model

import kotlinx.serialization.EncodeDefault
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/**
 * Wire `TargetRef` — protocol.go. The five legacy fields are always
 * serialized (Go has no omitempty there); the Phase-5 additions skip
 * when empty, matching the relay's `skip_serializing_if`
 * (`docs/13-phase5-wire-spec.md` §1.1).
 */
@Serializable
data class TargetRef(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS)
    @SerialName("server_session_id")
    val serverSessionId: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS)
    @SerialName("pane_id")
    val paneId: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS)
    @SerialName("terminal_id")
    val terminalId: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS)
    val generation: Long = 0,
    @EncodeDefault(EncodeDefault.Mode.ALWAYS)
    @SerialName("agent_session_id")
    val agentSessionId: String = "",
    @SerialName("workspace_id")
    val workspaceId: String = "",
    @SerialName("tab_id")
    val tabId: String = "",
)
