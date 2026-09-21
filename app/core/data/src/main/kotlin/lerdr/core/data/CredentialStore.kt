package lerdr.core.data

import kotlinx.coroutines.flow.StateFlow

/**
 * One authentication record per relay — the Kotlin counterpart of the
 * oracle's `BrowserDeviceCredentialStore` (`device-auth.ts:81-189`), backed
 * by a Keystore-sealed file instead of plaintext web storage.
 *
 * Lifecycle parity with the oracle:
 *
 * - [saveInvitation] stores a pending pairing (device invitation or the
 *   bootstrap relay key as `id="bootstrap"`).
 * - [redeemInvitation] swaps invitation → issued credential in one
 *   durable write; a failed write leaves the invitation untouched.
 * - [updateCredential] refreshes the enrolled record after a credential
 *   `e2ee_server_finish`; the secret never changes outside redemption.
 * - [get] auto-evicts expired non-bootstrap invitations.
 */
interface CredentialStore {

    /** All live records keyed by relay id, for observation UIs. */
    val records: StateFlow<Map<String, RelayDeviceAuth>>

    /** Current auth for [relayId], or null (expired invitations self-evict). */
    suspend fun get(relayId: String): RelayDeviceAuth?

    /**
     * Persists a pairing invitation for [relayId], replacing whatever was
     * stored. An already-expired invitation is rejected.
     */
    suspend fun saveInvitation(relayId: String, invitation: RelayInvitation): RelayInvitation

    /**
     * `commitDeviceEnrollment` for the invitation path: the stored record
     * must be the exact invitation that was presented — the swap is atomic
     * so a crash can neither lose the issued credential nor re-burn the
     * invitation (relay-side `pending_credential_id` makes a retry
     * idempotent anyway).
     */
    suspend fun redeemInvitation(
        relayId: String,
        invitationId: String,
        enrollment: CredentialEnrollment,
    ): RelayDeviceCredential

    /**
     * `commitDeviceEnrollment` for the credential path: the stored record
     * must be a credential for the same device; the relay re-issuing a
     * secret here is a protocol violation and is rejected.
     */
    suspend fun updateCredential(
        relayId: String,
        enrollment: CredentialEnrollment,
    ): RelayDeviceCredential

    /** Drops the record for [relayId]. @return true when one existed. */
    suspend fun remove(relayId: String): Boolean

    /** Wipes every record — `reset_devices` parity. */
    suspend fun clear()
}
