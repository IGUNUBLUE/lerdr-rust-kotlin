package com.lerdr.core.e2ee

import java.security.ProviderException
import java.util.Base64
import javax.crypto.AEADBadTagException
import javax.crypto.Cipher
import javax.crypto.Mac
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.SecretKeySpec

internal const val SHA256_BYTES = 32

internal object HmacSha256 {
    fun mac(key: ByteArray, vararg parts: ByteArray): ByteArray {
        val hmac = Mac.getInstance("HmacSHA256")
        hmac.init(SecretKeySpec(key, "HmacSHA256"))
        for (part in parts) hmac.update(part)
        return hmac.doFinal()
    }
}

/** RFC 5869 HKDF-SHA256, matching Go's crypto/hkdf. */
internal object HkdfSha256 {
    fun extract(salt: ByteArray, ikm: ByteArray): ByteArray =
        HmacSha256.mac(if (salt.isEmpty()) ByteArray(SHA256_BYTES) else salt, ikm)

    fun expand(prk: ByteArray, info: ByteArray, length: Int): ByteArray {
        require(length in 0..255 * SHA256_BYTES) { "invalid HKDF output length $length" }
        val out = ByteArray(length)
        var t = ByteArray(0)
        var offset = 0
        var counter = 1
        while (offset < length) {
            t = HmacSha256.mac(prk, t, info, byteArrayOf(counter.toByte()))
            val chunk = minOf(length - offset, SHA256_BYTES)
            t.copyInto(out, offset, 0, chunk)
            offset += chunk
            counter++
        }
        return out
    }

    fun derive(ikm: ByteArray, salt: ByteArray, info: String, length: Int): ByteArray =
        expand(extract(salt, ikm), info.toByteArray(Charsets.US_ASCII), length)
}

/**
 * AES-256-GCM with the Go `cipher.AEAD` wire shape: `Seal` output is
 * `ciphertext || 16-byte tag`, and `Open` consumes that same buffer.
 */
internal object AesGcm {
    const val TAG_BYTES = 16
    const val NONCE_BYTES = 12

    fun seal(key: ByteArray, nonce: ByteArray, aad: ByteArray, plaintext: ByteArray): ByteArray =
        cipher(Cipher.ENCRYPT_MODE, key, nonce, aad).doFinal(plaintext)

    /** Throws [AEADBadTagException] on tag mismatch or input shorter than the tag. */
    fun open(key: ByteArray, nonce: ByteArray, aad: ByteArray, ciphertext: ByteArray): ByteArray {
        val cipher = cipher(Cipher.DECRYPT_MODE, key, nonce, aad)
        return try {
            cipher.doFinal(ciphertext)
        } catch (e: ProviderException) {
            // SunJCE surfaces "input shorter than the GCM tag" as
            // ProviderException(ShortBufferException) instead of AEADBadTagException.
            throw AEADBadTagException(e.message).apply { initCause(e) }
        }
    }

    private fun cipher(mode: Int, key: ByteArray, nonce: ByteArray, aad: ByteArray): Cipher {
        require(key.size == 32) { "e2ee session key must be 32 bytes" }
        require(nonce.size == NONCE_BYTES) { "e2ee nonce must be $NONCE_BYTES bytes" }
        return Cipher.getInstance("AES/GCM/NoPadding").apply {
            init(mode, SecretKeySpec(key, "AES"), GCMParameterSpec(TAG_BYTES * 8, nonce))
            updateAAD(aad)
        }
    }
}

/**
 * base64 `RawURLEncoding` (no padding) — every binary field on the wire.
 * Decoding is strict: java.util's URL decoder tolerates `=` padding while
 * Go's RawURLEncoding rejects it, so the alphabet is checked first.
 */
internal object Base64Url {
    private val chars = ('A'..'Z') + ('a'..'z') + ('0'..'9') + '-' + '_'

    fun encode(bytes: ByteArray): String =
        Base64.getUrlEncoder().withoutPadding().encodeToString(bytes)

    fun decode(value: String): ByteArray {
        require(value.all { it in chars }) { "invalid base64url input" }
        return Base64.getUrlDecoder().decode(value)
    }
}
