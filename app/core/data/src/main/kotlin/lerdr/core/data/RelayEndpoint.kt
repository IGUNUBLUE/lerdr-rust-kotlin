package lerdr.core.data

import java.net.URI
import java.net.URISyntaxException
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/**
 * How a relay endpoint is dialed. The oracle's `TransportKind` is a single
 * `'websocket'` (`frontend/src/lib/transports/types.ts:4`) — the ws/wss
 * distinction lives in the URL scheme, so it is modeled here as the kind.
 */
@Serializable
enum class RelayTransport(val scheme: String, val defaultPort: Int) {
    @SerialName("ws") WEBSOCKET("ws", 80),
    @SerialName("wss") WEBSOCKET_TLS("wss", 443),
}

/**
 * One persisted relay entry — the non-secret slice of the oracle's
 * `RelayConfig` (`frontend/src/lib/types.ts:80-95`). The relay key and
 * pairing credentials are NOT here: secrets live in [CredentialStore] so a
 * DataStore read never exposes them.
 *
 * [paired] mirrors the oracle's `paired?: true` flag: the entry came from an
 * encrypted pairing link and keeps no relay key, so a missing credential
 * means "re-pair", not "dial plaintext".
 */
@Serializable
data class RelayEndpoint(
    val id: String,
    val label: String,
    val host: String,
    val port: Int,
    val transport: RelayTransport = RelayTransport.WEBSOCKET_TLS,
    val paired: Boolean = false,
) {
    init {
        require(id.isNotBlank()) { "relay id must not be blank" }
        require(host.isNotBlank()) { "relay host must not be blank" }
        require(port in 1..65535) { "relay port out of range: $port" }
    }

    /** `ws(s)://host[:port]` — default ports elided, matching URL.origin. */
    val socketOrigin: String
        get() = buildString {
            append(transport.scheme).append("://")
            append(if (':' in host) "[$host]" else host)
            if (port != transport.defaultPort) append(':').append(port)
        }

    companion object {
        /**
         * Parses a bare `ws://`/`wss://` origin (the oracle's
         * `safeSocketOrigin` contract: no credentials, path, query, or
         * fragment). Returns null when the value is not a socket origin.
         */
        fun fromSocketOrigin(origin: String, label: String, paired: Boolean = false): RelayEndpoint? {
            val parsed = parseSocketOrigin(origin) ?: return null
            val resolvedLabel = label.trim().ifEmpty { relayLabelFromUrl(origin) }
            val normalizedOrigin = buildString {
                append(parsed.scheme.scheme).append("://")
                append(if (':' in parsed.host) "[${parsed.host}]" else parsed.host)
                if (parsed.port != parsed.scheme.defaultPort) append(':').append(parsed.port)
            }
            return RelayEndpoint(
                id = makeRelayId(resolvedLabel, normalizedOrigin),
                label = resolvedLabel,
                host = parsed.host,
                port = parsed.port,
                transport = parsed.scheme,
                paired = paired,
            )
        }
    }
}

internal class SocketOrigin(val scheme: RelayTransport, val host: String, val port: Int)

/**
 * `safeSocketOrigin` (`config.ts:129-146`): bare ws/wss authority only —
 * no userinfo, no path beyond `/`, no query, no fragment. `java.net.URI`
 * under-parses exotic-but-legal hostnames (underscores, IPv6), so the
 * authority is split by hand when `URI.host` gives up.
 */
internal fun parseSocketOrigin(value: String): SocketOrigin? {
    val uri = try {
        URI(value.trim())
    } catch (e: URISyntaxException) {
        return null
    }
    val scheme = when (uri.scheme) {
        "ws" -> RelayTransport.WEBSOCKET
        "wss" -> RelayTransport.WEBSOCKET_TLS
        else -> return null
    }
    if (uri.userInfo != null) return null
    if (!uri.rawPath.isNullOrEmpty() && uri.rawPath != "/") return null
    if (uri.rawQuery != null || uri.rawFragment != null) return null
    val authority = uri.rawAuthority ?: return null
    if ('@' in authority) return null
    val host: String
    val port: Int
    if (authority.startsWith("[")) {
        val close = authority.indexOf(']')
        if (close < 0) return null
        host = authority.substring(1, close)
        val rest = authority.substring(close + 1)
        if (rest.isEmpty()) {
            port = -1
        } else if (rest.startsWith(":")) {
            port = rest.substring(1).toIntOrNull() ?: return null
        } else {
            return null
        }
    } else {
        val lastColon = authority.lastIndexOf(':')
        if (lastColon < 0) {
            host = authority
            port = -1
        } else {
            host = authority.substring(0, lastColon)
            port = authority.substring(lastColon + 1).toIntOrNull() ?: return null
        }
    }
    if (host.isEmpty() || host.any { it.isWhitespace() }) return null
    val resolvedPort = if (port == -1) scheme.defaultPort else port
    if (resolvedPort !in 1..65535) return null
    return SocketOrigin(scheme, host, resolvedPort)
}

/** `relayLabelFromUrl` — first hostname label, else `relay`. */
fun relayLabelFromUrl(url: String): String =
    parseSocketOrigin(url)?.host?.substringBefore('.')?.ifEmpty { null }
        ?: try {
            URI(url).host?.substringBefore('.')?.ifEmpty { null }
        } catch (e: URISyntaxException) {
            null
        } ?: "relay"

/**
 * `makeRelayId` — stable slug of `label-url`, so an invitation re-import for
 * the same relay lands on the same id and the stored credential follows it
 * (`config.ts:115-122`).
 */
fun makeRelayId(label: String, url: String): String {
    val base = "${label.ifEmpty { relayLabelFromUrl(url) }}-$url"
    return base.lowercase()
        .replace(Regex("^wss?://"), "")
        .replace(Regex("[^a-z0-9]+"), "-")
        .trim('-')
        .take(72)
        .ifEmpty { "relay" }
}
