package com.lerdr.core.e2ee

import java.nio.charset.StandardCharsets
import java.security.KeyPair
import java.security.MessageDigest
import java.security.SecureRandom
import java.security.interfaces.ECPrivateKey
import java.security.interfaces.ECPublicKey

/** `auth_kind` values in the client hello selector. */
enum class E2EEAuthKind(val wireName: String) {
    CREDENTIAL("credential"),
    INVITATION("invitation");

    companion object {
        fun fromWireName(value: String): E2EEAuthKind? = entries.firstOrNull { it.wireName == value }
    }
}

/**
 * The credential/invitation selector carried in the client hello and bound
 * into the HMAC proof. [version] is a u64 on the wire; values above
 * `Long.MAX_VALUE` are not representable here.
 */
data class E2EEAuthSelector(
    val kind: E2EEAuthKind,
    val id: String,
    val version: Long,
    val locale: String,
) {
    init {
        require(id.isNotEmpty() && id.toByteArray(StandardCharsets.UTF_8).size <= MAX_AUTH_ID_BYTES) {
            "invalid client authentication selector: auth_id length"
        }
        require(version > 0) { "invalid client authentication selector: auth_version" }
        require(locale.toByteArray(StandardCharsets.UTF_8).size <= MAX_LOCALE_BYTES) {
            "invalid client authentication selector: locale length"
        }
    }
}

private const val MAX_AUTH_ID_BYTES = 128
private const val MAX_LOCALE_BYTES = 32

internal const val E2EE_SECRET_BYTES = 32
internal const val E2EE_NONCE_BYTES = 32
internal const val E2EE_PROOF_BYTES = 32

private val BINDING_PREFIX = "herdr-e2ee-v2 auth\u0000".toByteArray(StandardCharsets.US_ASCII)
private val CLIENT_PROOF_LABEL = "herdr-e2ee-v2 client\u0000".toByteArray(StandardCharsets.US_ASCII)
private val SERVER_PROOF_LABEL = "herdr-e2ee-v2 server\u0000".toByteArray(StandardCharsets.US_ASCII)
private val KEY_SALT_LABEL = "herdr-e2ee-v2 key\u0000".toByteArray(StandardCharsets.US_ASCII)

private const val CLIENT_FINISH_JSON = "{\"type\":\"e2ee_client_finish\",\"version\":2}"

private val SECURE_RANDOM = SecureRandom()

/** `"herdr-e2ee-v2 auth\x00" || kind || 0 || id || 0 || decimal(version) || 0`. */
internal fun authBinding(selector: E2EEAuthSelector): ByteArray {
    val kind = selector.kind.wireName.toByteArray(StandardCharsets.UTF_8)
    val id = selector.id.toByteArray(StandardCharsets.UTF_8)
    val version = selector.version.toString().toByteArray(StandardCharsets.US_ASCII)
    val binding = ByteArray(BINDING_PREFIX.size + kind.size + id.size + version.size + 3)
    var offset = BINDING_PREFIX.size
    BINDING_PREFIX.copyInto(binding)
    kind.copyInto(binding, offset); offset += kind.size + 1
    id.copyInto(binding, offset); offset += id.size + 1
    version.copyInto(binding, offset)
    return binding
}

/** `binding || clientNonce || clientPublic || serverNonce || serverPublic`. */
internal fun e2eeTranscript(
    binding: ByteArray,
    clientNonce: ByteArray,
    clientPublic: ByteArray,
    serverNonce: ByteArray,
    serverPublic: ByteArray,
): ByteArray = binding + clientNonce + clientPublic + serverNonce + serverPublic

private class ServerHelloFields(
    val nonce: ByteArray,
    val publicKeyBytes: ByteArray,
    val publicKey: ECPublicKey,
    val proof: ByteArray,
)

/** Derived material from a verified server hello; keys live on [session]. */
class E2EEHandshakeResult internal constructor(
    val session: E2EESession,
    internal val sharedSecret: ByteArray,
    internal val keySalt: ByteArray,
    internal val transcript: ByteArray,
    internal val serverNonce: ByteArray,
    internal val serverPublicKey: ByteArray,
    internal val serverProof: ByteArray,
)

/** Identity fields from the decrypted `e2ee_server_finish` payload. */
class E2EEServerFinish internal constructor(
    internal val plaintext: ByteArray,
    val deviceId: String,
    val credentialId: String,
    val role: String,
    val locale: String,
    val credentialVersion: Long,
    /** Newly issued credential secret; only present after an invitation handshake. */
    val credentialSecret: ByteArray?,
)

/**
 * Client side of the `herdr-e2ee-v2` handshake:
 *
 * ```
 * clientHello()          → send plaintext
 * acceptServerHello()    ← server hello (verifies proof, derives session)
 * clientFinish(session)  → first sealed frame, c2s seq 0
 * acceptServerFinish()   ← sealed server finish, s2c seq 0
 * ```
 *
 * [nonce] and [ephemeral] default to fresh randomness; fixtures inject
 * deterministic values. [secret] is the 32-byte pairing credential or
 * invitation secret shared with the relay.
 */
class E2EEClientHandshake(
    private val selector: E2EEAuthSelector,
    secret: ByteArray,
    nonce: ByteArray = ByteArray(E2EE_NONCE_BYTES).also(SECURE_RANDOM::nextBytes),
    private val ephemeral: KeyPair = P256.generateKeyPair(),
    private val codec: E2EECodec = E2EECodec.JSON,
) {
    private val secret = secret.copyOf()
    private val nonce = nonce.copyOf()
    private val clientPublicBytes = P256.encodePublic(ephemeral.public as ECPublicKey)
    private val binding = authBinding(selector)

    init {
        require(this.secret.size == E2EE_SECRET_BYTES) { "e2ee auth secret must be 32 bytes" }
        require(this.nonce.size == E2EE_NONCE_BYTES) { "e2ee client nonce must be 32 bytes" }
    }

    /** Step 1: the plaintext `e2ee_client_hello` JSON document. */
    fun clientHello(): ByteArray {
        val proof = HmacSha256.mac(secret, CLIENT_PROOF_LABEL, binding, nonce, clientPublicBytes)
        return buildString {
            append("{\"type\":\"e2ee_client_hello\",\"version\":").append(E2EE_VERSION)
            append(",\"auth_kind\":").append(encodeJsonString(selector.kind.wireName))
            append(",\"auth_id\":").append(encodeJsonString(selector.id))
            append(",\"auth_version\":").append(selector.version)
            append(",\"locale\":").append(encodeJsonString(selector.locale))
            append(",\"nonce\":").append(encodeJsonString(Base64Url.encode(nonce)))
            append(",\"public_key\":").append(encodeJsonString(Base64Url.encode(clientPublicBytes)))
            append(",\"proof\":").append(encodeJsonString(Base64Url.encode(proof)))
            append('}')
        }.toByteArray(StandardCharsets.UTF_8)
    }

    /**
     * Step 2: parses the server hello, checks the server proof against the
     * transcript, and derives the session keys.
     *
     * @throws E2EEException.Format on a malformed or unsupported hello
     * @throws E2EEException.Auth when the server proof does not authenticate
     */
    fun acceptServerHello(rawHello: ByteArray): E2EEHandshakeResult {
        val hello = parseServerHello(rawHello)
        val sharedSecret = P256.ecdh(ephemeral.private as ECPrivateKey, hello.publicKey)
        val transcript = e2eeTranscript(
            binding, nonce, clientPublicBytes, hello.nonce, hello.publicKeyBytes,
        )
        val expectedProof = HmacSha256.mac(secret, SERVER_PROOF_LABEL, transcript)
        if (!MessageDigest.isEqual(expectedProof, hello.proof)) {
            throw E2EEException.Auth("server proof did not authenticate")
        }
        val keySalt = HmacSha256.mac(secret, KEY_SALT_LABEL, transcript)
        val c2sKey = HkdfSha256.derive(sharedSecret, keySalt, "herdr-e2ee-v2 c2s", 32)
        val s2cKey = HkdfSha256.derive(sharedSecret, keySalt, "herdr-e2ee-v2 s2c", 32)
        return E2EEHandshakeResult(
            session = E2EESession.client(c2sKey, s2cKey, codec),
            sharedSecret = sharedSecret,
            keySalt = keySalt,
            transcript = transcript,
            serverNonce = hello.nonce,
            serverPublicKey = hello.publicKeyBytes,
            serverProof = hello.proof,
        )
    }

    /** Step 3: seals `{"type":"e2ee_client_finish","version":2}` at c2s seq 0. */
    fun clientFinish(session: E2EESession): ByteArray =
        session.seal(CLIENT_FINISH_JSON.toByteArray(StandardCharsets.UTF_8))

    /**
     * Step 4: opens the sealed `e2ee_server_finish` frame and parses the
     * authenticated identity (plus the issued credential secret for
     * invitation handshakes).
     */
    fun acceptServerFinish(session: E2EESession, rawFrame: ByteArray): E2EEServerFinish {
        val plaintext = session.open(rawFrame)
        return parseServerFinish(plaintext)
    }

    private fun parseServerHello(rawHello: ByteArray): ServerHelloFields {
        val hello = parseJsonObject(rawHello) ?: throw E2EEException.Format("invalid server hello")
        val type = hello.stringField("type", "invalid server hello") ?: ""
        val version = hello.longField("version", "invalid server hello") ?: 0L
        val encodedNonce = hello.stringField("nonce", "invalid server hello")
        val encodedPublicKey = hello.stringField("public_key", "invalid server hello")
        val encodedProof = hello.stringField("proof", "invalid server hello")
        if (type != "e2ee_server_hello" || version != E2EE_VERSION.toLong()) {
            throw E2EEException.Format("unsupported server hello")
        }
        val serverNonce =
            decodeFixedField(encodedNonce, E2EE_NONCE_BYTES, "invalid server nonce")
        val publicBytes = decodeFixedField(
            encodedPublicKey, P256.PUBLIC_KEY_BYTES, "invalid server public key",
        )
        val proof = decodeFixedField(encodedProof, E2EE_PROOF_BYTES, "invalid server proof")
        val publicKey = try {
            P256.publicKey(publicBytes)
        } catch (e: IllegalArgumentException) {
            throw E2EEException.Format("invalid server public key", e)
        }
        return ServerHelloFields(serverNonce, publicBytes, publicKey, proof)
    }

    private fun parseServerFinish(plaintext: ByteArray): E2EEServerFinish {
        if (!isValidUtf8(plaintext)) throw E2EEException.Format("invalid server finish")
        val finish = parseJsonObject(plaintext) ?: throw E2EEException.Format("invalid server finish")
        val type = finish.stringField("type", "invalid server finish") ?: ""
        val version = finish.longField("version", "invalid server finish") ?: 0L
        val deviceId = finish.stringField("device_id", "invalid server finish")
        val credentialId = finish.stringField("credential_id", "invalid server finish")
        val role = finish.stringField("role", "invalid server finish")
        val locale = finish.stringField("locale", "invalid server finish")
        val credentialVersion = finish.ulongField("credential_version", "invalid server finish")
        val encodedCredentialSecret =
            finish.stringField("credential_secret", "invalid server finish")
        if (type != "e2ee_server_finish" || version != E2EE_VERSION.toLong()) {
            throw E2EEException.Format("invalid server finish")
        }
        val credentialSecret = encodedCredentialSecret?.let { encoded ->
            try {
                Base64Url.decode(encoded)
            } catch (e: IllegalArgumentException) {
                throw E2EEException.Format("invalid server finish", e)
            }
        }
        return E2EEServerFinish(
            plaintext = plaintext,
            deviceId = deviceId ?: "",
            credentialId = credentialId ?: "",
            role = role ?: "",
            locale = locale ?: "",
            credentialVersion = credentialVersion ?: 0L,
            credentialSecret = credentialSecret,
        )
    }

    private fun decodeFixedField(
        encoded: String?,
        size: Int,
        errorMessage: String,
    ): ByteArray {
        val decoded = try {
            Base64Url.decode(encoded ?: "")
        } catch (e: IllegalArgumentException) {
            throw E2EEException.Format(errorMessage, e)
        }
        if (decoded.size != size) throw E2EEException.Format(errorMessage)
        return decoded
    }
}
