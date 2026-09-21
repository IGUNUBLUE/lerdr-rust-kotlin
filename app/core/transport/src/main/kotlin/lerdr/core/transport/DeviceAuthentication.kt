package lerdr.core.transport

import com.lerdr.core.e2ee.E2EEAuthKind
import com.lerdr.core.e2ee.E2EEAuthSelector
import com.lerdr.core.e2ee.E2EEClientHandshake
import com.lerdr.core.e2ee.E2EEServerFinish

/**
 * `RelayDeviceCredential | RelayInvitation` — the material the client hello
 * selector binds. [secret] is the 32-byte pairing credential or invitation
 * secret shared with the relay; it is copied on construction and on every
 * handshake so callers cannot mutate a live proof input.
 */
class DeviceAuthentication private constructor(
    val kind: E2EEAuthKind,
    val id: String,
    val version: Long,
    private val secret: ByteArray,
    val locale: String,
) {
    /** The selector carried by `e2ee_client_hello` and bound into both proofs. */
    val selector: E2EEAuthSelector get() = E2EEAuthSelector(kind, id, version, locale)

    /** A fresh handshake driver — new nonce and ephemeral per connection. */
    fun newHandshake(): E2EEClientHandshake = E2EEClientHandshake(selector, secret.copyOf())

    /**
     * The credential the relay issued during an invitation handshake, ready to
     * present on the next connection (`commitDeviceEnrollment` in the oracle).
     * Returns null when [finish] carries no `credential_secret`.
     */
    fun issuedCredential(finish: E2EEServerFinish): DeviceAuthentication? {
        val secret = finish.credentialSecret ?: return null
        return credential(finish.credentialId, finish.credentialVersion, secret, finish.locale)
    }

    companion object {
        /** `auth_kind: "credential"` — an enrolled device identity. */
        fun credential(
            id: String,
            version: Long,
            secret: ByteArray,
            locale: String = "en",
        ): DeviceAuthentication = DeviceAuthentication(E2EEAuthKind.CREDENTIAL, id, version, secret, locale)

        /**
         * `auth_kind: "invitation"` — a pairing offer. The bootstrap
         * invitation is `id = "bootstrap"`, `version = 1`, secret = the
         * relay's `HERDR_RELAY_TOKEN` bytes (32 ASCII bytes).
         */
        fun invitation(
            id: String,
            secret: ByteArray,
            version: Long = 1L,
            locale: String = "en",
        ): DeviceAuthentication = DeviceAuthentication(E2EEAuthKind.INVITATION, id, version, secret, locale)
    }
}
