package com.lerdr.app.speech

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import com.google.common.truth.Truth.assertThat
import com.lerdr.app.session.FakeCredentialStore
import com.lerdr.app.session.FakeRelaySessionFactory
import com.lerdr.app.session.SessionRepository
import java.io.File
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonPrimitive
import lerdr.core.data.RelayEndpoint
import lerdr.core.data.RelayRegistry
import lerdr.core.data.RelayTransport
import lerdr.core.protocol.LerdrJson
import lerdr.core.store.AgentStore
import lerdr.core.store.ConnectionStore
import lerdr.core.store.WorkspaceStore
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

@OptIn(ExperimentalCoroutinesApi::class)
class SessionSpeechSenderTest {
    @get:Rule
    val tmp = TemporaryFolder()

    private class Harness(testScope: TestScope, directory: File) {
        private val scope = testScope.backgroundScope
        private val dataStore = PreferenceDataStoreFactory.create(scope = scope) {
            File(directory, "relays.preferences_pb")
        }
        val factory = FakeRelaySessionFactory(scope)
        val sessions = SessionRepository(
            scope = scope,
            credentialStore = FakeCredentialStore(),
            relayRegistry = RelayRegistry(dataStore, scope),
            agentStore = AgentStore(scope),
            workspaceStore = WorkspaceStore(),
            connectionStore = ConnectionStore(clock = { 0L }),
            sessionFactory = factory,
        )
        private val senderScope = CoroutineScope(
            scope.coroutineContext + SupervisorJob(scope.coroutineContext[Job]),
        )
        val sender = SessionSpeechSender(senderScope, sessions)
        val endpoint = RelayEndpoint(
            id = "r1",
            label = "workstation",
            host = "192.168.1.5",
            port = 7474,
            transport = RelayTransport.WEBSOCKET,
        )
        fun handle() = checkNotNull(factory.handleFor("ws://192.168.1.5:7474"))
        suspend fun connect() {
            sessions.connect(endpoint)
            handle().connect()
            handle().emit(json("""{"type":"push_config","capabilities":["speech_synthesis"],"inventory":{"state":"ready"}}"""))
        }
    }

    @Test
    fun `capability revocation rejects synthesis before dispatch`() = runTest {
        val h = Harness(this, tmp.newFolder())
        h.connect()
        runCurrent()
        h.handle().emit(json("""{"type":"caps_update","capabilities":[]}"""))
        runCurrent()
        val exchange = h.sender.send("r1", "Hello.", "en")
        val failure = async { runCatching { exchange.await() }.exceptionOrNull() }
        runCurrent()
        assertThat(failure.await()).isInstanceOf(SpeechPlaybackException::class.java)
        assertThat(h.handle().sentRaw.map { json(it)["type"]?.jsonPrimitive?.content })
            .doesNotContain("speak_text")
    }

    @Test
    fun `cancellation ends waiting without a relay reply`() = runTest {
        val h = Harness(this, tmp.newFolder())
        h.connect()
        runCurrent()
        val exchange = h.sender.send("r1", "Hello.", "en")
        val outcome = async { runCatching { exchange.await() }.exceptionOrNull() }
        runCurrent()
        assertThat(outcome.isCompleted).isFalse()
        exchange.cancel()
        runCurrent()
        assertThat(outcome.isCompleted).isTrue()
        assertThat(outcome.await()).isInstanceOf(CancellationException::class.java)
    }

    private companion object {
        fun json(raw: String): JsonObject = LerdrJson.parseToJsonElement(raw) as JsonObject
    }
}
