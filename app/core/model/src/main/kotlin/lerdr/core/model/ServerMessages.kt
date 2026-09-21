package lerdr.core.model

import kotlinx.serialization.EncodeDefault
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject

/**
 * Server→client (s2c) wire messages.
 *
 * Most of these are emitted as Go `map[string]any` — key presence varies
 * per code path, so fields are nullable-presence (`T? = null`): a decoded
 * `null`/absent field is not re-emitted, a decoded value always is. Fields
 * Go emits as explicit `null` when nil ([interaction], [options],
 * `agent_profiles`, the `any`-typed push_config payloads, delta `segments`)
 * use [WireField] so presence-triple survives a decode→encode round-trip.
 * Nested types that Go emits as structs keep struct-faithful omitempty
 * modeling.
 */
sealed interface ServerMessage {
    val type: String
}

// ── command / error pipeline ──────────────────────────────────────────

/** `commandResultMessage` — always carries action/ok/phase/error/pane_id. */
@Serializable
data class CommandResultMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "command_result",
    val action: String? = null,
    val data: JsonElement? = null,
    val error: String? = null,
    val ok: Boolean? = null,
    @SerialName("pane_id") val paneId: String? = null,
    val phase: String? = null,
    @SerialName("request_id") val requestId: String? = null,
) : ServerMessage {
    companion object {
        const val PHASE_ACCEPTED = "accepted"
        const val PHASE_COMPLETED = "completed"
        const val PHASE_SCHEDULED = "scheduled"
        const val PHASE_CONFIRMED = "confirmed"
        const val PHASE_ADVANCED = "advanced"
        const val PHASE_NAVIGATED = "navigated"
        const val PHASE_FAILED = "failed"
        const val PHASE_UNCONFIRMED = "unconfirmed"
    }
}

/** `ErrorResponse` — ApiError plus request correlation. */
@Serializable
data class ErrorMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "error",
    val error: ApiError? = null,
    @SerialName("request_id") val requestId: String? = null,
) : ServerMessage

/** `ActionReceiptResponse` — dispatch receipt for a coordinated action. */
@Serializable
data class ActionReceiptMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "action_receipt",
    val receipt: ActionReceipt? = null,
    @SerialName("request_id") val requestId: String? = null,
) : ServerMessage

// ── activity journal ──────────────────────────────────────────────────

@Serializable
data class ActivityMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "activity",
    val activity: ActivityEntry? = null,
) : ServerMessage

@Serializable
data class ActivityHistoryMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "activity_history",
    val activities: List<ActivityEntry>? = null,
) : ServerMessage

// ── agents / workspaces ───────────────────────────────────────────────

/** `agents` — post-handshake full inventory snapshot. */
@Serializable
data class AgentsMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "agents",
    val agents: List<AgentState>? = null,
) : ServerMessage

/** `agent_update` — map-emitted state change broadcast. */
@Serializable
data class AgentUpdateMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "agent_update",
    val agent: String? = null,
    @SerialName("attention_kind") val attentionKind: String? = null,
    val cwd: String? = null,
    @SerialName("event_id") val eventId: String? = null,
    val host: String? = null,
    @SerialName("pane_id") val paneId: String? = null,
    @SerialName("pane_revision") val paneRevision: Long? = null,
    val project: String? = null,
    @SerialName("raw_pane_id") val rawPaneId: String? = null,
    val session: String? = null,
    @SerialName("session_name") val sessionName: String? = null,
    val status: String? = null,
    @SerialName("tab_id") val tabId: String? = null,
    @SerialName("tab_label") val tabLabel: String? = null,
    @SerialName("tab_number") val tabNumber: Int? = null,
    @SerialName("updated_at") val updatedAt: Long? = null,
    @SerialName("workspace_id") val workspaceId: String? = null,
) : ServerMessage

/** `blocked` — agent transitioned into a waiting-for-input state. */
@Serializable
data class BlockedMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "blocked",
    val agent: String? = null,
    @SerialName("agent_session_id") val agentSessionId: String? = null,
    @SerialName("approval_fingerprint") val approvalFingerprint: String? = null,
    @SerialName("attention_kind") val attentionKind: String? = null,
    val command: String? = null,
    val cwd: String? = null,
    @SerialName("event_id") val eventId: String? = null,
    val generation: Long? = null,
    val host: String? = null,
    @Serializable(with = WireFieldSerializer::class)
    val interaction: WireField<Interaction> = WireField.Absent,
    @SerialName("interaction_id") val interactionId: String? = null,
    val name: String? = null,
    @Serializable(with = WireFieldSerializer::class)
    val options: WireField<List<String>> = WireField.Absent,
    @SerialName("pane_id") val paneId: String? = null,
    @SerialName("pane_revision") val paneRevision: Long? = null,
    val project: String? = null,
    val prompt: String? = null,
    @SerialName("question_layout") val questionLayout: Boolean? = null,
    @SerialName("raw_pane_id") val rawPaneId: String? = null,
    @SerialName("server_session_id") val serverSessionId: String? = null,
    val session: String? = null,
    @SerialName("session_name") val sessionName: String? = null,
    val status: String? = null,
    @SerialName("tab_id") val tabId: String? = null,
    @SerialName("tab_label") val tabLabel: String? = null,
    @SerialName("tab_number") val tabNumber: Int? = null,
    @SerialName("terminal_id") val terminalId: String? = null,
    @SerialName("updated_at") val updatedAt: Long? = null,
    @SerialName("workspace_id") val workspaceId: String? = null,
) : ServerMessage {
    companion object {
        const val ATTENTION_APPROVAL = "approval"
        const val ATTENTION_QUESTION = "question"
        const val ATTENTION_CHAT = "chat"
        const val ATTENTION_UNKNOWN = "unknown"
    }
}

/** `herdr.Workspace` row inside `workspaces`. */
@Serializable
data class WorkspaceInfo(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) @SerialName("workspace_id") val workspaceId: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) val number: Int = 0,
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) val label: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) val focused: Boolean = false,
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) @SerialName("pane_count") val paneCount: Int = 0,
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) @SerialName("tab_count") val tabCount: Int = 0,
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) @SerialName("active_tab_id") val activeTabId: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) @SerialName("agent_status") val agentStatus: String = "",
    val cwd: String = "",
    val worktree: WorkspaceWorktree? = null,
)

/** `herdr.WorkspaceWorktree`. */
@Serializable
data class WorkspaceWorktree(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) @SerialName("repo_key") val repoKey: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) @SerialName("repo_name") val repoName: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) @SerialName("repo_root") val repoRoot: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) @SerialName("checkout_path") val checkoutPath: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) @SerialName("is_linked_worktree") val isLinkedWorktree: Boolean = false,
)

@Serializable
data class WorkspacesMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "workspaces",
    val workspaces: List<WorkspaceInfo>? = null,
) : ServerMessage

// ── pane frames ───────────────────────────────────────────────────────

/** `pane_content` — full frame (watch base or `read_pane` response). */
@Serializable
data class PaneContentMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "pane_content",
    @SerialName("ack_required") val ackRequired: Boolean? = null,
    @SerialName("attention_kind") val attentionKind: String? = null,
    val command: String? = null,
    val content: String? = null,
    @SerialName("content_fingerprint") val contentFingerprint: String? = null,
    val error: String? = null,
    val format: String? = null,
    @Serializable(with = WireFieldSerializer::class)
    val interaction: WireField<Interaction> = WireField.Absent,
    @SerialName("no_echo") val noEcho: Boolean? = null,
    @SerialName("no_echo_prompt") val noEchoPrompt: String? = null,
    @Serializable(with = WireFieldSerializer::class)
    val options: WireField<List<String>> = WireField.Absent,
    @SerialName("pane_id") val paneId: String? = null,
    val prompt: String? = null,
    @SerialName("question_layout") val questionLayout: Boolean? = null,
    @SerialName("resize_settling") val resizeSettling: Boolean? = null,
    val target: TargetRef? = null,
    val truncated: Boolean? = null,
    @SerialName("viewport_only") val viewportOnly: Boolean? = null,
    @SerialName("viewport_rows") val viewportRows: Int? = null,
) : ServerMessage

/**
 * `pane_delta` — pane response minus `content`, plus `base_fingerprint`
 * and `segments`. `segments` may be null on released relays for
 * metadata-only deltas (docs/specs/pane-delta.md §6.1).
 */
@Serializable
data class PaneDeltaMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "pane_delta",
    @SerialName("ack_required") val ackRequired: Boolean? = null,
    @SerialName("attention_kind") val attentionKind: String? = null,
    @SerialName("base_fingerprint") val baseFingerprint: String? = null,
    val command: String? = null,
    @SerialName("content_fingerprint") val contentFingerprint: String? = null,
    val format: String? = null,
    @Serializable(with = WireFieldSerializer::class)
    val interaction: WireField<Interaction> = WireField.Absent,
    @SerialName("no_echo") val noEcho: Boolean? = null,
    @SerialName("no_echo_prompt") val noEchoPrompt: String? = null,
    @Serializable(with = WireFieldSerializer::class)
    val options: WireField<List<String>> = WireField.Absent,
    @SerialName("pane_id") val paneId: String? = null,
    val prompt: String? = null,
    @SerialName("question_layout") val questionLayout: Boolean? = null,
    @SerialName("resize_settling") val resizeSettling: Boolean? = null,
    @Serializable(with = WireFieldSerializer::class)
    val segments: WireField<List<Segment>> = WireField.Absent,
    val target: TargetRef? = null,
    val truncated: Boolean? = null,
    @SerialName("viewport_only") val viewportOnly: Boolean? = null,
    @SerialName("viewport_rows") val viewportRows: Int? = null,
) : ServerMessage

/** `pane_probe` — cheap watch-probe frame. */
@Serializable
data class PaneProbeMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "pane_probe",
    val content: String? = null,
    val error: String? = null,
    val format: String? = null,
    @SerialName("pane_id") val paneId: String? = null,
) : ServerMessage

/** `pane_resync` — server nudge; client must force `read_pane`. */
@Serializable
data class PaneResyncMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "pane_resync",
    @SerialName("pane_id") val paneId: String? = null,
    val target: TargetRef? = null,
) : ServerMessage

/** `pane_unchanged` — read_pane answered with the stored fingerprint. */
@Serializable
data class PaneUnchangedMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "pane_unchanged",
    @SerialName("content_fingerprint") val contentFingerprint: String? = null,
    @SerialName("pane_id") val paneId: String? = null,
    val target: TargetRef? = null,
) : ServerMessage

// ── relay status / config ─────────────────────────────────────────────

/** `protocol.HerdrFeatureStatus`. */
@Serializable
data class HerdrFeatureStatus(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) val state: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) val reason: String = "",
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) val generation: Long = 0,
)

/** `protocol.HerdrStatus` — capability/status block. */
@Serializable
data class HerdrStatus(
    @SerialName("installed_client_version") val installedClientVersion: String = "",
    @SerialName("server_version") val serverVersion: String = "",
    @SerialName("server_protocol") val serverProtocol: Int = 0,
    @EncodeDefault(EncodeDefault.Mode.ALWAYS)
    @SerialName("server_protocol_known") val serverProtocolKnown: Boolean = false,
    @SerialName("endpoint_protocol_generation") val endpointProtocolGeneration: Int? = null,
    @SerialName("surface_interest") val surfaceInterest: Boolean? = null,
    @SerialName("health_check") val healthCheck: Boolean? = null,
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) val generation: Long = 0,
    @EncodeDefault(EncodeDefault.Mode.ALWAYS)
    val features: Map<String, HerdrFeatureStatus> = emptyMap(),
)

/** `herdr_status` broadcast. */
@Serializable
data class HerdrStatusMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "herdr_status",
    val capabilities: List<String>? = null,
    val status: HerdrStatus? = null,
) : ServerMessage

/** `inventory_status` — Herdr subprocess inventory state. */
@Serializable
data class InventoryStatusMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "inventory_status",
    @SerialName("error_code") val errorCode: String? = null,
    @SerialName("last_attempt_at") val lastAttemptAt: Long? = null,
    @SerialName("last_success_at") val lastSuccessAt: Long? = null,
    val message: String? = null,
    val stale: Boolean? = null,
    val state: String? = null,
) : ServerMessage

/** `update.State` — self-update status payload (subset seen on wire). */
@Serializable
data class UpdateState(
    val state: String? = null,
    @SerialName("current_version") val currentVersion: String? = null,
    @SerialName("current_revision") val currentRevision: String? = null,
    @SerialName("available_version") val availableVersion: String? = null,
    @SerialName("available_revision") val availableRevision: String? = null,
    @SerialName("upstream_version") val upstreamVersion: String? = null,
    @SerialName("upstream_revision") val upstreamRevision: String? = null,
    @SerialName("target_version") val targetVersion: String? = null,
    @SerialName("target_revision") val targetRevision: String? = null,
    val target: String? = null,
    @SerialName("checked_at") val checkedAt: Long? = null,
    @SerialName("started_at") val startedAt: String? = null,
    @SerialName("finished_at") val finishedAt: String? = null,
    val mode: String? = null,
    val eligible: Boolean? = null,
    @SerialName("can_install") val canInstall: Boolean? = null,
    val reason: String? = null,
    val error: String? = null,
)

@Serializable
data class UpdateStatusMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "update_status",
    val update: UpdateState? = null,
) : ServerMessage

/** `appdeploy.PublicState` — emitted as a partial map when unconfigured. */
@Serializable
data class AppDeployState(
    val configured: Boolean? = null,
    val origin: String? = null,
    val project: String? = null,
    val branch: String? = null,
    val revision: String? = null,
    val reason: String? = null,
    val state: String? = null,
    @SerialName("target_version") val targetVersion: String? = null,
    @SerialName("target_revision") val targetRevision: String? = null,
    @SerialName("checked_at") val checkedAt: Long? = null,
    val error: String? = null,
)

@Serializable
data class AppDeployStatusMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "app_deploy_status",
    @SerialName("app_deploy") val appDeploy: AppDeployState? = null,
) : ServerMessage

// ── push config / policy ──────────────────────────────────────────────

/** `push_config` — post-handshake capability/config payload (struct). */
@Serializable
data class PushConfigMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "push_config",
    @SerialName("vapid_public_key") val vapidPublicKey: String? = null,
    val host: String? = null,
    val home: String? = null,
    val protocol: Int? = null,
    val version: String? = null,
    @SerialName("release_version") val releaseVersion: String? = null,
    val revision: String? = null,
    @Serializable(with = WireFieldSerializer::class)
    val update: WireField<UpdateState> = WireField.Absent,
    @Serializable(with = WireFieldSerializer::class)
    @SerialName("app_deploy") val appDeploy: WireField<AppDeployState> = WireField.Absent,
    @Serializable(with = WireFieldSerializer::class)
    val capabilities: WireField<List<String>> = WireField.Absent,
    @SerialName("herdr_status") val herdrStatus: HerdrStatus? = null,
    @SerialName("speech_languages") val speechLanguages: List<String>? = null,
    @Serializable(with = WireFieldSerializer::class)
    val inventory: WireField<InventoryState> = WireField.Absent,
    @Serializable(with = WireFieldSerializer::class)
    @SerialName("agent_profiles") val agentProfiles: WireField<List<AgentProfile>> = WireField.Absent,
    val hybrid: JsonObject? = null,
) : ServerMessage

@Serializable
data class InventoryState(
    val panes: Int? = null,
    val state: String? = null,
)

@Serializable
data class AgentProfile(
    val id: String? = null,
    val label: String? = null,
)

/** Wire view of a device's push policy (`pushPolicyResponse`). */
@Serializable
data class PushPolicyView(
    val categories: Map<String, Boolean>? = null,
    @SerialName("cooldown_ms") val cooldownMs: Long? = null,
    @SerialName("device_id") val deviceId: String? = null,
    val locale: String? = null,
    @SerialName("settle_ms") val settleMs: Long? = null,
    @SerialName("snooze_until") val snoozeUntil: String? = null,
    val snoozed: Boolean? = null,
    @SerialName("update_once") val updateOnce: Boolean? = null,
)

@Serializable
data class PushPolicyMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "push_policy",
    val policy: PushPolicyView? = null,
) : ServerMessage

@Serializable
data class PushPolicyResultMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "push_policy_result",
    val code: String? = null,
    val ok: Boolean? = null,
    val policy: PushPolicyView? = null,
) : ServerMessage

@Serializable
data class PushSubscribedMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "push_subscribed",
    val ok: Boolean? = null,
) : ServerMessage

@Serializable
data class PushUnsubscribedMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "push_unsubscribed",
    val ok: Boolean? = null,
) : ServerMessage

@Serializable
data class PushViewedPaneResultMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "push_viewed_pane_result",
    val ok: Boolean? = null,
) : ServerMessage

@Serializable
data class PushTestResultMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "push_test_result",
    val stage: String? = null,
) : ServerMessage

// ── speech ────────────────────────────────────────────────────────────

@Serializable
data class SpeechVoice(
    val bytes: Long? = null,
    val engine: String? = null,
    val installed: Boolean? = null,
    val language: String? = null,
    val name: String? = null,
)

/** `speech_voices` — voice catalog broadcast. */
@Serializable
data class SpeechVoicesMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "speech_voices",
    @SerialName("cache_dir") val cacheDir: String? = null,
    @SerialName("engine_installed") val engineInstalled: Boolean? = null,
    val languages: List<String>? = null,
    @SerialName("management_supported") val managementSupported: Boolean? = null,
    val voices: List<SpeechVoice>? = null,
) : ServerMessage

// ── uploads ───────────────────────────────────────────────────────────

@Serializable
data class UploadLimits(
    @SerialName("max_files") val maxFiles: Int? = null,
    @SerialName("max_file_bytes") val maxFileBytes: Long? = null,
    @SerialName("max_batch_bytes") val maxBatchBytes: Long? = null,
)

@Serializable
data class UploadBeginResult(
    @SerialName("upload_id") val uploadId: String? = null,
    @SerialName("chunk_bytes") val chunkBytes: Int? = null,
    @SerialName("expires_at") val expiresAt: String? = null,
    val limits: UploadLimits? = null,
)

@Serializable
data class UploadChunkResult(
    @SerialName("file_index") val fileIndex: Int? = null,
    @SerialName("next_sequence") val nextSequence: Int? = null,
    @SerialName("received_bytes") val receivedBytes: Long? = null,
)

@Serializable
data class UploadAttachment(
    val ref: String? = null,
    val name: String? = null,
    @SerialName("media_type") val mediaType: String? = null,
    val bytes: Long? = null,
    val sha256: String? = null,
    @SerialName("expires_at") val expiresAt: String? = null,
)

@Serializable
data class UploadFinishResult(
    val attachments: List<UploadAttachment>? = null,
)

/** `upload_begin_result` — carries either `result` or an ApiError `error`. */
@Serializable
data class UploadBeginResultMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "upload_begin_result",
    @SerialName("request_id") val requestId: String? = null,
    val result: UploadBeginResult? = null,
    val error: ApiError? = null,
) : ServerMessage

@Serializable
data class UploadCancelResultMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "upload_cancel_result",
    @SerialName("request_id") val requestId: String? = null,
    val result: JsonObject? = null,
    val error: ApiError? = null,
) : ServerMessage

@Serializable
data class UploadChunkResultMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "upload_chunk_result",
    @SerialName("request_id") val requestId: String? = null,
    val result: UploadChunkResult? = null,
    val error: ApiError? = null,
) : ServerMessage

@Serializable
data class UploadFinishResultMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "upload_finish_result",
    @SerialName("request_id") val requestId: String? = null,
    val result: UploadFinishResult? = null,
    val error: ApiError? = null,
) : ServerMessage

// ── webrtc ────────────────────────────────────────────────────────────

@Serializable
data class WebRtcAnswerMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "webrtc_answer",
    @SerialName("request_id") val requestId: String? = null,
    val sdp: String? = null,
) : ServerMessage

@Serializable
data class WebRtcClosedMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "webrtc_closed",
    val reason: String? = null,
    @SerialName("request_id") val requestId: String? = null,
) : ServerMessage

@Serializable
data class WebRtcIceMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "webrtc_ice",
    val candidate: String? = null,
    @SerialName("request_id") val requestId: String? = null,
    @SerialName("sdp_mid") val sdpMid: String? = null,
    @SerialName("sdp_mline_index") val sdpMlineIndex: Int? = null,
) : ServerMessage

/** Fallback for `type` values without a registered DTO — keeps the raw payload. */
@Serializable
data class UnknownServerMessage(
    @EncodeDefault(EncodeDefault.Mode.ALWAYS) override val type: String = "",
    val fields: JsonObject? = null,
) : ServerMessage
