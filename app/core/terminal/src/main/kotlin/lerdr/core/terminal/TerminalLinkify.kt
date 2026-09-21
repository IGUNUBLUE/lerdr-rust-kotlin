package lerdr.core.terminal

/**
 * `linkifyTerminalText` / `splitTerminalUrl` / `safeTerminalUrl` ported to a
 * chunk emitter. The URL normalization mirrors `new URL(value).href` for the
 * special schemes (http/https): host lowercasing, default-port removal,
 * backslash→slash path folding, dot-segment resolution, and the WHATWG
 * percent-encode sets for path/query/fragment.
 */
internal object TerminalLinkify {

    // JS /https?:\/\/[^\s<>"']+/giu — \s is Unicode-aware in JS.
    private val URL_PATTERN = Regex(
        "https?://[^\\s\\u00A0\\u1680\\u2000-\\u200A\\u2028\\u2029\\u202F\\u205F\\u3000\\uFEFF<>\"']+",
        RegexOption.IGNORE_CASE,
    )
    private val TRAILING_PUNCTUATION = setOf('.', ',', ';', ':', '!', '?', ']', ')')

    sealed class Chunk {
        abstract val text: String

        data class Plain(override val text: String) : Chunk()
        data class Link(override val text: String, val href: String) : Chunk()
    }

    fun linkify(value: String): List<Chunk> {
        val chunks = ArrayList<Chunk>()
        var plain = StringBuilder()
        var cursor = 0
        for (match in URL_PATTERN.findAll(value)) {
            plain.append(value.substring(cursor, match.range.first))
            val (candidate, suffix) = splitTerminalUrl(match.value)
            val href = safeTerminalUrl(candidate)
            if (href != null) {
                if (plain.isNotEmpty()) {
                    chunks.add(Chunk.Plain(plain.toString()))
                    plain = StringBuilder()
                }
                chunks.add(Chunk.Link(candidate, href))
                plain.append(suffix)
            } else {
                plain.append(match.value)
            }
            cursor = match.range.last + 1
        }
        plain.append(value.substring(cursor))
        if (plain.isNotEmpty()) chunks.add(Chunk.Plain(plain.toString()))
        return chunks
    }

    /** Strips trailing sentence punctuation unless brackets stay balanced. */
    internal fun splitTerminalUrl(value: String): Pair<String, String> {
        var candidate = value
        var suffix = ""
        while (candidate.isNotEmpty() && candidate.last() in TRAILING_PUNCTUATION) {
            val character = candidate.last()
            if (character == ')' && candidate.count { it == '(' } >= candidate.count { it == ')' }) break
            if (character == ']' && candidate.count { it == '[' } >= candidate.count { it == ']' }) break
            suffix = character + suffix
            candidate = candidate.dropLast(1)
        }
        return candidate to suffix
    }

    /**
     * `new URL(value).href` restricted to http/https. Returns null where the
     * constructor would throw (missing host, bad port, missing `//`).
     */
    fun safeTerminalUrl(value: String): String? {
        val schemeEnd = value.indexOf(':')
        if (schemeEnd <= 0) return null
        val scheme = value.substring(0, schemeEnd).lowercase()
        if (scheme != "http" && scheme != "https") return null
        var rest = value.substring(schemeEnd + 1)
        // Special schemes accept / and \ interchangeably after the scheme.
        if (!(rest.startsWith("//") || rest.startsWith("\\\\") ||
                rest.startsWith("/\\") || rest.startsWith("\\/"))
        ) {
            return null
        }
        rest = rest.substring(2)

        val authorityEnd = rest.indexOfFirst { it == '/' || it == '\\' || it == '?' || it == '#' }
            .let { if (it < 0) rest.length else it }
        val authority = rest.substring(0, authorityEnd)
        var remainder = rest.substring(authorityEnd)

        val host = serializeAuthority(authority, scheme) ?: return null

        val pathEnd = remainder.indexOfFirst { it == '?' || it == '#' }
            .let { if (it < 0) remainder.length else it }
        val rawPath = remainder.substring(0, pathEnd).replace('\\', '/')
        remainder = remainder.substring(pathEnd)
        val query = if (remainder.startsWith('?')) remainder.substring(1).substringBefore('#') else null
        val fragment = if ('#' in remainder) remainder.substringAfter('#') else null

        val path = normalizePath(rawPath)
        return buildString {
            append(scheme).append("://").append(host)
            append(percentEncode(path.ifEmpty { "/" }, PATH_ENCODE_SET))
            if (query != null) append('?').append(percentEncode(query, SPECIAL_QUERY_ENCODE_SET))
            if (fragment != null) append('#').append(percentEncode(fragment, FRAGMENT_ENCODE_SET))
        }
    }

    private const val INVALID = -2
    private const val ABSENT = -1

    private fun serializeAuthority(authority: String, scheme: String): String? {
        var rest = authority
        var userinfo = ""
        val at = rest.lastIndexOf('@')
        if (at >= 0) {
            userinfo = rest.substring(0, at + 1)
            rest = rest.substring(at + 1)
        }
        val host: String
        var tail = ""
        if (rest.startsWith('[')) {
            val close = rest.indexOf(']')
            if (close < 0) return null
            host = rest.substring(0, close + 1)
            tail = rest.substring(close + 1)
        } else {
            val colon = rest.lastIndexOf(':')
            if (colon >= 0) {
                host = rest.substring(0, colon)
                tail = rest.substring(colon)
            } else {
                host = rest
            }
        }
        if (host.isEmpty()) return null
        if (host.any { it <= ' ' || it == 0x7F.toChar() || it in "\"#%/:<>?@[\\]^|" }) return null

        val port = when {
            tail.isEmpty() -> ABSENT
            tail == ":" -> ABSENT // trailing colon = empty port, dropped
            !tail.startsWith(':') -> INVALID
            else -> {
                val digits = tail.substring(1)
                val value = if (digits.all { it.isDigit() }) digits.toLongOrNull() else null
                if (value == null || value > 65535) INVALID else value.toInt()
            }
        }
        if (port == INVALID) return null
        val defaultPort = if (scheme == "http") 80 else 443

        return buildString {
            append(userinfo)
            append(if (host.startsWith('[')) host else host.lowercase())
            if (port != ABSENT && port != defaultPort) append(':').append(port)
        }
    }

    /** WHATWG special-scheme path: `\` folded to `/` earlier, then dot-segment resolution. */
    private fun normalizePath(path: String): String {
        val segments = path.split('/')
        val output = ArrayList<String>()
        segments.forEachIndexed { index, segment ->
            val isLast = index == segments.size - 1
            val decoded = segment.lowercase().replace("%2e", ".")
            when {
                decoded == ".." -> {
                    if (output.isNotEmpty() && output.last() != "") output.removeAt(output.size - 1)
                    if (isLast && (output.isEmpty() || output.last() != "")) output.add("")
                }
                decoded == "." -> if (isLast) output.add("")
                else -> output.add(segment)
            }
        }
        return output.joinToString("/")
    }

    // Chars outside the WHATWG percent-encode sets stay literal (e.g. ] ; : @ ,).
    private const val PATH_ENCODE_SET = " \"<>^`{}"
    private const val SPECIAL_QUERY_ENCODE_SET = " \"#<>'"
    private const val FRAGMENT_ENCODE_SET = " \"<>`"

    private fun percentEncode(value: String, extra: String): String {
        val out = StringBuilder()
        for (byte in value.encodeToByteArray()) {
            val b = byte.toInt() and 0xFF
            val c = b.toChar()
            if (b <= 0x20 || b == 0x7F || b > 0x7E || c in extra) {
                out.append('%').append("%02X".format(b))
            } else {
                out.append(c)
            }
        }
        return out.toString()
    }
}
