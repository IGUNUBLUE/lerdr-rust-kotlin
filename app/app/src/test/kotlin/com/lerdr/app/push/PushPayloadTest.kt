package com.lerdr.app.push

import com.google.common.truth.Truth.assertThat
import com.lerdr.app.notify.NotificationCommand
import com.lerdr.app.notify.NotifyChannel
import com.lerdr.app.notify.NotifyDeepLinks
import com.lerdr.app.notify.NotifyIds
import org.junit.Test

/**
 * `push.Payload` v1 → shade commands — the pane-scoped ids must collide
 * with the socket-driven cards ([NotifyIds.attention]/[NotifyIds.finished])
 * so push and websocket never double-post.
 */
class PushPayloadTest {

    private fun payload(
        category: String = CATEGORY_ATTENTION,
        paneId: String = "wG:p3",
        retract: Boolean = false,
    ) = PushPayload(
        v = 1,
        category = category,
        key = PushEventKey(
            deviceId = "dev-1",
            serverSessionId = "srv-1",
            paneId = paneId,
            terminalId = "t-1",
            eventId = "evt-1",
            category = category,
        ),
        title = "Response needed",
        body = "Open the app to review and respond",
        tag = "tag-1",
        url = "./#push=ref-1",
        eventRef = "ref-1",
        retract = retract,
    )

    @Test
    fun parsesRelayPayload() {
        val wire = """{
            "v":1,"category":"question",
            "key":{"device_id":"dev-1","server_session_id":"srv-1",
                   "pane_id":"wG:p3","terminal_id":"t-1",
                   "agent_session_id":"as-1","generation":2,
                   "event_id":"evt-9","interaction_revision":4,
                   "category":"question"},
            "title":"Response needed","body":"Open the app to review and respond",
            "tag":"t","url":"./#push=abc","actions":[],"action_refs":{},
            "event_ref":"abc"
        }""".toByteArray()
        val parsed = PushPayload.parse(wire)!!
        assertThat(parsed.v).isEqualTo(1)
        assertThat(parsed.category).isEqualTo(CATEGORY_QUESTION)
        assertThat(parsed.key.paneId).isEqualTo("wG:p3")
        assertThat(parsed.key.interactionRevision).isEqualTo(4)
        assertThat(parsed.eventRef).isEqualTo("abc")
        assertThat(parsed.retract).isFalse()
    }

    @Test
    fun attentionPostsOnHighChannelWithAgentDeepLink() {
        val commands = payload(category = CATEGORY_ATTENTION).toCommands()
        val post = commands.single() as NotificationCommand.Post
        assertThat(post.notificationId).isEqualTo(NotifyIds.attention("wG:p3"))
        assertThat(post.channel).isEqualTo(NotifyChannel.AGENT_ATTENTION)
        assertThat(post.deepLink).isEqualTo(NotifyDeepLinks.agent("wG:p3"))
    }

    @Test
    fun questionAndBriefShareTheAttentionSlot() {
        for (category in listOf(CATEGORY_QUESTION, CATEGORY_BRIEF)) {
            val post = payload(category = category).toCommands()
                .single() as NotificationCommand.Post
            assertThat(post.notificationId).isEqualTo(NotifyIds.attention("wG:p3"))
            assertThat(post.channel).isEqualTo(NotifyChannel.AGENT_ATTENTION)
        }
    }

    @Test
    fun finishedPostsOnLowChannel() {
        val post = payload(category = CATEGORY_FINISHED).toCommands()
            .single() as NotificationCommand.Post
        assertThat(post.notificationId).isEqualTo(NotifyIds.finished("wG:p3"))
        assertThat(post.channel).isEqualTo(NotifyChannel.AGENT_ACTIVITY)
        assertThat(post.onlyAlertOnce).isTrue()
    }

    @Test
    fun testCategoryPostsOnceToSettings() {
        val post = payload(category = CATEGORY_TEST, paneId = "").toCommands()
            .single() as NotificationCommand.Post
        assertThat(post.notificationId).isEqualTo(NotifyIds.PUSH_TEST)
        assertThat(post.deepLink).isEqualTo(NotifyDeepLinks.SETTINGS)
    }

    @Test
    fun updatePostsToAppUpdateSlot() {
        val post = payload(category = CATEGORY_UPDATE, paneId = "").toCommands()
            .single() as NotificationCommand.Post
        assertThat(post.notificationId).isEqualTo(NotifyIds.APP_UPDATE)
        assertThat(post.channel).isEqualTo(NotifyChannel.APP_UPDATE)
    }

    @Test
    fun retractCancelsBothPaneSlots() {
        val commands = payload(retract = true).toCommands()
        assertThat(commands).containsExactly(
            NotificationCommand.Cancel(NotifyIds.attention("wG:p3")),
            NotificationCommand.Cancel(NotifyIds.finished("wG:p3")),
        )
    }

    @Test
    fun retractWithoutPaneIsNoop() {
        assertThat(payload(retract = true, paneId = "").toCommands()).isEmpty()
    }

    @Test
    fun panelessPostFallsBackToAgents() {
        val post = payload(paneId = "").toCommands()
            .single() as NotificationCommand.Post
        assertThat(post.deepLink).isEqualTo(NotifyDeepLinks.AGENTS)
    }

    @Test
    fun garbageParsesToNull() {
        assertThat(PushPayload.parse("not json".toByteArray())).isNull()
        assertThat(PushPayload.parse("{}".toByteArray())).isNotNull()
    }

    @Test
    fun emptyTextPostsNothing() {
        assertThat(payload().copy(title = "", body = "").toCommands()).isEmpty()
    }
}
