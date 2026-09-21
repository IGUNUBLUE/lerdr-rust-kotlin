package com.lerdr.navigation

import androidx.navigation3.runtime.NavKey
import kotlinx.serialization.Serializable

/**
 * MVP navigation graph (docs/04-app-design.md §Navigation model).
 *
 * All keys are `@Serializable` so the back stack survives config changes and
 * process death via `rememberLerdrBackStack`. Top-level destinations
 * ([Home], [Activity], [Settings]) sit on the bottom bar; everything else
 * stacks on top with predictive back.
 */
@Serializable
sealed interface LerdrKey : NavKey {

    /** Pairing/onboarding — QR scan, pasted link, or deep-linked setup. */
    @Serializable
    data class Pairing(val setupLink: SetupLink? = null) : LerdrKey

    /** Mission control: needs-you rail, agents grouped by relay, relays strip. */
    @Serializable
    data object Home : LerdrKey

    /** Cross-agent journal (top-level tab). */
    @Serializable
    data object Activity : LerdrKey

    /** Semantic agent feed — default agent-session mode. */
    @Serializable
    data class AgentFeed(val paneId: String) : LerdrKey

    /** Full-fidelity interactive terminal for the same agent session. */
    @Serializable
    data class Terminal(val paneId: String) : LerdrKey

    /** Relays, devices, speech, app. */
    @Serializable
    data object Settings : LerdrKey

    companion object {
        /** Keys rendered in the bottom navigation bar. */
        val topLevel: Set<LerdrKey> = setOf(Home, Activity, Settings)
    }
}

/**
 * Parsed `lerdr://pair?…` / `<origin>/#…` setup link — the Android mirror of
 * the oracle's `SetupFragment`/`parseSetupLink` (docs/specs/pairing-store.md
 * §A.2). Two shapes exist: `setup` alone is the relay-token bootstrap;
 * `setup` + `invite` is a device invitation.
 */
@Serializable
data class SetupLink(
    /** Invitation secret or raw relay token (43-char b64url for invites). */
    val setup: String,
    /** Relay display name. */
    val label: String? = null,
    /** Direct `ws(s)://` relay origin. */
    val relay: String? = null,
    /** Invitation id — presence means this is an invitation link. */
    val invite: String? = null,
    /** Invitation record version (currently always 1). */
    val inviteVersion: Int? = null,
    /** Invitation expiry as unix ms. */
    val inviteExpires: Long? = null,
    /** `ws(s)://` gateway origins, when the relay is gateway-only. */
    val gateways: List<String> = emptyList(),
    /** Rendezvous identity — required with [gateways]. */
    val relayId: String? = null,
    /** Rendezvous secret — required with [gateways]. */
    val rendezvous: String? = null,
) {
    val isInvitation: Boolean get() = invite != null

    /** Human-readable target for the pairing confirmation card. */
    val displayName: String get() = label ?: relay ?: gateways.firstOrNull() ?: "relay"
}
