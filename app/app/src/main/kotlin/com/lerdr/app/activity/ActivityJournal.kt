package com.lerdr.app.activity

import androidx.compose.runtime.Immutable
import com.lerdr.app.di.AppScope
import com.lerdr.app.session.SessionRepository
import java.util.concurrent.atomic.AtomicLong
import javax.inject.Inject
import javax.inject.Singleton
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import lerdr.core.data.RelayEndpoint
import lerdr.core.store.RelayConnection
import lerdr.core.store.RelayStatus
import lerdr.core.transport.RelaySession

/**
 * App-side session journal — an in-memory, newest-first log of local
 * lifecycle events (relay added/removed, connect, disconnect, auth
 * rejection, pairing requests).
 *
 * The relay's own action journal (`activity`/`activity_history` frames)
 * stays in [SessionRepository.activities]; this log covers what the wire
 * never sees — the socket lifecycle this device drives. The Activity
 * screen merges both sources.
 *
 * Feeding contract: the journal observes [SessionRepository.connections]
 * and [SessionRepository.relays] on the injected app scope and emits an
 * event per *state transition* — the first snapshot of each flow seeds a
 * baseline silently so a late-created journal never floods with history.
 * A connection row only records when its [Signature] changes, so keepalive
 * churn can't spam the log.
 *
 * Lifetime: app-module scoped `@Singleton`, reached through
 * [ActivityEntryPoint] (Hilt binds are lazy — recording starts on first
 * access, typically the first Activity screen visit). Events are lost on
 * process death by design; durable history is the relay's job.
 */
@Singleton
class ActivityJournal @Inject constructor(
    @param:AppScope private val scope: CoroutineScope,
    private val sessions: SessionRepository,
    private val now: () -> Long,
) {

    /** What happened — drives both the row copy and its icon. */
    @Immutable
    enum class Kind {
        RELAY_ADDED,
        RELAY_REMOVED,
        CONNECTING,
        CONNECTED,
        DISCONNECTED,
        AUTH_REJECTED,
        PAIRING_REQUIRED,
    }

    /** One journal row. [detail] carries context like the close reason. */
    @Immutable
    data class Event(
        val id: String,
        val timestampEpochMs: Long,
        val relayId: String,
        val relayLabel: String,
        val kind: Kind,
        val detail: String = "",
    )

    private val sequence = AtomicLong(0)

    private val _events = MutableStateFlow<List<Event>>(emptyList())

    /** Newest-first event log, capped at [MAX_EVENTS]. */
    val events: StateFlow<List<Event>> = _events.asStateFlow()

    private val lock = Any()

    /** Last resolved label per relay — survives registry removal. */
    private val labels = mutableMapOf<String, String>()

    /** Null until the first `connections` emission seeds it. */
    private var connectionBaseline: MutableMap<String, Signature>? = null

    /** Null until the first `relays` emission seeds it (id → label). */
    private var relayBaseline: MutableMap<String, String>? = null

    init {
        scope.launch { sessions.connections.collect(::onConnections) }
        scope.launch { sessions.relays.collect(::onRelays) }
    }

    /**
     * Appends one event. Public so screens/tests can log local actions
     * the session flows don't express (e.g. a user-initiated note).
     */
    fun record(kind: Kind, relayId: String, relayLabel: String = relayId, detail: String = "") {
        val event = Event(
            id = "local/${sequence.incrementAndGet()}",
            timestampEpochMs = now(),
            relayId = relayId,
            relayLabel = relayLabel,
            kind = kind,
            detail = detail,
        )
        _events.update { (listOf(event) + it).take(MAX_EVENTS) }
    }

    /** Drops every recorded event (relay-sourced rows are not affected). */
    fun clear() {
        _events.value = emptyList()
    }

    // ── feed: connection lifecycle ────────────────────────────────────

    private fun onConnections(snapshot: Map<String, RelayConnection>) {
        val baseline = synchronized(lock) {
            snapshot.forEach { (id, conn) -> if (conn.relayLabel.isNotEmpty()) labels[id] = conn.relayLabel }
            if (connectionBaseline == null) {
                // Baseline — the state the journal was born into is not news.
                connectionBaseline = snapshot.mapValues { Signature(it.value) }.toMutableMap()
                return
            }
            connectionBaseline!!
        }
        val next = snapshot.mapValues { Signature(it.value) }
        for ((relayId, signature) in next) {
            if (baseline[relayId] == signature) continue
            val conn = snapshot.getValue(relayId)
            record(signature.kind, relayId, labelOf(relayId), detailOf(relayId, conn, signature.kind))
        }
        synchronized(lock) {
            baseline.keys.retainAll(next.keys)
            baseline.putAll(next)
        }
    }

    // ── feed: registry membership ─────────────────────────────────────

    private fun onRelays(endpoints: List<RelayEndpoint>) {
        val baseline = synchronized(lock) {
            endpoints.forEach { labels[it.id] = it.label }
            if (relayBaseline == null) {
                relayBaseline = endpoints.associate { it.id to it.label }.toMutableMap()
                return
            }
            relayBaseline!!
        }
        val next = endpoints.associate { it.id to it.label }
        for ((relayId, label) in next) {
            if (!baseline.containsKey(relayId)) {
                record(Kind.RELAY_ADDED, relayId, label)
            }
        }
        for ((relayId, label) in baseline) {
            if (!next.containsKey(relayId)) {
                record(Kind.RELAY_REMOVED, relayId, label)
            }
        }
        synchronized(lock) {
            baseline.clear()
            baseline.putAll(next)
        }
    }

    // ── internals ─────────────────────────────────────────────────────

    private fun labelOf(relayId: String): String = synchronized(lock) {
        labels[relayId]
    } ?: relayId

    /** Pulls the close reason off the session handle — the store drops it. */
    private fun detailOf(relayId: String, conn: RelayConnection, kind: Kind): String {
        val sessionState = sessions.sessionState(relayId)?.value
        return when (kind) {
            Kind.CONNECTED -> listOfNotNull(
                conn.path?.wire,
                conn.releaseVersion.ifEmpty { conn.version }
                    .takeIf { it.isNotEmpty() }
                    ?.let { "relay $it" },
            ).joinToString(" · ")
            Kind.DISCONNECTED, Kind.AUTH_REJECTED -> when (sessionState) {
                is RelaySession.SessionState.Disconnected -> sessionState.reason?.reason.orEmpty()
                is RelaySession.SessionState.AuthRejected -> sessionState.reason.reason
                else -> ""
            }
            else -> ""
        }
    }

    /** The rendered slice of a [RelayConnection] — equal signatures never re-log. */
    @Immutable
    private data class Signature(
        val status: RelayStatus,
        val authRejected: Boolean,
        val pairingRequired: Boolean,
        val pairingDeferred: Boolean,
    ) {
        constructor(conn: RelayConnection) : this(
            status = conn.status,
            authRejected = conn.authRejected,
            pairingRequired = conn.pairingRequired,
            pairingDeferred = conn.pairingDeferred,
        )

        val kind: Kind
            get() = when {
                authRejected -> Kind.AUTH_REJECTED
                pairingRequired || pairingDeferred -> Kind.PAIRING_REQUIRED
                status == RelayStatus.CONNECTED -> Kind.CONNECTED
                status == RelayStatus.CONNECTING -> Kind.CONNECTING
                else -> Kind.DISCONNECTED
            }
    }

    private companion object {
        const val MAX_EVENTS = 500
    }
}
