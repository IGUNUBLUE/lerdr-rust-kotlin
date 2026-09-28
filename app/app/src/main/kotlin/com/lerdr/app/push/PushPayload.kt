package com.lerdr.app.push

import com.lerdr.app.notify.NotificationCommand
import com.lerdr.app.notify.NotifyChannel
import com.lerdr.app.notify.NotifyDeepLinks
import com.lerdr.app.notify.NotifyIds
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/**
 * The decrypted Web Push record the relay's `push_delivery` worker seals —
 * `push.Payload` v1: signed deep-link reference + localized title/body
 * (`relay/crates/lerdr-coord/src/actions/push.rs` `build_payload`).
 *
 * Fields mirror the wire verbatim; unknown fields are ignored so a newer
 * relay payload still parses.
 */
@Serializable
data class PushPayload(
    val v: Int = 0,
    val category: String = "",
    val key: PushEventKey = PushEventKey(),
    val title: String = "",
    val body: String = "",
    val tag: String = "",
    val url: String = "",
    @SerialName("event_ref") val eventRef: String = "",
    val retract: Boolean = false,
) {
    companion object {
        private val json = Json { ignoreUnknownKeys = true }

        fun parse(content: ByteArray): PushPayload? = try {
            json.decodeFromString(content.decodeToString())
        } catch (_: Exception) {
            null
        }
    }
}

/** `push.PushEventKey` — every field the relay serializes, same names. */
@Serializable
data class PushEventKey(
    @SerialName("device_id") val deviceId: String = "",
    @SerialName("server_session_id") val serverSessionId: String = "",
    @SerialName("pane_id") val paneId: String = "",
    @SerialName("terminal_id") val terminalId: String = "",
    @SerialName("agent_session_id") val agentSessionId: String = "",
    val generation: Long = 0,
    @SerialName("event_id") val eventId: String = "",
    @SerialName("interaction_revision") val interactionRevision: Long = 0,
    val category: String = "",
)

/**
 * Payload → shade commands. Pure: the pane-scoped ids reuse
 * [NotifyIds.attention]/[NotifyIds.finished] so a push-rendered card and a
 * socket-driven card occupy the same slot — last write wins, never a
 * duplicate. Retractions cancel both pane slots (the retracted event's
 * category may differ from the originally posted one).
 */
fun PushPayload.toCommands(): List<NotificationCommand> {
    val paneId = key.paneId
    if (retract) {
        if (paneId.isEmpty()) return emptyList()
        return listOf(
            NotificationCommand.Cancel(NotifyIds.attention(paneId)),
            NotificationCommand.Cancel(NotifyIds.finished(paneId)),
        )
    }
    if (title.isEmpty() && body.isEmpty()) return emptyList()
    return when (category) {
        CATEGORY_FINISHED -> listOf(
            NotificationCommand.Post(
                notificationId = NotifyIds.finished(paneId.ifEmpty { eventRef }),
                channel = NotifyChannel.AGENT_ACTIVITY,
                title = title,
                body = body,
                deepLink = paneTarget(paneId),
                onlyAlertOnce = true,
            ),
        )
        CATEGORY_UPDATE -> listOf(
            NotificationCommand.Post(
                notificationId = NotifyIds.APP_UPDATE,
                channel = NotifyChannel.APP_UPDATE,
                title = title,
                body = body,
                deepLink = NotifyDeepLinks.SETTINGS,
                onlyAlertOnce = true,
            ),
        )
        CATEGORY_TEST -> listOf(
            NotificationCommand.Post(
                notificationId = NotifyIds.PUSH_TEST,
                channel = NotifyChannel.AGENT_ATTENTION,
                title = title,
                body = body,
                deepLink = NotifyDeepLinks.SETTINGS,
            ),
        )
        // attention / question / brief — the needs-you family.
        else -> listOf(
            NotificationCommand.Post(
                notificationId = NotifyIds.attention(paneId.ifEmpty { eventRef }),
                channel = NotifyChannel.AGENT_ATTENTION,
                title = title,
                body = body,
                deepLink = paneTarget(paneId),
            ),
        )
    }
}

private fun paneTarget(paneId: String): String =
    if (paneId.isEmpty()) NotifyDeepLinks.AGENTS else NotifyDeepLinks.agent(paneId)

const val CATEGORY_ATTENTION = "attention"
const val CATEGORY_QUESTION = "question"
const val CATEGORY_BRIEF = "brief"
const val CATEGORY_FINISHED = "finished"
const val CATEGORY_UPDATE = "update"
const val CATEGORY_TEST = "test"
