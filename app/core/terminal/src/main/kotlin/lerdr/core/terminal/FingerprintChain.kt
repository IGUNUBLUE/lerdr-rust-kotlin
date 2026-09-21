package lerdr.core.terminal

import java.security.MessageDigest
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonPrimitive

/**
 * Rolling `content_fingerprint` validation — the per-pane integrity chain
 * the released client runs in its `pane_delta` / `pane_content` /
 * `pane_unchanged` handlers (`store.ts` pane frame dispatch).
 *
 * The fingerprint itself is `hex(sha256(content_utf8))[0:16]`, computed
 * server-side (`server.go` `paneFingerprint`). The oracle client does NOT
 * recompute it on the hot path — `docs/03-protocol.md` §5: "recompute
 * nothing — the server sends the new fingerprint; store it as next base."
 * What the chain validates is *continuity*: a `pane_delta` applies only
 * when its `base_fingerprint` equals the fingerprint of the last accepted
 * frame. A break means a delta was lost or the base diverged; the only
 * legal reaction is a forced `read_pane` resync — never silently diverge.
 *
 * All wire-field comparisons use JS `===` semantics: a JSON `null` or a
 * non-string primitive never equals a stored string, while an *absent*
 * `base_fingerprint` (`undefined`) equals an unset stored fingerprint.
 */
class FingerprintChain {

    /**
     * Fingerprint of the last accepted frame — `paneContentFingerprints`
     * entry. Null until the first `pane_content`/`pane_unchanged` supplies
     * one, or a `pane_delta` commits one verbatim.
     */
    var fingerprint: String? = null
        private set

    /**
     * `pane_content` / `pane_unchanged` adopt the wire fingerprint only
     * when it is a non-empty string (`typeof === 'string' && value`).
     */
    fun adopt(fingerprint: String?) {
        if (!fingerprint.isNullOrEmpty()) this.fingerprint = fingerprint
    }

    /**
     * The `pane_delta` path stores whatever string the wire carried —
     * `typeof === 'string'` accepts even `""`.
     */
    fun commitDelta(fingerprint: String) {
        this.fingerprint = fingerprint
    }

    /** Drops the chain head — disconnect/reset rebuilds from `pane_content`. */
    fun reset() {
        fingerprint = null
    }

    /**
     * The `pane_delta` pre-apply gate — ports the decision tree the JS
     * handler runs before touching the frame:
     *
     * ```
     * metadataOnly = stored === base_fingerprint
     *     && base_fingerprint === content_fingerprint
     *     && (segments === null || segments === [])
     * nextContent  = frame && stored === base_fingerprint
     *     ? (metadataOnly ? frame.content : applyPaneDelta(...))
     *     : null
     * nextContent === null || typeof content_fingerprint !== 'string'
     *     → forced read_pane
     * ```
     *
     * [hasFrame] is whether the caller holds a committed frame (the JS
     * `frame &&` guard). [baseFingerprint], [contentFingerprint] and
     * [segments] are raw wire values — Kotlin null means the key was absent
     * (JS `undefined`), [JsonNull] means an explicit `null`.
     */
    fun evaluateDelta(
        hasFrame: Boolean,
        baseFingerprint: JsonElement?,
        contentFingerprint: JsonElement?,
        segments: JsonElement?,
    ): DeltaVerdict {
        if (!hasFrame) return DeltaVerdict.ResyncRequired(ResyncReason.NO_BASE_FRAME)
        if (!storedEquals(baseFingerprint)) {
            return DeltaVerdict.ResyncRequired(ResyncReason.BASE_MISMATCH)
        }
        val fingerprint = (contentFingerprint as? JsonPrimitive)
            ?.takeIf { it.isString }
            ?.content
            ?: return DeltaVerdict.ResyncRequired(ResyncReason.MISSING_FINGERPRINT)
        // docs/specs/pane-delta.md §6.1 — "metadata changed, content didn't":
        // released relays encode it as segments:null, the Go relay emits the
        // boundary-table copy; both keep the existing content verbatim.
        if (jsEquals(baseFingerprint, contentFingerprint) && segmentsNullOrEmpty(segments)) {
            return DeltaVerdict.MetadataOnly(fingerprint)
        }
        return DeltaVerdict.Apply(fingerprint)
    }

    /** `stored === base_fingerprint` under JS strict equality. */
    private fun storedEquals(wire: JsonElement?): Boolean = when {
        // undefined === undefined: an absent base matches an unset chain.
        wire == null -> fingerprint == null
        // JSON null === nothing else (null !== undefined, null !== string).
        wire is JsonNull -> false
        wire is JsonPrimitive && wire.isString -> fingerprint == wire.content
        else -> false
    }

    /** `a === b` for two wire values (fingerprints are strings or absent). */
    private fun jsEquals(a: JsonElement?, b: JsonElement?): Boolean = when {
        a == null || b == null -> a == null && b == null
        a is JsonNull || b is JsonNull -> a is JsonNull && b is JsonNull
        a is JsonPrimitive && b is JsonPrimitive -> when {
            a.isString || b.isString -> a.isString && b.isString && a.content == b.content
            a.content == "true" || a.content == "false" ||
                b.content == "true" || b.content == "false" -> a.content == b.content
            else -> {
                val x = a.content.toDoubleOrNull()
                x != null && x == b.content.toDoubleOrNull()
            }
        }
        // Arrays/objects compare by identity in JS — fresh parses never equal.
        else -> false
    }

    private fun segmentsNullOrEmpty(segments: JsonElement?): Boolean =
        segments is JsonNull || (segments is JsonArray && segments.isEmpty())

    /** What the chain decided about an incoming `pane_delta`. */
    sealed interface DeltaVerdict {
        /** The fingerprint the delta claims for its post-apply frame. */
        val contentFingerprint: String?

        /** Chain intact — run [PaneDelta.applyStrict], then commit. */
        data class Apply(override val contentFingerprint: String) : DeltaVerdict

        /**
         * §6.1 fast path — keep the existing content verbatim, adopt the
         * fingerprint, still ack like any committed delta.
         */
        data class MetadataOnly(override val contentFingerprint: String) : DeltaVerdict

        /** Chain broken or unverifiable — nothing may be stored. */
        data class ResyncRequired(val reason: ResyncReason) : DeltaVerdict {
            override val contentFingerprint: String? get() = null
        }
    }

    companion object {
        /** `paneFingerprint`: first 8 bytes of sha256, lowercase hex. */
        fun fingerprint(content: String): String {
            val digest = MessageDigest.getInstance("SHA-256")
                .digest(content.encodeToByteArray())
            return digest.copyOf(8).toHexString()
        }

        /**
         * Local re-hash of committed content against the claimed
         * fingerprint — hardening beyond the oracle (which trusts the wire
         * value). A mismatch proves the post-apply bytes diverged from the
         * server's frame (corrupted delta or non-conforming relay); the
         * correct reaction is a resync, identical to a chain break.
         */
        fun verify(content: String, claimedFingerprint: String): Boolean =
            fingerprint(content) == claimedFingerprint
    }
}

/** Why a pane frame failed integrity and forced a resync. */
enum class ResyncReason {
    /** `pane_delta` before any committed frame — nothing to chain onto. */
    NO_BASE_FRAME,

    /** `base_fingerprint` ≠ stored fingerprint — a delta was lost. */
    BASE_MISMATCH,

    /** `content_fingerprint` absent or not a string. */
    MISSING_FINGERPRINT,

    /** Segment list rejected by the strict client apply (§6). */
    APPLY_REJECTED,

    /** Post-apply sha256 disagreed with the claimed fingerprint. */
    HASH_MISMATCH,

    /** Server sent `pane_resync` — its ack gate saw a stale/foreign ack. */
    SERVER_NUDGE,
}
