package com.lerdr.app.notify

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import com.lerdr.app.MainActivity
import dagger.hilt.android.qualifiers.ApplicationContext
import javax.inject.Inject
import javax.inject.Singleton

/**
 * The Android half of the notification path — owns the channels and renders
 * [NotificationCommand]s from [AttentionReducer]. Deliberately dumb: every
 * decision (dedupe, collapse, cancel) is made upstream in the pure reducer.
 */
@Singleton
class LerdrNotifier @Inject constructor(
    @param:ApplicationContext private val context: Context,
) {
    private val compat: NotificationManagerCompat = NotificationManagerCompat.from(context)

    /**
     * Declares every channel from docs/04 §Notifications. Channel
     * re-declaration is idempotent — safe to call on each boot and from the
     * service.
     */
    fun ensureChannels() {
        context.getSystemService(NotificationManager::class.java)
            ?.createNotificationChannels(
                listOf(
                    NotificationChannel(
                        NotifyChannel.AGENT_ATTENTION.id,
                        "Agents needing attention",
                        NotificationManager.IMPORTANCE_HIGH,
                    ),
                    NotificationChannel(
                        NotifyChannel.AGENT_ACTIVITY.id,
                        "Agent activity",
                        NotificationManager.IMPORTANCE_LOW,
                    ),
                    NotificationChannel(
                        NotifyChannel.SERVICE.id,
                        "Background connection",
                        NotificationManager.IMPORTANCE_LOW,
                    ),
                ),
            )
    }

    /** POST_NOTIFICATIONS gate — false when denied on Android 13+. */
    fun notificationsEnabled(): Boolean = compat.areNotificationsEnabled()

    /** Executes one reducer batch: cancels first, then posts. */
    fun execute(commands: List<NotificationCommand>) {
        if (commands.isEmpty()) return
        ensureChannels()
        for (command in commands) {
            when (command) {
                is NotificationCommand.Cancel -> compat.cancel(command.notificationId)
                is NotificationCommand.Post -> post(command)
            }
        }
    }

    private fun post(command: NotificationCommand.Post) {
        if (!notificationsEnabled()) return
        val open = Intent(context, MainActivity::class.java).apply {
            data = Uri.parse(command.deepLink)
            addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP)
        }
        val pendingIntent = PendingIntent.getActivity(
            context,
            command.notificationId,
            open,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val notification = NotificationCompat.Builder(context, command.channel.id)
            .setSmallIcon(com.lerdr.app.R.drawable.ic_notification)
            .setContentTitle(command.title)
            .setContentText(command.body)
            .setContentIntent(pendingIntent)
            .setAutoCancel(true)
            .setOnlyAlertOnce(command.onlyAlertOnce)
            .apply {
                if (command.inboxLines.isNotEmpty()) {
                    val style = NotificationCompat.InboxStyle()
                    command.inboxLines.forEach(style::addLine)
                    setStyle(style)
                }
            }
            .build()
        try {
            compat.notify(command.notificationId, notification)
        } catch (_: SecurityException) {
            // Permission revoked between the check and the post — drop it.
        }
    }

    // ── foreground-service pin ────────────────────────────────────────

    /**
     * The [RelaySyncService] persistent notification — low priority, ongoing,
     * one-line rollup of what the pinned process is keeping alive.
     */
    fun serviceNotification(agentCount: Int, relayCount: Int): Notification {
        val open = Intent(context, MainActivity::class.java).apply {
            data = Uri.parse(NotifyDeepLinks.AGENTS)
            addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP)
        }
        val pendingIntent = PendingIntent.getActivity(
            context,
            NotifyIds.SERVICE,
            open,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val relays = if (relayCount == 1) "1 relay" else "$relayCount relays"
        val agents = if (agentCount == 1) "1 agent" else "$agentCount agents"
        return NotificationCompat.Builder(context, NotifyChannel.SERVICE.id)
            .setSmallIcon(com.lerdr.app.R.drawable.ic_notification)
            .setContentTitle("Lerdr")
            .setContentText("$relays connected · $agents")
            .setContentIntent(pendingIntent)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setCategory(NotificationCompat.CATEGORY_SERVICE)
            .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
            .build()
    }

    /** In-place refresh of the service pin; no-op while notifications are off. */
    fun updateServiceNotification(agentCount: Int, relayCount: Int) {
        if (!notificationsEnabled()) return
        try {
            compat.notify(NotifyIds.SERVICE, serviceNotification(agentCount, relayCount))
        } catch (_: SecurityException) {
            // Permission revoked mid-flight — the FGS pin itself still holds.
        }
    }
}
