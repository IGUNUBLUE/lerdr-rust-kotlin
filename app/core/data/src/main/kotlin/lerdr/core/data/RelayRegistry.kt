package lerdr.core.data

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.stateIn
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject

/**
 * The ordered list of relays this device knows — the oracle's
 * `lerdr_relays` localStorage array (`config.ts:163-192`) on DataStore
 * preferences.
 *
 * NON-SECRET fields only: a [RelayEndpoint] carries no relay key or
 * credential — those live in [CredentialStore] behind Keystore wrapping.
 * List order is the display/connection order the user arranges.
 *
 * A stored entry that fails to decode or validate is dropped (the oracle's
 * `loadRelayConfigs` filter+normalize); one bad row never bricks the list.
 */
class RelayRegistry(
    private val dataStore: DataStore<Preferences>,
    scope: CoroutineScope,
    private val json: Json = Json { ignoreUnknownKeys = true },
) {
    /** Ordered relay endpoints; empty until the first DataStore read lands. */
    val relays: StateFlow<List<RelayEndpoint>> = dataStore.data
        .map { decode(it[RELAYS_KEY]) }
        .distinctUntilChanged()
        .stateIn(scope, SharingStarted.Eagerly, emptyList())

    /** Authoritative current list — reads through to DataStore, no lag. */
    suspend fun snapshot(): List<RelayEndpoint> = decode(dataStore.data.first()[RELAYS_KEY])

    /** Insert or replace by [RelayEndpoint.id]; new ids append at the end. */
    suspend fun upsert(endpoint: RelayEndpoint) {
        dataStore.edit { prefs ->
            val current = decode(prefs[RELAYS_KEY])
            val index = current.indexOfFirst { it.id == endpoint.id }
            val next = if (index >= 0) {
                current.toMutableList().apply { set(index, endpoint) }
            } else {
                current + endpoint
            }
            prefs[RELAYS_KEY] = encode(next)
        }
    }

    /** @return true when an entry was actually removed. */
    suspend fun remove(id: String): Boolean {
        var removed = false
        dataStore.edit { prefs ->
            val current = decode(prefs[RELAYS_KEY])
            val next = current.filterNot { it.id == id }
            removed = next.size != current.size
            if (removed) prefs[RELAYS_KEY] = encode(next)
        }
        return removed
    }

    /**
     * Sets the display order. Ids missing from [orderedIds] keep their
     * relative order at the end; unknown ids are ignored — a reorder can
     * never lose or invent relays.
     */
    suspend fun reorder(orderedIds: List<String>) {
        dataStore.edit { prefs ->
            val current = decode(prefs[RELAYS_KEY])
            val rank = orderedIds.withIndex().associate { (index, id) -> id to index }
            val next = current.sortedWith(
                compareBy({ rank[it.id] ?: Int.MAX_VALUE }, { current.indexOf(it) }),
            )
            prefs[RELAYS_KEY] = encode(next)
        }
    }

    suspend fun clear() {
        dataStore.edit { it.remove(RELAYS_KEY) }
    }

    private fun decode(raw: String?): List<RelayEndpoint> {
        if (raw.isNullOrEmpty()) return emptyList()
        val array = try {
            json.parseToJsonElement(raw) as? JsonArray
        } catch (e: IllegalArgumentException) {
            null
        } ?: return emptyList()
        return array.mapNotNull { element ->
            try {
                if (element !is JsonObject) return@mapNotNull null
                json.decodeFromJsonElement(RelayEndpoint.serializer(), element)
            } catch (e: Exception) {
                null
            }
        }
    }

    private fun encode(relays: List<RelayEndpoint>): String =
        json.encodeToString(ListSerializer(RelayEndpoint.serializer()), relays)

    companion object {
        /** Same storage key name as the oracle's localStorage entry. */
        val RELAYS_KEY = stringPreferencesKey("lerdr_relays")
    }
}
