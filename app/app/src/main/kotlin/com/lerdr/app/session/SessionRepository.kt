package com.lerdr.app.session

import com.lerdr.app.di.AppScope
import com.lerdr.core.e2ee.E2EEServerFinish
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicBoolean
import javax.inject.Inject
import javax.inject.Singleton
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import lerdr.core.conversation.ConversationPage
import lerdr.core.conversation.ConversationPageRequest
import lerdr.core.conversation.ConversationProjector
import lerdr.core.data.CredentialEnrollment
import lerdr.core.data.CredentialStore
import lerdr.core.data.RelayDeviceAuth
import lerdr.core.data.RelayEndpoint
import lerdr.core.data.RelayRegistry
import lerdr.core.data.fromFinish
import lerdr.core.model.ActionReceiptMessage
import lerdr.core.model.ActionReceiptPhase
import lerdr.core.model.ActivityEntry
import lerdr.core.model.ActivityHistoryMessage
import lerdr.core.model.ActivityMessage
import lerdr.core.model.CommandResultMessage
import lerdr.core.model.ErrorMessage
import lerdr.core.model.Inbound
import lerdr.core.model.Interaction
import lerdr.core.model.ServerMessage
import lerdr.core.model.UnknownServerMessage
import lerdr.core.model.UploadBeginResult
import lerdr.core.model.UploadBeginResultMessage
import lerdr.core.model.UploadCancelResultMessage
import lerdr.core.model.UploadChunkResult
import lerdr.core.model.UploadChunkResultMessage
import lerdr.core.model.UploadFinishResult
import lerdr.core.model.UploadFinishResultMessage
import lerdr.core.protocol.Protocol
import lerdr.core.protocol.ServerMessageCodec
import lerdr.core.store.Agent
import lerdr.core.store.AgentStore
import lerdr.core.store.ConnectionStore
import lerdr.core.store.RelayConnection
import lerdr.core.store.StoreReducer
import lerdr.core.store.TransportKind
import lerdr.core.store.TransportStatus
import lerdr.core.store.TransportStatusDetail
import lerdr.core.store.WorkspaceStore
import lerdr.core.store.clientPaneId
import lerdr.core.terminal.AckGate
import lerdr.core.terminal.PaneSurface
import lerdr.core.transport.CommandException
import lerdr.core.transport.DeviceAuthentication
import lerdr.core.transport.ReconnectPolicy
import lerdr.core.transport.RelaySession
import lerdr.core.transport.TransportException

/**
 * One journaled activity row, scoped to the relay that emitted it — the
 * oracle's normalized `Activity` (`activity_key = relayId:id` dedupe).
 */
data class RelayActivity(
    val key: String,
    val relayId: String,
    val entry: ActivityEntry,
)

/** `upload.FileSpec` — one `upload_begin` files[] entry (`{name, media_type, bytes}`). */
data class UploadFileSpec(
    val name: String,
    val mediaType: String,
    val bytes: Long,
)

/** `upload.FileDigest` — one `upload_finish` files[] entry (`{file_index, sha256}`). */
data class UploadFileDigest(
    val fileIndex: Int,
    val sha256: String,
)

/**
 * Session hub — owns the per-relay [RelaySessionHandle] lifecycle and routes
 * everything a live connection produces or consumes:
 *
 * - [relayRegistry] diffs drive connect/teardown (`connectRelay` /
 *   `disconnectRelay` in the oracle);
 * - [credentialStore] records feed `getAuthentication`; [RelaySession]'s
 *   `onEnrolled` commits the issued credential back through the store
 *   (`commitDeviceEnrollment` parity — invitation redemption included);
 * - inbound frames demux: [StoreReducer] first (agents/workspaces/
 *   connection), then pane frames to their [PaneSurface] (raw JSON —
 *   presence semantics the typed DTOs erase), then command results to the
 *   raw-command correlator, then the activity journal;
 * - pane runtimes carry the [AckGate] intents back onto the wire —
 *   `pane_applied`/`read_pane`/`watch_pane`/`unwatch_pane` with the exact
 *   `content_fingerprint`/`interval_ms`/`lines` fields the relay reads from
 *   the raw map;
 * - UI-facing actions are suspend calls that throw [CommandException] /
 *   [TransportException] / [IllegalStateException] (local preconditions).
 */
@Singleton
class SessionRepository @Inject constructor(
    @param:AppScope private val scope: CoroutineScope,
    private val credentialStore: CredentialStore,
    private val relayRegistry: RelayRegistry,
    private val agentStore: AgentStore,
    private val workspaceStore: WorkspaceStore,
    private val connectionStore: ConnectionStore,
    private val sessionFactory: RelaySessionFactory,
    private val budget: PaneBudget = PaneBudget(),
) {
    private val reducer = StoreReducer(
        agentStore = agentStore,
        workspaceStore = workspaceStore,
        connectionStore = connectionStore,
        relayLabel = { relayId ->
            relayRegistry.relays.value.firstOrNull { it.id == relayId }?.label ?: relayId
        },
    )

    private val lock = Any()
    private val sessions = LinkedHashMap<String, SessionRuntime>()
    private val panes = LinkedHashMap<String, PaneRuntime>()

    /** Panes the UI has open — watched intent, survives session recreation. */
    private val openPanes = LinkedHashSet<String>()

    private val pendingRaw = ConcurrentHashMap<String, PendingRaw>()

    /** `pendingUploads` — `upload_*` requests answer on their own frame type. */
    private val pendingUploads = ConcurrentHashMap<String, PendingUpload>()

    /** Latest auth records — `getAuthentication` is sync, so this is the cache. */
    @Volatile
    private var authByRelay: Map<String, RelayDeviceAuth> = emptyMap()

    private val started = AtomicBoolean(false)

    private val _activities = MutableStateFlow<List<RelayActivity>>(emptyList())

    /** Cross-relay activity journal, newest first, capped like the oracle's 500. */
    val activities: StateFlow<List<RelayActivity>> = _activities.asStateFlow()

    // ── lifecycle ─────────────────────────────────────────────────────

    /** Boots the reconcile loops. Idempotent; called once from `LerdrApp`. */
    fun start() {
        if (!started.compareAndSet(false, true)) return
        scope.launch {
            credentialStore.records.collect { records ->
                authByRelay = records
                unparkSessions(records.keys)
            }
        }
        scope.launch {
            relayRegistry.relays.collect { endpoints ->
                reconcileSessions(endpoints)
            }
        }
    }

    /** `setHidden` fan-out — hidden sessions retire keepalives per policy. */
    fun setHidden(hidden: Boolean) {
        synchronized(lock) { sessions.values.toList() }
            .forEach { it.handle.setHidden(hidden) }
    }

    /** `revalidateConnections` — foreground/wake/network-restore probe. */
    fun revalidateAll() {
        synchronized(lock) { sessions.values.toList() }
            .forEach { it.handle.revalidate() }
    }

    // ── session reconcile ─────────────────────────────────────────────

    /**
     * Ensures a session exists for [endpoint] and starts dialing. Used by
     * both the registry reconcile and the pairing flow — idempotent.
     */
    fun connect(endpoint: RelayEndpoint) {
        synchronized(lock) {
            if (sessions.containsKey(endpoint.id)) return
            connectionStore.connect(endpoint.id, endpoint.label)
            val handle = sessionFactory.create(
                url = socketUrl(endpoint),
                scope = scope,
                getAuthentication = { authByRelay[endpoint.id]?.toAuthentication() },
                onEnrolled = { auth, finish -> commitEnrollment(endpoint.id, auth, finish) },
            )
            val runtime = SessionRuntime(endpoint, handle)
            runtime.jobs += scope.launch { handle.state.collect { onSessionState(endpoint, it) } }
            runtime.jobs += scope.launch { handle.incoming.collect { demux(endpoint.id, it) } }
            sessions[endpoint.id] = runtime
        }
    }

    private fun reconcileSessions(endpoints: List<RelayEndpoint>) {
        val wanted = endpoints.associateBy { it.id }
        val removed: List<SessionRuntime>
        synchronized(lock) {
            removed = sessions.keys
                .filter { it !in wanted }
                .mapNotNull { sessions.remove(it) }
        }
        for (runtime in removed) {
            teardown(runtime)
        }
        endpoints.forEach(::connect)
    }

    private fun teardown(runtime: SessionRuntime) {
        runtime.jobs.forEach { it.cancel() }
        runtime.handle.close()
        rejectUploads(runtime.endpoint.id, "Relay disconnected")
        connectionStore.disconnect(runtime.endpoint.id)
        agentStore.removeRelay(runtime.endpoint.id)
        synchronized(lock) {
            panes.values
                .filter { it.relayId == runtime.endpoint.id }
                .forEach { it.snapshots.value = null }
            panes.values.removeAll { it.relayId == runtime.endpoint.id }
        }
    }

    /** `commitDeviceEnrollment` — persist the issued credential per auth kind. */
    private suspend fun commitEnrollment(
        relayId: String,
        auth: DeviceAuthentication,
        finish: E2EEServerFinish,
    ) {
        try {
            val enrollment = CredentialEnrollment.fromFinish(finish)
            if (finish.credentialSecret != null) {
                // Invitation redemption — swap the presented invitation for
                // the issued credential in one durable write.
                credentialStore.redeemInvitation(relayId, auth.id, enrollment)
            } else {
                credentialStore.updateCredential(relayId, enrollment)
            }
        } catch (failure: Exception) {
            // The handshake already succeeded — a persistence failure must not
            // kill the session; the next successful enroll retries the write.
        }
    }

    /** A session parked for missing auth dials once the record lands. */
    private fun unparkSessions(knownRelays: Set<String>) {
        val parked = synchronized(lock) {
            sessions.values.filter {
                it.endpoint.id in knownRelays &&
                    (it.handle.state.value as? RelaySession.SessionState.Disconnected)
                        ?.reason == null
            }
        }
        parked.forEach { it.handle.reconnect() }
    }

    private suspend fun onSessionState(endpoint: RelayEndpoint, state: RelaySession.SessionState) {
        when (state) {
            is RelaySession.SessionState.Connected -> {
                connectionStore.onTransportStatus(
                    endpoint.id,
                    TransportStatus.CONNECTED,
                    TransportStatusDetail(path = TransportKind.WEBSOCKET),
                )
                resyncPanes(endpoint.id)
                requestActivities(endpoint.id)
            }
            is RelaySession.SessionState.Connecting ->
                connectionStore.onTransportStatus(endpoint.id, TransportStatus.CONNECTING)
            is RelaySession.SessionState.Disconnected -> {
                connectionStore.onTransportStatus(
                    endpoint.id,
                    TransportStatus.CLOSED,
                    TransportStatusDetail(
                        reason = state.reason?.reason,
                        fatal = state.reason?.fatal == true,
                        code = state.reason?.code,
                    ),
                )
                rejectUploads(endpoint.id, state.reason?.reason ?: "Relay disconnected")
                disconnectPanes(endpoint.id)
            }
            is RelaySession.SessionState.AuthRejected -> {
                connectionStore.onTransportStatus(
                    endpoint.id,
                    TransportStatus.CLOSED,
                    TransportStatusDetail(
                        reason = state.reason.reason,
                        fatal = true,
                        code = TransportStatusDetail.DEVICE_UNAUTHORIZED,
                    ),
                )
                rejectUploads(endpoint.id, state.reason.reason)
            }
            RelaySession.SessionState.Closed -> {
                connectionStore.disconnect(endpoint.id)
                rejectUploads(endpoint.id, "Relay disconnected")
            }
            RelaySession.SessionState.Idle -> Unit
        }
    }

    /** `socketOrigin + /ws` — the relay's single websocket endpoint. */
    private fun socketUrl(endpoint: RelayEndpoint): String = "${endpoint.socketOrigin}/ws"

    // ── demux ─────────────────────────────────────────────────────────

    private suspend fun demux(relayId: String, raw: JsonObject) {
        val type = (raw["type"] as? JsonPrimitive)?.takeIf { it.isString }?.content.orEmpty()

        // Stores first — every frame is proof of life and may carry
        // agent/workspace/connection mutations the other routes depend on.
        val message = try {
            ServerMessageCodec.decode(raw)
        } catch (invalid: IllegalArgumentException) {
            UnknownServerMessage(type = type, fields = raw)
        }
        reducer.handle(relayId, message)

        // Pane frames — raw JsonObject into the surface (presence semantics).
        when (type) {
            "pane_content", "pane_delta", "pane_unchanged", "pane_resync" ->
                applyPaneFrame(relayId, type, raw)
        }

        // Raw-command correlation for extras-bearing sends.
        when (message) {
            is CommandResultMessage -> resolveCommandResult(message)
            is ActionReceiptMessage -> resolveActionReceipt(message)
            is ErrorMessage -> resolveError(message)
            is UploadBeginResultMessage -> resolveUploadResult(relayId, message)
            is UploadChunkResultMessage -> resolveUploadResult(relayId, message)
            is UploadFinishResultMessage -> resolveUploadResult(relayId, message)
            is UploadCancelResultMessage -> resolveUploadResult(relayId, message)
            is ActivityMessage -> upsertActivity(relayId, message.activity)
            is ActivityHistoryMessage -> mergeActivityHistory(relayId, message.activities)
            else -> Unit
        }
    }

    private suspend fun applyPaneFrame(relayId: String, type: String, raw: JsonObject) {
        val rawPaneId = (raw["pane_id"] as? JsonPrimitive)
            ?.takeIf { it.isString }?.content ?: return
        val runtime = synchronized(lock) { panes[clientPaneId(relayId, rawPaneId)] } ?: return
        runtime.mutex.withLock {
            val result = when (type) {
                "pane_content" -> runtime.surface.applyContent(raw)
                "pane_delta" -> runtime.surface.applyDelta(raw)
                "pane_unchanged" -> runtime.surface.applyUnchanged(raw)
                else -> runtime.surface.onResync()
            }
            if (result is PaneSurface.Result.Committed) {
                runtime.snapshots.value = result.snapshot
            }
            dispatchIntents(runtime, result.intents)
        }
    }

    /** Encode + send gate intents; roll the optimistic marks back on refusal. */
    private fun dispatchIntents(runtime: PaneRuntime, intents: List<AckGate.Intent>) {
        if (intents.isEmpty()) return
        val agent = agentStore.agentNow(runtime.paneId)
        val capable = connectionStore.connectionNow(runtime.relayId)
            ?.capabilities
            ?.contains(REALTIME_DELTA_CAPABILITY) == true
        val session = synchronized(lock) { sessions[runtime.relayId]?.handle } ?: run {
            intents.forEach(runtime.surface::onSendFailed)
            return
        }
        for (intent in intents) {
            val frame = encodePaneIntent(intent, agent, runtime.surface, budget, capable)
            val sent = frame != null && session.sendRaw(frame.toString())
            if (!sent) runtime.surface.onSendFailed(intent)
        }
    }

    // ── pane lifecycle (UI-facing) ────────────────────────────────────

    /** `watchPane` + `readPane` — the terminal view opened this pane. */
    suspend fun openPane(paneId: String) {
        synchronized(lock) { openPanes.add(paneId) }
        val runtime = paneRuntime(paneId) ?: return
        runtime.mutex.withLock {
            dispatchIntents(runtime, runtime.surface.watch() + runtime.surface.requestRead())
        }
    }

    /** `unwatchPane` — the terminal view left this pane. */
    suspend fun closePane(paneId: String) {
        synchronized(lock) { openPanes.remove(paneId) }
        val runtime = synchronized(lock) { panes.remove(paneId) } ?: return
        runtime.mutex.withLock {
            dispatchIntents(runtime, runtime.surface.unwatch())
            runtime.surface.reset()
        }
        runtime.snapshots.value = null
    }

    /** Manual refresh — throttled by the gate's 35 s coalescing window. */
    suspend fun refreshPane(paneId: String) {
        val runtime = synchronized(lock) { panes[paneId] } ?: return
        runtime.mutex.withLock {
            dispatchIntents(runtime, runtime.surface.requestRead())
        }
    }

    /** The render seam — null until the first frame commits. */
    fun paneSnapshot(paneId: String): Flow<PaneSurface.Snapshot?> = flow {
        val runtime = synchronized(lock) { panes[paneId] }
        if (runtime == null) emit(null) else runtime.snapshots.collect { emit(it) }
    }

    private fun paneRuntime(paneId: String): PaneRuntime? {
        val separator = paneId.indexOf("::")
        if (separator <= 0) return null
        val relayId = paneId.substring(0, separator)
        synchronized(lock) { sessions[relayId] } ?: return null
        return synchronized(lock) {
            panes.getOrPut(paneId) {
                PaneRuntime(
                    paneId = paneId,
                    relayId = relayId,
                    surface = PaneSurface(paneId = paneId),
                    snapshots = MutableStateFlow(null),
                )
            }
        }
    }

    /**
     * Post-connect resync — a `read_pane` re-arms the watch on reply. Also
     * covers panes opened before the session existed (runtime is created
     * lazily here once the relay row can serve them).
     */
    private suspend fun resyncPanes(relayId: String) {
        val paneIds = synchronized(lock) {
            openPanes.filter { it.startsWith("$relayId::") }
        }
        for (paneId in paneIds) {
            val runtime = paneRuntime(paneId) ?: continue
            runtime.mutex.withLock {
                val intents = if (runtime.surface.watching) {
                    runtime.surface.requestRead()
                } else {
                    runtime.surface.watch() + runtime.surface.requestRead()
                }
                dispatchIntents(runtime, intents)
            }
        }
    }

    private suspend fun disconnectPanes(relayId: String) {
        val relayPanes = synchronized(lock) { panes.values.filter { it.relayId == relayId } }
        for (runtime in relayPanes) {
            runtime.mutex.withLock { runtime.surface.onDisconnect() }
        }
    }

    // ── command API (ViewModel → repository → transport) ─────────────

    /** `respond` — answer a blocked approval (`source: 'App'` rides raw). */
    suspend fun respond(paneId: String, index: Int, choice: String): CommandResultMessage {
        val agent = requireAgent(paneId)
        val fingerprint = agent.approvalFingerprint
            ?: throw IllegalStateException(
                "This approval no longer has an exact verified identity.",
            )
        agentStore.markResponding(paneId)
        try {
            return requestRaw(
                agent.relayId,
                Inbound(
                    type = "respond",
                    index = index,
                    total = agent.options?.size ?: 0,
                    choice = choice,
                    eventId = agent.eventId.orEmpty(),
                    approvalFingerprint = fingerprint,
                ).withAgentTarget(agent),
                extras = stringExtras("source" to "App"),
                timeoutMs = RESPOND_TIMEOUT_MS,
            )
        } catch (failure: Exception) {
            agentStore.clearResponding(paneId)
            throw failure
        }
    }

    /** `answer_question` — submit the structured-question draft. */
    suspend fun answerQuestion(
        paneId: String,
        interaction: Interaction,
        selectedIndices: List<Int>,
        otherSelected: Boolean,
        otherText: String,
    ): CommandResultMessage {
        val agent = requireAgent(paneId)
        agentStore.markResponding(paneId)
        try {
            return requestRaw(
                agent.relayId,
                Inbound(
                    type = "answer_question",
                    interactionId = interaction.id,
                    selectedIndices = selectedIndices.sorted(),
                    otherSelected = otherSelected,
                    otherText = if (otherSelected) otherText else "",
                ).withAgentTarget(agent),
                extras = stringExtras("source" to "App"),
                timeoutMs = QUESTION_TIMEOUT_MS,
            )
        } catch (failure: Exception) {
            agentStore.clearResponding(paneId)
            throw failure
        }
    }

    /** `navigate_question` — previous/next in a multi-question card. */
    suspend fun navigateQuestion(
        paneId: String,
        interaction: Interaction,
        direction: String,
    ): CommandResultMessage {
        val agent = requireAgent(paneId)
        agentStore.markResponding(paneId)
        try {
            return requestRaw(
                agent.relayId,
                Inbound(
                    type = "navigate_question",
                    interactionId = interaction.id,
                    direction = direction,
                ).withAgentTarget(agent),
                extras = stringExtras("source" to "App"),
                timeoutMs = QUESTION_TIMEOUT_MS,
            )
        } catch (failure: Exception) {
            agentStore.clearResponding(paneId)
            throw failure
        }
    }

    /** `clarify_question` — ask the agent to rephrase the open question. */
    suspend fun clarifyQuestion(paneId: String, interaction: Interaction): CommandResultMessage {
        val agent = requireAgent(paneId)
        agentStore.markResponding(paneId)
        try {
            return requestRaw(
                agent.relayId,
                Inbound(type = "clarify_question", interactionId = interaction.id)
                    .withAgentTarget(agent),
                extras = stringExtras("source" to "App"),
                timeoutMs = QUESTION_TIMEOUT_MS,
            )
        } catch (failure: Exception) {
            agentStore.clearResponding(paneId)
            throw failure
        }
    }

    /** `acknowledge_pane` — dismiss a finished pane's attention state. */
    suspend fun acknowledgePane(paneId: String): CommandResultMessage {
        val agent = requireAgent(paneId)
        return sendToAgent(agent, Inbound(type = "acknowledge_pane"))
    }

    /** `submit_prompt` — composer text in feed mode. */
    suspend fun submitPrompt(paneId: String, text: String): CommandResultMessage {
        val agent = requireAgent(paneId)
        return sendToAgent(agent, Inbound(type = "submit_prompt", text = text))
    }

    /** `send_input` — terminal text mode (text + Enter as one action). */
    suspend fun sendTerminalText(
        paneId: String,
        text: String,
        activityLabel: String = "Submitted terminal text",
    ): CommandResultMessage {
        val agent = requireAgent(paneId)
        return requestRaw(
            agent.relayId,
            Inbound(type = "send_input", text = text, keys = listOf("Enter"))
                .withAgentTarget(agent),
            extras = stringExtras("activity_label" to activityLabel),
        )
    }

    /** `send_keys` — terminal key chords ("Enter", "Ctrl+C", arrows…). */
    suspend fun sendKeys(
        paneId: String,
        keys: List<String>,
        activityLabel: String = keys.joinToString(", "),
    ): CommandResultMessage {
        val agent = requireAgent(paneId)
        return requestRaw(
            agent.relayId,
            Inbound(type = "send_keys", keys = keys).withAgentTarget(agent),
            extras = stringExtras("activity_label" to activityLabel),
        )
    }

    /** `send_text` — literal text injection without the Enter key. */
    suspend fun sendText(
        paneId: String,
        text: String,
        activityLabel: String = "Typed text",
    ): CommandResultMessage {
        val agent = requireAgent(paneId)
        return requestRaw(
            agent.relayId,
            Inbound(type = "send_text", text = text).withAgentTarget(agent),
            extras = stringExtras("activity_label" to activityLabel),
        )
    }

    // ── attachment uploads ────────────────────────────────────────────

    /**
     * `upload_begin` — stages a batch on the relay; answers
     * `upload_begin_result` `{upload_id, chunk_bytes, expires_at, limits}`.
     * Upload frames carry only `target` + their own fields — the oracle's
     * `sendUploadRequest` spreads the request over the top-level map, so
     * `files`/`upload_id`/`file_index`/`sequence`/`sha256` ride as raw
     * extras the flat [Inbound] does not declare.
     */
    suspend fun uploadBegin(paneId: String, files: List<UploadFileSpec>): UploadBeginResult {
        val agent = requireAgent(paneId)
        val frame = requestUpload(
            agent.relayId,
            uploadInbound(agent, "upload_begin"),
            extras = mapOf(
                "files" to buildJsonArray {
                    files.forEach { spec ->
                        add(
                            buildJsonObject {
                                put("name", spec.name)
                                put("media_type", spec.mediaType)
                                put("bytes", spec.bytes)
                            },
                        )
                    }
                },
            ),
            resultType = "upload_begin_result",
        )
        return (frame as? UploadBeginResultMessage)?.let(::unwrapUploadResult)
            ?: throw invalidUploadResponse()
    }

    /**
     * `upload_chunk` — one base64 `data` slice; answers `upload_chunk_result`
     * `{file_index, next_sequence, received_bytes}`.
     */
    suspend fun uploadChunk(
        paneId: String,
        uploadId: String,
        fileIndex: Int,
        sequence: Int,
        data: ByteArray,
        sha256: String,
    ): UploadChunkResult {
        val agent = requireAgent(paneId)
        val frame = requestUpload(
            agent.relayId,
            uploadInbound(agent, "upload_chunk").copy(
                data = java.util.Base64.getEncoder().encodeToString(data),
            ),
            extras = mapOf(
                "upload_id" to JsonPrimitive(uploadId),
                "file_index" to JsonPrimitive(fileIndex),
                "sequence" to JsonPrimitive(sequence),
                "sha256" to JsonPrimitive(sha256),
            ),
            resultType = "upload_chunk_result",
        )
        return (frame as? UploadChunkResultMessage)?.let(::unwrapUploadResult)
            ?: throw invalidUploadResponse()
    }

    /**
     * `upload_finish` — whole-file SHA-256 claims; answers
     * `upload_finish_result` `{attachments:[{ref,name,media_type,bytes,sha256,expires_at}]}`.
     */
    suspend fun uploadFinish(
        paneId: String,
        uploadId: String,
        files: List<UploadFileDigest>,
    ): UploadFinishResult {
        val agent = requireAgent(paneId)
        val frame = requestUpload(
            agent.relayId,
            uploadInbound(agent, "upload_finish"),
            extras = mapOf(
                "upload_id" to JsonPrimitive(uploadId),
                "files" to buildJsonArray {
                    files.forEach { digest ->
                        add(
                            buildJsonObject {
                                put("file_index", digest.fileIndex)
                                put("sha256", digest.sha256)
                            },
                        )
                    }
                },
            ),
            resultType = "upload_finish_result",
        )
        return (frame as? UploadFinishResultMessage)?.let(::unwrapUploadResult)
            ?: throw invalidUploadResponse()
    }

    /** `upload_cancel` — discards the staged session; answers `upload_cancel_result` `{}`. */
    suspend fun uploadCancel(paneId: String, uploadId: String) {
        val agent = requireAgent(paneId)
        val frame = requestUpload(
            agent.relayId,
            uploadInbound(agent, "upload_cancel"),
            extras = mapOf("upload_id" to JsonPrimitive(uploadId)),
            resultType = "upload_cancel_result",
        )
        val message = frame as? UploadCancelResultMessage
            ?: throw invalidUploadResponse()
        message.error?.let { error ->
            throw CommandException(
                message = error.code,
                code = error.code,
                apiError = error,
            )
        }
        if (message.result == null) throw invalidUploadResponse()
    }

    /**
     * The oracle's `attachmentController` gate — upload frames need an exact
     * target tuple; without one the relay answers `upload_scope_mismatch`.
     */
    fun canAttachTo(paneId: String): Boolean =
        agentStore.agentNow(paneId)?.wireTarget() != null

    private fun uploadInbound(agent: Agent, type: String): Inbound {
        val target = agent.wireTarget()
            ?: throw IllegalStateException("This terminal does not have a stable attachment target.")
        return Inbound(type = type, target = target)
    }

    /**
     * `sendUploadRequest` — `upload_*` answers arrive on their own
     * `upload_*_result` type (never `command_result`), so they correlate on
     * [pendingUploads]. Same request_id/protocol/write discipline as
     * [requestRaw]; the deferred resolves with the decoded frame.
     */
    private suspend fun requestUpload(
        relayId: String,
        message: Inbound,
        extras: Map<String, JsonElement>,
        resultType: String,
        timeoutMs: Long = UPLOAD_TIMEOUT_MS,
    ): ServerMessage {
        val session = sessionFor(relayId) ?: throw TransportException.NotConnected()
        if (session.state.value !is RelaySession.SessionState.Connected) {
            throw TransportException.NotConnected()
        }
        val requestId = UUID.randomUUID().toString()
        val framed = message.copy(requestId = requestId, protocol = Protocol.VERSION)
        val wire = withExtras(framed, extras)
        val pending = PendingUpload(
            deferred = CompletableDeferred(),
            relayId = relayId,
            responseType = resultType,
        )
        pendingUploads[requestId] = pending
        pending.rearm(requestId, timeoutMs)
        if (!session.sendRaw(wire.toString())) {
            pendingUploads.remove(requestId)
            pending.timeoutJob?.cancel()
            throw TransportException.WriteRejected("Could not send command to relay")
        }
        try {
            return pending.deferred.await()
        } catch (cancelled: CancellationException) {
            pendingUploads.remove(requestId)
            pending.timeoutJob?.cancel()
            throw cancelled
        }
    }

    /** `handleUploadResult` — request_id + relay + matching `*_result` type. */
    private fun resolveUploadResult(relayId: String, message: ServerMessage) {
        val requestId = when (message) {
            is UploadBeginResultMessage -> message.requestId
            is UploadChunkResultMessage -> message.requestId
            is UploadFinishResultMessage -> message.requestId
            is UploadCancelResultMessage -> message.requestId
            else -> return
        } ?: return
        val pending = pendingUploads[requestId] ?: return
        if (pending.relayId != relayId || pending.responseType != message.type) return
        pendingUploads.remove(requestId)
        pending.timeoutJob?.cancel()
        pending.deferred.complete(message)
    }

    /**
     * `rejectPendingOperations` for uploads — a dropped session strands every
     * in-flight `upload_*`; the frame may still have landed, so the failure is
     * `dispatched_unknown` like the oracle.
     */
    private fun rejectUploads(relayId: String, message: String) {
        for ((requestId, pending) in pendingUploads) {
            if (pending.relayId != relayId) continue
            if (pendingUploads.remove(requestId, pending)) {
                pending.timeoutJob?.cancel()
                pending.deferred.completeExceptionally(
                    CommandException(
                        message = message,
                        phase = "dispatched_unknown",
                        dispatchedUnknown = true,
                    ),
                )
            }
        }
    }

    private fun PendingUpload.rearm(requestId: String, timeoutMs: Long) {
        timeoutJob?.cancel()
        timeoutJob = scope.launch {
            delay(timeoutMs)
            if (pendingUploads.remove(requestId, this@rearm)) {
                deferred.completeExceptionally(
                    CommandException(
                        message = "Attachment upload did not finish in time.",
                        phase = "dispatched_unknown",
                        dispatchedUnknown = true,
                    ),
                )
            }
        }
    }

    private fun <T> unwrapUploadResult(message: ServerMessage): T {
        val (result, error) = when (message) {
            is UploadBeginResultMessage -> message.result to message.error
            is UploadChunkResultMessage -> message.result to message.error
            is UploadFinishResultMessage -> message.result to message.error
            is UploadCancelResultMessage -> message.result to message.error
            else -> null to null
        }
        if (error != null) {
            throw CommandException(
                message = error.code,
                code = error.code,
                apiError = error,
            )
        }
        @Suppress("UNCHECKED_CAST")
        return (result as? T) ?: throw invalidUploadResponse()
    }

    private fun invalidUploadResponse(): CommandException = CommandException(
        message = "Relay returned an invalid attachment upload result.",
        code = "attachment_invalid_response",
    )

    /**
     * `lease_pane_size` — the terminal view's measured grid. Rows ride only
     * when the relay advertises `pane_size_lease_rows` (an old relay would
     * silently ignore them); returns the applied dimensions.
     */
    suspend fun leasePaneSize(paneId: String, columns: Int, rows: Int = 0): Pair<Int, Int> {
        val agent = requireAgent(paneId)
        val connection = connectionStore.connectionNow(agent.relayId)
        if (connection?.capabilities?.contains(LEASE_CAPABILITY) != true) {
            throw CommandException("Relay lacks pane-size lease support")
        }
        if (columns < MIN_PANE_COLUMNS || columns > MAX_PANE_COLUMNS) {
            throw CommandException(
                "Terminal columns must be between $MIN_PANE_COLUMNS and $MAX_PANE_COLUMNS",
            )
        }
        val leaseRows = if (connection.capabilities.contains(LEASE_ROWS_CAPABILITY)) rows else 0
        if (leaseRows != 0 && (leaseRows < MIN_PANE_ROWS || leaseRows > MAX_PANE_ROWS)) {
            throw CommandException(
                "Terminal rows must be between $MIN_PANE_ROWS and $MAX_PANE_ROWS",
            )
        }
        val result = sendToAgent(
            agent,
            Inbound(type = "lease_pane_size", columns = columns, rows = leaseRows),
        )
        val appliedColumns = result.intField("columns") ?: throw CommandException(
            "Relay did not confirm the applied terminal columns",
        )
        val appliedRows = if (leaseRows != 0) {
            result.intField("rows") ?: throw CommandException(
                "Relay did not confirm the applied terminal rows",
            )
        } else {
            0
        }
        val runtime = synchronized(lock) { panes[paneId] }
        if (runtime != null) {
            runtime.mutex.withLock {
                runtime.surface.resize(appliedColumns, appliedRows)?.let {
                    runtime.snapshots.value = it
                }
            }
        }
        return appliedColumns to appliedRows
    }

    /** `release_pane_size` — drop the lease on close/background. */
    suspend fun releasePaneSize(paneId: String) {
        val agent = requireAgent(paneId)
        if (connectionStore.connectionNow(agent.relayId)
                ?.capabilities?.contains(LEASE_CAPABILITY) != true
        ) {
            throw CommandException("Relay lacks pane-size lease support")
        }
        sendToAgent(agent, Inbound(type = "release_pane_size"))
    }

    // ── workspace inspection (Files mode) ────────────────────────────

    /**
     * `workspace_tree` — the whole workspace as one bounded flat listing
     * (relay caps at 4,000 entries and reports `truncated`).
     */
    suspend fun workspaceTree(paneId: String): WorkspaceTree {
        val agent = requireAgent(paneId)
        requireWorkspaceInspection(agent)
        val result = sendToAgent(
            agent,
            Inbound(type = "workspace_tree"),
            timeoutMs = WORKSPACE_TIMEOUT_MS,
        )
        return parseWorkspaceTree(result.data)
    }

    /** `workspace_file` — bounded text (1 MiB) / image (5 MiB, data-url) preview. */
    suspend fun workspaceFile(paneId: String, path: String): WorkspaceFilePreview {
        val agent = requireAgent(paneId)
        requireWorkspaceInspection(agent)
        val result = sendToAgent(
            agent,
            Inbound(type = "workspace_file", path = path),
            timeoutMs = WORKSPACE_TIMEOUT_MS,
        )
        return parseWorkspaceFile(result.data, path)
    }

    /** `workspace_git_status` — porcelain status; `available=false` is "not a repo". */
    suspend fun workspaceGitStatus(paneId: String): WorkspaceGitStatus {
        val agent = requireAgent(paneId)
        requireWorkspaceInspection(agent)
        val result = sendToAgent(
            agent,
            Inbound(type = "workspace_git_status"),
            timeoutMs = WORKSPACE_TIMEOUT_MS,
        )
        return parseWorkspaceGitStatus(result.data)
    }

    /** `workspace_git_diff` — staged + unstaged unified diff for one changed path. */
    suspend fun workspaceGitDiff(paneId: String, path: String): WorkspaceGitDiff {
        val agent = requireAgent(paneId)
        requireWorkspaceInspection(agent)
        val result = sendToAgent(
            agent,
            Inbound(type = "workspace_git_diff", path = path),
            timeoutMs = WORKSPACE_TIMEOUT_MS,
        )
        return parseWorkspaceGitDiff(result.data, path)
    }

    /** `workspaceInspectionAvailable` — the oracle's capability + cwd gate. */
    private fun requireWorkspaceInspection(agent: Agent) {
        val connection = connectionStore.connectionNow(agent.relayId)
        if (connection?.capabilities?.contains(WORKSPACE_INSPECTION_CAPABILITY) != true) {
            throw CommandException("This relay does not support workspace inspection.")
        }
        if (agent.cwd.isNullOrBlank()) {
            throw CommandException("This agent does not report a workspace path.")
        }
    }

    /** `get_conversation_history` — one tail-first page, projected. */
    suspend fun conversationPage(
        paneId: String,
        request: ConversationPageRequest = ConversationPageRequest(),
    ): ConversationPage {
        val agent = requireAgent(paneId)
        val result = sendToAgent(
            agent,
            Inbound(
                type = ConversationPageRequest.ACTION,
                cursor = request.cursor,
                limit = request.limit,
                retry = request.retry,
            ),
            timeoutMs = CONVERSATION_TIMEOUT_MS,
        )
        return ConversationProjector.project(result)
    }

    /** `refresh_agents` fan-out — the keepalive action doubles as refresh. */
    fun refreshAgents() {
        broadcast("{\"type\":\"refresh_agents\"}")
    }

    /** `get_activity` fan-out — pull the relay's journal (limit 500). */
    fun requestActivities() {
        synchronized(lock) { sessions.keys.toList() }.forEach(::requestActivities)
    }

    private fun requestActivities(relayId: String) {
        sessionFor(relayId)?.sendRaw("{\"type\":\"get_activity\",\"limit\":$ACTIVITY_LIMIT}")
    }

    /** `removeRelay` — config + credentials + live session teardown. */
    suspend fun removeRelay(relayId: String) {
        relayRegistry.remove(relayId)
        credentialStore.remove(relayId)
    }

    // ── read seams for screens ────────────────────────────────────────

    fun agent(paneId: String): Flow<Agent?> = agentStore.agent(paneId)
    fun connection(relayId: String): Flow<RelayConnection?> = connectionStore.connection(relayId)
    fun connectionNow(relayId: String): RelayConnection? = connectionStore.connectionNow(relayId)

    /** Session state for the pairing flow's outcome await. */
    fun sessionState(relayId: String): StateFlow<RelaySession.SessionState>? =
        synchronized(lock) { sessions[relayId]?.handle?.state }

    val agents: StateFlow<List<Agent>> get() = agentStore.agents
    val responding: StateFlow<Set<String>> get() = agentStore.responding
    val connections get() = connectionStore.connections
    val relays get() = relayRegistry.relays

    // ── command plumbing ─────────────────────────────────────────────

    private fun requireAgent(paneId: String): Agent =
        agentStore.agentNow(paneId)
            ?: throw IllegalStateException("This agent is no longer available.")

    private fun sessionFor(relayId: String): RelaySessionHandle? =
        synchronized(lock) { sessions[relayId]?.handle }

    private fun broadcast(json: String) {
        synchronized(lock) { sessions.values.toList() }.forEach { it.handle.sendRaw(json) }
    }

    /**
     * `sendToAgent` — typed request over [RelaySession.request]: the relay
     * reads every field this path needs from the declared [Inbound].
     */
    private suspend fun sendToAgent(
        agent: Agent,
        message: Inbound,
        timeoutMs: Long = ReconnectPolicy.COMMAND_TIMEOUT_MS,
    ): CommandResultMessage {
        val session = sessionFor(agent.relayId)
            ?: throw TransportException.NotConnected()
        return session.request(message.withAgentTarget(agent), timeoutMs)
    }

    /**
     * `sendCommand` for messages carrying fields the flat `Inbound` cannot
     * express (`activity_label`, `source`, speech ids — the relay reads them
     * from the raw map). Correlates its own `command_result`/`action_receipt`
     * here; [RelaySession]'s pending map ignores foreign request ids, so the
     * frames still arrive through [RelaySessionHandle.incoming].
     */
    private suspend fun requestRaw(
        relayId: String,
        message: Inbound,
        extras: Map<String, kotlinx.serialization.json.JsonElement>,
        timeoutMs: Long = ReconnectPolicy.COMMAND_TIMEOUT_MS,
    ): CommandResultMessage {
        val session = sessionFor(relayId) ?: throw TransportException.NotConnected()
        if (session.state.value !is RelaySession.SessionState.Connected) {
            throw TransportException.NotConnected()
        }
        val requestId = UUID.randomUUID().toString()
        val framed = message.copy(requestId = requestId, protocol = Protocol.VERSION)
        val wire = withExtras(framed, extras)
        val pending = PendingRaw(
            deferred = CompletableDeferred(),
            action = framed.type,
            actionId = framed.actionId.ifEmpty { null },
        )
        pendingRaw[requestId] = pending
        pending.rearm(requestId, timeoutMs)
        if (!session.sendRaw(wire.toString())) {
            pendingRaw.remove(requestId)
            pending.timeoutJob?.cancel()
            throw TransportException.WriteRejected("Could not send command to relay")
        }
        return pending.deferred.await()
    }

    private fun resolveCommandResult(message: CommandResultMessage) {
        val requestId = message.requestId ?: return
        val pending = pendingRaw[requestId] ?: return
        if (message.phase == CommandResultMessage.PHASE_ACCEPTED) {
            pending.rearm(requestId, ReconnectPolicy.ACCEPTED_COMMAND_TIMEOUT_MS)
            return
        }
        pendingRaw.remove(requestId)
        pending.timeoutJob?.cancel()
        if (message.ok == true) {
            pending.deferred.complete(message)
        } else {
            pending.deferred.completeExceptionally(
                CommandException(
                    message = message.error ?: "Command failed",
                    phase = message.phase,
                    dispatchedUnknown = message.phase == "dispatched_unknown",
                ),
            )
        }
    }

    private fun resolveActionReceipt(message: ActionReceiptMessage) {
        val receipt = message.receipt ?: return
        var key = message.requestId
        var pending = key?.let { pendingRaw[it] }
        if (pending == null && receipt.actionId.isNotEmpty()) {
            for ((candidateId, candidate) in pendingRaw) {
                if (candidate.actionId == receipt.actionId) {
                    key = candidateId
                    pending = candidate
                    break
                }
            }
        }
        if (pending == null || key == null) return
        when (receipt.phase) {
            ActionReceiptPhase.PREPARED, ActionReceiptPhase.AWAITING_EVIDENCE ->
                pending.rearm(key, ReconnectPolicy.ACCEPTED_COMMAND_TIMEOUT_MS)
            ActionReceiptPhase.CONFIRMED -> {
                pendingRaw.remove(key)
                pending.timeoutJob?.cancel()
                pending.deferred.complete(
                    CommandResultMessage(
                        action = pending.action,
                        ok = true,
                        phase = CommandResultMessage.PHASE_CONFIRMED,
                        requestId = key,
                    ),
                )
            }
            else -> {
                pendingRaw.remove(key)
                pending.timeoutJob?.cancel()
                pending.deferred.completeExceptionally(
                    CommandException(
                        message = receipt.error?.code ?: "Command failed",
                        phase = receipt.phase.wireName(),
                        apiError = receipt.error,
                        dispatchedUnknown = receipt.phase == ActionReceiptPhase.DISPATCHED_UNKNOWN,
                    ),
                )
            }
        }
    }

    private fun resolveError(message: ErrorMessage) {
        val requestId = message.requestId ?: return
        val pending = pendingRaw.remove(requestId) ?: return
        pending.timeoutJob?.cancel()
        val apiError = message.error
        pending.deferred.completeExceptionally(
            CommandException(
                message = apiError?.code ?: "Command failed",
                code = apiError?.code,
                phase = "failed_before_dispatch",
                apiError = apiError,
            ),
        )
    }

    private fun PendingRaw.rearm(requestId: String, timeoutMs: Long) {
        timeoutJob?.cancel()
        timeoutJob = scope.launch {
            delay(timeoutMs)
            if (pendingRaw.remove(requestId, this@rearm)) {
                deferred.completeExceptionally(
                    CommandException(
                        message = "Relay confirmation timed out",
                        phase = "dispatched_unknown",
                        dispatchedUnknown = true,
                    ),
                )
            }
        }
    }

    // ── activity journal ─────────────────────────────────────────────

    private fun upsertActivity(relayId: String, entry: ActivityEntry?) {
        if (entry == null || entry.timestamp == 0L) return
        val item = RelayActivity(activityKey(relayId, entry), relayId, entry)
        _activities.value = (
            listOf(item) + _activities.value.filterNot { it.key == item.key }
            ).sortedByDescending { it.entry.timestamp }.take(MAX_ACTIVITIES)
    }

    private fun mergeActivityHistory(relayId: String, entries: List<ActivityEntry>?) {
        val incoming = entries.orEmpty()
            .filter { it.timestamp != 0L }
            .map { RelayActivity(activityKey(relayId, it), relayId, it) }
        _activities.value = (_activities.value.filterNot { it.relayId == relayId } + incoming)
            .sortedByDescending { it.entry.timestamp }
            .take(MAX_ACTIVITIES)
    }

    private fun activityKey(relayId: String, entry: ActivityEntry): String =
        "$relayId:${entry.id.ifEmpty { "${entry.timestamp}:${entry.kind}:${entry.requestId}" }}"

    // ── runtime records ──────────────────────────────────────────────

    private inner class SessionRuntime(
        val endpoint: RelayEndpoint,
        val handle: RelaySessionHandle,
        val jobs: MutableList<Job> = mutableListOf(),
    )

    private inner class PaneRuntime(
        val paneId: String,
        val relayId: String,
        val surface: PaneSurface,
        val snapshots: MutableStateFlow<PaneSurface.Snapshot?>,
        val mutex: Mutex = Mutex(),
    )

    private class PendingRaw(
        val deferred: CompletableDeferred<CommandResultMessage>,
        val action: String,
        val actionId: String?,
        @Volatile var timeoutJob: Job? = null,
    )

    private class PendingUpload(
        val deferred: CompletableDeferred<ServerMessage>,
        val relayId: String,
        val responseType: String,
        @Volatile var timeoutJob: Job? = null,
    )

    companion object {
        const val REALTIME_DELTA_CAPABILITY = "pane_realtime_delta"
        const val LEASE_CAPABILITY = "pane_size_lease"
        const val LEASE_ROWS_CAPABILITY = "pane_size_lease_rows"
        const val MIN_PANE_COLUMNS = 40
        const val MAX_PANE_COLUMNS = 240
        const val MIN_PANE_ROWS = 10
        const val MAX_PANE_ROWS = 120
        const val RESPOND_TIMEOUT_MS = 12_000L
        const val QUESTION_TIMEOUT_MS = 20_000L
        const val CONVERSATION_TIMEOUT_MS = 20_000L
        const val WORKSPACE_TIMEOUT_MS = 20_000L
        const val WORKSPACE_INSPECTION_CAPABILITY = "workspace_inspection"
        /** `ATTACHMENT_UPLOAD_TIMEOUT_MS` — per-request, chunks included. */
        const val UPLOAD_TIMEOUT_MS = 60_000L
        const val ACTIVITY_LIMIT = 500
        const val MAX_ACTIVITIES = 500
    }
}

private fun Inbound.withAgentTarget(agent: Agent): Inbound {
    val target = agent.wireTarget()
        ?: throw IllegalStateException("This agent no longer has an exact terminal identity")
    return withPaneTarget(agent, target)
}

private fun ActionReceiptPhase.wireName(): String = when (this) {
    ActionReceiptPhase.PREPARED -> "prepared"
    ActionReceiptPhase.FAILED_BEFORE_DISPATCH -> "failed_before_dispatch"
    ActionReceiptPhase.AWAITING_EVIDENCE -> "awaiting_evidence"
    ActionReceiptPhase.CONFIRMED -> "confirmed"
    ActionReceiptPhase.DISPATCHED_UNKNOWN -> "dispatched_unknown"
}

private fun CommandResultMessage.intField(name: String): Int? =
    (data as? JsonObject)?.get(name)
        ?.let { it as? JsonPrimitive }
        ?.let { primitive -> primitive.content.toIntOrNull() }
        ?.takeIf { it > 0 }
