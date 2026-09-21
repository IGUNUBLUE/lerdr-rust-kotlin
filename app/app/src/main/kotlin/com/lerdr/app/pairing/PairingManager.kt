package com.lerdr.app.pairing

import javax.inject.Inject
import javax.inject.Singleton
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.withTimeoutOrNull
import lerdr.core.data.CredentialStore
import lerdr.core.data.InvitePayload
import lerdr.core.data.RelayDeviceCredential
import lerdr.core.data.RelayRegistry
import com.lerdr.app.session.SessionRepository
import lerdr.core.transport.RelaySession

/** The end states a pairing attempt can land in, user-visible. */
sealed interface PairingOutcome {
    /** Enrolled — the issued credential is persisted and the session is live. */
    data class Success(val relayId: String) : PairingOutcome

    /** The link did not survive `SetupLink.parse`/payload validation. */
    data object InvalidLink : PairingOutcome

    /** The invitation's `expires_at` already passed. */
    data object InvitationExpired : PairingOutcome

    /** The relay rejected the proof (`AuthRejected` — bad secret/version). */
    data class Rejected(val reason: String) : PairingOutcome

    /** No terminal state within the window — relay unreachable or stalled. */
    data object TimedOut : PairingOutcome

    /** Persistence or transport plumbing failed before a verdict. */
    data class Failed(val message: String) : PairingOutcome
}

/**
 * `importSetupLink` parity: invitation → registry + pending credential →
 * connect → await the enrollment handshake.
 *
 * The transport needs no dedicated redemption call — presenting
 * `DeviceAuthentication.invitation` at dial runs the normal E2EE handshake,
 * and the relay's `e2ee_server_finish` (with `credential_secret`) reaches
 * [SessionRepository]'s `onEnrolled`, which redeems the stored invitation
 * atomically. Here we only judge the outcome.
 */
@Singleton
class PairingManager @Inject constructor(
    private val relayRegistry: RelayRegistry,
    private val credentialStore: CredentialStore,
    private val sessions: SessionRepository,
    private val now: () -> Long = System::currentTimeMillis,
) {
    suspend fun pair(payload: InvitePayload): PairingOutcome {
        val endpoint = payload.relayEndpoint()
            ?: return PairingOutcome.InvalidLink
        val invitation = try {
            payload.toPendingInvitation()
        } catch (invalid: IllegalArgumentException) {
            return PairingOutcome.InvalidLink
        }
        if (invitation.isExpired(now())) return PairingOutcome.InvitationExpired

        try {
            relayRegistry.upsert(endpoint)
        } catch (failure: Exception) {
            return PairingOutcome.Failed(failure.message ?: "Could not save the relay")
        }
        try {
            credentialStore.saveInvitation(endpoint.id, invitation)
        } catch (failure: IllegalArgumentException) {
            return PairingOutcome.InvitationExpired
        } catch (failure: Exception) {
            return PairingOutcome.Failed(failure.message ?: "Could not store the invitation")
        }

        sessions.connect(endpoint)
        val state = sessions.sessionState(endpoint.id)
            ?: return PairingOutcome.Failed("Session did not start")
        val verdict = withTimeoutOrNull(PAIRING_TIMEOUT_MS) {
            state.first {
                it is RelaySession.SessionState.Connected ||
                    it is RelaySession.SessionState.AuthRejected ||
                    it is RelaySession.SessionState.Closed
            }
        } ?: return PairingOutcome.TimedOut

        return when (verdict) {
            is RelaySession.SessionState.Connected -> {
                // onEnrolled has already run — success means the invitation
                // redeemed to a credential (or a credential path connected).
                if (credentialStore.get(endpoint.id) is RelayDeviceCredential) {
                    PairingOutcome.Success(endpoint.id)
                } else {
                    PairingOutcome.Failed("Enrollment was not persisted")
                }
            }
            is RelaySession.SessionState.AuthRejected ->
                PairingOutcome.Rejected(
                    verdict.reason.reason.ifEmpty { "The relay rejected this device" },
                )
            else -> PairingOutcome.Failed("Connection closed while pairing")
        }
    }

    companion object {
        /** Oracle budget for the whole redemption round-trip. */
        const val PAIRING_TIMEOUT_MS = 30_000L
    }
}
