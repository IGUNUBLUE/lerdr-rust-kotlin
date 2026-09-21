package lerdr.core.model

import kotlinx.serialization.EncodeDefault
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement

/** `ApiError` — bounded error object embedded in `error` and receipts. */
@Serializable
data class ApiError(
    val code: String,
    val args: Map<String, JsonElement>? = null,
) {
    companion object {
        const val INVALID_REQUEST = "invalid_request"
        const val UNKNOWN_ACTION = "unknown_action"
        const val INCOMPATIBLE_PROTOCOL = "incompatible_protocol"
        const val READER_DENIED = "reader_denied"
    }
}

/** `ActionReceiptPhase` — the five phases the relay emits (frozen v3). */
@Serializable
enum class ActionReceiptPhase {
    @SerialName("prepared")
    PREPARED,

    @SerialName("failed_before_dispatch")
    FAILED_BEFORE_DISPATCH,

    @SerialName("awaiting_evidence")
    AWAITING_EVIDENCE,

    @SerialName("confirmed")
    CONFIRMED,

    @SerialName("dispatched_unknown")
    DISPATCHED_UNKNOWN,
}

/** `ActionReceipt` — receipt body inside `action_receipt` messages. */
@Serializable
data class ActionReceipt(
    @SerialName("action_id") val actionId: String,
    val phase: ActionReceiptPhase,
    val error: ApiError? = null,
)

/** `DeviceRole` — pairing role negotiated during handshake. */
@Serializable
enum class DeviceRole {
    @SerialName("reader")
    READER,

    @SerialName("controller")
    CONTROLLER,

    @SerialName("bootstrap")
    BOOTSTRAP,
}

/** `DeviceContext` — the authenticated device identity for a connection. */
@Serializable
data class DeviceContext(
    @SerialName("device_id") val deviceId: String,
    @SerialName("credential_id") val credentialId: String,
    val role: DeviceRole,
    val locale: String,
    @SerialName("credential_version") val credentialVersion: Long,
)
