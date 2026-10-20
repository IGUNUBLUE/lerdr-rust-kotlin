package com.lerdr.app.push

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import com.google.common.truth.Truth.assertThat
import java.io.File
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import lerdr.core.model.Inbound
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

@OptIn(ExperimentalCoroutinesApi::class)
class PushEndpointStoreTest {
    @get:Rule val tmp = TemporaryFolder()

    @Test fun `legacy registration repairs unknown stale endpoints before registering`() = runTest {
        val data = PreferenceDataStoreFactory.create(scope = backgroundScope) {
            File(tmp.root, "push.preferences_pb")
        }
        data.edit {
            it[stringPreferencesKey("push_up_endpoint")] = "https://push.example/current"
            it[stringPreferencesKey("push_up_p256dh")] = "key"
            it[stringPreferencesKey("push_up_auth")] = "auth"
            it[stringPreferencesKey("push_client_id")] = "original-install"
        }
        val store = PushEndpointStore(data)
        val sent = mutableListOf<Inbound>()
        store.synchronize("r1") { sent += it }
        assertThat(sent.map { it.type }).containsExactly("push_unsubscribe", "push_subscribe").inOrder()
        assertThat(sent.map { it.clientId }).containsExactly("original-install", "original-install")
        assertThat(sent.first().endpoints).isEmpty()
        sent.clear()
        PushEndpointStore(data).synchronize("r1") { sent += it }
        assertThat(sent.map { it.type }).containsExactly("push_subscribe")
    }

    @Test fun `offline relay and failed subscription retain independent cleanup after restart`() = runTest {
        val data = PreferenceDataStoreFactory.create(scope = backgroundScope) {
            File(tmp.root, "push.preferences_pb")
        }
        val store = PushEndpointStore(data)
        store.save("https://ntfy.example/old", "key", "auth")
        store.synchronize("offline-later") {}
        store.save("https://mozilla.example/new", "key2", "auth2")
        store.synchronize("online") {}
        val sent = mutableListOf<Inbound>()
        val restarted = PushEndpointStore(data)
        try {
            restarted.synchronize("offline-later") {
                sent += it
                if (it.type == "push_subscribe") error("connection lost")
            }
        } catch (_: IllegalStateException) { }
        assertThat(sent.map { it.type }).containsExactly("push_unsubscribe", "push_subscribe").inOrder()
        sent.clear()
        restarted.synchronize("offline-later") { sent += it }
        assertThat(sent.map { it.type }).containsExactly("push_unsubscribe", "push_subscribe").inOrder()
        assertThat(sent.last().subscription.toString()).contains("https://mozilla.example/new")
    }

    @Test fun `switching back and unregistering require a fresh generation`() = runTest {
        val data = PreferenceDataStoreFactory.create(scope = backgroundScope) {
            File(tmp.root, "push.preferences_pb")
        }
        val store = PushEndpointStore(data)
        store.save("https://push.example/a", "key", "auth")
        store.synchronize("r1") {}
        store.save("https://push.example/b", "key", "auth")
        store.save("https://push.example/a", "key", "auth")
        val sent = mutableListOf<Inbound>()
        store.synchronize("r1") { sent += it }
        assertThat(sent.first().type).isEqualTo("push_unsubscribe")
        val client = store.clientId()
        store.clear()
        assertThat(store.endpoint()).isNull()
        store.save("https://push.example/a", "key", "auth")
        sent.clear()
        store.synchronize("r1") { sent += it }
        assertThat(sent.first().type).isEqualTo("push_unsubscribe")
        assertThat(store.clientId()).isEqualTo(client)
    }

    @Test fun `rotation waits for in-flight synchronization and identical callbacks keep generation`() = runTest {
        val data = PreferenceDataStoreFactory.create(scope = backgroundScope) {
            File(tmp.root, "push.preferences_pb")
        }
        val store = PushEndpointStore(data)
        store.save("https://push.example/a", "key", "auth")
        val gate = CompletableDeferred<Unit>()
        val syncing = launch { store.synchronize("r1") { gate.await() } }
        runCurrent()
        val rotating = launch { store.save("https://push.example/b", "key", "auth") }
        runCurrent()
        assertThat(rotating.isCompleted).isFalse()
        gate.complete(Unit)
        syncing.join()
        rotating.join()
        val generation = store.endpoint()!!.generation
        store.save("https://push.example/b", "key", "auth")
        assertThat(store.endpoint()!!.generation).isEqualTo(generation)
        val sent = mutableListOf<Inbound>()
        store.synchronize("r1") { sent += it }
        assertThat(sent.first().type).isEqualTo("push_unsubscribe")
    }
}
