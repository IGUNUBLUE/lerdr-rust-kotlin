package lerdr.core.data

/**
 * The structured contents of a pairing link — what `importSetupLink` feeds
 * the registry and credential store (`config.ts` `quickSetupConfig` +
 * `quickSetupInvitation`, `store.ts:646-675`).
 *
 * Two payload kinds share the [setup] field:
 * - **Bootstrap**: `setup` is the raw relay key text (`SetupFragment`,
 *   `setuphelper.go:14-22`); [invitation] is null.
 * - **Device invitation**: `setup` is the 43-char b64url invitation secret
 *   and [invitation] carries id/version/expiry (`store.ts:1830-1840`).
 */
data class InvitePayload(
    /** Relay display name (`label=`, ≤48 chars, default "This computer"). */
    val label: String,
    /** Dialable `ws(s)://` origin — the `relay=` param, or the page's own host. */
    val socketOrigin: String,
    /** Raw `setup=` value — bootstrap relay key or invitation secret. */
    val setup: String,
    /** Present on the invitation path only. */
    val invitation: Invitation?,
    /** Which link shape this came from. */
    val source: Source,
) {
    enum class Source {
        /** `http(s)://host/path#params` — the oracle's QR/clipboard link. */
        PAGE_LINK,

        /** `lerdr://pair#params` — the app's deep-link scheme. */
        LERDR_LINK,
    }

    /** `quickSetupInvitation` (`config.ts:208-224`). */
    data class Invitation(
        val id: String,
        val version: Long,
        /** 43-char b64url — always identical to [InvitePayload.setup]. */
        val secret: String,
        val expiresAtEpochMs: Long,
    )

    val isInvitation: Boolean get() = invitation != null

    /**
     * The registry entry this link creates. `paired` is set on the
     * invitation path (`importQuickSetup`: an invitation-paired entry keeps
     * no relay key).
     */
    fun relayEndpoint(): RelayEndpoint? =
        RelayEndpoint.fromSocketOrigin(socketOrigin, label, paired = isInvitation)

    /**
     * The pending auth record to hand [CredentialStore.saveInvitation]:
     * the device invitation, or the bootstrap relay key. Bootstrap keys are
     * raw strings whose UTF-8 bytes must be exactly 32 — the relay refuses
     * to run on anything else (`RELAY_KEY_BYTES`, `config.ts:105`).
     */
    fun toPendingInvitation(): RelayInvitation {
        val invite = invitation
        if (invite != null) {
            return RelayInvitation(invite.id, invite.version, invite.secret, invite.expiresAtEpochMs)
        }
        val keyBytes = setup.toByteArray(Charsets.UTF_8)
        require(keyBytes.size == SECRET_BYTES) { "Invalid relay key length." }
        return RelayInvitation(keyBytes)
    }
}
