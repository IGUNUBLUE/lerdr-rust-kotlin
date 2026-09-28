package com.lerdr.app.notify

import com.lerdr.app.di.AppScope
import com.lerdr.app.session.SessionRepository
import java.util.concurrent.atomic.AtomicBoolean
import javax.inject.Inject
import javax.inject.Singleton
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import lerdr.core.store.Agent
import lerdr.core.store.AgentStore

/**
 * Notification coordinator — the seam between the session snapshot stream
 * and the shade.
 *
 * Collects [AgentStore.agents] (the same flow [SessionRepository] republishes)
 * and diffs each snapshot against the previous through [AttentionReducer] —
 * the whole post/cancel policy lives there, pure-JVM testable. The resulting
 * [NotificationCommand]s go to [LerdrNotifier].
 *
 * The [RelaySyncService] pin policy lives in
 * [com.lerdr.app.push.PushSubscriptionManager]: push reachability decides
 * whether the socket keep-alive runs at all, so the foreground pin is
 * the *fallback* channel — only while no push endpoint can reach a dead
 * process. Keeping that decision next to the subscription state also
 * keeps this class (and `di/NotifyModule`) free of a
 * notifier → push-manager → repository cycle.
 *
 * Depends on the stores rather than [SessionRepository] so the DI graph
 * stays acyclic — `di/NotifyModule` can then start the notifier from the
 * same binding that creates the repository.
 */
@Singleton
class AgentAttentionNotifier @Inject constructor(
    private val agentStore: AgentStore,
    private val notifier: LerdrNotifier,
    @param:AppScope private val scope: CoroutineScope,
) {
    private val started = AtomicBoolean(false)

    /** Boots the collectors. Idempotent — called once per process by DI. */
    fun start() {
        if (!started.compareAndSet(false, true)) return
        notifier.ensureChannels()
        scope.launch {
            var previous = emptyList<Agent>()
            agentStore.agents.collect { current ->
                val commands = AttentionReducer.reduce(previous, current)
                previous = current
                if (commands.isNotEmpty()) notifier.execute(commands)
            }
        }
    }
}
