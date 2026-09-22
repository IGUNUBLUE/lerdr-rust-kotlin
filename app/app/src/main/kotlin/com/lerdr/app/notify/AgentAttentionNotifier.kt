package com.lerdr.app.notify

import android.content.Context
import com.lerdr.app.di.AppScope
import com.lerdr.app.session.SessionRepository
import dagger.hilt.android.qualifiers.ApplicationContext
import java.util.concurrent.atomic.AtomicBoolean
import javax.inject.Inject
import javax.inject.Singleton
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import lerdr.core.store.Agent
import lerdr.core.store.AgentStore
import lerdr.core.store.ConnectionStore
import lerdr.core.store.RelayStatus

/**
 * Notification coordinator — the seam between the session snapshot stream
 * and the shade.
 *
 * Collects [AgentStore.agents] (the same flow [SessionRepository] republishes)
 * and diffs each snapshot against the previous through [AttentionReducer] —
 * the whole post/cancel policy lives there, pure-JVM testable. The resulting
 * [NotificationCommand]s go to [LerdrNotifier].
 *
 * Also owns the [RelaySyncService] contract, kept deliberately minimal:
 * **start on the first CONNECTED relay, stop when none are.** The service
 * keeps the process (and therefore the websocket sessions) alive while the
 * app is backgrounded; the socket is the realtime channel, so there is
 * nothing else for it to do. `startForegroundService` may be rejected on
 * API 31+ if we are already backgrounded — the failure is swallowed there
 * and attention posts keep flowing without the pin.
 *
 * Depends on the stores rather than [SessionRepository] so the DI graph
 * stays acyclic — `di/NotifyModule` can then start the notifier from the
 * same binding that creates the repository.
 */
@Singleton
class AgentAttentionNotifier @Inject constructor(
    @param:ApplicationContext private val context: Context,
    private val agentStore: AgentStore,
    private val connectionStore: ConnectionStore,
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
        scope.launch {
            connectionStore.connections
                .map { connections ->
                    connections.values.count { it.status == RelayStatus.CONNECTED }
                }
                .distinctUntilChanged()
                .collect { connected ->
                    if (connected > 0) {
                        RelaySyncService.start(context)
                    } else {
                        RelaySyncService.stop(context)
                    }
                }
        }
    }
}
