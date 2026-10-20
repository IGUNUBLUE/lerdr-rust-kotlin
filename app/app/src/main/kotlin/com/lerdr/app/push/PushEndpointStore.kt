package com.lerdr.app.push

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import java.util.UUID
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonObject
import lerdr.core.model.Inbound

/** Durable, per-install registration. Each relay must acknowledge each endpoint generation. */
internal class PushEndpointStore(private val store: DataStore<Preferences>) {
    private val mutex = Mutex()

    data class Endpoint(val url: String, val p256dh: String, val auth: String, val generation: String)

    suspend fun endpoint(): Endpoint? = read(store.data.first())

    suspend fun save(url: String, p256dh: String, auth: String) = mutex.withLock {
        store.edit { prefs ->
            val previous = read(prefs)
            if (previous == null || previous.url != url || previous.p256dh != p256dh ||
                previous.auth != auth || prefs[GENERATION] == null
            ) {
                prefs[GENERATION] = UUID.randomUUID().toString()
            }
            prefs[ENDPOINT] = url
            prefs[P256DH] = p256dh
            prefs[AUTH] = auth
            prefs.remove(PREVIOUS)
        }
        Unit
    }

    suspend fun clear() = mutex.withLock {
        store.edit {
            it.remove(ENDPOINT)
            it.remove(P256DH)
            it.remove(AUTH)
            it.remove(GENERATION)
            it.remove(PREVIOUS)
        }
        Unit
    }

    suspend fun clientId(): String {
        var result = ""
        store.edit {
            result = it[CLIENT_ID] ?: "android-${UUID.randomUUID()}".also { id -> it[CLIENT_ID] = id }
        }
        return result
    }

    /**
     * Repair endpoints lost by older app versions. The existing unsubscribe action's client
     * filter is scoped by the authenticated device. Reset this installation BEFORE subscribing:
     * client_id matches all its endpoints, including the new one. Failed requests leave this
     * generation unacknowledged, so restart/reconnect retries both steps independently per relay.
     */
    suspend fun synchronize(relayId: String, send: suspend (Inbound) -> Unit): Endpoint? = mutex.withLock {
        val prefs = store.data.first()
        val endpoint = read(prefs) ?: return@withLock null
        val client = clientId()
        val ack = stringPreferencesKey("push_up_ack:$relayId")
        if (prefs[ack] != endpoint.generation) {
            send(Inbound(type = "push_unsubscribe", clientId = client))
        }
        send(Inbound(
            type = "push_subscribe",
            clientId = client,
            subscription = buildJsonObject {
                put("endpoint", endpoint.url)
                putJsonObject("keys") {
                    put("p256dh", endpoint.p256dh)
                    put("auth", endpoint.auth)
                }
            },
        ))
        store.edit { it[ack] = endpoint.generation }
        endpoint
    }

    private fun read(prefs: Preferences): Endpoint? {
        val url = prefs[ENDPOINT] ?: return null
        return Endpoint(url, prefs[P256DH] ?: return null, prefs[AUTH] ?: return null,
            prefs[GENERATION] ?: "legacy:$url")
    }

    private companion object {
        val ENDPOINT = stringPreferencesKey("push_up_endpoint")
        val P256DH = stringPreferencesKey("push_up_p256dh")
        val AUTH = stringPreferencesKey("push_up_auth")
        val GENERATION = stringPreferencesKey("push_up_generation")
        val PREVIOUS = stringPreferencesKey("push_up_prev_endpoint")
        val CLIENT_ID = stringPreferencesKey("push_client_id")
    }
}
