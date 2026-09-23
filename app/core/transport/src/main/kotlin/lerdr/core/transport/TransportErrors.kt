package lerdr.core.transport

/**
 * `TransportStatusDetail` — why a socket ended. [code] carries the stable
 * machine reason (`device_unauthorized`, `unknown_relay`) when the transport
 * or relay supplies one; [wsCloseCode] is the WebSocket close code.
 * [fatal] marks refusals that must not be retried on the normal cadence.
 */
data class DisconnectReason(
    val reason: String,
    val code: String? = null,
    val wsCloseCode: Int? = null,
    val wsReason: String? = null,
    val fatal: Boolean = false,
    val cause: Throwable? = null,
) {
    /** The oracle's `authRejected` latch — a 4401 close. */
    val isAuthRejection: Boolean
        get() = code == ReconnectPolicy.DEVICE_UNAUTHORIZED_CODE ||
            wsCloseCode == ReconnectPolicy.UNAUTHORIZED_CLOSE_CODE
}

/** Transport-level failures. Crypto failures surface as `E2EEException` unchanged. */
sealed class TransportException(message: String, cause: Throwable? = null) : Exception(message, cause) {

    /** The socket never opened (DNS, refused, TLS) or failed mid-handshake. */
    class DialFailed(message: String, cause: Throwable? = null) : TransportException(message, cause)

    /** The peer did not echo `herdr-e2ee-v2` — 'Relay did not negotiate encrypted transport'. */
    class EncryptionRequired(message: String = "Relay did not negotiate encrypted transport") :
        TransportException(message)

    /** `e2eeHandshakeTimeout` — the server finish did not arrive in 10 s. */
    class HandshakeTimeout(message: String = "Encrypted relay handshake timed out") :
        TransportException(message)

    /** The socket ended while connecting, handshaking, or idling mid-exchange. */
    class ConnectionClosed(val detail: DisconnectReason) :
        TransportException(detail.reason)

    /** `WebSocket.send` refused the frame (closing, cancelled, or queue over capacity). */
    class WriteRejected(message: String = "Could not send relay frame") : TransportException(message)

    /** `request`/`send` while the session has no live connection. */
    class NotConnected(message: String = "Relay is not connected") : TransportException(message)
}

/**
 * A relay-confirmed command failure — the oracle's `CommandError`. [phase]
 * distinguishes `failed_before_dispatch` from `dispatched_unknown`, where the
 * relay may still have acted after the write succeeded.
 */
class CommandException(
    message: String,
    val code: String? = null,
    val phase: String? = null,
    val apiError: lerdr.core.model.ApiError? = null,
    /** `dispatched_unknown` — outcome unknowable; the frame may have landed. */
    val dispatchedUnknown: Boolean = false,
    /**
     * Refusal payloads still carry structured state (e.g. `update` on a
     * declined `install_update`); the oracle folds it into connection state
     * even on failure, so it must survive the throw.
     */
    val data: kotlinx.serialization.json.JsonElement? = null,
) : Exception(message)
