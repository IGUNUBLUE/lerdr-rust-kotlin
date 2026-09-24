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
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.flowOf
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
import lerdr.core.data.DeviceRole
import lerdr.core.data.RelayDeviceAuth
import lerdr.core.data.RelayDeviceCredential
import lerdr.core.data.RelayEndpoint
import lerdr.core.data.RelayRegistry
import lerdr.core.data.fromFinish
import lerdr.core.model.ActionReceiptMessage
import lerdr.core.model.ActionReceiptPhase
import lerdr.core.model.ActivityEntry
import lerdr.core.model.ActivityHistoryMessage
import lerdr.core.model.ActivityMessage
import lerdr.core.model.ClientCapabilities
import lerdr.core.model.CommandResultMessage
import lerdr.core.model.ErrorMessage
import lerdr.core.model.Inbound
import lerdr.core.model.Interaction
import lerdr.core.model.PaneLinkActivatedResult
import lerdr.core.model.PaneLinkResolvedResult
import lerdr.core.model.PaneSearchResult
import lerdr.core.model.PaneSelectionResult
import lerdr.core.model.PaneTextPoint
import lerdr.core.model.PaneTextRange
import lerdr.core.model.ServerMessage
import lerdr.core.model.TargetRef
import lerdr.core.model.UnknownServerMessage
import lerdr.core.model.UpdateState
import lerdr.core.model.UpdateStatusMessage
import lerdr.core.model.UploadBeginResult
import lerdr.core.model.UploadBeginResultMessage
import lerdr.core.model.UploadCancelResultMessage
import lerdr.core.model.UploadChunkResult
import lerdr.core.model.UploadChunkResultMessage
import lerdr.core.model.UploadFinishResult
import lerdr.core.model.UploadFinishResultMessage
import lerdr.core.protocol.LerdrJson
import lerdr.core.protocol.Protocol
import lerdr.core.protocol.ServerMessageCodec
import lerdr.core.store.Agent
import lerdr.core.store.AgentStore
import lerdr.core.store.ConnectionStore
import lerdr.core.store.RelayConnection
import lerdr.core.store.RelayStatus
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

    /** Bumped on every `panes` insert/remove so `paneSnapshot` re-resolves. */
    private val paneGeneration = MutableStateFlow(0L)

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
        // Oracle parity: the viewed-pane effect re-derives when the agent
        // projection (regeneration/respawn) or a relay's connection status
        // moves, not only when the screen changes.
        scope.launch { agents.collect { repushViewedPane() } }
        scope.launch {
            connectionStore.connections.collect {
                repushViewedPane()
                reconcilePendingUpdates()
                maybeAutoCheckUpdates()
            }
        }
    }

    /**
     * `setHidden` fan-out — hidden sessions retire keepalives per policy, and
     * open panes unwatch/re-watch with the app (`visibilitychange` parity:
     * the `openPanes` intent survives, so resume re-arms watch + read).
     */
    fun setHidden(hidden: Boolean) {
        synchronized(lock) { sessions.values.toList() }
            .forEach { it.handle.setHidden(hidden) }
        if (_hidden.value == hidden) return
        _hidden.value = hidden
        synchronized(lock) { hiddenSince = if (hidden) System.currentTimeMillis() else 0L }
        repushViewedPane()
        scope.launch {
            val paneIds = synchronized(lock) { openPanes.toList() }
            for (paneId in paneIds) {
                val runtime = synchronized(lock) { panes[paneId] } ?: continue
                runtime.mutex.withLock {
                    val intents = if (hidden) {
                        runtime.surface.unwatch()
                    } else {
                        runtime.surface.requestRead() + runtime.surface.watch()
                    }
                    dispatchIntents(runtime, intents)
                }
            }
        }
    }

    /** App visibility — the terminal's lease renewal gates on it. */
    val hidden: StateFlow<Boolean> get() = _hidden
    private val _hidden = MutableStateFlow(false)

    /** `hiddenAt` — when the app last went hidden; 0 while visible. */
    @Volatile
    private var hiddenSince = 0L

    /**
     * `paneLeaseRenewalAllowed` — a hidden app renews only within the 5 min
     * grace; after it the relay TTL hands the pane's size back to the desktop.
     */
    fun paneLeaseRenewalAllowed(): Boolean {
        if (!_hidden.value) return true
        val since = synchronized(lock) { hiddenSince }
        return since > 0 && System.currentTimeMillis() - since < PANE_LEASE_HIDDEN_GRACE_MS
    }

    /**
     * This device's enrolled role on the relay — the oracle's
     * `readOnlyRelayIds` source. Only a stored credential carries a role;
     * invitations and unpaired relays return null.
     */
    fun deviceRole(relayId: String): DeviceRole? =
        (authByRelay[relayId] as? RelayDeviceCredential)?.role

    /**
     * The oracle's `readOnly` gate (fail-closed): mutating UI enables only
     * when the enrolled role is proven CONTROLLER.
     */
    fun canControl(relayId: String): Boolean = deviceRole(relayId) == DeviceRole.CONTROLLER

    // ── viewed pane (push_viewed_pane) ─────────────────────────────

    private val _locked = MutableStateFlow(false)

    /** The pane the UI is showing; [repushViewedPane] derives the wire state. */
    @Volatile
    private var viewedPaneId: String? = null

    @Volatile
    private var viewedRelayId: String? = null

    @Volatile
    private var viewedSignature: String = ""

    /**
     * The oracle's `$securityState.locked` input — `LerdrApp` feeds
     * `lockState.locked`. A transition re-derives the signature: locking
     * clears the viewed pane (`unlocked: false`), unlocking re-publishes it.
     */
    fun setLocked(locked: Boolean) {
        if (_locked.value == locked) return
        _locked.value = locked
        repushViewedPane()
    }

    /**
     * `push_viewed_pane` — tells each relay which pane this device is
     * viewing so push delivery suppresses it (the oracle's App-level
     * `$effect`). Deduped by the `relay:pane:terminal:session:generation`
     * signature; a change clears the previous relay's view before
     * publishing the new one. Session screens call this on enter/leave;
     * agent regeneration, reconnect, lock, and hide re-push reactively.
     */
    fun setViewedPane(paneId: String?) {
        viewedPaneId = paneId
        repushViewedPane()
    }

    /**
     * The oracle's reactive core: the signature is non-empty only while the
     * app is visible + unlocked, the agent is a `primary`-session pane with
     * a complete target tuple, and its relay is `connected`.
     */
    private fun repushViewedPane() {
        synchronized(lock) {
            val agent = viewedPaneId?.let { id ->
                agents.value.firstOrNull { it.paneId == id }
            }
            val viewed = agent
                ?.takeIf {
                    !_hidden.value && !_locked.value && it.serverSessionId == "primary"
                }
                ?.let { a -> a.wireTarget()?.let { t -> a to t } }
                ?.takeIf { (a, _) ->
                    connectionStore.connectionNow(a.relayId)?.status == RelayStatus.CONNECTED
                }
            val signature = viewed?.let { (a, t) ->
                "${a.relayId}:${t.paneId}:${t.terminalId}:" +
                    "${t.agentSessionId}:${t.generation}"
            }.orEmpty()
            if (signature == viewedSignature) return
            viewedRelayId?.let { previous ->
                sendViewedFrame(previous, visible = false, target = null)
            }
            viewedRelayId = viewed?.first?.relayId
            viewedSignature = signature
            viewed?.let { (a, t) ->
                sendViewedFrame(a.relayId, visible = true, target = t)
            }
        }
    }

    private fun sendViewedFrame(
        relayId: String,
        visible: Boolean,
        target: TargetRef?,
    ) {
        val session = sessionFor(relayId) ?: return
        session.sendRaw(
            buildJsonObject {
                put("type", "push_viewed_pane")
                put("protocol", Protocol.VERSION)
                put("visible", visible)
                // Oracle parity: the set frame reports `unlocked: true`
                // outright (a locked app yields an empty signature, never a
                // set frame); only the clear frame reports the real state.
                put("unlocked", if (visible) true else !_locked.value)
                target?.let {
                    put("target", LerdrJson.encodeToJsonElement(TargetRef.serializer(), it))
                }
            }.toString(),
        )
    }

    // ── relay self-update (check_update / install_update) ──────────

    /**
     * `check_update` — `self_update`-gated (the oracle's checkRelayUpdate).
     * The reply's `data.update` folds into the connection row like the
     * oracle's connection.update assignment.
     */
    suspend fun checkUpdate(relayId: String): CommandResultMessage {
        if (connectionStore.connectionNow(relayId)
                ?.capabilities?.contains(SELF_UPDATE_CAPABILITY) != true
        ) {
            throw CommandException("This relay does not support phone-driven updates yet")
        }
        val result = try {
            request(
                relayId,
                Inbound(type = "check_update"),
                timeoutMs = UPDATE_COMMAND_TIMEOUT_MS,
            )
        } catch (error: CommandException) {
            applyUpdatePayload(relayId, error.data)
            throw error
        }
        applyUpdatePayload(relayId, result.data)
        return result
    }

    /**
     * `install_update` — deploys the checked candidate (the oracle's
     * `installRelayUpdate`). Only an `available` + `can_install` update
     * with a `target_revision` is installable; the expected version and
     * revision ride the request so a mid-flight upstream move refuses
     * instead of installing the wrong build. A refusal's `data.update`
     * still refreshes the row and clears the pending marker.
     */
    suspend fun installUpdate(relayId: String): CommandResultMessage {
        val connection = connectionStore.connectionNow(relayId)
        if (connection?.capabilities?.contains(SELF_UPDATE_CAPABILITY) != true) {
            throw CommandException("This relay does not support phone-driven updates yet")
        }
        val update = connection.update
        val expectedRevision = update?.targetRevision.orEmpty()
        if (update?.state != "available" || update.canInstall != true ||
            expectedRevision.isEmpty()
        ) {
            throw CommandException(update?.reason ?: "No installable update is available")
        }
        val expectedVersion = update.availableVersion.orEmpty()
        pendingUpdates[relayId] = PendingUpdate(expectedVersion, expectedRevision)
        val result = try {
            request(
                relayId,
                Inbound(type = "install_update"),
                extras = mapOf(
                    "expected_version" to JsonPrimitive(expectedVersion),
                    "expected_revision" to JsonPrimitive(expectedRevision),
                ),
                timeoutMs = UPDATE_COMMAND_TIMEOUT_MS,
            )
        } catch (error: CommandException) {
            pendingUpdates.remove(relayId)
            applyUpdatePayload(relayId, error.data)
            throw error
        }
        applyUpdatePayload(relayId, result.data)
        return result
    }

    /**
     * A `install_update` we dispatched whose relay restarted mid-install —
     * kept until the reconnect reports `releaseVersion`/`revision` matching
     * the target (the oracle's `pendingRelayUpdates`, process-scoped here:
     * the restart lands within seconds while the app stays alive).
     */
    /** A dispatched `install_update` awaiting post-restart confirmation. */
    data class PendingUpdate(val version: String, val revision: String)

    private val pendingUpdates = java.util.concurrent.ConcurrentHashMap<String, PendingUpdate>()

    /** `pendingRelayUpdate` consumers: relays that just came back updated. */
    private val _completedUpdates = MutableStateFlow<Map<String, PendingUpdate>>(emptyMap())

    /** One-shot events keyed by relay id — UI shows "updated to vX". */
    val completedUpdates: StateFlow<Map<String, PendingUpdate>> = _completedUpdates

    fun consumeCompletedUpdate(relayId: String) {
        _completedUpdates.value = _completedUpdates.value - relayId
    }

    /**
     * The oracle's `$connections` effect: on reconnect a pending install
     * whose `releaseVersion`/`revision` (modulo `-dirty`) now matches the
     * target is declared complete — the restart dropped the session before
     * `command_result` could land.
     */
    private fun reconcilePendingUpdates() {
        for ((relayId, pending) in pendingUpdates) {
            val connection = connectionStore.connectionNow(relayId) ?: continue
            if (connection.status != RelayStatus.CONNECTED) continue
            if (connection.releaseVersion != pending.version) continue
            val revision = connection.revision.removeSuffix("-dirty")
            if (revision.isEmpty() || !pending.revision.startsWith(revision)) continue
            if (pendingUpdates.remove(relayId, pending)) {
                _completedUpdates.value = _completedUpdates.value + (relayId to pending)
            }
        }
    }

    /**
     * The oracle's auto-check effect (App.svelte): once per
     * `relay:releaseVersion:revision:appVersion` identity, a connected
     * `self_update`-capable relay gets a `check_update` — a failed check
     * releases the identity so the next reconnect retries.
     */
    private val autoCheckedUpdates = java.util.concurrent.ConcurrentHashMap.newKeySet<String>()

    private fun maybeAutoCheckUpdates() {
        for ((relayId, connection) in connectionStore.connections.value) {
            if (connection.status != RelayStatus.CONNECTED) continue
            if (!connection.capabilities.contains(SELF_UPDATE_CAPABILITY)) continue
            val identity = "$relayId:${connection.releaseVersion}:" +
                "${connection.revision}:${com.lerdr.app.BuildConfig.VERSION_NAME}"
            if (!autoCheckedUpdates.add(identity)) continue
            scope.launch {
                try {
                    checkUpdate(relayId)
                } catch (cancelled: kotlinx.coroutines.CancellationException) {
                    autoCheckedUpdates.remove(identity)
                    throw cancelled
                } catch (_: Exception) {
                    autoCheckedUpdates.remove(identity)
                }
            }
        }
    }

    private fun applyUpdatePayload(relayId: String, data: JsonElement?) {
        val update = (data as? JsonObject)?.get("update") as? JsonObject ?: return
        runCatching {
            connectionStore.applyUpdateStatus(
                relayId,
                UpdateStatusMessage(
                    update = LerdrJson.decodeFromJsonElement(UpdateState.serializer(), update),
                ),
            )
        }
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
            runtime.jobs += scope.launch {
                handle.rttMs.collect { connectionStore.noteRtt(endpoint.id, it) }
            }
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
            if (panes.values.removeAll { it.relayId == runtime.endpoint.id }) {
                paneGeneration.value += 1
            }
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
        _frames.tryEmit(RelayFrame(relayId, message))

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
        val runtime = synchronized(lock) {
            panes.remove(paneId)?.also { paneGeneration.value += 1 }
        } ?: return
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

    /**
     * The render seam — null until the first frame commits. Re-resolves the
     * runtime whenever the registry mutates so collectors survive runtime
     * creation (collector racing `openPane`) and replacement (reconnect,
     * `closePane`/`openPane` cycles) instead of pinning a dead runtime.
     */
    @OptIn(ExperimentalCoroutinesApi::class)
    fun paneSnapshot(paneId: String): Flow<PaneSurface.Snapshot?> =
        paneGeneration.flatMapLatest {
            synchronized(lock) { panes[paneId] }?.snapshots ?: flowOf(null)
        }

    private fun paneRuntime(paneId: String): PaneRuntime? {
        val separator = paneId.indexOf("::")
        if (separator <= 0) return null
        val relayId = paneId.substring(0, separator)
        synchronized(lock) { sessions[relayId] } ?: return null
        return synchronized(lock) {
            val isNew = paneId !in panes
            val runtime = panes.getOrPut(paneId) {
                PaneRuntime(
                    paneId = paneId,
                    relayId = relayId,
                    surface = PaneSurface(paneId = paneId),
                    snapshots = MutableStateFlow(null),
                )
            }
            if (isNew) paneGeneration.value += 1
            runtime
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
            // A hidden app stays unwatched — `setHidden(false)` re-arms on
            // resume; resyncing here would leak watch traffic in background.
            if (_hidden.value) return
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

    /**
     * `acknowledge_pane` — dismiss a finished pane's attention state. Like
     * the oracle, a `done` pane flips to `idle` optimistically before the
     * command lands; readers never reach this (callers gate on
     * [canControl]).
     */
    suspend fun acknowledgePane(paneId: String): CommandResultMessage {
        val agent = requireAgent(paneId)
        agentStore.acknowledgeDone(paneId)
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

    /** `send_secret` — password-prompt answer; the relay never journals it. */
    suspend fun sendSecret(paneId: String, text: String): CommandResultMessage {
        val agent = requireAgent(paneId)
        if (connectionStore.connectionNow(agent.relayId)
                ?.capabilities?.contains(SECRET_CAPABILITY) != true
        ) {
            throw CommandException("This relay does not support password prompts")
        }
        if (text.isEmpty()) throw CommandException("Enter the password first")
        return sendToAgent(agent, Inbound(type = "send_secret", text = text))
    }

    /** `copy_agent_response` — the relay answers with the rendered reply text. */
    suspend fun copyAgentResponse(paneId: String): CommandResultMessage {
        val agent = requireAgent(paneId)
        return sendToAgent(
            agent,
            Inbound(type = "copy_agent_response"),
            timeoutMs = COPY_RESPONSE_TIMEOUT_MS,
        )
    }

    /** `tab_reorder` — move a pane within its workspace tab strip. */
    suspend fun reorderTab(paneId: String, insertIndex: Int): CommandResultMessage {
        val agent = requireAgent(paneId)
        if (connectionStore.connectionNow(agent.relayId)
                ?.capabilities?.contains(TAB_REORDER_CAPABILITY) != true
        ) {
            throw CommandException("This relay does not support tab ordering")
        }
        if (insertIndex < 0) throw CommandException("Tab position is invalid")
        return sendToAgent(agent, Inbound(type = "tab_reorder", insertIndex = insertIndex))
    }

    // ── Phase-5 Track A — focus / pane content / links / layout (docs/13 §1)

    /** `focus_pane` — raise the pane's tab and window (notification tap → jump). */
    suspend fun focusPane(paneId: String): CommandResultMessage {
        val agent = requireAgent(paneId)
        requireCapability(agent, ClientCapabilities.FOCUS)
        return sendToAgent(agent, Inbound(type = "focus_pane"))
    }

    /** `focus_tab` — activate the pane's tab and its workspace. */
    suspend fun focusTab(paneId: String): CommandResultMessage {
        val agent = requireAgent(paneId)
        requireCapability(agent, ClientCapabilities.FOCUS)
        return sendToAgent(agent, Inbound(type = "focus_tab"))
    }

    /** `focus_workspace` — activate the pane's workspace. */
    suspend fun focusWorkspace(paneId: String): CommandResultMessage {
        val agent = requireAgent(paneId)
        requireCapability(agent, ClientCapabilities.FOCUS)
        return sendToAgent(agent, Inbound(type = "focus_workspace"))
    }

    /** `focus_agent` — focus the pane hosting the agent session. */
    suspend fun focusAgent(paneId: String): CommandResultMessage {
        val agent = requireAgent(paneId)
        requireCapability(agent, ClientCapabilities.FOCUS)
        if (agent.agentSessionId.isNullOrEmpty()) {
            throw CommandException("This agent does not report an agent session id")
        }
        return sendToAgent(agent, Inbound(type = "focus_agent"))
    }

    /**
     * `pane_search` — server-side find over full scrollback. `cursor` is the
     * copy-engine point to continue from (`{0,0}` searches from the top);
     * `previous` re-anchors next/previous at the last hit. The relay injects
     * the `content_revision` fence — clients never send it.
     */
    suspend fun paneSearch(
        paneId: String,
        query: String,
        direction: String = "forward",
        cursor: PaneTextPoint = PaneTextPoint(),
        previous: PaneTextRange? = null,
    ): PaneSearchResult {
        val agent = requireAgent(paneId)
        requireCapability(agent, ClientCapabilities.PANE_SEARCH)
        if (query.isEmpty()) throw CommandException("Query is required")
        val result = sendToAgent(
            agent,
            Inbound(
                type = "pane_search",
                query = query,
                direction = direction,
                cursor = LerdrJson.encodeToJsonElement(
                    PaneTextPoint.serializer(), cursor,
                ),
                previous = previous?.let {
                    LerdrJson.encodeToJsonElement(PaneTextRange.serializer(), it)
                },
            ),
        )
        return decodeResult(result, PaneSearchResult.serializer())
    }

    /** `pane_selection_read` — read the `{anchor,cursor}` range's text. */
    suspend fun paneSelectionRead(
        paneId: String,
        anchor: PaneTextPoint,
        cursor: PaneTextPoint,
    ): PaneSelectionResult {
        val agent = requireAgent(paneId)
        requireCapability(agent, ClientCapabilities.PANE_SEARCH)
        val result = sendToAgent(
            agent,
            Inbound(
                type = "pane_selection_read",
                anchor = LerdrJson.encodeToJsonElement(
                    PaneTextPoint.serializer(), anchor,
                ),
                cursor = LerdrJson.encodeToJsonElement(
                    PaneTextPoint.serializer(), cursor,
                ),
            ),
        )
        return decodeResult(result, PaneSelectionResult.serializer())
    }

    /**
     * `pane_link_resolve` — hit-test a viewport cell for a link; returns its
     * cell **regions** (the highlight affordance). The URL surfaces only on
     * [paneLinkActivate] — resolve never carries it (docs/13 §1.4).
     * Viewport coordinates apply to the live viewport; scrolled-back cells
     * address scrollback and may resolve empty.
     */
    suspend fun paneLinkResolve(
        paneId: String,
        row: Int,
        col: Int,
    ): PaneLinkResolvedResult {
        val agent = requireAgent(paneId)
        requireCapability(agent, ClientCapabilities.PANE_LINKS)
        val result = sendToAgent(
            agent,
            Inbound(type = "pane_link_resolve", row = row, col = col),
        )
        return decodeResult(result, PaneLinkResolvedResult.serializer())
    }

    /** `pane_link_activate` — open the link under a viewport cell. */
    suspend fun paneLinkActivate(
        paneId: String,
        row: Int,
        col: Int,
    ): PaneLinkActivatedResult {
        val agent = requireAgent(paneId)
        requireCapability(agent, ClientCapabilities.PANE_LINKS)
        val result = sendToAgent(
            agent,
            Inbound(type = "pane_link_activate", row = row, col = col),
        )
        return decodeResult(result, PaneLinkActivatedResult.serializer())
    }

    /**
     * `layout_export` — the pane's verbatim herdr `LayoutNode` tree.
     * The wire also accepts `target.tab_id`; the app's agent always
     * carries it once reported.
     */
    suspend fun layoutExport(paneId: String): JsonElement? {
        val agent = requireAgent(paneId)
        requireCapability(agent, ClientCapabilities.LAYOUT)
        val result = sendToAgent(agent, Inbound(type = "layout_export"))
        return (result.data as? JsonObject)?.get("root")
    }

    /** `layout_apply` — rebuild a layout from an exported tree (audited). */
    suspend fun layoutApply(
        paneId: String,
        root: JsonElement,
        tabLabel: String = "",
        focus: Boolean = false,
    ): CommandResultMessage {
        val agent = requireAgent(paneId)
        requireCapability(agent, ClientCapabilities.LAYOUT)
        return sendToAgent(
            agent,
            Inbound(
                type = "layout_apply",
                root = root,
                tabLabel = tabLabel,
                focus = focus,
            ),
        )
    }

    // ── agent management ──────────────────────────────────────────────

    /** `agent_start` — launch a profile into a workspace (relay-scoped). */
    suspend fun startAgent(
        relayId: String,
        profileId: String,
        name: String,
        cwd: String,
        prompt: String = "",
        workspaceId: String = "",
    ): CommandResultMessage = request(
        relayId,
        Inbound(
            type = "agent_start",
            profileId = profileId,
            name = name,
            cwd = cwd,
            prompt = prompt,
            workspaceId = workspaceId,
        ),
        timeoutMs = AGENT_START_TIMEOUT_MS,
    )

    /** `agent_rename` — retitle a running pane. */
    suspend fun renameAgent(paneId: String, name: String): CommandResultMessage {
        val agent = requireAgent(paneId)
        return sendToAgent(agent, Inbound(type = "agent_rename", name = name))
    }

    /** `agent_restart` — respawn the pane's process in place. */
    suspend fun restartAgent(paneId: String): CommandResultMessage {
        val agent = requireAgent(paneId)
        return sendToAgent(agent, Inbound(type = "agent_restart"))
    }

    /** `agent_clear` — wipe the pane's transcript (oracle's 45 s window). */
    suspend fun clearAgent(paneId: String): CommandResultMessage {
        val agent = requireAgent(paneId)
        return sendToAgent(
            agent,
            Inbound(type = "agent_clear"),
            timeoutMs = AGENT_CLEAR_TIMEOUT_MS,
        )
    }

    /** `agent_stop` — terminate the pane's process. */
    suspend fun stopAgent(paneId: String): CommandResultMessage {
        val agent = requireAgent(paneId)
        return sendToAgent(agent, Inbound(type = "agent_stop"))
    }

    // ── workspace management ──────────────────────────────────────────

    private fun requireWorkspaceManagement(relayId: String) {
        if (connectionStore.connectionNow(relayId)
                ?.capabilities?.contains(WORKSPACE_MANAGEMENT_CAPABILITY) != true
        ) {
            throw CommandException("This relay does not support workspace management")
        }
    }

    /** `workspace_create` — new workspace rooted at [cwd]. */
    suspend fun createWorkspace(
        relayId: String,
        cwd: String,
        label: String,
    ): CommandResultMessage {
        requireWorkspaceManagement(relayId)
        val result = request(
            relayId,
            Inbound(type = "workspace_create", cwd = cwd, label = label),
            timeoutMs = AGENT_START_TIMEOUT_MS,
        )
        refreshAgents()
        return result
    }

    /** `workspace_rename` — retitle a workspace tab. */
    suspend fun renameWorkspace(
        relayId: String,
        workspaceId: String,
        label: String,
    ): CommandResultMessage {
        requireWorkspaceManagement(relayId)
        val result = request(
            relayId,
            Inbound(type = "workspace_rename", workspaceId = workspaceId, label = label),
        )
        refreshAgents()
        return result
    }

    /**
     * `workspace_reorder` — block form (`workspace_ids` + `before_workspace_id`)
     * when the relay advertises `workspace_reorder_block`, legacy
     * single-workspace `insert_index` otherwise.
     */
    suspend fun reorderWorkspaceBlock(
        relayId: String,
        workspaceIds: List<String>,
        beforeWorkspaceId: String,
        legacyInsertIndex: Int,
    ): CommandResultMessage {
        requireWorkspaceManagement(relayId)
        if (workspaceIds.isEmpty() || workspaceIds.size != workspaceIds.toSet().size) {
            throw CommandException("Workspace selection is invalid")
        }
        val capabilities = connectionStore.connectionNow(relayId)?.capabilities.orEmpty()
        val message = when {
            WORKSPACE_REORDER_BLOCK_CAPABILITY in capabilities -> Inbound(
                type = "workspace_reorder",
                workspaceIds = workspaceIds,
                beforeWorkspaceId = beforeWorkspaceId,
            )
            workspaceIds.size == 1 && legacyInsertIndex >= 0 -> Inbound(
                type = "workspace_reorder",
                workspaceId = workspaceIds.single(),
                insertIndex = legacyInsertIndex,
            )
            else -> throw CommandException(
                "Update Herdr to reorder a workspace with linked worktrees",
            )
        }
        val result = request(relayId, message)
        refreshAgents()
        return result
    }

    /**
     * `workspace_close` — optionally the whole linked-worktree group when
     * [closeGroup] is set (the oracle's 30 s window).
     */
    suspend fun closeWorkspace(
        relayId: String,
        workspaceId: String,
        closeGroup: Boolean = false,
        expectedWorkspaceIds: List<String> = emptyList(),
    ): CommandResultMessage {
        requireWorkspaceManagement(relayId)
        val result = request(
            relayId,
            Inbound(
                type = "workspace_close",
                workspaceId = workspaceId,
                closeGroup = closeGroup,
                expectedWorkspaceIds = if (closeGroup) expectedWorkspaceIds else emptyList(),
            ),
            timeoutMs = WORKSPACE_CLOSE_TIMEOUT_MS,
        )
        refreshAgents()
        return result
    }

    /** `list_directories` — the launch-form directory browser. */
    suspend fun listDirectories(relayId: String, path: String = ""): DirectoryListing {
        val result = request(
            relayId,
            Inbound(type = "list_directories", path = path),
            timeoutMs = LIST_DIRECTORIES_TIMEOUT_MS,
        )
        return parseDirectoryListing(result.data)
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
                cursor = request.cursor.takeIf { it.isNotEmpty() }
                    ?.let(::JsonPrimitive),
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

    /**
     * `inventoryRefresh` — the oracle's pull-to-refresh action
     * (`App.svelte` → `requestInventoryRefresh`): `refresh_agents` to
     * every connected relay, plus `connect()` for registered endpoints
     * currently `disconnected`.
     */
    fun inventoryRefresh() {
        val disconnected = connectionStore.connections.value.values
            .filter { it.status == RelayStatus.DISCONNECTED }
            .map { it.relayId }
            .toSet()
        refreshAgents()
        if (disconnected.isEmpty()) return
        relayRegistry.relays.value
            .filter { it.id in disconnected }
            .forEach(::connect)
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

    /**
     * Decoded server frames after the internal routes ran — feature VMs
     * (push policy, devices, speech, worktrees) observe replies that are
     * not `command_result`-correlated here (`push_policy`,
     * `push_test_result`, unsolicited `speech_voices`, …). Buffer-bounded;
     * slow collectors drop frames rather than stall the session.
     */
    val frames: kotlinx.coroutines.flow.SharedFlow<RelayFrame> get() = _frames
    private val _frames =
        kotlinx.coroutines.flow.MutableSharedFlow<RelayFrame>(extraBufferCapacity = 64)

    /** One decoded frame tagged with its origin relay. */
    data class RelayFrame(val relayId: String, val message: ServerMessage)

    // ── command plumbing ─────────────────────────────────────────────

    private fun requireAgent(paneId: String): Agent =
        agentStore.agentNow(paneId)
            ?: throw IllegalStateException("This agent is no longer available.")

    /**
     * Phase-5 §0 gate — a Track-A action may only ride the negotiated live
     * set (server-advertised ∩ app-announced); older relays refuse locally
     * instead of eating an `unknown_action`.
     */
    private fun requireCapability(agent: Agent, capability: String) {
        if (connectionStore.connectionNow(agent.relayId)
                ?.capabilityLive(capability) != true
        ) {
            throw CommandException("This relay does not support $capability")
        }
    }

    /** Decode a `command_result.data` payload into its Track-A DTO. */
    private fun <T> decodeResult(
        result: CommandResultMessage,
        serializer: kotlinx.serialization.KSerializer<T>,
    ): T = result.data?.let {
        runCatching { LerdrJson.decodeFromJsonElement(serializer, it) }.getOrNull()
    } ?: throw CommandException("Relay returned an empty result")

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
     * Public `requestRaw` — feature VMs send catalog actions (`device_list`,
     * `push_policy_set`, `speech_voices_list`, `worktree_*`, `tab_reorder`,
     * `speak_text`, …) whose replies come back as the correlated
     * `command_result` (payload in `data`) and/or as typed frames on
     * [frames]. Extras spread over the top-level map exactly like the
     * oracle's `sendCommand`.
     */
    suspend fun request(
        relayId: String,
        message: Inbound,
        extras: Map<String, kotlinx.serialization.json.JsonElement> = emptyMap(),
        timeoutMs: Long = ReconnectPolicy.COMMAND_TIMEOUT_MS,
    ): CommandResultMessage = requestRaw(relayId, message, extras, timeoutMs)

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
                    data = message.data,
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
        const val SECRET_CAPABILITY = "secret_input"
        const val TAB_REORDER_CAPABILITY = "tab_reorder"
        const val WORKSPACE_MANAGEMENT_CAPABILITY = "workspace_management"
        const val WORKSPACE_REORDER_BLOCK_CAPABILITY = "workspace_reorder_block"
        const val AGENT_START_TIMEOUT_MS = 45_000L
        const val AGENT_CLEAR_TIMEOUT_MS = 45_000L
        /** `PANE_LEASE_HIDDEN_GRACE_MS` — hidden renewals stop past this. */
        const val PANE_LEASE_HIDDEN_GRACE_MS = 5 * 60_000L
        const val WORKSPACE_CLOSE_TIMEOUT_MS = 30_000L
        const val SELF_UPDATE_CAPABILITY = "self_update"
        const val UPDATE_COMMAND_TIMEOUT_MS = 30_000L
        const val COPY_RESPONSE_TIMEOUT_MS = 15_000L
        const val LIST_DIRECTORIES_TIMEOUT_MS = 10_000L
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
