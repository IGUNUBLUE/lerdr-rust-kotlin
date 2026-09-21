package com.lerdr.app.pairing

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import com.google.common.truth.Truth.assertThat
import com.lerdr.app.session.FakeCredentialStore
import com.lerdr.app.session.FakeRelaySessionFactory
import com.lerdr.app.session.FakeRelaySessionHandle
import com.lerdr.app.session.SessionRepository
import java.io.File
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.async
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import lerdr.core.data.InvitePayload
import lerdr.core.data.RelayDeviceCredential
import lerdr.core.data.RelayRegistry
import lerdr.core.store.AgentStore
import lerdr.core.store.ConnectionStore
import lerdr.core.store.WorkspaceStore
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

@OptIn(ExperimentalCoroutinesApi::class)
class PairingManagerTest {

    @get:Rule
    val tmp = TemporaryFolder()

    private class Harness(private val testScope: TestScope, tmpDir: File) {
        private val scope = testScope.backgroundScope
        val credentials = FakeCredentialStore()
        private val dataStore = PreferenceDataStoreFactory.create(scope = scope) {
            File(tmpDir, "relays.preferences_pb")
        }
        val registry = RelayRegistry(dataStore, scope)
        val factory = FakeRelaySessionFactory(scope)
        val sessions = SessionRepository(
            scope = scope,
            credentialStore = credentials,
            relayRegistry = registry,
            agentStore = AgentStore(scope),
            workspaceStore = WorkspaceStore(),
            connectionStore = ConnectionStore(clock = { 0L }),
            sessionFactory = factory,
        )
        val manager = PairingManager(registry, credentials, sessions)

        /**
         * pair() suspends on real DataStore IO between upsert and connect —
         * poll (with a real clock) until the transport handle appears.
         */
        fun awaitHandle(origin: String): FakeRelaySessionHandle {
            val deadline = System.currentTimeMillis() + 5_000
            var handle: FakeRelaySessionHandle? = null
            while (handle == null && System.currentTimeMillis() < deadline) {
                handle = factory.handleFor(origin)
                if (handle == null) {
                    testScope.runCurrent()
                    Thread.sleep(5)
                }
            }
            return handle ?: error("pairing never connected to $origin")
        }
    }

    private fun invitePayload(
        origin: String = "ws://192.168.1.9:7474",
        expiresAt: Long = Long.MAX_VALUE,
    ) = InvitePayload(
        label = "desk",
        socketOrigin = origin,
        setup = "s".repeat(43),
        invitation = InvitePayload.Invitation(
            id = "inv1",
            version = 1,
            secret = "k".repeat(43),
            expiresAtEpochMs = expiresAt,
        ),
        source = InvitePayload.Source.LERDR_LINK,
    )

    @Test
    fun `valid invitation redeems to a credential and reports success`() = runTest {
        val h = Harness(this, tmp.root)
        val attempt = backgroundScope.async { h.manager.pair(invitePayload()) }
        val handle = h.awaitHandle("ws://192.168.1.9:7474")

        val endpoint = h.registry.relays.value.single()
        handle.enroll(finish = FakeRelaySessionHandle.testFinish(withSecret = true))
        handle.connect()
        runCurrent()

        assertThat(attempt.await()).isEqualTo(PairingOutcome.Success(endpoint.id))
        val record = h.credentials.get(endpoint.id)
        assertThat(record).isInstanceOf(RelayDeviceCredential::class.java)
        assertThat((record as RelayDeviceCredential).invitationId).isEqualTo("inv1")
    }

    @Test
    fun `expired invitation short-circuits before any writes`() = runTest {
        val h = Harness(this, tmp.root)
        val outcome = h.manager.pair(invitePayload(expiresAt = 1L))
        assertThat(outcome).isEqualTo(PairingOutcome.InvitationExpired)
        assertThat(h.registry.relays.value).isEmpty()
        assertThat(h.credentials.records.value).isEmpty()
    }

    @Test
    fun `bad origin reports InvalidLink`() = runTest {
        val h = Harness(this, tmp.root)
        val outcome = h.manager.pair(invitePayload(origin = "not-a-url"))
        assertThat(outcome).isEqualTo(PairingOutcome.InvalidLink)
    }

    @Test
    fun `auth rejection surfaces as Rejected`() = runTest {
        val h = Harness(this, tmp.root)
        val attempt = backgroundScope.async { h.manager.pair(invitePayload()) }
        val handle = h.awaitHandle("ws://192.168.1.9:7474")
        handle.rejectAuth()
        runCurrent()
        val outcome = attempt.await()
        assertThat(outcome).isInstanceOf(PairingOutcome.Rejected::class.java)
        assertThat((outcome as PairingOutcome.Rejected).reason).isNotEmpty()
    }

    @Test
    fun `no terminal state within the budget reports TimedOut`() = runTest {
        val h = Harness(this, tmp.root)
        val attempt = backgroundScope.async { h.manager.pair(invitePayload()) }
        h.awaitHandle("ws://192.168.1.9:7474")
        // await suspends the body; the scheduler auto-advances virtual time
        // past the 30 s pairing budget.
        assertThat(attempt.await()).isEqualTo(PairingOutcome.TimedOut)
    }
}
