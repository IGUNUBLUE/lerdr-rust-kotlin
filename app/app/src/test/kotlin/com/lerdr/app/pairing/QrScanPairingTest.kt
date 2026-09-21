package com.lerdr.app.pairing

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import com.google.common.truth.Truth.assertThat
import com.lerdr.app.session.FakeCredentialStore
import com.lerdr.app.session.FakeRelaySessionFactory
import com.lerdr.app.session.FakeRelaySessionHandle
import com.lerdr.app.session.SessionRepository
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import lerdr.core.data.RelayDeviceCredential
import lerdr.core.data.RelayRegistry
import lerdr.core.store.AgentStore
import lerdr.core.store.ConnectionStore
import lerdr.core.store.WorkspaceStore
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

/**
 * The camera-free half of QR pairing: analyzer decode → [QrScanGate] →
 * [PairingViewModel.connectScanned] → `SetupLink.parse` → [PairingManager]
 * → enrollment. The frame pipeline itself needs a real device.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class QrScanPairingTest {

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
        val viewModel = PairingViewModel(manager)

        /** pair() suspends on real DataStore IO — poll on a real clock. */
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

        /** The vm mutates uiState off the test dispatcher — poll it. */
        fun awaitPhase(phase: PairingUiState.Phase) {
            val deadline = System.currentTimeMillis() + 5_000
            while (viewModel.uiState.value.phase != phase &&
                System.currentTimeMillis() < deadline
            ) {
                testScope.runCurrent()
                Thread.sleep(5)
            }
        }
    }

    private fun invitationLink(
        relay: String = "ws://192.168.1.9:7474",
        expiresAt: Long = System.currentTimeMillis() + 600_000,
    ): String = "lerdr://pair#" +
        "setup=${"s".repeat(43)}" +
        "&invite=invitation_000001" +
        "&invite_version=1" +
        "&invite_expires=$expiresAt" +
        "&label=desk" +
        "&relay=$relay"

    @Test
    fun `scanned invitation link runs the pairing flow to success`() = runTest {
        // viewModelScope needs a Main dispatcher on JVM tests.
        Dispatchers.setMain(UnconfinedTestDispatcher(testScheduler))
        try {
            val h = Harness(this, tmp.root)
            val link = invitationLink()

            // Analyzer boundary first: the gate accepts the decode…
            val gate = QrScanGate()
            val decoded = gate.offer(link)
            assertThat(decoded).isEqualTo(link)

            // …then the screen hands the decoded text to the view model.
            h.viewModel.connectScanned(decoded!!)
            assertThat(h.viewModel.uiState.value.phase)
                .isEqualTo(PairingUiState.Phase.CONNECTING)

            val handle = h.awaitHandle("ws://192.168.1.9:7474")
            val endpoint = h.registry.relays.value.single()
            handle.enroll(finish = FakeRelaySessionHandle.testFinish(withSecret = true))
            handle.connect()
            runCurrent()
            h.awaitPhase(PairingUiState.Phase.SUCCESS)

            assertThat(h.viewModel.uiState.value.phase)
                .isEqualTo(PairingUiState.Phase.SUCCESS)
            assertThat(h.credentials.get(endpoint.id))
                .isInstanceOf(RelayDeviceCredential::class.java)
        } finally {
            Dispatchers.resetMain()
        }
    }

    @Test
    fun `scanned text that is not a setup link surfaces invalid`() = runTest {
        val h = Harness(this, tmp.root)

        h.viewModel.connectScanned("not a setup link")

        assertThat(h.viewModel.uiState.value.error)
            .isEqualTo(PairingUiState.Error.INVALID_LINK)
    }

    @Test
    fun `a rejected link never reaches pairing`() = runTest {
        val h = Harness(this, tmp.root)
        val gate = QrScanGate()

        // Valid shape, but a retired-gateway link the parser refuses.
        assertThat(
            gate.offer("lerdr://pair#setup=${"s".repeat(43)}&gateways=ws://g:1"),
        ).isNull()

        assertThat(h.viewModel.uiState.value.phase).isEqualTo(PairingUiState.Phase.IDLE)
        assertThat(h.registry.relays.value).isEmpty()
    }
}
