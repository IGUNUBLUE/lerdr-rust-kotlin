package lerdr.core.protocol

/** Frozen wire contract constants (`internal/protocol/protocol.go`). */
object Protocol {
    const val VERSION = 3
    const val ENCRYPTED_WEBSOCKET_SUBPROTOCOL = "herdr-e2ee-v2"
    const val SPEECH_SYNTHESIS_CAPABILITY = "speech_synthesis"
    const val SPEECH_VOICE_MANAGEMENT_CAPABILITY = "speech_voice_management"
}
