package lerdr.core.data

import java.net.URI
import java.net.URISyntaxException
import java.net.URLDecoder

/**
 * Result of [SetupLink.parse] — the oracle's `SetupLinkImport` enum
 * (`setup-link.ts:3`) split so the UI can toast the right message.
 */
sealed interface SetupLinkResult {
    /** Blank input — nothing to import. */
    data object Empty : SetupLinkResult

    /** Not a usable link: unparseable, wrong scheme, or no fragment. */
    data object Malformed : SetupLinkResult

    /**
     * A link, but no usable setup payload: missing/out-of-range `setup`,
     * retired `gateway` params, bad `relay` origin, or a malformed
     * `invite` set. Maps to the oracle's `'no-invite'`.
     */
    data object NoSetup : SetupLinkResult

    data class Parsed(val payload: InvitePayload) : SetupLinkResult
}

/**
 * Parses `lerdr://pair#…` deep links and the oracle's `http(s)://…#…` QR
 * links into a strict [InvitePayload] (`config.ts` `quickSetupConfig` /
 * `quickSetupInvitation`, `safeSocketOrigin`).
 *
 * Strictness notes:
 * - `invite=` present means the invitation fields must ALL validate — the
 *   oracle silently degrades a malformed invitation to a bootstrap import;
 *   that would store a 43-char secret as a relay key, so here it is
 *   rejected instead.
 * - `gateway`/`gateways` links are rejected outright (retired transport).
 * - `relay=` is required for `lerdr://` links (no page origin to inherit)
 *   and may be `ws` or `wss` there — the mixed-content rule that gates
 *   `ws` behind an `http` page does not apply to a native deep link.
 */
object SetupLink {

    private val INVITATION_ID = Regex("^[A-Za-z0-9_-]{16,128}$")
    private val INVITATION_SECRET = Regex("^[A-Za-z0-9_-]{43}$")

    fun parse(text: String?): SetupLinkResult {
        if (text.isNullOrBlank()) return SetupLinkResult.Empty
        val uri = try {
            URI(text.trim())
        } catch (e: URISyntaxException) {
            return SetupLinkResult.Malformed
        }
        val scheme = uri.scheme?.lowercase()
        val source = when (scheme) {
            "http", "https" -> InvitePayload.Source.PAGE_LINK
            "lerdr" -> InvitePayload.Source.LERDR_LINK
            else -> return SetupLinkResult.Malformed
        }
        val fragment = uri.rawFragment?.takeIf { it.isNotEmpty() }
            ?: return SetupLinkResult.Malformed
        val params = FormParams(fragment)

        // `gateway`/`gateways=` belong to the retired transport — reject
        // rather than silently treating one as a direct relay link.
        if (params.has("gateway") || params.has("gateways")) return SetupLinkResult.NoSetup

        val setup = params["setup"] ?: return SetupLinkResult.NoSetup
        if (setup.length < MIN_SETUP_CHARS || setup.length > MAX_SETUP_CHARS) return SetupLinkResult.NoSetup

        val inviteId = params["invite"]
        val invitation = if (inviteId != null) {
            parseInvitation(params, inviteId, setup) ?: return SetupLinkResult.NoSetup
        } else {
            null
        }

        val label = (params["label"] ?: DEFAULT_LABEL).trim().take(MAX_LABEL_CHARS)
            .ifEmpty { DEFAULT_LABEL }

        val socketOrigin = resolveSocketOrigin(uri, scheme, params) ?: return SetupLinkResult.NoSetup

        return SetupLinkResult.Parsed(
            InvitePayload(
                label = label,
                socketOrigin = socketOrigin,
                setup = setup,
                invitation = invitation,
                source = source,
            ),
        )
    }

    /** Convenience: the payload or null. */
    fun parseOrNull(text: String?): InvitePayload? =
        (parse(text) as? SetupLinkResult.Parsed)?.payload

    private fun parseInvitation(
        params: FormParams,
        inviteId: String,
        setup: String,
    ): InvitePayload.Invitation? {
        if (!INVITATION_ID.matches(inviteId)) return null
        if (!INVITATION_SECRET.matches(setup)) return null
        val version = params["invite_version"]?.toLongOrNull() ?: return null
        val expires = params["invite_expires"]?.toLongOrNull() ?: return null
        if (version < 1 || expires < 1) return null
        return InvitePayload.Invitation(inviteId, version, setup, expires)
    }

    /**
     * `relay=` wins when present; page links fall back to the link's own
     * host (`wss` for https pages, `ws` for http). `lerdr://` has no page
     * origin — `relay=` is mandatory there.
     */
    private fun resolveSocketOrigin(
        uri: URI,
        scheme: String,
        params: FormParams,
    ): String? {
        val configured = params["relay"]
        if (configured != null) {
            val origin = parseSocketOrigin(configured) ?: return null
            val insecureAllowed = scheme == "http" || scheme == "lerdr"
            if (origin.scheme == RelayTransport.WEBSOCKET && !insecureAllowed) return null
            return normalizedOrigin(origin)
        }
        if (scheme == "lerdr") return null
        val host = uri.host ?: return null
        if (uri.userInfo != null) return null
        val socketScheme = if (scheme == "https") RelayTransport.WEBSOCKET_TLS else RelayTransport.WEBSOCKET
        val port = uri.port
        return buildString {
            append(socketScheme.scheme).append("://")
            append(if (':' in host) "[$host]" else host)
            if (port != -1 && port != socketScheme.defaultPort) append(':').append(port)
        }
    }

    private fun normalizedOrigin(origin: SocketOrigin): String = buildString {
        append(origin.scheme.scheme).append("://")
        append(if (':' in origin.host) "[${origin.host}]" else origin.host)
        if (origin.port != origin.scheme.defaultPort) append(':').append(origin.port)
    }

    private const val MAX_SETUP_CHARS = 512
    private const val MIN_SETUP_CHARS = 16
    private const val MAX_LABEL_CHARS = 48
    private const val DEFAULT_LABEL = "This computer"

    /**
     * `URLSearchParams` over the fragment: `&`-separated `name=value`,
     * first wins, `+`/`%XX` decode as form data.
     */
    private class FormParams(encoded: String) {
        private val entries: Map<String, List<String>>

        init {
            val map = linkedMapOf<String, MutableList<String>>()
            for (part in encoded.split('&')) {
                if (part.isEmpty()) continue
                val eq = part.indexOf('=')
                val name = decode(if (eq < 0) part else part.substring(0, eq))
                val value = decode(if (eq < 0) "" else part.substring(eq + 1))
                map.getOrPut(name) { mutableListOf() } += value
            }
            entries = map
        }

        operator fun get(name: String): String? = entries[name]?.firstOrNull()

        fun has(name: String): Boolean = entries.containsKey(name)

        private fun decode(value: String): String = try {
            URLDecoder.decode(value, Charsets.UTF_8)
        } catch (e: Exception) {
            value
        }
    }
}
