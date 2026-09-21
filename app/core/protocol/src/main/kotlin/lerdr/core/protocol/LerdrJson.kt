package lerdr.core.protocol

import kotlinx.serialization.json.Json

/**
 * The shared codec configuration for the `protocol v3` wire contract.
 *
 * - `ignoreUnknownKeys`: Go's `encoding/json` ignores undeclared fields;
 *   the flat `Inbound` struct deliberately drops wire fields it has no
 *   declaration for (e.g. `content_fingerprint` on `pane_applied`).
 * - `encodeDefaults=false`: Go `omitempty` — zero values are not emitted.
 *   Fields that must always serialize carry `@EncodeDefault(ALWAYS)`.
 * - `explicitNulls=false`: Go `omitempty` on nullable fields — absent and
 *   null both encode as absent. Fields where Go emits an explicit `null`
 *   (map `any` slots) use `WireField`, which preserves the distinction
 *   independently of this setting.
 */
val LerdrJson: Json = Json {
    ignoreUnknownKeys = true
    encodeDefaults = false
    explicitNulls = false
    isLenient = false
    prettyPrint = false
}
