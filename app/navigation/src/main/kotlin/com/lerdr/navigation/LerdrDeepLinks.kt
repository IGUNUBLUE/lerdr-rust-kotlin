package com.lerdr.navigation

import java.net.URI
import java.net.URLDecoder

/**
 * Deep-link intake for `lerdr://` URIs.
 *
 * nav3 1.1.x has no built-in URI matcher (the `deeplink` package arrives in
 * a later release), so matching is an explicit parse — which also lets the
 * same code validate pasted setup links on the Pairing screen.
 *
 * Recognized shapes:
 * - `lerdr://pair?<query>` and `lerdr://pair#<fragment>` — the Android form
 *   of the setup link (`<appOrigin>/#<query>` in the oracle; see
 *   docs/specs/pairing-store.md §A.2).
 * - `<scheme>://<anything>[?|#]…setup=…` — a pasted/web setup link. Any link
 *   carrying a `setup` param routes to Pairing.
 */
object LerdrDeepLinks {

    const val SCHEME = "lerdr"
    const val HOST_PAIR = "pair"

    /**
     * Resolve a URI string to its destination key, or null when the link is
     * not ours. `lerdr://pair` with no params still opens Pairing (empty
     * form); links with a `setup` param carry it pre-parsed.
     */
    fun match(uriString: String): LerdrKey? {
        val uri = parseUri(uriString) ?: return null
        if (uri.scheme == SCHEME) {
            return when (uri.host) {
                HOST_PAIR -> LerdrKey.Pairing(setupLinkFromUri(uri))
                else -> null
            }
        }
        return setupLinkFromUri(uri)?.let { link -> LerdrKey.Pairing(link) }
    }

    /** Parse a full setup link — `lerdr://pair?…`, `https://host/#…`, etc. */
    fun parseSetupLink(uriString: String): SetupLink? =
        parseUri(uriString)?.let(::setupLinkFromUri)

    private fun parseUri(uriString: String): URI? = try {
        URI(uriString.trim())
    } catch (_: Exception) {
        null
    }?.takeIf { it.scheme != null }

    /** Read setup params from query or fragment (`url.Values`-encoded). */
    private fun setupLinkFromUri(uri: URI): SetupLink? {
        val params = decodeParams(uri.rawQuery) + decodeParams(uri.rawFragment)
        val setup = params["setup"] ?: return null
        return SetupLink(
            setup = setup,
            label = params["label"],
            relay = params["relay"],
            invite = params["invite"],
            inviteVersion = params["invite_version"]?.toIntOrNull(),
            inviteExpires = params["invite_expires"]?.toLongOrNull(),
            gateways = params["gateways"]
                ?.split(',')
                ?.mapNotNull { it.trim().takeIf(String::isNotEmpty) }
                .orEmpty(),
            relayId = params["relay_id"],
            rendezvous = params["rendezvous"],
        )
    }

    private fun decodeParams(raw: String?): Map<String, String> {
        if (raw.isNullOrEmpty()) return emptyMap()
        val out = LinkedHashMap<String, String>()
        for (pair in raw.split('&')) {
            if (pair.isEmpty()) continue
            val idx = pair.indexOf('=')
            val key = if (idx >= 0) pair.substring(0, idx) else pair
            val value = if (idx >= 0) pair.substring(idx + 1) else ""
            out[urlDecode(key)] = urlDecode(value)
        }
        return out
    }

    private fun urlDecode(value: String): String = try {
        URLDecoder.decode(value, Charsets.UTF_8)
    } catch (_: Exception) {
        value
    }
}
