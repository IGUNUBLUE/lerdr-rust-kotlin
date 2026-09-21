package com.lerdr.app.pairing

import lerdr.core.data.SetupLink
import lerdr.core.data.SetupLinkResult

/**
 * Analyzer → pairing boundary: decides which decoded QR strings count as
 * setup links and latches on the first one, so a QR that stays in frame
 * can never emit twice.
 *
 * Pure JVM on purpose — the camera analyzer feeds it raw strings and the
 * unit tests exercise it without a device. Validity is judged by
 * [SetupLink.parse], the same strict oracle parser the paste path uses,
 * so a foreign QR (random URL, Wi-Fi card, …) is ignored and scanning
 * continues instead of kicking off a bogus pairing attempt.
 */
internal class QrScanGate {

    /** True once a setup link has been accepted — analysis stops there. */
    var consumed = false
        private set

    /**
     * Returns [raw] the first time it parses to a usable setup payload —
     * a `lerdr://pair#…` deep link or the oracle's `http(s)://…#…` link.
     * Null for blank input, foreign QRs, invalid setup links, and every
     * call after the first accept.
     */
    fun offer(raw: String?): String? {
        if (consumed || raw.isNullOrBlank()) return null
        if (SetupLink.parse(raw) !is SetupLinkResult.Parsed) return null
        consumed = true
        return raw
    }
}
