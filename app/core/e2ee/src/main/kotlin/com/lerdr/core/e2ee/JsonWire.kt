package com.lerdr.core.e2ee

import java.nio.charset.CharacterCodingException
import java.nio.charset.StandardCharsets
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject

internal val E2EEJson = Json { ignoreUnknownKeys = true }

/**
 * Escapes a string exactly like Go's `encoding/json` (escapeHTML=true):
 * `"` `\` and control characters, plus `<` `>` `&` and U+2028/U+2029.
 * kotlinx.serialization does not escape the HTML set, so wire encoders go
 * through this to stay byte-identical for arbitrary `auth_id`/`locale` input.
 */
internal fun encodeJsonString(value: String): String = buildString {
    append('"')
    for (ch in value) {
        when (ch) {
            '"' -> append("\\\"")
            '\\' -> append("\\\\")
            '\n' -> append("\\n")
            '\r' -> append("\\r")
            '\t' -> append("\\t")
            '<' -> append("\\u003c")
            '>' -> append("\\u003e")
            '&' -> append("\\u0026")
            '\u2028' -> append("\\u2028")
            '\u2029' -> append("\\u2029")
            else -> if (ch < ' ') append("\\u%04x".format(ch.code)) else append(ch)
        }
    }
    append('"')
}

/** Parses [raw] as a JSON object, or null on any syntax/shape failure. */
internal fun parseJsonObject(raw: ByteArray): JsonObject? = try {
    E2EEJson.parseToJsonElement(String(raw, StandardCharsets.UTF_8)).jsonObject
} catch (e: IllegalArgumentException) {
    null
}

/** UTF-8 validation matching Go's `utf8.Valid` (the default decoder replaces). */
internal fun isValidUtf8(bytes: ByteArray): Boolean = try {
    Charsets.UTF_8.newDecoder().decode(java.nio.ByteBuffer.wrap(bytes))
    true
} catch (e: CharacterCodingException) {
    false
}

/**
 * Reads [key] as a JSON string, or null if absent. Throws [E2EEException.Format]
 * with [invalidMessage] when the field is present but not a string — the same
 * surface Go's `json.Unmarshal` produces for a type mismatch.
 */
internal fun JsonObject.stringField(key: String, invalidMessage: String): String? {
    val element = this[key] ?: return null
    if (element !is JsonPrimitive || !element.isString) throw E2EEException.Format(invalidMessage)
    return element.content
}

/**
 * Reads [key] as a non-negative u64 (returned as a signed [Long]; values above
 * `Long.MAX_VALUE` wrap negative, which callers treat as "above the sequence
 * ceiling"). Absent yields null; wrong kind or out-of-range is a format error.
 */
internal fun JsonObject.ulongField(key: String, invalidMessage: String): Long? {
    val element = this[key] ?: return null
    if (element !is JsonPrimitive || element.isString) throw E2EEException.Format(invalidMessage)
    return element.content.toULongOrNull()?.toLong()
        ?: throw E2EEException.Format(invalidMessage)
}

/** Reads [key] as a JSON number that must fit in a signed [Long]. */
internal fun JsonObject.longField(key: String, invalidMessage: String): Long? {
    val element = this[key] ?: return null
    if (element !is JsonPrimitive || element.isString) throw E2EEException.Format(invalidMessage)
    return element.content.toLongOrNull()
        ?: throw E2EEException.Format(invalidMessage)
}
