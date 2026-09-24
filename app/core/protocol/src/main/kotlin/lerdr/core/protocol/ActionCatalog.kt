package lerdr.core.protocol

import lerdr.core.model.Inbound

/** `protocol.ActionClass`. */
enum class ActionClass(val wire: String) {
    READ_ONLY("read_only"),
    MUTATING("mutating"),
}

/** `protocol.ActionMetadata` — per-action dispatch metadata. */
data class ActionMetadata(
    val operation: String,
    val actionClass: ActionClass,
    val requiresProtocol: Boolean,
    val coordinated: Boolean,
    val audited: Boolean,
)

/**
 * `protocol.actionCatalog` — every inbound action the relay dispatches,
 * with its dispatch metadata. `requiresProtocol` gates the
 * `incompatible_protocol` receipt.
 */
object ActionCatalog {

    private fun read(operation: String) =
        ActionMetadata(operation, ActionClass.READ_ONLY, requiresProtocol = false, coordinated = false, audited = false)

    private fun mutate(operation: String, coordinated: Boolean, audited: Boolean) =
        ActionMetadata(
            operation,
            ActionClass.MUTATING,
            requiresProtocol = operation != "install_update",
            coordinated = coordinated,
            audited = audited,
        )

    private val catalog: Map<String, ActionMetadata> = listOf(
        mutate("acknowledge_pane", coordinated = true, audited = false),
        mutate("agent_clear", coordinated = true, audited = true),
        mutate("agent_rename", coordinated = true, audited = true),
        mutate("agent_restart", coordinated = true, audited = true),
        mutate("agent_start", coordinated = true, audited = true),
        mutate("agent_stop", coordinated = true, audited = true),
        mutate("answer_question", coordinated = true, audited = true),
        read("cancel_speech"),
        read("caps_update"),
        read("check_update"),
        mutate("clarify_question", coordinated = true, audited = true),
        mutate("clear_activities", coordinated = false, audited = false),
        read("client_caps"),
        mutate("copy_agent_response", coordinated = false, audited = false),
        mutate("deploy_app_update", coordinated = false, audited = false),
        mutate("create_device_invitation", coordinated = false, audited = true),
        read("device_list"),
        mutate("focus_agent", coordinated = true, audited = false),
        mutate("focus_pane", coordinated = true, audited = false),
        mutate("focus_tab", coordinated = true, audited = false),
        mutate("focus_workspace", coordinated = true, audited = false),
        read("get_activity"),
        read("get_conversation_history"),
        mutate("install_update", coordinated = false, audited = false),
        mutate("lease_pane_size", coordinated = true, audited = false),
        read("list_directories"),
        read("list_slash_commands"),
        mutate("layout_apply", coordinated = true, audited = true),
        read("layout_export"),
        mutate("navigate_question", coordinated = true, audited = true),
        read("pane_applied"),
        mutate("pane_link_activate", coordinated = true, audited = false),
        read("pane_link_resolve"),
        read("pane_search"),
        read("pane_selection_read"),
        read("push_open_ref"),
        read("push_policy_get"),
        mutate("push_policy_set", coordinated = false, audited = false),
        mutate("push_snooze", coordinated = false, audited = false),
        mutate("push_subscribe", coordinated = false, audited = false),
        mutate("push_test_device", coordinated = false, audited = true),
        mutate("push_unsubscribe", coordinated = false, audited = false),
        mutate("push_viewed_pane", coordinated = false, audited = false),
        read("qr_code"),
        read("read_pane"),
        read("refresh_agents"),
        mutate("register_app_origin", coordinated = false, audited = false),
        mutate("release_pane_size", coordinated = true, audited = false),
        mutate("rename_device", coordinated = false, audited = true),
        mutate("respond", coordinated = true, audited = true),
        mutate("send_keys", coordinated = true, audited = true),
        mutate("send_input", coordinated = true, audited = true),
        mutate("send_secret", coordinated = true, audited = true),
        mutate("reset_devices", coordinated = false, audited = true),
        mutate("revoke_device", coordinated = false, audited = true),
        mutate("send_text", coordinated = true, audited = true),
        read("speak_text"),
        mutate("speech_voice_install", coordinated = false, audited = true),
        mutate("speech_voice_remove", coordinated = false, audited = true),
        read("speech_voices_list"),
        mutate("submit_prompt", coordinated = true, audited = true),
        mutate("tab_reorder", coordinated = true, audited = true),
        read("unwatch_pane"),
        mutate("upload_begin", coordinated = false, audited = true),
        mutate("upload_cancel", coordinated = false, audited = true),
        mutate("upload_chunk", coordinated = false, audited = true),
        mutate("upload_finish", coordinated = false, audited = true),
        read("watch_pane"),
        read("webrtc_close"),
        read("webrtc_ice"),
        read("webrtc_offer"),
        mutate("workspace_close", coordinated = true, audited = true),
        mutate("workspace_create", coordinated = true, audited = true),
        read("workspace_file"),
        read("workspace_git_diff"),
        read("workspace_git_status"),
        mutate("workspace_rename", coordinated = true, audited = true),
        mutate("workspace_reorder", coordinated = true, audited = true),
        read("workspace_tree"),
        mutate("worktree_create", coordinated = true, audited = true),
        read("worktree_list"),
        mutate("worktree_open", coordinated = true, audited = true),
        mutate("worktree_remove", coordinated = true, audited = true),
    ).associateBy { it.operation }

    fun classify(operation: String): ActionMetadata? = catalog[operation]

    /** `protocol.RequiresProtocol` — unknown types require the handshake version. */
    fun requiresProtocol(messageType: String): Boolean =
        catalog[messageType]?.requiresProtocol ?: true

    /** `protocol.Compatible` — protocol-gated actions must carry v3. */
    fun compatible(message: Inbound): Boolean =
        !requiresProtocol(message.type) || message.protocol == Protocol.VERSION
}
