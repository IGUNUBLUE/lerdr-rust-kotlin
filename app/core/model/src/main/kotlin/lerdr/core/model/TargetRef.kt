package lerdr.core.model

import kotlinx.serialization.EncodeDefault
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/**
 * Wire `TargetRef` — protocol.go. All five fields are always serialized
 * (Go has no omitempty here; absent fields decode to zero values and are
 * re-emitted as `""`/`0`).
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
)
