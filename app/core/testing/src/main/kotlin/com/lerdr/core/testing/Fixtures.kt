package com.lerdr.core.testing

import java.nio.file.Path
import java.util.Base64
import kotlin.io.path.exists
import kotlin.io.path.isDirectory
import kotlin.io.path.readText
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.long

/**
 * Locates and loads the repo-level golden vector suites under `fixtures/`.
 *
 * Suite files are canonical JSON envelopes (`format_version`, `suite`,
 * `source`, `vectors`); see `fixtures/README.md` for the contract.
 */
object Fixtures {
    private val json = Json { ignoreUnknownKeys = true }

    /**
     * Resolves the fixtures directory: `$FIXTURES_DIR` when set, otherwise the
     * nearest `fixtures/` directory found by walking up from the working
     * directory (covers runs from `app/`, a module dir, or the repo root).
     */
    fun dir(): Path {
        System.getenv("FIXTURES_DIR")?.takeIf { it.isNotBlank() }?.let { configured ->
            val path = Path.of(configured)
            require(path.isDirectory()) { "FIXTURES_DIR is not a directory: $path" }
            return path
        }
        var dir = Path.of("").toAbsolutePath()
        while (true) {
            val candidate = dir.resolve("fixtures")
            if (candidate.isDirectory()) return candidate
            dir = dir.parent ?: break
        }
        error("fixtures directory not found; set FIXTURES_DIR")
    }

    /**
     * Loads a suite by repo-relative path,
     * e.g. `load("crypto/crypto.frames.json.json")`.
     */
    fun load(relativePath: String): FixtureSuite {
        val path = dir().resolve(relativePath)
        require(path.exists()) { "fixture file not found: $path" }
        return FixtureSuite(path, json.parseToJsonElement(path.readText()).jsonObject)
    }

    /**
     * Loads a suite by its canonical name, e.g. `named("crypto.frames.json")`
     * resolves `crypto/crypto.frames.json.json`.
     */
    fun named(suiteName: String): FixtureSuite {
        val area = suiteName.substringBefore('.')
        return load("$area/$suiteName.json")
    }

    /** Parses a JSON literal embedded in a fixture field or a test. */
    fun parse(text: String): JsonElement = json.parseToJsonElement(text)
}

/** A parsed vector suite. Asserts the non-empty-vectors CI rule on load. */
class FixtureSuite(val path: Path, val root: JsonObject) {
    val formatVersion: Int get() = root.int("format_version")
    val name: String get() = root.string("suite")
    val source: JsonObject get() = root.obj("source")
    val vectors: List<JsonObject> get() = root.array("vectors").map(JsonElement::jsonObject)

    init {
        require(vectors.isNotEmpty()) { "suite '$name' in $path has zero vectors" }
    }

    /** Returns the vector named [name] or fails the test. */
    fun vector(name: String): JsonObject =
        vectors.firstOrNull { it.string("name") == name }
            ?: error("vector '$name' not found in $path")
}

private fun JsonObject.member(key: String): JsonElement =
    this[key] ?: throw IllegalArgumentException("missing field '$key'")

/** Required string field. */
fun JsonObject.string(key: String): String {
    val element = member(key)
    require(element is JsonPrimitive && element.isString) { "field '$key' is not a string" }
    return element.content
}

/** Optional string field; absent or JSON null yields [default]. */
fun JsonObject.stringOrNull(key: String, default: String? = null): String? {
    val element = this[key] ?: return default
    require(element is JsonPrimitive && element.isString) { "field '$key' is not a string" }
    return element.content
}

/** Required integer field (u64 range fits in [Long] for every suite). */
fun JsonObject.long(key: String): Long = member(key).jsonPrimitive.long

fun JsonObject.int(key: String): Int = member(key).jsonPrimitive.int

fun JsonObject.bool(key: String): Boolean = member(key).jsonPrimitive.boolean

fun JsonObject.obj(key: String): JsonObject = member(key).jsonObject

fun JsonObject.array(key: String): JsonArray = member(key).jsonArray

/** Required field decoded as base64 RawURLEncoding (no padding). */
fun JsonObject.b64(key: String): ByteArray = decodeBase64Url(string(key))

/** Required field decoded as lowercase hex. */
fun JsonObject.hexBytes(key: String): ByteArray = string(key).hexToByteArray()

fun JsonObject.stringList(key: String): List<String> =
    array(key).map { it.jsonPrimitive.content }

private val BASE64URL_CHARS = ('A'..'Z') + ('a'..'z') + ('0'..'9') + '-' + '_'

/**
 * Strict `RawURLEncoding` decoder: rejects `+`, `/`, and `=` padding like Go's
 * `base64.RawURLEncoding` does (java.util's URL decoder would accept padding).
 */
fun decodeBase64Url(value: String): ByteArray {
    require(value.all { it in BASE64URL_CHARS }) { "invalid base64url input" }
    return Base64.getUrlDecoder().decode(value)
}

fun ByteArray.encodeBase64Url(): String =
    Base64.getUrlEncoder().withoutPadding().encodeToString(this)

fun ByteArray.encodeHex(): String = toHexString()
