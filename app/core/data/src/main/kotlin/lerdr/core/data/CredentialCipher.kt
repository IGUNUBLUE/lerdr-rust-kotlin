package lerdr.core.data

import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Seals and opens the serialized credential payload at rest. The production
 * implementation wraps an Android Keystore key ([AndroidKeystoreCipher]);
 * unit tests substitute an in-memory fake — the interface is pure
 * `ByteArray → ByteArray`, no Android types, so fakes stay JVM-only.
 */
interface CredentialCipher {
    /** plaintext → opaque sealed blob (nonce/tag included by the impl). */
    fun seal(plaintext: ByteArray): ByteArray

    /** sealed blob → plaintext; throws on tamper or key loss. */
    fun open(sealed: ByteArray): ByteArray
}

/**
 * AES-256/GCM seal under a non-exportable `AndroidKeyStore` key. Wire shape
 * on disk is `iv(12) ‖ ciphertext ‖ tag(16)` — one-shot per blob, random IV
 * per seal. The key never leaves the TEE/StrongBox when the hardware has
 * one; nothing secret is ever written unwrapped or logged.
 *
 * Device/emulator only — instantiate lazily from app code, never in JVM
 * tests (there is no `AndroidKeyStore` provider off-device).
 */
class AndroidKeystoreCipher(
    private val alias: String = DEFAULT_ALIAS,
) : CredentialCipher {

    private val keyStore = KeyStore.getInstance(KEYSTORE_PROVIDER).apply { load(null) }

    override fun seal(plaintext: ByteArray): ByteArray {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, key())
        return cipher.iv + cipher.doFinal(plaintext)
    }

    override fun open(sealed: ByteArray): ByteArray {
        require(sealed.size > GCM_IV_BYTES) { "sealed payload too short" }
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(
            Cipher.DECRYPT_MODE,
            key(),
            GCMParameterSpec(GCM_TAG_BITS, sealed, 0, GCM_IV_BYTES),
        )
        return cipher.doFinal(sealed, GCM_IV_BYTES, sealed.size - GCM_IV_BYTES)
    }

    private fun key(): SecretKey {
        (keyStore.getEntry(alias, null) as? KeyStore.SecretKeyEntry)?.let { return it.secretKey }
        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, KEYSTORE_PROVIDER)
        generator.init(
            KeyGenParameterSpec.Builder(
                alias,
                KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
            )
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build(),
        )
        return generator.generateKey()
    }

    private companion object {
        const val KEYSTORE_PROVIDER = "AndroidKeyStore"
        const val TRANSFORMATION = "AES/GCM/NoPadding"
        const val GCM_IV_BYTES = 12
        const val GCM_TAG_BITS = 128
        const val DEFAULT_ALIAS = "lerdr_device_credential_wrap"
    }
}
