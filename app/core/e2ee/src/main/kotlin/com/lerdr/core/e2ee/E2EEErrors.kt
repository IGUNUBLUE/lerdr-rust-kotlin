package com.lerdr.core.e2ee

/**
 * Errors thrown by the E2EE handshake and session layer.
 *
 * The subclasses are the error taxonomy the `crypto.failures` fixture suite
 * pins down (`expected_error`): [Format], [Replay], [Sequence], [Auth].
 * Go collapses replay and forward-skip into one "invalid encrypted frame
 * sequence"; the split here lets callers report replays distinctly.
 */
sealed class E2EEException(message: String, cause: Throwable? = null) : Exception(message, cause) {
    /** Malformed frame or handshake message (JSON, header, field, or base64 shape). */
    class Format(message: String, cause: Throwable? = null) : E2EEException(message, cause)

    /**
     * Frame sequence is ahead of the receiver's expectation, exceeds the
     * 2^53-1 ceiling, or the send direction is exhausted.
     */
    class Sequence(message: String) : E2EEException(message)

    /** Frame sequence is behind the receiver's expectation: a replayed delivery. */
    class Replay(val sequence: Long, val expected: Long) : E2EEException(
        "invalid encrypted frame sequence: $sequence already consumed (next expected $expected)",
    )

    /** GCM tag verification or handshake proof check failed. */
    class Auth(message: String, cause: Throwable? = null) : E2EEException(message, cause)
}
