package com.lerdr.app.notify

import com.google.common.truth.Truth.assertThat
import lerdr.core.model.BlockedMessage
import lerdr.core.store.Agent
import org.junit.Test

/**
 * Pure-JVM coverage of the transition → notification contract:
 * dedupe keys, summary collapse/uncollapse, exits, and the asymmetric
 * first-sight rule (blocked = state, finished = event).
 */
class AttentionReducerTest {

    private fun agent(
        pane: String,
        status: String?,
        attentionKind: String? = null,
        eventId: String? = null,
        name: String? = null,
        prompt: String? = null,
    ): Agent = Agent(
        relayId = RELAY,
        relayLabel = "workstation",
        rawPaneId = pane,
        paneId = "$RELAY::$pane",
        agent = "claude",
        name = name,
        status = status,
        attentionKind = attentionKind,
        eventId = eventId,
        prompt = prompt,
    )

    private fun blockedApproval(pane: String, eventId: String? = "e1") =
        agent(pane, "blocked", BlockedMessage.ATTENTION_APPROVAL, eventId)

    private fun blockedQuestion(pane: String, eventId: String? = "e1") =
        agent(pane, "blocked", BlockedMessage.ATTENTION_QUESTION, eventId)

    private fun working(pane: String) = agent(pane, "working")
    private fun done(pane: String) = agent(pane, "done")
    private fun idle(pane: String) = agent(pane, "idle")

    private fun posts(commands: List<NotificationCommand>) =
        commands.filterIsInstance<NotificationCommand.Post>()

    private fun cancels(commands: List<NotificationCommand>) =
        commands.filterIsInstance<NotificationCommand.Cancel>()

    // ── no-notification cases ─────────────────────────────────────────

    @Test
    fun `empty to empty emits nothing`() {
        assertThat(AttentionReducer.reduce(emptyList(), emptyList())).isEmpty()
    }

    @Test
    fun `ordinary status churn emits nothing`() {
        val before = listOf(idle("p1"), working("p2"))
        val after = listOf(working("p1"), idle("p2"))
        assertThat(AttentionReducer.reduce(before, after)).isEmpty()
    }

    @Test
    fun `identical snapshots emit nothing`() {
        val agents = listOf(blockedApproval("p1"), done("p2"), working("p3"))
        assertThat(AttentionReducer.reduce(agents, agents)).isEmpty()
    }

    @Test
    fun `blocked pane staying blocked with same event stays silent`() {
        val before = listOf(blockedApproval("p1", eventId = "e1"))
        val after = listOf(blockedApproval("p1", eventId = "e1"))
        assertThat(AttentionReducer.reduce(before, after)).isEmpty()
    }

    @Test
    fun `chat attention kind is not notification-worthy`() {
        // blocked+chat maps to READY in agentStatusGroup — oracle semantics.
        val before = listOf(working("p1"))
        val after = listOf(agent("p1", "blocked", BlockedMessage.ATTENTION_CHAT))
        assertThat(AttentionReducer.reduce(before, after)).isEmpty()
    }

    @Test
    fun `first-sight finished row is history, not an event`() {
        val commands = AttentionReducer.reduce(emptyList(), listOf(done("p1")))
        assertThat(commands).isEmpty()
    }

    // ── attention transitions ─────────────────────────────────────────

    @Test
    fun `newly blocked approval posts a high-priority card`() {
        val commands = AttentionReducer.reduce(
            listOf(working("p1")),
            listOf(blockedApproval("p1")),
        )
        assertThat(commands).containsExactly(
            NotificationCommand.Post(
                notificationId = NotifyIds.attention("r1::p1"),
                channel = NotifyChannel.AGENT_ATTENTION,
                title = "claude needs approval",
                body = "workstation",
                deepLink = "lerdr://agent?pane_id=r1%3A%3Ap1",
            ),
        )
    }

    @Test
    fun `first-sight blocked notifies - state-scoped not event-scoped`() {
        val commands = AttentionReducer.reduce(emptyList(), listOf(blockedQuestion("p1")))
        assertThat(posts(commands)).hasSize(1)
        assertThat(posts(commands).single().title).isEqualTo("claude has a question")
    }

    @Test
    fun `blocked with unknown kind posts needs-attention`() {
        val before = listOf(working("p1"))
        val after = listOf(agent("p1", "blocked", BlockedMessage.ATTENTION_UNKNOWN))
        val posts = posts(AttentionReducer.reduce(before, after))
        assertThat(posts).hasSize(1)
        assertThat(posts.single().title).isEqualTo("claude needs attention")
        assertThat(posts.single().channel).isEqualTo(NotifyChannel.AGENT_ATTENTION)
    }

    @Test
    fun `fresh event id on an already-blocked pane re-notifies`() {
        val before = listOf(blockedApproval("p1", eventId = "e1"))
        val after = listOf(blockedApproval("p1", eventId = "e2"))
        val posts = posts(AttentionReducer.reduce(before, after))
        assertThat(posts).hasSize(1)
        assertThat(posts.single().notificationId).isEqualTo(NotifyIds.attention("r1::p1"))
    }

    @Test
    fun `attention kind change re-notifies on the same shade slot`() {
        val before = listOf(blockedApproval("p1"))
        val after = listOf(blockedQuestion("p1"))
        val commands = AttentionReducer.reduce(before, after)
        // Same notification id — the post updates the card in place; no cancel.
        assertThat(commands).containsExactly(
            NotificationCommand.Post(
                notificationId = NotifyIds.attention("r1::p1"),
                channel = NotifyChannel.AGENT_ATTENTION,
                title = "claude has a question",
                body = "workstation",
                deepLink = "lerdr://agent?pane_id=r1%3A%3Ap1",
            ),
        )
    }

    @Test
    fun `name wins over agent binary in the title`() {
        val before = listOf(working("p1"))
        val after = listOf(blockedApproval("p1").copy(name = "Reviewer"))
        assertThat(posts(AttentionReducer.reduce(before, after)).single().title)
            .isEqualTo("Reviewer needs approval")
    }

    @Test
    fun `approval prompt becomes the body preview`() {
        val before = listOf(working("p1"))
        val after = listOf(blockedApproval("p1").copy(prompt = "Delete build/ ?"))
        assertThat(posts(AttentionReducer.reduce(before, after)).single().body)
            .isEqualTo("Delete build/ ?")
    }

    // ── exits ─────────────────────────────────────────────────────────

    @Test
    fun `resolving a lone blocked pane cancels its card`() {
        val before = listOf(blockedApproval("p1"))
        val after = listOf(working("p1"))
        assertThat(AttentionReducer.reduce(before, after)).containsExactly(
            NotificationCommand.Cancel(NotifyIds.attention("r1::p1")),
        )
    }

    @Test
    fun `pane disappearing while blocked cancels its card`() {
        val before = listOf(blockedApproval("p1"))
        assertThat(AttentionReducer.reduce(before, emptyList())).containsExactly(
            NotificationCommand.Cancel(NotifyIds.attention("r1::p1")),
        )
    }

    @Test
    fun `blocked to finished cancels attention and posts activity`() {
        val before = listOf(blockedApproval("p1"))
        val after = listOf(done("p1"))
        val commands = AttentionReducer.reduce(before, after)
        assertThat(cancels(commands)).containsExactly(
            NotificationCommand.Cancel(NotifyIds.attention("r1::p1")),
        )
        val post = posts(commands).single()
        assertThat(post.channel).isEqualTo(NotifyChannel.AGENT_ACTIVITY)
        assertThat(post.notificationId).isEqualTo(NotifyIds.finished("r1::p1"))
        assertThat(post.title).isEqualTo("claude finished")
    }

    @Test
    fun `finished pane resuming work cancels the finished card`() {
        val before = listOf(done("p1"))
        val after = listOf(working("p1"))
        assertThat(AttentionReducer.reduce(before, after)).containsExactly(
            NotificationCommand.Cancel(NotifyIds.finished("r1::p1")),
        )
    }

    @Test
    fun `staying finished does not re-post`() {
        val before = listOf(done("p1"))
        val after = listOf(done("p1").copy(updatedAt = 999))
        assertThat(AttentionReducer.reduce(before, after)).isEmpty()
    }

    @Test
    fun `finished posts never collapse - two completions post two cards`() {
        val before = listOf(working("p1"), working("p2"))
        val after = listOf(done("p1"), done("p2"))
        val posts = posts(AttentionReducer.reduce(before, after))
        assertThat(posts).hasSize(2)
        assertThat(posts.map { it.channel }.distinct())
            .containsExactly(NotifyChannel.AGENT_ACTIVITY)
    }

    // ── summary collapse ──────────────────────────────────────────────

    @Test
    fun `two panes blocking in one pass collapse to a single summary`() {
        val commands = AttentionReducer.reduce(
            emptyList(),
            listOf(blockedApproval("p1"), blockedQuestion("p2")),
        )
        assertThat(commands).containsExactly(
            NotificationCommand.Cancel(NotifyIds.attention("r1::p1")),
            NotificationCommand.Cancel(NotifyIds.attention("r1::p2")),
            NotificationCommand.Post(
                notificationId = NotifyIds.SUMMARY,
                channel = NotifyChannel.AGENT_ATTENTION,
                title = "2 agents need attention",
                body = "claude — needs approval, claude — has a question",
                deepLink = NotifyDeepLinks.AGENTS,
                inboxLines = listOf("claude — needs approval", "claude — has a question"),
                onlyAlertOnce = true,
            ),
        )
    }

    @Test
    fun `a second blocker folds the standing individual into the summary`() {
        val commands = AttentionReducer.reduce(
            listOf(blockedApproval("p1")),
            listOf(blockedApproval("p1"), blockedQuestion("p2")),
        )
        assertThat(cancels(commands)).containsExactly(
            NotificationCommand.Cancel(NotifyIds.attention("r1::p1")),
            NotificationCommand.Cancel(NotifyIds.attention("r1::p2")),
        )
        val post = posts(commands).single()
        assertThat(post.notificationId).isEqualTo(NotifyIds.SUMMARY)
        assertThat(post.title).isEqualTo("2 agents need attention")
    }

    @Test
    fun `stable summary membership does not re-post`() {
        val agents = listOf(blockedApproval("p1"), blockedQuestion("p2"))
        assertThat(AttentionReducer.reduce(agents, agents)).isEmpty()
    }

    @Test
    fun `summary refreshes when a third agent joins`() {
        val before = listOf(blockedApproval("p1"), blockedQuestion("p2"))
        val after = listOf(blockedApproval("p1"), blockedQuestion("p2"), blockedApproval("p3"))
        val commands = AttentionReducer.reduce(before, after)
        val post = posts(commands).single()
        assertThat(post.notificationId).isEqualTo(NotifyIds.SUMMARY)
        assertThat(post.title).isEqualTo("3 agents need attention")
        assertThat(post.inboxLines).hasSize(3)
    }

    @Test
    fun `dropping to one survivor restores its individual card`() {
        val before = listOf(blockedApproval("p1"), blockedQuestion("p2"))
        val after = listOf(blockedApproval("p1"), working("p2"))
        val commands = AttentionReducer.reduce(before, after)
        assertThat(commands).containsExactly(
            NotificationCommand.Cancel(NotifyIds.attention("r1::p2")),
            NotificationCommand.Cancel(NotifyIds.SUMMARY),
            NotificationCommand.Post(
                notificationId = NotifyIds.attention("r1::p1"),
                channel = NotifyChannel.AGENT_ATTENTION,
                title = "claude needs approval",
                body = "workstation",
                deepLink = "lerdr://agent?pane_id=r1%3A%3Ap1",
            ),
        )
    }

    @Test
    fun `all resolved cancels summary and individuals`() {
        val before = listOf(blockedApproval("p1"), blockedQuestion("p2"))
        val after = listOf(working("p1"), working("p2"))
        assertThat(AttentionReducer.reduce(before, after)).containsExactly(
            NotificationCommand.Cancel(NotifyIds.attention("r1::p1")),
            NotificationCommand.Cancel(NotifyIds.attention("r1::p2")),
            NotificationCommand.Cancel(NotifyIds.SUMMARY),
        )
    }

    @Test
    fun `kind change inside a summary refreshes it silently`() {
        val before = listOf(blockedApproval("p1"), blockedQuestion("p2"))
        val after = listOf(blockedQuestion("p1"), blockedQuestion("p2"))
        val post = posts(AttentionReducer.reduce(before, after)).single()
        assertThat(post.notificationId).isEqualTo(NotifyIds.SUMMARY)
        assertThat(post.onlyAlertOnce).isTrue()
    }

    @Test
    fun `attention and finished coexist on different channels`() {
        val before = listOf(working("p1"), working("p2"))
        val after = listOf(blockedApproval("p1"), done("p2"))
        val posts = posts(AttentionReducer.reduce(before, after))
        assertThat(posts.map { it.channel })
            .containsExactly(NotifyChannel.AGENT_ACTIVITY, NotifyChannel.AGENT_ATTENTION)
    }

    // ── ids and links ─────────────────────────────────────────────────

    @Test
    fun `notification ids are deterministic per pane`() {
        assertThat(NotifyIds.attention("r1::p1")).isEqualTo(NotifyIds.attention("r1::p1"))
        assertThat(NotifyIds.attention("r1::p1")).isNotEqualTo(NotifyIds.finished("r1::p1"))
        assertThat(NotifyIds.attention("r1::p1")).isNotEqualTo(NotifyIds.attention("r1::p2"))
    }

    @Test
    fun `pane deep link carries the url-encoded client pane id`() {
        assertThat(NotifyDeepLinks.agent("r1::p1")).isEqualTo("lerdr://agent?pane_id=r1%3A%3Ap1")
    }

    companion object {
        private const val RELAY = "r1"
    }
}
