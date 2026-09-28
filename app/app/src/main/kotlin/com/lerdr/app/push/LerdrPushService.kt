package com.lerdr.app.push

import dagger.hilt.android.AndroidEntryPoint
import javax.inject.Inject
import org.unifiedpush.android.connector.FailedReason
import org.unifiedpush.android.connector.PushService
import org.unifiedpush.android.connector.data.PushEndpoint
import org.unifiedpush.android.connector.data.PushMessage

/**
 * UnifiedPush entry point — the connector binds here (same-uid enforced by
 * [PushService.PushBinder]) whenever the distributor pushes an event. All
 * work delegates to [PushSubscriptionManager]; this class stays a thin
 * Android-facing shell.
 */
@AndroidEntryPoint
class LerdrPushService : PushService() {

    @Inject
    lateinit var subscriptions: PushSubscriptionManager

    override fun onNewEndpoint(endpoint: PushEndpoint, instance: String) {
        subscriptions.onNewEndpoint(endpoint, instance)
    }

    override fun onMessage(message: PushMessage, instance: String) {
        subscriptions.onMessage(message, instance)
    }

    override fun onRegistrationFailed(reason: FailedReason, instance: String) {
        subscriptions.onRegistrationFailed(reason, instance)
    }

    override fun onUnregistered(instance: String) {
        subscriptions.onUnregistered(instance)
    }

    override fun onTempUnavailable(instance: String) {
        subscriptions.onTempUnavailable(instance)
    }
}
