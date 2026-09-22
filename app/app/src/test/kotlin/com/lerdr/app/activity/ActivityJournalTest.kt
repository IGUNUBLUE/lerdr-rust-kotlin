package com.lerdr.app.activity

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import com.google.common.truth.Truth.assertThat
import com.lerdr.app.session.FakeCredentialStore
import com.lerdr.app.session.FakeRelaySessionFactory
import com.lerdr.app.session.FakeRelaySessionHandle
import com.lerdr.app.session.SessionRepository
import java.io.File
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import lerdr.core.data.RelayEndpoint
import lerdr.core.data.RelayRegistry
import lerdr.core.data.RelayTransport
import lerdr.core.store.AgentStore
import lerdr.core.store.ConnectionStore
import lerdr.core.store.WorkspaceStore
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

@OptIn(ExperimentalCoroutinesApi::class)
class ActivityJournalTest {

    @get:Rule
    val tmp = TemporaryFolder()

    private class Harness(
        private val testScope: TestScope,
        tmpDir: File,
    ) {
        private val scope = testScope.backgroundScope
        var clock = 1_000_000L
        val credentials = FakeCredentialStore()
        private val dataStore = PreferenceDataStoreFactory.create(scope = scope) {
            File(tmpDir, "relays.preferences_pb")
        }
        val registry = RelayRegistry(dataStore, scope)
        val agents = AgentStore(scope)
        val workspaces = WorkspaceStore()
        val connections = ConnectionStore(clock = { 0L })
        val factory = FakeRelaySessionFactory(scope)
        val repository = SessionRepository(
            scope = scope,
            credentialStore = credentials,
            relayRegistry = registry,
            agentStore = agents,
            workspaceStore = workspaces,
            connectionStore = connections,
            sessionFactory = factory,
        )

        /** Journal over the live repository — the same wiring Hilt builds. */
        val journal = ActivityJournal(
            scope = scope,
            sessions = repository,
            now = { clock },
        )

        val endpoint = RelayEndpoint(
            id = "r1",
            label = "workstation",
            host = "192.168.1.5",
            port = 7474,
            transport = RelayTransport.WEBSOCKET,
        )
        val origin = "ws://192.168.1.5:7474"

        fun pump() = testScope.runCurrent()

        fun handle(): FakeRelaySessionHandle =
            factory.handleFor(origin) ?: error("no session for $origin")

        /** Poll on a real clock — DataStore IO is off the test scheduler. */
        fun await(condition: () -> Boolean) {
            val deadline = System.currentTimeMillis() + 5_000
            while (!condition() && System.currentTimeMillis() < deadline) {
                testScope.runCurrent()
                Thread.sleep(5)
            }
            check(condition()) { "condition not met within deadline" }
        }
    }

    @Test
    fun `late-created journal baselines silently instead of flooding`() = runTest {
        val h = Harness(this, tmp.root)
        h.repository.connect(h.endpoint)
        h.handle().connect()
        h.pump()

        // A journal born after the connect records nothing for it.
        val late = ActivityJournal(
            scope = backgroundScope,
            sessions = h.repository,
            now = { h.clock },
        )
        h.pump()
        assertThat(late.events.value).isEmpty()
    }

    @Test
    fun `connect then disconnect records the session lifecycle`() = runTest {
        val h = Harness(this, tmp.root)
        h.pump() // seed the baselines before any transition

        h.repository.connect(h.endpoint)
        h.pump()
        assertThat(h.journal.events.value.map { it.kind })
            .containsExactly(ActivityJournal.Kind.CONNECTING)

        h.handle().connect()
        h.pump()
        assertThat(h.journal.events.value.first().kind)
            .isEqualTo(ActivityJournal.Kind.CONNECTED)

        h.handle().disconnect()
        h.pump()
        val disconnected = h.journal.events.value.first()
        assertThat(disconnected.kind).isEqualTo(ActivityJournal.Kind.DISCONNECTED)
        assertThat(disconnected.detail).isEqualTo("test disconnect")
        assertThat(disconnected.relayId).isEqualTo("r1")
        assertThat(disconnected.relayLabel).isEqualTo("workstation")
    }

    @Test
    fun `auth rejection records AUTH_REJECTED once`() = runTest {
        val h = Harness(this, tmp.root)
        h.pump()

        h.repository.connect(h.endpoint)
        h.pump()
        h.handle().rejectAuth()
        h.pump()

        val rejected = h.journal.events.value
            .filter { it.kind == ActivityJournal.Kind.AUTH_REJECTED }
        assertThat(rejected).hasSize(1)
        assertThat(rejected.single().detail).contains("unauthorized")
    }

    @Test
    fun `registry add and remove record membership events`() = runTest {
        val h = Harness(this, tmp.root)
        h.pump() // seed the (empty) baselines

        h.registry.upsert(h.endpoint)
        h.await {
            h.journal.events.value.any { it.kind == ActivityJournal.Kind.RELAY_ADDED }
        }

        h.repository.removeRelay("r1")
        h.await {
            h.journal.events.value.any { it.kind == ActivityJournal.Kind.RELAY_REMOVED }
        }
        assertThat(h.journal.events.value.first().kind)
            .isEqualTo(ActivityJournal.Kind.RELAY_REMOVED)
    }

    @Test
    fun `unchanged connection signature does not re-record`() = runTest {
        val h = Harness(this, tmp.root)
        h.pump()

        h.repository.connect(h.endpoint)
        h.pump()
        h.handle().connect()
        h.pump()
        val size = h.journal.events.value.size

        // Same status pushed again (e.g. a keepalive-driven refresh).
        h.handle().connect()
        h.pump()
        assertThat(h.journal.events.value.size).isEqualTo(size)
    }

    @Test
    fun `events are newest-first and capped at 500`() = runTest {
        val h = Harness(this, tmp.root)
        repeat(600) { index ->
            h.clock += 1
            h.journal.record(
                ActivityJournal.Kind.DISCONNECTED,
                relayId = "r$index",
                detail = "drop $index",
            )
        }
        val events = h.journal.events.value
        assertThat(events).hasSize(500)
        assertThat(events.first().detail).isEqualTo("drop 599")
        assertThat(events.last().detail).isEqualTo("drop 100")
    }
}
