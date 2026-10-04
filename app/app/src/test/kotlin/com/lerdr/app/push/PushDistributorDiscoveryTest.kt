package com.lerdr.app.push

import android.content.Intent
import android.content.pm.ActivityInfo
import android.content.pm.ApplicationInfo
import android.content.pm.ResolveInfo
import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import com.google.common.truth.Truth.assertThat
import com.lerdr.app.notify.LerdrNotifier
import com.lerdr.app.session.FakeCredentialStore
import com.lerdr.app.session.FakeRelaySessionFactory
import com.lerdr.app.session.SessionRepository
import java.io.File
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import lerdr.core.data.RelayRegistry
import lerdr.core.store.AgentStore
import lerdr.core.store.ConnectionStore
import lerdr.core.store.WorkspaceStore
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = com.lerdr.app.TestApp::class)
@OptIn(ExperimentalCoroutinesApi::class)
class PushDistributorDiscoveryTest {
    @get:Rule
    val tmp = TemporaryFolder()

    @Test
    fun `returning after distributor installation offers the available choices`() = runTest {
        val context = RuntimeEnvironment.getApplication()
        val scope = backgroundScope
        val relays = PreferenceDataStoreFactory.create(scope = scope) {
            File(tmp.root, "relays.preferences_pb")
        }
        val push = PreferenceDataStoreFactory.create(scope = scope) {
            File(tmp.root, "push.preferences_pb")
        }
        val sessions = SessionRepository(
            scope = scope,
            credentialStore = FakeCredentialStore(),
            relayRegistry = RelayRegistry(relays, scope),
            agentStore = AgentStore(scope),
            workspaceStore = WorkspaceStore(),
            connectionStore = ConnectionStore(clock = { 0L }),
            sessionFactory = FakeRelaySessionFactory(scope),
        )
        val manager = PushSubscriptionManager(context, sessions, LerdrNotifier(context), push, scope)
        manager.start()
        runCurrent()
        assertThat(manager.uiState.value.stage).isEqualTo(PushStage.NO_DISTRIBUTOR)

        sessions.setHidden(true)
        runCurrent()
        val registration = Intent("org.unifiedpush.android.distributor.REGISTER")
        val packageManager = shadowOf(context.packageManager)
        fun distributor(packageName: String) = ResolveInfo().apply {
            activityInfo = ActivityInfo().apply {
                this.packageName = packageName
                name = "$packageName.Receiver"
                applicationInfo = ApplicationInfo().apply { this.packageName = packageName }
                exported = true
            }
        }
        packageManager.addResolveInfoForIntent(registration, distributor("audit.distributor.a"))
        packageManager.addResolveInfoForIntent(registration, distributor("audit.distributor.b"))
        sessions.setHidden(false)
        runCurrent()
        assertThat(manager.uiState.value.stage).isEqualTo(PushStage.NEEDS_PICK)
        assertThat(manager.uiState.value.distributors)
            .containsExactly("audit.distributor.a", "audit.distributor.b")
    }
}
