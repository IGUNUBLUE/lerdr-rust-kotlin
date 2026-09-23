package com.lerdr.app.notify

import androidx.compose.runtime.Immutable
import java.net.URLEncoder
import lerdr.core.model.BlockedMessage
import lerdr.core.store.Agent
import lerdr.core.store.AgentStatusGroup
import lerdr.core.store.agentStatusGroup
import lerdr.core.store.attentionKind

/**
 * docs/04-app-design.md §Notifications — one channel per urgency.
 * [id] is the platform channel id declared on the [LerdrNotifier] side.
 */
enum class NotifyChannel(val id: String) {
    /** High — a blocked agent is waiting on the user (approval/question). */
    AGENT_ATTENTION("agent_attention"),

    /** Low — completions and other FYI transitions. */
    AGENT_ACTIVITY("agent_activity"),

    /** Low — the foreground-service pin. */
    SERVICE("service"),
}

/**
 * What one pane's notification-relevant state is for a single snapshot.
 * Derived through the store's own `agentStatusGroup`/`attentionKind` so the
 * shade mirrors the needs-you rail's semantics exactly.
 */
enum class AttentionSignal {
    /** blocked + `attention_kind=approval` — an allow/deny prompt. */
    APPROVAL,

    /** blocked + `attention_kind=question` — a structured question card. */
    QUESTION,

    /** blocked with an unrecognized kind — needs inspection, not an answer. */
    ATTENTION,

    /** done/complete/finish — completed work; informational, low channel. */
    FINISHED,
}

/**
 * The shade mutation set one store-snapshot diff produces — the pure-JVM
 * half of the notification path. [Post] carries fully rendered copy so the
 * Android side ([LerdrNotifier]) stays a dumb executor and tests assert on
 * text, not mocks.
 */
@Immutable
sealed interface NotificationCommand {
    val notificationId: Int

    data class Post(
        override val notificationId: Int,
        val channel: NotifyChannel,
        val title: String,
        val body: String,
        /** `lerdr://` URI the tap PendingIntent carries. */
        val deepLink: String,
        /** Expanded inbox lines — set only on the collapsed summary. */
        val inboxLines: List<String> = emptyList(),
        /** True for in-place summary refreshes that must not re-alert. */
        val onlyAlertOnce: Boolean = false,
    ) : NotificationCommand

    data class Cancel(override val notificationId: Int) : NotificationCommand
}

/**
 * Deterministic notification ids — derived from the client-scoped pane id so
 * re-posts update in place instead of stacking. The hash slice is wide
 * enough that collisions across panes are vanishingly rare for the pane
 * counts a phone actually drives.
 */
object NotifyIds {
    /** The foreground-service pin (used by [RelaySyncService]). */
    const val SERVICE = 40_000

    /** The collapsed "N agents need attention" card. */
    const val SUMMARY = 40_001

    private const val SPAN = 900_000
    private const val ATTENTION_BASE = 41_000_000
    private const val FINISHED_BASE = 42_000_000

    fun attention(paneId: String): Int =
        ATTENTION_BASE + Math.floorMod(paneId.hashCode(), SPAN)

    fun finished(paneId: String): Int =
        FINISHED_BASE + Math.floorMod(paneId.hashCode(), SPAN)
}

/**
 * `lerdr://` URIs carried by tap PendingIntents.
 *
 * `lerdr://agent?pane_id=<clientPaneId>` is the pane-targeted form the
 * notification contract owns. `LerdrDeepLinks.match` routes it to
 * `LerdrKey.AgentFeed` and `lerdr://agents` to Home, so a tap lands directly
 * in the relevant session; unknown `lerdr://` hosts resolve to null and
 * MainActivity falls back to mission control.
 */
object NotifyDeepLinks {
    const val AGENT = "lerdr://agent"
    const val AGENTS = "lerdr://agents"
    const val PARAM_PANE_ID = "pane_id"

    fun agent(paneId: String): String =
        "$AGENT?$PARAM_PANE_ID=${URLEncoder.encode(paneId, Charsets.UTF_8.name())}"
}

/**
 * Transition → notification reducer. A pure function of two consecutive
 * [AgentStore.agents] snapshots: every dedupe, rate-limit and collapse
 * decision is derivable from prev→current alone, so the whole policy is
 * unit-testable without Android.
 *
 * Contract:
 * - **Dedupe key** is `(paneId, signal[, eventId])` — one notification per
 *   state *entry*. Staying blocked does not re-notify; a new blocking event
 *   (fresh `event_id`) on an already-blocked pane does.
 * - **Collapse**: while ≥2 panes sit in an attention signal, the shade shows
 *   one summary card ([NotifyIds.SUMMARY]) and no per-pane attention cards.
 *   Dropping back to one restores that pane's individual card.
 * - **FINISHED is event-scoped**: a pane must be observed transitioning
 *   *into* done — a cold-start snapshot row that is already finished is
 *   history, not an event, and does not post. Attention signals are
 *   state-scoped instead: a pane first *seen* blocked does post, because it
 *   needs the user now regardless of when it blocked (reconnect while
 *   blocked is the core scenario).
 * - **Exits cancel**: a pane that resolves, resumes, or disappears (relay
 *   teardown tombstones its rows) loses its shade entry — stale "needs
 *   attention" cards are worse than none.
 */
object AttentionReducer {

    fun reduce(previous: List<Agent>, current: List<Agent>): List<NotificationCommand> {
        val prev = signals(previous)
        val curr = signals(current)
        val prevPaneIds = previous.mapTo(HashSet()) { it.paneId }
        val commands = mutableListOf<NotificationCommand>()

        val prevAttention = prev.filterValues { it.signal != AttentionSignal.FINISHED }
        val currAttention = curr.filterValues { it.signal != AttentionSignal.FINISHED }

        // 1. Exits — a pane that left its notified state loses its entry.
        //    attention↔attention kind changes are NOT exits (same shade slot;
        //    the post below updates it in place), while attention→finished is.
        for ((paneId, prevRow) in prev) {
            val currRow = curr[paneId]
            val exited = when {
                currRow == null -> true
                prevRow.signal == AttentionSignal.FINISHED ->
                    currRow.signal != AttentionSignal.FINISHED
                else -> currRow.signal == AttentionSignal.FINISHED
            }
            if (exited) {
                commands += NotificationCommand.Cancel(idFor(prevRow.signal, paneId))
            }
        }

        // 2. Completions — event-scoped: only a pane we already knew can
        //    "finish". Individual low-priority posts; never collapsed.
        //    (attention→finished exits already canceled the attention card
        //    in step 1.)
        for (currRow in curr.values) {
            if (currRow.signal != AttentionSignal.FINISHED) continue
            // First-sight done rows are history, not completions — the pane
            // must have been *seen* (any prior row) to "finish".
            if (currRow.agent.paneId !in prevPaneIds) continue
            if (prev[currRow.agent.paneId]?.signal == AttentionSignal.FINISHED) continue
            commands += postFinished(currRow.agent)
        }

        // 3. Attention — collapse to a single summary while ≥2 panes wait.
        when {
            currAttention.isEmpty() -> {
                if (prevAttention.size >= 2) {
                    // A summary only ever existed while ≥2 panes waited —
                    // lone-pane exits have no summary to cancel.
                    commands += NotificationCommand.Cancel(NotifyIds.SUMMARY)
                }
            }
            currAttention.size == 1 -> {
                val (paneId, row) = currAttention.entries.single()
                if (prevAttention.size >= 2) {
                    // Uncollapse — the lone survivor gets its own card back.
                    commands += NotificationCommand.Cancel(NotifyIds.SUMMARY)
                }
                if (prev[paneId]?.key != row.key || prevAttention.size >= 2) {
                    commands += postAttention(row.agent, row.signal)
                }
            }
            else -> {
                val changed = currAttention.size != prevAttention.size ||
                    currAttention.any { (paneId, row) -> prevAttention[paneId]?.key != row.key }
                if (changed) {
                    // Fold any standing individuals into the summary first.
                    for (paneId in currAttention.keys) {
                        commands += NotificationCommand.Cancel(NotifyIds.attention(paneId))
                    }
                    commands += postSummary(
                        currAttention.values.map { it.agent to it.signal },
                    )
                }
            }
        }
        return commands
    }

    // ── signal derivation ─────────────────────────────────────────────

    @Immutable
    private class Row(val agent: Agent, val signal: AttentionSignal, val key: String)

    /** paneId → notified state, in snapshot order for deterministic output. */
    private fun signals(agents: List<Agent>): Map<String, Row> {
        val out = LinkedHashMap<String, Row>()
        for (agent in agents) {
            val signal = signalOf(agent) ?: continue
            out[agent.paneId] = Row(agent, signal, keyOf(agent, signal))
        }
        return out
    }

    private fun signalOf(agent: Agent): AttentionSignal? = when (agentStatusGroup(agent)) {
        AgentStatusGroup.BLOCKED -> when (attentionKind(agent)) {
            BlockedMessage.ATTENTION_APPROVAL -> AttentionSignal.APPROVAL
            else -> AttentionSignal.QUESTION
        }
        AgentStatusGroup.ATTENTION -> AttentionSignal.ATTENTION
        AgentStatusGroup.DONE -> AttentionSignal.FINISHED
        else -> null
    }

    /**
     * The dedupe key. Attention signals carry the blocking `event_id` so a
     * *new* approval on an already-blocked pane re-notifies (fresh event,
     * fresh card) while frames repeating the same event stay silent.
     * FINISHED ignores `event_id` — it can retain a stale blocked id.
     */
    private fun keyOf(agent: Agent, signal: AttentionSignal): String =
        if (signal == AttentionSignal.FINISHED) "FINISHED"
        else "${signal.name}|${agent.eventId.orEmpty()}"

    private fun idFor(signal: AttentionSignal, paneId: String): Int =
        if (signal == AttentionSignal.FINISHED) NotifyIds.finished(paneId)
        else NotifyIds.attention(paneId)

    // ── copy ──────────────────────────────────────────────────────────

    private fun displayName(agent: Agent): String =
        agent.name?.takeIf(String::isNotBlank)
            ?: agent.agent?.takeIf(String::isNotBlank)
            ?: agent.rawPaneId.takeIf(String::isNotBlank)
            ?: "Agent"

    /** `relay ▸ project` context line, empty when neither is known. */
    private fun contextLine(agent: Agent): String = listOfNotNull(
        agent.relayLabel.takeIf(String::isNotBlank),
        agent.project?.takeIf(String::isNotBlank),
    ).joinToString(" ▸ ")

    /** The most informative payload the row carries, truncated for the shade. */
    private fun preview(agent: Agent): String? =
        agent.interaction?.question?.takeIf(String::isNotBlank)
            ?: agent.prompt?.takeIf(String::isNotBlank)
            ?: agent.command?.takeIf(String::isNotBlank)

    private fun truncate(text: String): String =
        if (text.length <= 140) text else text.take(137).trimEnd() + "…"

    private fun bodyOf(agent: Agent): String =
        preview(agent)?.let(::truncate)
            ?: contextLine(agent).ifEmpty { "Tap to open" }

    private fun signalLabel(signal: AttentionSignal): String = when (signal) {
        AttentionSignal.APPROVAL -> "needs approval"
        AttentionSignal.QUESTION -> "has a question"
        AttentionSignal.ATTENTION -> "needs attention"
        AttentionSignal.FINISHED -> "finished"
    }

    private fun postAttention(agent: Agent, signal: AttentionSignal): NotificationCommand.Post =
        NotificationCommand.Post(
            notificationId = NotifyIds.attention(agent.paneId),
            channel = NotifyChannel.AGENT_ATTENTION,
            title = "${displayName(agent)} ${signalLabel(signal)}",
            body = bodyOf(agent),
            deepLink = NotifyDeepLinks.agent(agent.paneId),
        )

    private fun postFinished(agent: Agent): NotificationCommand.Post =
        NotificationCommand.Post(
            notificationId = NotifyIds.finished(agent.paneId),
            channel = NotifyChannel.AGENT_ACTIVITY,
            title = "${displayName(agent)} finished",
            body = bodyOf(agent),
            deepLink = NotifyDeepLinks.agent(agent.paneId),
        )

    private fun postSummary(
        rows: List<Pair<Agent, AttentionSignal>>,
    ): NotificationCommand.Post {
        val lines = rows.map { (agent, signal) ->
            "${displayName(agent)} — ${signalLabel(signal)}"
        }
        return NotificationCommand.Post(
            notificationId = NotifyIds.SUMMARY,
            channel = NotifyChannel.AGENT_ATTENTION,
            title = "${rows.size} agents need attention",
            body = lines.joinToString(", "),
            deepLink = NotifyDeepLinks.AGENTS,
            inboxLines = lines,
            onlyAlertOnce = true,
        )
    }
}
