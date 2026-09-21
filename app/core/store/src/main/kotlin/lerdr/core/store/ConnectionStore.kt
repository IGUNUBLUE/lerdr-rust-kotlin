package lerdr.core.store

import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import lerdr.core.model.AgentProfile
import lerdr.core.model.AppDeployStatusMessage
import lerdr.core.model.AppDeployState
import lerdr.core.model.HerdrStatus
import lerdr.core.model.HerdrStatusMessage
import lerdr.core.model.InventoryStatusMessage
import lerdr.core.model.PushConfigMessage
import lerdr.core.model.SpeechVoice
import lerdr.core.model.SpeechVoicesMessage
import lerdr.core.model.UpdateState
import lerdr.core.model.UpdateStatusMessage
import lerdr.core.model.orNull

/** `TransportStatus` — lifecycle of one transport attempt (`transports/types.ts`). */
enum class TransportStatus { CONNECTING, CONNECTED, CLOSED }

/** `TransportKind` — which physical path carries traffic. */
enum class TransportKind(val wire: String) {
    WEBSOCKET("websocket"),
    GATEWAY("gateway"),
    WEBRTC("webrtc"),
}

/** `TransportStatusDetail` — close/connect metadata reported by a transport. */
data class TransportStatusDetail(
    val reason: String? = null,
    val fatal: Boolean = false,
    val code: String? = null,
    val path: TransportKind? = null,
    val gatewayUrl: String? = null,
) {
    companion object {
        /** Stable close reason for relays that refuse this device's credential. */
        const val DEVICE_UNAUTHORIZED = "device_unauthorized"
        const val UNKNOWN_RELAY = "unknown_relay"
    }
}

/** `RelayStatus` — the oracle's three-state wire status (`types.ts`). */
enum class RelayStatus { CONNECTING, CONNECTED, DISCONNECTED }

/**
 * View-level connection phase — the app's five-state rollup. The oracle
 * keeps `status` plus pairing flags and derives "degraded" at render
 * (`connected && inventory.state !== 'ready'`); [RelayConnection.phase]
 * folds that derivation into one enum.
 */
enum class ConnectionPhase { DISCONNECTED, CONNECTING, PAIRING, CONNECTED, DEGRADED }

/** `AgentInventoryState` — Herdr subprocess inventory state. */
enum class AgentInventoryState { STARTING, READY, ERROR }

/** `AgentInventoryStatus` after `normalizeAgentInventory` (caps + state parse). */
data class AgentInventoryStatus(
    val state: AgentInventoryState,
    val errorCode: String = "",
    val message: String = "",
    val lastAttemptAt: Long = 0,
    val lastSuccessAt: Long = 0,
    val stale: Boolean = false,
)

private fun normalizeInventory(
    state: String?,
    errorCode: String?,
    message: String?,
    lastAttemptAt: Long?,
    lastSuccessAt: Long?,
    stale: Boolean?,
    fallback: AgentInventoryState,
): AgentInventoryStatus = AgentInventoryStatus(
    state = when (state) {
        "starting" -> AgentInventoryState.STARTING
        "ready" -> AgentInventoryState.READY
        "error" -> AgentInventoryState.ERROR
        else -> fallback
    },
    errorCode = errorCode.orEmpty().take(80),
    message = message.orEmpty().take(500),
    lastAttemptAt = lastAttemptAt ?: 0,
    lastSuccessAt = lastSuccessAt ?: 0,
    stale = stale == true,
)

/**
 * `RelayConnectionView` — everything the UI shows for one configured relay.
 * The store owns the raw [status] plus the pairing flags; [phase] derives
 * the five-state view.
 */
data class RelayConnection(
    val relayId: String,
    val relayLabel: String,
    val status: RelayStatus = RelayStatus.CONNECTING,
    /** Physical path currently carrying traffic; null until first connect. */
    val path: TransportKind? = null,
    /** Gateway carrying the session; empty on the direct-URL path. */
    val activeGatewayUrl: String = "",
    val host: String = "",
    val home: String = "",
    val protocol: Int = 0,
    val version: String = "",
    val releaseVersion: String = "",
    val revision: String = "",
    val update: UpdateState? = null,
    val appDeploy: AppDeployState? = null,
    val inventory: AgentInventoryStatus =
        AgentInventoryStatus(AgentInventoryState.STARTING),
    val capabilities: List<String> = emptyList(),
    val herdrStatus: HerdrStatus? = null,
    /** The relay refused this device's credential; re-pairing is the only fix. */
    val authRejected: Boolean = false,
    /** Relay speaks the encrypted handshake but we hold nothing to present. */
    val pairingRequired: Boolean = false,
    /** Pairing deferred to the installed app (iOS Home Screen flow). */
    val pairingDeferred: Boolean = false,
    /** Set once the entry is terminal — late transport callbacks are ignored. */
    val closed: Boolean = false,
    val connectingSince: Long = 0,
    val speechLanguages: List<String> = emptyList(),
    val speechVoices: List<SpeechVoice> = emptyList(),
    val speechCacheDir: String = "",
    val speechEngineInstalled: Boolean = false,
    val agentProfiles: List<AgentProfile> = emptyList(),
    val pushStatus: String = "",
    val vapidPublicKey: String = "",
    val gatewayAvailableVersion: String = "",
) {
    val phase: ConnectionPhase
        get() = when {
            pairingRequired || pairingDeferred || authRejected -> ConnectionPhase.PAIRING
            status == RelayStatus.CONNECTING -> ConnectionPhase.CONNECTING
            status == RelayStatus.DISCONNECTED -> ConnectionPhase.DISCONNECTED
            inventory.state != AgentInventoryState.READY -> ConnectionPhase.DEGRADED
            else -> ConnectionPhase.CONNECTED
        }

    /** Whether the relay advertised `attention_classification` on this connection. */
    val attentionCapable: Boolean
        get() = ATTENTION_CAPABILITY in capabilities

    companion object {
        const val ATTENTION_CAPABILITY = "attention_classification"
    }
}

/**
 * Connection store — port of the oracle's `connections` map plus the
 * transport-status transitions in `applyTransportStatus`. Reconnect
 * scheduling, keepalives and dials stay in `:core:transport`; this store is
 * the state the UI renders.
 */
class ConnectionStore(
    private val clock: () -> Long = System::currentTimeMillis,
) {
    private val lock = Any()

    private val _connections = MutableStateFlow<Map<String, RelayConnection>>(emptyMap())

    /**
     * Per-relay freshness bound — last frame/handshake heard. Kept out of the
     * emitted [RelayConnection] so per-message updates don't churn
     * subscribers; the oracle mutates `connection.lastMessageAt` in place.
     */
    private val lastMessageAt = mutableMapOf<String, Long>()

    /** Per-relay connection views, keyed by relay config id. */
    val connections: StateFlow<Map<String, RelayConnection>> = _connections.asStateFlow()

    fun connection(relayId: String): Flow<RelayConnection?> =
        _connections.map { it[relayId] }.distinctUntilChanged()

    fun connectionNow(relayId: String): RelayConnection? =
        synchronized(lock) { _connections.value[relayId] }

    /** Millis timestamp of the last frame heard from [relayId], or 0. */
    fun lastMessageAt(relayId: String): Long =
        synchronized(lock) { lastMessageAt[relayId] ?: 0 }

    /** Records proof-of-life — the transport calls this for every received frame. */
    fun noteMessage(relayId: String) {
        synchronized(lock) {
            if (relayId in _connections.value) lastMessageAt[relayId] = clock()
        }
    }

    /**
     * Snapshot gating (`agents`/`workspaces`): a connection whose inventory
     * never reached `ready` drops inventory frames unless marked `stale`.
     * No connection at all passes — the oracle checks `connection &&`.
     */
    fun acceptsInventorySnapshots(relayId: String): Boolean =
        synchronized(lock) {
            _connections.value[relayId]?.let {
                it.inventory.state == AgentInventoryState.READY || it.inventory.stale
            } ?: true
        }

    /**
     * `connectRelay` — registers a fresh attempt at `connecting`, resetting
     * pairing/auth flags. The actual dial is the transport's job.
     */
    fun connect(relayId: String, relayLabel: String) {
        synchronized(lock) {
            _connections.value = _connections.value + (relayId to RelayConnection(
                relayId = relayId,
                relayLabel = relayLabel,
                status = RelayStatus.CONNECTING,
                connectingSince = clock(),
            ))
        }
    }

    /**
     * `applyTransportStatus` — transition rules:
     * - `connected` records the answering path and marks the row ready.
     * - `connecting` is a no-op while already connecting.
     * - `closed` lands at `disconnected`; `device_unauthorized` additionally
     *   latches [RelayConnection.authRejected] and closes the entry (the
     *   transport must not retry it).
     */
    fun onTransportStatus(
        relayId: String,
        status: TransportStatus,
        detail: TransportStatusDetail = TransportStatusDetail(),
    ) {
        synchronized(lock) {
            val connection = _connections.value[relayId] ?: return
            if (connection.closed) return
            when (status) {
                TransportStatus.CONNECTED -> {
                    val path = detail.path ?: TransportKind.WEBSOCKET
                    _connections.value = _connections.value + (relayId to connection.copy(
                        path = path,
                        activeGatewayUrl =
                            if (path == TransportKind.WEBSOCKET) "" else detail.gatewayUrl.orEmpty(),
                        status = RelayStatus.CONNECTED,
                    ))
                    // A completed handshake is the freshest proof of life.
                    lastMessageAt[relayId] = clock()
                }
                TransportStatus.CONNECTING -> {
                    if (connection.status == RelayStatus.CONNECTING) return
                    _connections.value = _connections.value + (relayId to
                        connection.copy(status = RelayStatus.CONNECTING))
                }
                TransportStatus.CLOSED -> {
                    val unauthorized =
                        detail.code == TransportStatusDetail.DEVICE_UNAUTHORIZED
                    _connections.value = _connections.value + (relayId to connection.copy(
                        status = RelayStatus.DISCONNECTED,
                        authRejected = connection.authRejected || unauthorized,
                        closed = connection.closed || unauthorized,
                    ))
                }
            }
        }
    }

    /** `markPairingRequired` — a disconnected row that explains why it never dials. */
    fun markPairingRequired(relayId: String, relayLabel: String) {
        synchronized(lock) {
            if (_connections.value[relayId]?.pairingRequired == true) return
            _connections.value = _connections.value + (relayId to RelayConnection(
                relayId = relayId,
                relayLabel = relayLabel,
                status = RelayStatus.DISCONNECTED,
                closed = true,
                pairingRequired = true,
                connectingSince = clock(),
            ))
        }
    }

    /** `markPairingDeferred` — same shape as pairing-required, different flag. */
    fun markPairingDeferred(relayId: String, relayLabel: String) {
        synchronized(lock) {
            _connections.value = _connections.value + (relayId to RelayConnection(
                relayId = relayId,
                relayLabel = relayLabel,
                status = RelayStatus.DISCONNECTED,
                closed = true,
                pairingDeferred = true,
                connectingSince = clock(),
            ))
        }
    }

    /**
     * `push_config` intake — connection metadata, capability set, inventory
     * fallback `ready`. The agent-side effects (pane-revision reset,
     * attention renormalization) are [StoreReducer]'s job.
     */
    fun applyPushConfig(relayId: String, message: PushConfigMessage) {
        synchronized(lock) {
            val connection = _connections.value[relayId] ?: return
            val releaseVersion = message.releaseVersion.orEmpty().take(32)
            val revision =
                (message.revision?.takeIf { it.isNotEmpty() } ?: message.version)
                    .orEmpty().take(40)
            val update = message.update.orNull
            _connections.value = _connections.value + (relayId to connection.copy(
                vapidPublicKey = message.vapidPublicKey.orEmpty(),
                host = message.host.orEmpty(),
                home = message.home.orEmpty().take(1024),
                protocol = message.protocol?.takeIf { it > 0 } ?: 1,
                version = message.version?.take(40).orEmpty(),
                releaseVersion = releaseVersion,
                revision = revision,
                update = update,
                gatewayAvailableVersion = update?.availableVersion ?: releaseVersion,
                herdrStatus = message.herdrStatus,
                appDeploy = message.appDeploy.orNull,
                inventory = normalizeInventory(
                    state = message.inventory.orNull?.state,
                    errorCode = null,
                    message = null,
                    lastAttemptAt = null,
                    lastSuccessAt = null,
                    stale = null,
                    fallback = AgentInventoryState.READY,
                ),
                capabilities = message.capabilities.orNull?.filter { it.isNotEmpty() }
                    ?: emptyList(),
                speechLanguages = message.speechLanguages?.filter { it.isNotBlank() }
                    ?: emptyList(),
                agentProfiles = normalizeAgentProfiles(message.agentProfiles.orNull),
            ))
        }
    }

    /** `herdr_status` — generation-gated capability/status update. */
    fun applyHerdrStatus(relayId: String, message: HerdrStatusMessage) {
        synchronized(lock) {
            val connection = _connections.value[relayId] ?: return
            val status = message.status ?: return
            if (status.generation <= (connection.herdrStatus?.generation ?: 0)) return
            _connections.value = _connections.value + (relayId to connection.copy(
                herdrStatus = status,
                capabilities = message.capabilities?.filter { it.isNotEmpty() }
                    ?: connection.capabilities,
            ))
        }
    }

    /** `inventory_status` — replaces the inventory block. */
    fun applyInventoryStatus(relayId: String, message: InventoryStatusMessage) {
        synchronized(lock) {
            val connection = _connections.value[relayId] ?: return
            _connections.value = _connections.value + (relayId to connection.copy(
                inventory = normalizeInventory(
                    state = message.state,
                    errorCode = message.errorCode,
                    message = message.message,
                    lastAttemptAt = message.lastAttemptAt,
                    lastSuccessAt = message.lastSuccessAt,
                    stale = message.stale,
                    fallback = AgentInventoryState.STARTING,
                ),
            ))
        }
    }

    fun applyUpdateStatus(relayId: String, message: UpdateStatusMessage) {
        synchronized(lock) {
            val connection = _connections.value[relayId] ?: return
            val update = message.update
            _connections.value = _connections.value + (relayId to connection.copy(
                update = update,
                gatewayAvailableVersion =
                    update?.availableVersion ?: connection.gatewayAvailableVersion,
            ))
        }
    }

    fun applyAppDeployStatus(relayId: String, message: AppDeployStatusMessage) {
        synchronized(lock) {
            val connection = _connections.value[relayId] ?: return
            _connections.value = _connections.value + (relayId to
                connection.copy(appDeploy = message.appDeploy))
        }
    }

    /** `speech_voices` — voice catalog update. */
    fun applySpeechVoices(relayId: String, message: SpeechVoicesMessage) {
        synchronized(lock) {
            val connection = _connections.value[relayId] ?: return
            _connections.value = _connections.value + (relayId to connection.copy(
                speechVoices = message.voices ?: emptyList(),
                speechLanguages = message.languages?.filter { it.isNotBlank() }
                    ?: connection.speechLanguages,
                speechCacheDir = message.cacheDir ?: connection.speechCacheDir,
                speechEngineInstalled =
                    message.engineInstalled ?: connection.speechEngineInstalled,
            ))
        }
    }

    fun applyPushSubscribed(relayId: String, ok: Boolean) {
        synchronized(lock) {
            val connection = _connections.value[relayId] ?: return
            _connections.value = _connections.value + (relayId to
                connection.copy(pushStatus = if (ok) "subscribed" else "failed"))
        }
    }

    fun applyPushUnsubscribed(relayId: String, ok: Boolean) {
        if (!ok) return
        synchronized(lock) {
            val connection = _connections.value[relayId] ?: return
            _connections.value = _connections.value + (relayId to connection.copy(pushStatus = ""))
        }
    }

    /** `disconnectRelay` — removes the row entirely. */
    fun disconnect(relayId: String) {
        synchronized(lock) {
            lastMessageAt.remove(relayId)
            _connections.value = _connections.value - relayId
        }
    }

    fun clear() {
        synchronized(lock) {
            lastMessageAt.clear()
            _connections.value = emptyMap()
        }
    }
}

/** push_config `agent_profiles` — valid rows only, case-insensitive label sort. */
private fun normalizeAgentProfiles(profiles: List<AgentProfile>?): List<AgentProfile> =
    profiles.orEmpty()
        .filter { !it.id.isNullOrEmpty() }
        .sortedWith(
            compareBy<AgentProfile, String>(String.CASE_INSENSITIVE_ORDER) {
                it.label ?: it.id.orEmpty()
            }.thenBy(String.CASE_INSENSITIVE_ORDER) { it.id.orEmpty() },
        )
