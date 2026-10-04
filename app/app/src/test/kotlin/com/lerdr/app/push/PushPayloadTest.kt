package com.lerdr.app.push

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import com.google.common.truth.Truth.assertThat
import com.lerdr.app.notify.AttentionReducer
import com.lerdr.app.notify.NotificationCommand
import com.lerdr.app.notify.NotifyIds
import com.lerdr.app.session.FakeRelaySessionFactory
import com.lerdr.app.session.SessionRepository
import com.lerdr.navigation.LerdrDeepLinks
import com.lerdr.navigation.LerdrKey
import java.io.File
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.runTest
import lerdr.core.data.CredentialCipher
import lerdr.core.data.CredentialEnrollment
import lerdr.core.data.DeviceRole
import lerdr.core.data.KeystoreCredentialStore
import lerdr.core.data.RelayInvitation
import lerdr.core.data.RelayEndpoint
import lerdr.core.data.RelayRegistry
import lerdr.core.model.AgentState
import lerdr.core.store.AgentStore
import lerdr.core.store.ConnectionStore
import lerdr.core.store.WorkspaceStore
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

@OptIn(ExperimentalCoroutinesApi::class)
class PushPayloadTest {
    @get:Rule val tmp = TemporaryFolder()

    private class TestCipher : CredentialCipher {
        override fun seal(plaintext: ByteArray) = ByteArray(plaintext.size) { (plaintext[it] + 1).toByte() }
        override fun open(sealed: ByteArray) = ByteArray(sealed.size) { (sealed[it] - 1).toByte() }
    }

    private fun TestScope.store() =
        KeystoreCredentialStore(File(tmp.root, "credentials.dat"), TestCipher(), backgroundScope, { 1_000L })

    private suspend fun KeystoreCredentialStore.enroll(relayId: String, deviceId: String) {
        saveInvitation(relayId, RelayInvitation(ByteArray(32) { 7 }))
        redeemInvitation(relayId, RelayInvitation.BOOTSTRAP_ID, CredentialEnrollment(
            deviceId, "cred-$relayId", 1, ByteArray(32) { 9 }, DeviceRole.CONTROLLER, "en",
        ))
    }

    private suspend fun TestScope.seed(vararg devices: Pair<String, String?>) {
        val writer = store()
        val job = SupervisorJob()
        val scope = CoroutineScope(UnconfinedTestDispatcher(testScheduler) + job)
        val registry = RelayRegistry(PreferenceDataStoreFactory.create(scope = scope) {
            File(tmp.root, "relays.preferences_pb")
        }, scope)
        try {
            for ((relayId, deviceId) in devices) {
                if (deviceId == null) writer.saveInvitation(relayId, RelayInvitation(ByteArray(32) { 7 }))
                else writer.enroll(relayId, deviceId)
                registry.upsert(RelayEndpoint(id = relayId, label = relayId, host = "192.168.1.5", port = 7474))
            }
        } finally {
            job.cancelAndJoin()
        }
    }

    private class Delivery(testScope: TestScope, directory: File, val credentials: KeystoreCredentialStore) {
        val agents = AgentStore(testScope.backgroundScope)
        private val factory = FakeRelaySessionFactory(testScope.backgroundScope)
        private val sessions = SessionRepository(
            scope = testScope.backgroundScope,
            credentialStore = credentials,
            relayRegistry = RelayRegistry(PreferenceDataStoreFactory.create(scope = testScope.backgroundScope) {
                File(directory, "relays.preferences_pb")
            }, testScope.backgroundScope),
            agentStore = agents,
            workspaceStore = WorkspaceStore(),
            connectionStore = ConnectionStore(),
            sessionFactory = factory,
        )

        // Deliberately never start sessions or populate connection/snapshot state.
        suspend fun commands(payload: PushPayload) =
            payload.toCommands(sessions.enrolledRelayForPushDevice(payload.key.deviceId))

        fun row(relayId: String, status: String): List<lerdr.core.store.Agent> {
            agents.mergeSnapshot(relayId, relayId, listOf(AgentState(
                paneId = "wG:p3", rawPaneId = "wG:p3", status = status,
            )), attentionCapable = true)
            return agents.agents.value.filter { it.relayId == relayId }
        }
    }

    private fun payload(category: String = CATEGORY_ATTENTION, deviceId: String = "dev-1", paneId: String = "wG:p3") =
        PushPayload(v = 1, category = category,
            key = PushEventKey(deviceId = deviceId, paneId = paneId, category = category),
            title = "Response needed", body = "Review and respond", eventRef = "signed-reference")

    @Test
    fun `cold stored enrollment targets exact agent and socket shade slot`() = runTest {
        seed("r1" to "dev-1")
        val cold = store()
        assertThat(cold.records.value).isEmpty()
        val delivery = Delivery(this, tmp.root, cold)
        for (category in listOf(CATEGORY_ATTENTION, CATEGORY_QUESTION, CATEGORY_BRIEF, CATEGORY_FINISHED)) {
            val post = delivery.commands(payload(category)).single() as NotificationCommand.Post
            delivery.agents.removeRelay("r1")
            val prior = delivery.row("r1", "working")
            val current = delivery.row("r1", if (category == CATEGORY_FINISHED) "done" else "blocked")
            val socketPost = AttentionReducer.reduce(prior, current).filterIsInstance<NotificationCommand.Post>().single()
            val route = LerdrDeepLinks.match(post.deepLink) as LerdrKey.AgentFeed
            assertThat(delivery.agents.agentNow(route.paneId)).isSameInstanceAs(current.single())
            assertThat(current.single().relayId).isEqualTo("r1")
            assertThat(post.notificationId).isEqualTo(socketPost.notificationId)
            assertThat(post.channel).isEqualTo(socketPost.channel)
        }
    }

    @Test
    fun `same raw pane on two relays stays distinct and retract removes only owning card`() = runTest {
        seed("r1" to "dev-1", "r2" to "dev-2")
        val delivery = Delivery(this, tmp.root, store())
        val first = delivery.commands(payload()).single() as NotificationCommand.Post
        val second = delivery.commands(payload(deviceId = "dev-2")).single() as NotificationCommand.Post
        assertThat(first.notificationId).isNotEqualTo(second.notificationId)
        val firstRoute = LerdrDeepLinks.match(first.deepLink) as LerdrKey.AgentFeed
        val secondRoute = LerdrDeepLinks.match(second.deepLink) as LerdrKey.AgentFeed
        delivery.row("r1", "blocked")
        delivery.row("r2", "blocked")
        assertThat(delivery.agents.agentNow(firstRoute.paneId)?.relayId).isEqualTo("r1")
        assertThat(delivery.agents.agentNow(secondRoute.paneId)?.relayId).isEqualTo("r2")
        val finished = delivery.commands(payload(CATEGORY_FINISHED)).single() as NotificationCommand.Post
        val shade = mutableMapOf(first.notificationId to first, second.notificationId to second, finished.notificationId to finished)
        val cancels = delivery.commands(payload().copy(retract = true)).filterIsInstance<NotificationCommand.Cancel>()
        assertThat(cancels.map { it.notificationId }).containsExactly(first.notificationId, finished.notificationId)
        cancels.forEach { shade.remove(it.notificationId) }
        assertThat(shade.values).containsExactly(second)
    }

    @Test
    fun `missing foreign invitation and ambiguous device owners cannot post or retract`() = runTest {
        seed("r1" to "dev-1", "r2" to "dev-1", "r3" to "dev-3", "r4" to null)
        val delivery = Delivery(this, tmp.root, store())
        for (device in listOf("", "unknown", RelayInvitation.BOOTSTRAP_ID, "dev-1")) {
            for (category in listOf(CATEGORY_ATTENTION, CATEGORY_FINISHED, CATEGORY_TEST, CATEGORY_UPDATE)) {
                assertThat(delivery.commands(payload(category, device))).isEmpty()
                assertThat(delivery.commands(payload(category, device).copy(retract = true))).isEmpty()
            }
        }
        val known = delivery.commands(payload(deviceId = "dev-3")).single() as NotificationCommand.Post
        delivery.row("r3", "blocked")
        val route = LerdrDeepLinks.match(known.deepLink) as LerdrKey.AgentFeed
        assertThat(delivery.agents.agentNow(route.paneId)?.relayId).isEqualTo("r3")
    }

    @Test
    fun `known paneless delivery uses safe destinations and never retracts pane slots`() = runTest {
        seed("r1" to "dev-1")
        val delivery = Delivery(this, tmp.root, store())
        val post = delivery.commands(payload(paneId = "")).single() as NotificationCommand.Post
        assertThat(LerdrDeepLinks.match(post.deepLink)).isEqualTo(LerdrKey.Home)
        assertThat(delivery.commands(payload(paneId = "").copy(retract = true))).isEmpty()
        for ((category, id) in listOf(CATEGORY_TEST to NotifyIds.PUSH_TEST, CATEGORY_UPDATE to NotifyIds.APP_UPDATE)) {
            val settings = delivery.commands(payload(category, paneId = "")).single() as NotificationCommand.Post
            assertThat(settings.notificationId).isEqualTo(id)
            assertThat(LerdrDeepLinks.match(settings.deepLink)).isEqualTo(LerdrKey.Settings)
        }
        assertThat(delivery.commands(payload().copy(title = "", body = ""))).isEmpty()
    }

    @Test
    fun `parsing retains signed event reference and ignores future fields`() {
        val parsed = PushPayload.parse("""{"v":1,"category":"question","key":{"device_id":"dev-1","pane_id":"wG:p3","interaction_revision":4},"event_ref":"signed-reference","future":{}}""".toByteArray())!!
        assertThat(parsed.eventRef).isEqualTo("signed-reference")
        assertThat(parsed.key.paneId).isEqualTo("wG:p3")
        assertThat(parsed.key.interactionRevision).isEqualTo(4)
        assertThat(PushPayload.parse("not json".toByteArray())).isNull()
    }
}
