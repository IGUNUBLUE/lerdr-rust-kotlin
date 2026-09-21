package lerdr.core.data

import java.util.Base64
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import lerdr.core.transport.DeviceAuthentication

/**
 * `DeviceRole` — the two stored roles (`store.go:33-36`). `bootstrap`
 * exists on the wire but is never a stored role.
 */
enum class DeviceRole(val wireName: String) {
    READER("reader"),
    CONTROLLER("controller");

    companion object {
        fun fromWireName(value: String): DeviceRole? = entries.firstOrNull { it.wireName == value }
    }
}

/**
 * The one authentication record held per relay — the Kotlin shape of the
 * oracle's `RelayInvitation | RelayDeviceCredential` union
 * (`frontend/src/lib/device-auth.ts:8-27`). Secrets are the 32-byte pairing
 * secret encoded as 43-char base64url-no-pad — the same encoding the Go
 * store and the wire handshake use (`pairing-store.md §A.1`).
 *
 * Every field is validated at construction; a malformed record can never
 * exist inside the store (oracle `validIdentifier`/`validSecret` parity).
 */
@Serializable
sealed interface RelayDeviceAuth {
    /** `auth_id` — credential_id or invitation_id. */
    val id: String

    /** `auth_version` — ≥1, exact-match against the relay's record. */
    val version: Long

    /** 32-byte secret, base64url-no-pad (43 chars). */
    val secret: String

    /** Decoded 32-byte secret for handshake use. */
    fun secretBytes(): ByteArray = Base64Url.decode(secret)

    /** The live handshake material for `:core:transport`'s RelayConnection. */
    fun toAuthentication(locale: String = "en"): DeviceAuthentication
}

/**
 * `RelayInvitation` — a pairing offer this device holds until redemption.
 *
 * [expiresAtEpochMs] is null only for the bootstrap invitation: the relay
 * re-arms it server-side (`rearmBootstrap`, `resolver.go:34-49`), so the
 * relay key never expires client-side.
 */
@Serializable
@SerialName("invitation")
data class RelayInvitation(
    override val id: String,
    override val version: Long,
    override val secret: String,
    @SerialName("expires_at") val expiresAtEpochMs: Long? = null,
) : RelayDeviceAuth {

    init {
        requireIdentifier(id, "invitation id")
        requireVersion(version)
        requireSecret(secret)
        expiresAtEpochMs?.let { requireTimestamp(it, "invitation expiry") }
    }

    /** Bootstrap invitation — `id = "bootstrap"`, version 1, never expires. */
    constructor(relayKey: ByteArray) : this(
        id = BOOTSTRAP_ID,
        version = BOOTSTRAP_VERSION,
        secret = Base64Url.encode(relayKey),
    )

    fun isExpired(now: Long): Boolean = expiresAtEpochMs?.let { it <= now } ?: false

    override fun toAuthentication(locale: String): DeviceAuthentication =
        DeviceAuthentication.invitation(id, secretBytes(), version, locale)

    companion object {
        const val BOOTSTRAP_ID = "bootstrap"
        const val BOOTSTRAP_VERSION = 1L
    }
}

/**
 * `RelayDeviceCredential` — the enrolled device identity a relay issued on
 * invitation redemption (`e2ee_server_finish`, `e2ee.go:330-337`).
 */
@Serializable
@SerialName("credential")
data class RelayDeviceCredential(
    /** `credential_id` — the `auth_id` presented on reconnect. */
    override val id: String,
    override val version: Long,
    override val secret: String,
    @SerialName("device_id") val deviceId: String,
    val role: DeviceRole,
    val locale: String,
    @SerialName("issued_at") val issuedAtEpochMs: Long,
    /** Invitation this credential was redeemed from (audit trail). */
    @SerialName("invitation_id") val invitationId: String? = null,
) : RelayDeviceAuth {

    init {
        requireIdentifier(id, "credential id")
        requireVersion(version)
        requireSecret(secret)
        requireIdentifier(deviceId, "device id")
        requireLocale(locale)
        requireTimestamp(issuedAtEpochMs, "credential issue time")
        invitationId?.let { requireIdentifier(it, "invitation id") }
    }

    override fun toAuthentication(locale: String): DeviceAuthentication =
        DeviceAuthentication.credential(id, version, secretBytes(), this.locale)
}

/**
 * `DeviceEnrollmentResult` — the identity fields of an authenticated
 * `e2ee_server_finish` (`e2ee.go:330-337`). [credentialSecret] is present
 * only on invitation redemption.
 */
data class CredentialEnrollment(
    val deviceId: String,
    val credentialId: String,
    val credentialVersion: Long,
    val credentialSecret: ByteArray?,
    val role: DeviceRole,
    val locale: String,
) {
    init {
        requireIdentifier(deviceId, "device id")
        requireIdentifier(credentialId, "credential id")
        requireVersion(credentialVersion)
        credentialSecret?.let { requireSecretBytes(it) }
        requireLocale(locale)
    }

    companion object
}

/** `DeviceEnrollmentResult` straight from a finished handshake. */
fun CredentialEnrollment.Companion.fromFinish(
    finish: com.lerdr.core.e2ee.E2EEServerFinish,
): CredentialEnrollment = CredentialEnrollment(
    deviceId = finish.deviceId,
    credentialId = finish.credentialId,
    credentialVersion = finish.credentialVersion,
    credentialSecret = finish.credentialSecret,
    role = DeviceRole.fromWireName(finish.role)
        ?: throw IllegalArgumentException("Invalid device role."),
    locale = finish.locale,
)

// ── validation (device-auth.ts:307-350 parity) ────────────────────────

private val CONTROL_CHARS = Regex("[\\u0000-\\u001f\\u007f]")
private val SECRET_PATTERN = Regex("^[A-Za-z0-9_-]{43}$")
private val LOCALE_PATTERN = Regex("^[A-Za-z]{2,3}(?:-[A-Za-z0-9]{2,8})*$")

internal fun requireIdentifier(value: String, label: String): String {
    val id = value.trim()
    require(id.isNotEmpty() && id.length <= 256 && !CONTROL_CHARS.containsMatchIn(id)) {
        "Invalid $label."
    }
    return id
}

internal fun requireVersion(value: Long): Long {
    require(value >= 1) { "Invalid credential version." }
    return value
}

internal fun requireSecret(value: String): String {
    require(SECRET_PATTERN.matches(value)) { "Invalid device authentication secret." }
    requireSecretBytes(Base64Url.decode(value))
    return value
}

internal fun requireSecretBytes(value: ByteArray): ByteArray {
    require(value.size == SECRET_BYTES) { "Invalid device authentication secret." }
    return value
}

internal fun requireLocale(value: String): String {
    require(value.length <= 35 && LOCALE_PATTERN.matches(value)) { "Invalid device locale." }
    return value
}

internal fun requireTimestamp(value: Long, label: String): Long {
    require(value >= 0) { "Invalid $label." }
    return value
}

internal const val SECRET_BYTES = 32

/** `base64.RawURLEncoding` — URL-safe, no padding (`store.go:233`). */
internal object Base64Url {
    private val ALPHABET = Regex("^[A-Za-z0-9_-]*$")

    fun encode(bytes: ByteArray): String =
        Base64.getUrlEncoder().withoutPadding().encodeToString(bytes)

    fun decode(value: String): ByteArray {
        require(ALPHABET.matches(value)) { "invalid base64url input" }
        return Base64.getUrlDecoder().decode(value)
    }
}
