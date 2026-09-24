package lerdr.core.protocol

import com.github.luben.zstd.Zstd
import java.util.Base64
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

/**
 * Phase-5 §2.2 `frame_zstd` — negotiated zstd compression of `pane_content`
 * payloads (`relay/crates/lerdr-core/src/framezstd.rs`).
 *
 * A negotiated frame keeps its envelope in plaintext while `content` folds
 * into `{"encoding":"zstd","payload":"<standard-base64 zstd>"}`; the zstd
 * plaintext is the JSON object of compressed members — today
 * `{"content":"…"}`. Only `pane_content` compresses: `pane_delta` already
 * compresses well and `pane_resync` carries no payload member.
 */
object FrameZstd {

    const val CAPABILITY = "frame_zstd"
    const val ENCODING = "zstd"

    /**
     * Inflate bound — mirrors the relay's `MAX_INFLATED` (the outbound
     * message cap), so a `payload` inflating past it is malformed, not
     * merely large. Keeps decompression safe on untrusted input.
     */
    private const val MAX_INFLATED = 4 * 1024 * 1024

    /**
     * `PaneContent::decompress_payload` — restore `content` from `payload`
     * on an `encoding:"zstd"` frame, returning a plaintext-shaped copy with
     * both negotiated fields removed. Frames without `encoding` pass
     * through unchanged.
     *
     * @throws IllegalArgumentException on an unknown encoding, a missing or
     *   undecodable `payload`, or an inflated body that is not the payload
     *   object — the caller drops the frame and lets the ack/resync chain
     *   recover, matching the relay's error surfacing.
     */
    fun decompressPaneContentPayload(raw: JsonObject): JsonObject {
        val encoding = (raw["encoding"] as? JsonPrimitive)
            ?.takeIf { it.isString }?.content ?: return raw
        require(encoding == ENCODING) { "unknown pane frame encoding: $encoding" }
        val payload = (raw["payload"] as? JsonPrimitive)
            ?.takeIf { it.isString }?.content
            ?: throw IllegalArgumentException("pane frame declares zstd encoding without payload")
        val compressed = try {
            Base64.getDecoder().decode(payload)
        } catch (invalid: IllegalArgumentException) {
            throw IllegalArgumentException("pane frame payload is not valid base64", invalid)
        }
        val json = try {
            Zstd.decompress(compressed, MAX_INFLATED)
        } catch (failure: Exception) {
            throw IllegalArgumentException("pane frame payload does not inflate", failure)
        }
        val members = try {
            LerdrJson.parseToJsonElement(String(json, Charsets.UTF_8)).jsonObject
        } catch (invalid: Exception) {
            throw IllegalArgumentException("pane frame payload is not the payload object", invalid)
        }
        return buildJsonObject {
            raw.forEach { (key, value) ->
                if (key != "encoding" && key != "payload") put(key, value)
            }
            members["content"]?.let { put("content", it) }
        }
    }
}
