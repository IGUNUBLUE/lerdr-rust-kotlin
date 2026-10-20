package com.lerdr.app.push

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import com.google.common.truth.Truth.assertThat
import com.lerdr.app.TestApp
import com.lerdr.app.notify.LerdrNotifier
import com.lerdr.app.session.FakeCredentialStore
import com.lerdr.app.session.FakeRelaySessionFactory
import com.lerdr.app.session.SessionRepository
import java.io.File
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import lerdr.core.data.RelayRegistry
import lerdr.core.data.RelayEndpoint
import lerdr.core.data.RelayTransport
import lerdr.core.store.AgentStore
import lerdr.core.store.ConnectionStore
import lerdr.core.store.WorkspaceStore
import org.unifiedpush.android.connector.data.PushEndpoint
import org.unifiedpush.android.connector.data.PublicKeySet
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = TestApp::class)
@OptIn(ExperimentalCoroutinesApi::class)
class PushSubscriptionMigrationTest {
    @get:Rule val tmp = TemporaryFolder()

    @Test fun `manager waits for reset and subscription receipts then resubscribes after reconnect`() = runTest {
        val context = RuntimeEnvironment.getApplication()
        val scope = backgroundScope
        val push = PreferenceDataStoreFactory.create(scope = scope) { File(tmp.root, "push.preferences_pb") }
        val relays = PreferenceDataStoreFactory.create(scope = scope) { File(tmp.root, "relays.preferences_pb") }
        val factory = FakeRelaySessionFactory(scope)
        val sessions = SessionRepository(scope, FakeCredentialStore(), RelayRegistry(relays, scope),
            AgentStore(scope), WorkspaceStore(), ConnectionStore(clock = { 0L }), factory)
        val endpoint = RelayEndpoint(id = "r1", label = "Test", host = "127.0.0.1", port = 8377,
            transport = RelayTransport.WEBSOCKET)
        sessions.connect(endpoint)
        val handle = factory.handleFor(endpoint.socketOrigin)!!
        handle.connect()
        val manager = PushSubscriptionManager(context, sessions, LerdrNotifier(context), push, scope)
        manager.start()
        runCurrent()
        manager.onNewEndpoint(PushEndpoint("https://updates.push.services.mozilla.com/test",
            PublicKeySet("key", "auth"), false), "default")
        fun frames() = handle.sentRaw.map { Json.parseToJsonElement(it).jsonObject }
            .filter { it["type"]?.jsonPrimitive?.content in setOf("push_subscribe", "push_unsubscribe") }
        fun awaitCount(count: Int) {
            val until = System.currentTimeMillis() + 5_000
            while (frames().size < count && System.currentTimeMillis() < until) {
                runCurrent()
                Thread.sleep(5)
            }
            assertThat(frames()).hasSize(count)
        }
        suspend fun acknowledge(index: Int) {
            val id = frames()[index]["request_id"]!!.jsonPrimitive.content
            handle.emit(Json.parseToJsonElement(
                """{"type":"command_result","request_id":"$id","ok":true,"phase":"completed"}""",
            ).jsonObject)
            runCurrent()
        }
        awaitCount(1)
        assertThat(frames()[0]["type"]!!.jsonPrimitive.content).isEqualTo("push_unsubscribe")
        assertThat(manager.uiState.value.subscribedRelays).isEmpty()
        acknowledge(0)
        awaitCount(2)
        assertThat(frames()[1]["type"]!!.jsonPrimitive.content).isEqualTo("push_subscribe")
        assertThat(manager.uiState.value.subscribedRelays).isEmpty()
        acknowledge(1)
        val until = System.currentTimeMillis() + 5_000
        while (manager.uiState.value.stage != PushStage.SUBSCRIBED && System.currentTimeMillis() < until) {
            runCurrent()
            Thread.sleep(5)
        }
        assertThat(manager.uiState.value.subscribedRelays).containsExactly("r1")
        handle.disconnect()
        runCurrent()
        handle.connect()
        awaitCount(3)
        assertThat(frames()[2]["type"]!!.jsonPrimitive.content).isEqualTo("push_subscribe")
        acknowledge(2)
        manager.showDistributorPicker()
        assertThat(manager.uiState.value.choosingDistributor).isTrue()
        manager.dismissDistributorPicker()
        assertThat(manager.uiState.value.choosingDistributor).isFalse()
        assertThat(manager.uiState.value.subscribedRelays).containsExactly("r1")
    }
}
