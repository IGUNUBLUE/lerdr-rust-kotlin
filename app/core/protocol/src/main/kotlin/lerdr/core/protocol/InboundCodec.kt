package lerdr.core.protocol

import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import lerdr.core.model.Inbound

/**
 * `protocol.DecodeMap` — decodes a raw client message into the flat
 * [Inbound] struct and applies the command-envelope normalization:
 * a non-empty `action` field overrides `type` when `type` is absent or
 * `"command"`. An empty final `type` is a decode error.
 */
object InboundCodec {

    fun decode(payload: String): Inbound =
        decode(LerdrJson.parseToJsonElement(payload).jsonObject)

    fun decode(raw: JsonObject): Inbound {
        var message = LerdrJson.decodeFromJsonElement(Inbound.serializer(), raw)
        // Go `raw["action"].(string)` — only string values count.
        val action = (raw["action"] as? kotlinx.serialization.json.JsonPrimitive)
            ?.takeIf { it.isString }?.content
        if (!action.isNullOrEmpty() && (message.type.isEmpty() || message.type == "command")) {
            message = message.copy(type = action)
        }
        if (message.type.isEmpty()) {
            throw SerializationException("message type is required")
        }
        return message
    }

    /** Canonical re-serialization of a decoded message (Go marshal order). */
    fun encode(message: Inbound): String =
        LerdrJson.encodeToString(Inbound.serializer(), message)
}
