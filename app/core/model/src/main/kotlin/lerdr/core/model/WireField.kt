package lerdr.core.model

import kotlinx.serialization.KSerializer
import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.descriptors.SerialDescriptor
import kotlinx.serialization.encoding.Decoder
import kotlinx.serialization.encoding.Encoder
import kotlinx.serialization.json.JsonDecoder
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonEncoder
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.decodeFromJsonElement

/**
 * Presence-preserving field for messages Go emits as `map[string]any`:
 * three states — key absent ([Absent]), key present with JSON `null`
 * ([ExplicitNull]), key present with a value ([Present]).
 *
 * kotlinx cannot model this with `T?` (absent and null both decode to
 * Kotlin null, and null is skipped on encode under `explicitNulls=false`),
 * so the wire keeps the distinction in a non-null wrapper whose default is
 * the [Absent] sentinel — a skipped-on-encode value.
 */
sealed interface WireField<out T> {
    /** The key was absent; omitted when encoding. */
    data object Absent : WireField<Nothing>

    /** The key was present with value `null`; emitted as `null`. */
    data object ExplicitNull : WireField<Nothing>

    /** The key carried a value. */
    data class Present<T>(val value: T) : WireField<T>

    companion object {
        fun <T> of(value: T?): WireField<T> =
            if (value == null) ExplicitNull else Present(value)
    }
}

/** The decoded value, or null for both [WireField.Absent] and [WireField.ExplicitNull]. */
val <T> WireField<T>.orNull: T?
    get() = (this as? WireField.Present)?.value

/** True when the key was present on the wire (null counts as present). */
val WireField<*>.isPresent: Boolean
    get() = this !is WireField.Absent

/**
 * Serializer for a [WireField]-typed property. Null tokens decode to
 * [WireField.ExplicitNull], everything else to [WireField.Present]; absent
 * keys fall back to the [WireField.Absent] default. Registered per-field via
 * `@Serializable(with = WireFieldSerializer::class)`.
 */
class WireFieldSerializer<T>(private val valueSerializer: KSerializer<T>) :
    KSerializer<WireField<T>> {

    override val descriptor: SerialDescriptor = valueSerializer.descriptor

    override fun serialize(encoder: Encoder, value: WireField<T>) {
        val json = encoder as? JsonEncoder
            ?: throw SerializationException("wire fields are JSON-only")
        when (value) {
            // Absent is unreachable: it equals the declared default and is
            // skipped before serialize() under encodeDefaults=false.
            WireField.Absent, WireField.ExplicitNull -> json.encodeJsonElement(JsonNull)
            is WireField.Present -> json.encodeSerializableValue(valueSerializer, value.value)
        }
    }

    override fun deserialize(decoder: Decoder): WireField<T> {
        val json = decoder as? JsonDecoder
            ?: throw SerializationException("wire fields are JSON-only")
        val element = json.decodeJsonElement()
        if (element is JsonNull) return WireField.ExplicitNull
        return WireField.Present(json.json.decodeFromJsonElement(valueSerializer, element))
    }
}
