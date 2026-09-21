package com.lerdr.core.e2ee

import java.math.BigInteger
import javax.crypto.KeyAgreement
import java.security.KeyFactory
import java.security.KeyPair
import java.security.KeyPairGenerator
import java.security.interfaces.ECPrivateKey
import java.security.interfaces.ECPublicKey
import java.security.spec.ECFieldFp
import java.security.spec.ECGenParameterSpec
import java.security.spec.ECParameterSpec
import java.security.spec.ECPoint
import java.security.spec.ECPrivateKeySpec
import java.security.spec.ECPublicKeySpec

/**
 * P-256 (secp256r1) helpers for the ephemeral ECDH exchange.
 *
 * Wire format is the uncompressed point `0x04 || X || Y` (65 bytes), matching
 * Go's `ecdh.PublicKey.Bytes()`. The shared secret is the 32-byte
 * x-coordinate; JCE may return it stripped of leading zeros (Conscrypt does),
 * so it is always left-padded back to the field size.
 */
internal object P256 {
    const val PUBLIC_KEY_BYTES = 65
    const val SHARED_SECRET_BYTES = 32

    private const val COORDINATE_BYTES = 32
    private const val UNCOMPRESSED_PREFIX: Byte = 0x04

    private val parameterSpec: ECParameterSpec by lazy {
        val parameters = java.security.AlgorithmParameters.getInstance("EC")
        parameters.init(ECGenParameterSpec("secp256r1"))
        parameters.getParameterSpec(ECParameterSpec::class.java)
    }

    private val keyFactory: KeyFactory get() = KeyFactory.getInstance("EC")

    fun generateKeyPair(): KeyPair =
        KeyPairGenerator.getInstance("EC").run {
            initialize(ECGenParameterSpec("secp256r1"))
            generateKeyPair()
        }

    /** Rebuilds a private key from its 32-byte big-endian scalar (fixtures use this). */
    fun privateKey(scalar: ByteArray): ECPrivateKey =
        keyFactory.generatePrivate(ECPrivateKeySpec(BigInteger(1, scalar), parameterSpec)) as ECPrivateKey

    /** Parses a 65-byte uncompressed point and verifies it lies on the curve. */
    fun publicKey(uncompressed: ByteArray): ECPublicKey {
        require(uncompressed.size == PUBLIC_KEY_BYTES && uncompressed[0] == UNCOMPRESSED_PREFIX) {
            "invalid uncompressed P-256 point"
        }
        val x = BigInteger(1, uncompressed.copyOfRange(1, 33))
        val y = BigInteger(1, uncompressed.copyOfRange(33, PUBLIC_KEY_BYTES))
        checkOnCurve(x, y)
        return keyFactory.generatePublic(ECPublicKeySpec(ECPoint(x, y), parameterSpec)) as ECPublicKey
    }

    /** Encodes a public key as `0x04 || X || Y`, each coordinate padded to 32 bytes. */
    fun encodePublic(key: ECPublicKey): ByteArray {
        val out = ByteArray(PUBLIC_KEY_BYTES)
        out[0] = UNCOMPRESSED_PREFIX
        copyPadded(key.w.affineX, out, 1)
        copyPadded(key.w.affineY, out, 33)
        return out
    }

    /** ECDH shared secret: the x-coordinate of the product, always 32 bytes. */
    fun ecdh(privateKey: ECPrivateKey, publicKey: ECPublicKey): ByteArray {
        val agreement = KeyAgreement.getInstance("ECDH")
        agreement.init(privateKey)
        agreement.doPhase(publicKey, true)
        return paddedToCoordinate(agreement.generateSecret())
    }

    private fun checkOnCurve(x: BigInteger, y: BigInteger) {
        val curve = parameterSpec.curve
        val p = (curve.field as ECFieldFp).p
        val lhs = y.modPow(BigInteger.TWO, p)
        val rhs = x.modPow(BigInteger.valueOf(3), p)
            .add(curve.a.multiply(x)).add(curve.b).mod(p)
        require(lhs == rhs) { "P-256 point is not on the curve" }
    }

    private fun paddedToCoordinate(bytes: ByteArray): ByteArray {
        val stripped = if (bytes.size > COORDINATE_BYTES && bytes[0] == 0.toByte()) {
            bytes.copyOfRange(1, bytes.size)
        } else {
            bytes
        }
        require(stripped.size <= COORDINATE_BYTES) { "ECDH secret larger than the P-256 field" }
        val out = ByteArray(COORDINATE_BYTES)
        stripped.copyInto(out, COORDINATE_BYTES - stripped.size)
        return out
    }

    private fun copyPadded(value: BigInteger, out: ByteArray, offset: Int) {
        val bytes = paddedToCoordinate(value.toByteArray())
        bytes.copyInto(out, offset)
    }
}
