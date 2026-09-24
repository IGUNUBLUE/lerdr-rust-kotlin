package lerdr.core.protocol

import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import lerdr.core.model.ActionReceiptMessage
import lerdr.core.model.ActivityHistoryMessage
import lerdr.core.model.ActivityMessage
import lerdr.core.model.AgentUpdateMessage
import lerdr.core.model.AgentsMessage
import lerdr.core.model.AppDeployStatusMessage
import lerdr.core.model.BlockedMessage
import lerdr.core.model.CapsUpdateMessage
import lerdr.core.model.CommandResultMessage
import lerdr.core.model.ConversationUpdateMessage
import lerdr.core.model.ErrorMessage
import lerdr.core.model.HerdrStatusMessage
import lerdr.core.model.InventoryStatusMessage
import lerdr.core.model.PaneContentMessage
import lerdr.core.model.PaneDeltaMessage
import lerdr.core.model.PaneProbeMessage
import lerdr.core.model.PaneResyncMessage
import lerdr.core.model.PaneUnchangedMessage
import lerdr.core.model.PushConfigMessage
import lerdr.core.model.PushPolicyMessage
import lerdr.core.model.PushPolicyResultMessage
import lerdr.core.model.PushSubscribedMessage
import lerdr.core.model.PushTestResultMessage
import lerdr.core.model.PushUnsubscribedMessage
import lerdr.core.model.PushViewedPaneResultMessage
import lerdr.core.model.ServerMessage
import lerdr.core.model.SpeechVoicesMessage
import lerdr.core.model.UnknownServerMessage
import lerdr.core.model.UpdateStatusMessage
import lerdr.core.model.UploadBeginResultMessage
import lerdr.core.model.UploadCancelResultMessage
import lerdr.core.model.UploadChunkResultMessage
import lerdr.core.model.UploadFinishResultMessage
import lerdr.core.model.WebRtcAnswerMessage
import lerdr.core.model.WebRtcClosedMessage
import lerdr.core.model.WebRtcIceMessage
import lerdr.core.model.WorkspacesMessage

/**
 * Type-dispatching codec for server→client messages. The `type` field
 * selects the DTO; unrecognized types decode to [UnknownServerMessage]
 * with the raw payload preserved.
 */
object ServerMessageCodec {

    fun decode(payload: String): ServerMessage =
        decode(LerdrJson.parseToJsonElement(payload).jsonObject)

    fun decode(raw: JsonObject): ServerMessage {
        val type = raw["type"]?.jsonPrimitive?.content ?: ""
        return when (type) {
            "action_receipt" -> LerdrJson.decodeFromJsonElement(ActionReceiptMessage.serializer(), raw)
            "activity" -> LerdrJson.decodeFromJsonElement(ActivityMessage.serializer(), raw)
            "activity_history" -> LerdrJson.decodeFromJsonElement(ActivityHistoryMessage.serializer(), raw)
            "agent_update" -> LerdrJson.decodeFromJsonElement(AgentUpdateMessage.serializer(), raw)
            "agents" -> LerdrJson.decodeFromJsonElement(AgentsMessage.serializer(), raw)
            "app_deploy_status" -> LerdrJson.decodeFromJsonElement(AppDeployStatusMessage.serializer(), raw)
            "blocked" -> LerdrJson.decodeFromJsonElement(BlockedMessage.serializer(), raw)
            "caps_update" -> LerdrJson.decodeFromJsonElement(CapsUpdateMessage.serializer(), raw)
            "command_result" -> LerdrJson.decodeFromJsonElement(CommandResultMessage.serializer(), raw)
            "conversation_update" -> LerdrJson.decodeFromJsonElement(ConversationUpdateMessage.serializer(), raw)
            "error" -> LerdrJson.decodeFromJsonElement(ErrorMessage.serializer(), raw)
            "herdr_status" -> LerdrJson.decodeFromJsonElement(HerdrStatusMessage.serializer(), raw)
            "inventory_status" -> LerdrJson.decodeFromJsonElement(InventoryStatusMessage.serializer(), raw)
            "pane_content" -> LerdrJson.decodeFromJsonElement(PaneContentMessage.serializer(), raw)
            "pane_delta" -> LerdrJson.decodeFromJsonElement(PaneDeltaMessage.serializer(), raw)
            "pane_probe" -> LerdrJson.decodeFromJsonElement(PaneProbeMessage.serializer(), raw)
            "pane_resync" -> LerdrJson.decodeFromJsonElement(PaneResyncMessage.serializer(), raw)
            "pane_unchanged" -> LerdrJson.decodeFromJsonElement(PaneUnchangedMessage.serializer(), raw)
            "push_config" -> LerdrJson.decodeFromJsonElement(PushConfigMessage.serializer(), raw)
            "push_policy" -> LerdrJson.decodeFromJsonElement(PushPolicyMessage.serializer(), raw)
            "push_policy_result" -> LerdrJson.decodeFromJsonElement(PushPolicyResultMessage.serializer(), raw)
            "push_subscribed" -> LerdrJson.decodeFromJsonElement(PushSubscribedMessage.serializer(), raw)
            "push_test_result" -> LerdrJson.decodeFromJsonElement(PushTestResultMessage.serializer(), raw)
            "push_unsubscribed" -> LerdrJson.decodeFromJsonElement(PushUnsubscribedMessage.serializer(), raw)
            "push_viewed_pane_result" -> LerdrJson.decodeFromJsonElement(PushViewedPaneResultMessage.serializer(), raw)
            "speech_voices" -> LerdrJson.decodeFromJsonElement(SpeechVoicesMessage.serializer(), raw)
            "update_status" -> LerdrJson.decodeFromJsonElement(UpdateStatusMessage.serializer(), raw)
            "upload_begin_result" -> LerdrJson.decodeFromJsonElement(UploadBeginResultMessage.serializer(), raw)
            "upload_cancel_result" -> LerdrJson.decodeFromJsonElement(UploadCancelResultMessage.serializer(), raw)
            "upload_chunk_result" -> LerdrJson.decodeFromJsonElement(UploadChunkResultMessage.serializer(), raw)
            "upload_finish_result" -> LerdrJson.decodeFromJsonElement(UploadFinishResultMessage.serializer(), raw)
            "webrtc_answer" -> LerdrJson.decodeFromJsonElement(WebRtcAnswerMessage.serializer(), raw)
            "webrtc_closed" -> LerdrJson.decodeFromJsonElement(WebRtcClosedMessage.serializer(), raw)
            "webrtc_ice" -> LerdrJson.decodeFromJsonElement(WebRtcIceMessage.serializer(), raw)
            "workspaces" -> LerdrJson.decodeFromJsonElement(WorkspacesMessage.serializer(), raw)
            else -> UnknownServerMessage(type = type, fields = raw)
        }
    }

    fun encode(message: ServerMessage): String {
        if (message is UnknownServerMessage) {
            val merged = buildJsonObject {
                message.fields?.forEach { (key, value) -> put(key, value) }
                put("type", kotlinx.serialization.json.JsonPrimitive(message.type))
            }
            return LerdrJson.encodeToString(JsonObject.serializer(), merged)
        }
        @Suppress("UNCHECKED_CAST")
        return LerdrJson.encodeToString(
            serializer(message) as kotlinx.serialization.KSerializer<ServerMessage>,
            message,
        )
    }

    private fun serializer(message: ServerMessage) = when (message) {
        is ActionReceiptMessage -> ActionReceiptMessage.serializer()
        is ActivityMessage -> ActivityMessage.serializer()
        is ActivityHistoryMessage -> ActivityHistoryMessage.serializer()
        is AgentUpdateMessage -> AgentUpdateMessage.serializer()
        is AgentsMessage -> AgentsMessage.serializer()
        is AppDeployStatusMessage -> AppDeployStatusMessage.serializer()
        is BlockedMessage -> BlockedMessage.serializer()
        is CapsUpdateMessage -> CapsUpdateMessage.serializer()
        is CommandResultMessage -> CommandResultMessage.serializer()
        is ConversationUpdateMessage -> ConversationUpdateMessage.serializer()
        is ErrorMessage -> ErrorMessage.serializer()
        is HerdrStatusMessage -> HerdrStatusMessage.serializer()
        is InventoryStatusMessage -> InventoryStatusMessage.serializer()
        is PaneContentMessage -> PaneContentMessage.serializer()
        is PaneDeltaMessage -> PaneDeltaMessage.serializer()
        is PaneProbeMessage -> PaneProbeMessage.serializer()
        is PaneResyncMessage -> PaneResyncMessage.serializer()
        is PaneUnchangedMessage -> PaneUnchangedMessage.serializer()
        is PushConfigMessage -> PushConfigMessage.serializer()
        is PushPolicyMessage -> PushPolicyMessage.serializer()
        is PushPolicyResultMessage -> PushPolicyResultMessage.serializer()
        is PushSubscribedMessage -> PushSubscribedMessage.serializer()
        is PushTestResultMessage -> PushTestResultMessage.serializer()
        is PushUnsubscribedMessage -> PushUnsubscribedMessage.serializer()
        is PushViewedPaneResultMessage -> PushViewedPaneResultMessage.serializer()
        is SpeechVoicesMessage -> SpeechVoicesMessage.serializer()
        is UpdateStatusMessage -> UpdateStatusMessage.serializer()
        is UploadBeginResultMessage -> UploadBeginResultMessage.serializer()
        is UploadCancelResultMessage -> UploadCancelResultMessage.serializer()
        is UploadChunkResultMessage -> UploadChunkResultMessage.serializer()
        is UploadFinishResultMessage -> UploadFinishResultMessage.serializer()
        is WebRtcAnswerMessage -> WebRtcAnswerMessage.serializer()
        is WebRtcClosedMessage -> WebRtcClosedMessage.serializer()
        is WebRtcIceMessage -> WebRtcIceMessage.serializer()
        is WorkspacesMessage -> WorkspacesMessage.serializer()
        is UnknownServerMessage -> UnknownServerMessage.serializer()
    }
}
