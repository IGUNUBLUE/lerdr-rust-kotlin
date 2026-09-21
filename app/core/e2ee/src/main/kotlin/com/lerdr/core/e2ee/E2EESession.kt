package com.lerdr.core.e2ee

import java.nio.ByteBuffer
import java.nio.charset.StandardCharsets
import java.security.GeneralSecurityException

/** Per-direction label, bound into the frame AAD (`herdr-e2ee-v2 c2s`/`s2c`). */
enum class E2EEDirection(internal val wireLabel: String) {
    C2S("c2s"),
    S2C("s2c"),
}

/**
 * Frame envelope codec. `JSON` is the WebSocket-text form
 * (`{"type":"e2ee","version":2,"sequence":N,"ciphertext":"<b64url>"}`);
 * `BINARY` is `[0x02, 0x00, BE64 sequence, ciphertext…]` for WS binary frames.
 */
enum class E2EECodec {
    JSON,
    BINARY,
}

/** Negotiated `herdr-e2ee-v2` version carried by every frame. */
const val E2EE_VERSION = 2

internal const val MAX_SEQUENCE: Long = (1L shl 53) - 1
private const val BINARY_HEADER_BYTES = 10
private const val BINARY_KIND_DATA: Byte = 0
private val AAD_PREFIX = "herdr-e2ee-v2 ".toByteArray(StandardCharsets.US_ASCII)

/** `4 zero bytes || BE64(sequence)` — 12-byte GCM nonce. */
internal fun frameNonce(sequence: Long): ByteArray =
    ByteBuffer.allocate(AesGcm.NONCE_BYTES).putInt(0).putLong(sequence).array()

/** `"herdr-e2ee-v2 " || direction || 0x00 || BE64(sequence)`. */
internal fun frameAad(direction: E2EEDirection, sequence: Long): ByteArray =
    ByteBuffer.allocate(AAD_PREFIX.size + direction.wireLabel.length + 1 + 8)
        .put(AAD_PREFIX)
        .put(direction.wireLabel.toByteArray(StandardCharsets.US_ASCII))
        .put(0)
        .putLong(sequence)
        .array()

internal fun E2EECodec.encodeFrame(sequence: Long, ciphertext: ByteArray): ByteArray = when (this) {
    E2EECodec.BINARY -> ByteBuffer.allocate(BINARY_HEADER_BYTES + ciphertext.size)
        .put(E2EE_VERSION.toByte())
        .put(BINARY_KIND_DATA)
        .putLong(sequence)
        .put(ciphertext)
        .array()
    E2EECodec.JSON -> buildString {
        append("{\"type\":\"e2ee\",\"version\":").append(E2EE_VERSION)
        append(",\"sequence\":").append(sequence)
        append(",\"ciphertext\":").append(encodeJsonString(Base64Url.encode(ciphertext)))
        append('}')
    }.toByteArray(StandardCharsets.UTF_8)
}

internal fun E2EECodec.decodeFrame(rawFrame: ByteArray): Pair<Long, ByteArray> = when (this) {
    E2EECodec.BINARY -> {
        if (rawFrame.size < BINARY_HEADER_BYTES) {
            throw E2EEException.Format("invalid encrypted frame")
        }
        if (rawFrame[0] != E2EE_VERSION.toByte() || rawFrame[1] != BINARY_KIND_DATA) {
            throw E2EEException.Format("unsupported encrypted frame")
        }
        ByteBuffer.wrap(rawFrame, 2, 8).long to rawFrame.copyOfRange(BINARY_HEADER_BYTES, rawFrame.size)
    }
    E2EECodec.JSON -> decodeJsonFrame(rawFrame)
}

private fun decodeJsonFrame(rawFrame: ByteArray): Pair<Long, ByteArray> {
    val frame = parseJsonObject(rawFrame) ?: throw E2EEException.Format("invalid encrypted frame")
    // Field kind checks mirror Go's json.Unmarshal failure surface; they run
    // before the type/version check like unmarshal-then-validate does.
    val type = frame.stringField("type", "invalid encrypted frame") ?: ""
    val version = frame.longField("version", "invalid encrypted frame") ?: 0L
    val sequence = frame.ulongField("sequence", "invalid encrypted frame") ?: 0L
    val encoded = frame.stringField("ciphertext", "invalid encrypted frame") ?: ""
    if (type != "e2ee" || version != E2EE_VERSION.toLong()) {
        throw E2EEException.Format("unsupported encrypted frame")
    }
    val ciphertext = try {
        Base64Url.decode(encoded)
    } catch (e: IllegalArgumentException) {
        throw E2EEException.Format("invalid encrypted frame ciphertext", e)
    }
    return sequence to ciphertext
}

/**
 * An established `herdr-e2ee-v2` channel: two AES-256-GCM keys, two monotonic
 * sequence counters, one envelope codec. Sequences start at 0 per direction
 * and must arrive strictly in order; sealing past 2^53-1 fails.
 */
class E2EESession internal constructor(
    sendKey: ByteArray,
    receiveKey: ByteArray,
    private val sendDirection: E2EEDirection,
    private val receiveDirection: E2EEDirection,
    val codec: E2EECodec,
) {
    private val sendKey = sendKey.copyOf()
    private val receiveKey = receiveKey.copyOf()
    internal var sendSequence: Long = 0
    internal var receiveSequence: Long = 0

    internal fun sendKeyBytes(): ByteArray = sendKey.copyOf()
    internal fun receiveKeyBytes(): ByteArray = receiveKey.copyOf()

    companion object {
        /** Client-side session: seals `c2s`, opens `s2c`. */
        fun client(c2sKey: ByteArray, s2cKey: ByteArray, codec: E2EECodec): E2EESession =
            E2EESession(c2sKey, s2cKey, E2EEDirection.C2S, E2EEDirection.S2C, codec)

        /** Server-side session: seals `s2c`, opens `c2s` (test harnesses, relay mirrors). */
        fun server(c2sKey: ByteArray, s2cKey: ByteArray, codec: E2EECodec): E2EESession =
            E2EESession(s2cKey, c2sKey, E2EEDirection.S2C, E2EEDirection.C2S, codec)
    }

    /**
     * Encrypts [plaintext] under the send key at the current send sequence and
     * wraps it in the negotiated envelope. Throws [E2EEException.Sequence] once
     * the sequence space is exhausted.
     */
    fun seal(plaintext: ByteArray): ByteArray {
        if (sendSequence > MAX_SEQUENCE || sendSequence < 0) {
            throw E2EEException.Sequence("encrypted send sequence exhausted")
        }
        val sequence = sendSequence
        val ciphertext = AesGcm.seal(
            sendKey,
            frameNonce(sequence),
            frameAad(sendDirection, sequence),
            plaintext,
        )
        sendSequence++
        return codec.encodeFrame(sequence, ciphertext)
    }

    /**
     * Decodes one frame, checks its sequence against the receive counter, then
     * verifies and decrypts. Order matches the Go implementation: envelope
     * ([E2EEException.Format]) → sequence ([E2EEException.Sequence] /
     * [E2EEException.Replay]) → GCM tag ([E2EEException.Auth]).
     */
    fun open(rawFrame: ByteArray): ByteArray {
        val (sequence, ciphertext) = codec.decodeFrame(rawFrame)
        if (sequence < 0 || sequence > MAX_SEQUENCE || sequence > receiveSequence) {
            throw E2EEException.Sequence("invalid encrypted frame sequence")
        }
        if (sequence < receiveSequence) throw E2EEException.Replay(sequence, receiveSequence)
        val plaintext = try {
            AesGcm.open(
                receiveKey,
                frameNonce(sequence),
                frameAad(receiveDirection, sequence),
                ciphertext,
            )
        } catch (e: GeneralSecurityException) {
            throw E2EEException.Auth("encrypted frame authentication failed", e)
        }
        receiveSequence++
        return plaintext
    }
}
