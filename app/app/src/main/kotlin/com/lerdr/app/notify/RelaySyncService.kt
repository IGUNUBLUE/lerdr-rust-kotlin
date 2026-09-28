package com.lerdr.app.notify

import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.IBinder
import androidx.core.app.ServiceCompat
import com.lerdr.app.push.PushSubscriptionManager
import dagger.hilt.android.AndroidEntryPoint
import javax.inject.Inject
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.FlowPreview
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.debounce
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.launch
import lerdr.core.store.AgentStore
import lerdr.core.store.ConnectionStore
import lerdr.core.store.RelayStatus

/**
 * Foreground pin for live relay sessions (`foregroundServiceType=dataSync`,
 * docs/04: "the socket *is* the realtime channel" — this service does no
 * sync work of its own, it only keeps the process eligible to keep the
 * websockets connected while the app is backgrounded).
 *
 * Lifecycle contract (driven by `PushSubscriptionManager`'s pin policy):
 * started when a relay is CONNECTED **and** no push endpoint can reach a
 * dead process yet — the pin is the fallback channel, not the default.
 * A subscribed UnifiedPush endpoint makes the process free to die:
 * distributor push wakes it back up. The ongoing notification is a
 * one-line `N relays · M agents` rollup on the low `service` channel.
 *
 * `onTaskRemoved` re-arms the pin — but only while push can't cover
 * dead-process delivery; a subscribed push channel means task removal
 * should leave the process (and the pin) free to die.
 */
@AndroidEntryPoint
class RelaySyncService : Service() {

    @Inject
    lateinit var agentStore: AgentStore

    @Inject
    lateinit var connectionStore: ConnectionStore

    @Inject
    lateinit var notifier: LerdrNotifier

    @Inject
    lateinit var pushSubscriptions: PushSubscriptionManager

    private val serviceScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    @OptIn(FlowPreview::class)
    override fun onCreate() {
        super.onCreate()
        notifier.ensureChannels()
        try {
            ServiceCompat.startForeground(
                this,
                NotifyIds.SERVICE,
                notifier.serviceNotification(
                    agentCount = agentStore.agents.value.size,
                    relayCount = connectedRelays(),
                ),
                ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC,
            )
        } catch (_: IllegalStateException) {
            // API 31+ ForegroundServiceStartNotAllowedException lands HERE,
            // not at the caller: `startForegroundService` is accepted while
            // the exemption still holds, then expires before onCreate runs
            // (app slipped to background mid-start). Uncaught it crashes the
            // process in handleCreateService — seen live in v0.0.15. Degrade
            // to no pin: stop before the FGS-start watchdog can ANR us.
            stopSelf()
            return
        }
        // Keep the rollup honest — and re-evaluate the pin predicate
        // continuously: a system-restarted service (START_STICKY) must
        // not outlive push coverage just because the policy collector
        // in this fresh process never started it.
        serviceScope.launch {
            combine(
                agentStore.agents,
                connectionStore.connections,
                pushSubscriptions.uiState,
            ) { agents, connections, push ->
                Triple(
                    agents.size,
                    connections.values.count { it.status == RelayStatus.CONNECTED },
                    push.deliversWhileDead,
                )
            }
                .distinctUntilChanged()
                // A stop must settle before it can self-stop — the pin
                // policy debounces the same flap window on its side.
                .debounce { (_, connected, delivers) ->
                    if (PushSubscriptionManager.shouldPin(connected > 0, delivers)) {
                        0L
                    } else {
                        STOP_DEBOUNCE_MS
                    }
                }
                .collect { (agentCount, connected, delivers) ->
                    if (!PushSubscriptionManager.shouldPin(connected > 0, delivers)) {
                        stopSelf()
                    } else {
                        notifier.updateServiceNotification(agentCount, connected)
                    }
                }
        }
    }

    /** Sticky: if the system kills us while sessions live, come back. */
    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            // Scoped stop: if a newer command (e.g. a start from a session
            // that re-connected mid-flap) already queued, this stop is
            // stale and the service stays up.
            stopSelf(startId)
            return START_NOT_STICKY
        }
        return START_STICKY
    }

    override fun onTaskRemoved(rootIntent: Intent?) {
        // Push covers dead-process delivery — the user's swipe should
        // leave us unpinned (the pin-policy collector stops or never
        // started us); without it, sessions keep running so re-arm.
        if (!pushSubscriptions.uiState.value.deliversWhileDead) start(this)
    }

    override fun onDestroy() {
        serviceScope.cancel()
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    private fun connectedRelays(): Int =
        connectionStore.connections.value.values.count {
            it.status == RelayStatus.CONNECTED
        }

    companion object {
        fun intent(context: Context): Intent = Intent(context, RelaySyncService::class.java)

        /**
         * `startForegroundService` — API 31+ can refuse when the caller is
         * already backgrounded ([android.app.ForegroundServiceStartNotAllowedException]);
         * that path is rare (a session reaching CONNECTED almost always
         * follows user-foregrounded pairing/app-open) and non-fatal —
         * attention notifications still post without the pin.
         */
        fun start(context: Context) {
            try {
                context.startForegroundService(intent(context))
            } catch (_: IllegalStateException) {
            }
        }

        /**
         * Stops via a queued [ACTION_STOP] start-command, never
         * `stopService`: tearing the record down between
         * `startForegroundService` and `onCreate`'s `startForeground`
         * makes the platform crash the process with
         * `ForegroundServiceDidNotStartInTimeException` (seen live during
         * pairing, when CONNECTED→CLOSED flapped 5ms after CONNECTED).
         * A command always runs after `startForeground`, so `stopSelf`
         * is safe by construction.
         */
        fun stop(context: Context) {
            try {
                context.startService(intent(context).setAction(ACTION_STOP))
            } catch (_: IllegalStateException) {
                // Backgrounded with the FGS already dead — nothing to stop.
            }
        }

        private const val ACTION_STOP = "com.lerdr.app.notify.STOP"

        /** Mirror of the notifier's settle window for a zero-connection stop. */
        private const val STOP_DEBOUNCE_MS = 3_000L
    }
}
