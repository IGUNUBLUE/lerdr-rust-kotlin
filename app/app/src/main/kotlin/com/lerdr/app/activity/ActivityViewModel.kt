package com.lerdr.app.activity

import androidx.compose.runtime.Immutable
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.lerdr.app.session.RelayActivity
import com.lerdr.app.session.SessionRepository
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import kotlin.math.max
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.stateIn

/** Row iconography — the screen maps kinds to icons/tints. */
@Immutable
enum class ActivityItemKind {
    RELAY_ADDED,
    RELAY_REMOVED,
    CONNECTING,
    CONNECTED,
    DISCONNECTED,
    AUTH_REJECTED,
    PAIRING_REQUIRED,

    /** Relay-emitted action entry (`activity`/`activity_history`). */
    ACTION,
}

/** One journal row — local session event or relay-reported action. */
@Immutable
data class ActivityItemUi(
    val key: String,
    val relayId: String,
    val relayLabel: String,
    val headline: String,
    val detail: String,
    val timestampEpochMs: Long,
    /** Absolute "HH:mm" (<24 h) or "MMM d, HH:mm" — the journal's timestamp. */
    val timestampLabel: String,
    /** Relative "3m ago" — refreshed by the ticker. */
    val ageLabel: String,
    val kind: ActivityItemKind,
)

/** One per-relay filter chip. */
@Immutable
data class RelayFilterUi(
    val relayId: String,
    val label: String,
    val selected: Boolean,
)

@Immutable
data class ActivityUiState(
    val items: List<ActivityItemUi> = emptyList(),
    val filters: List<RelayFilterUi> = emptyList(),
    /** Null = "All". Mirrors the last [selectRelayFilter] intent. */
    val selectedFilter: String? = null,
)

/**
 * Activity journal (docs/04 §Activity) — merges two real sources
 * newest-first:
 *
 * - [ActivityJournal]: this device's session lifecycle log (connects,
 *   disconnects, auth rejections, pair/unpair) — the part the wire never
 *   reports.
 * - [SessionRepository.activities]: the relay's own action journal
 *   (`activity` pushes + `get_activity` history).
 *
 * Intents: [selectRelayFilter] narrows the list to one relay;
 * [refresh] re-pulls each relay's history.
 */
class ActivityViewModel(
    private val sessions: SessionRepository,
    private val journal: ActivityJournal,
    private val now: () -> Long = System::currentTimeMillis,
) : ViewModel() {

    private val relayFilter = MutableStateFlow<String?>(null)

    private val ticker = flow<Unit> {
        while (true) {
            emit(Unit)
            delay(30_000)
        }
    }

    val uiState: StateFlow<ActivityUiState> = combine(
        journal.events,
        sessions.activities,
        sessions.relays,
        relayFilter,
        ticker,
    ) { events, activities, relays, filter, _ ->
        val labels = relays.associate { it.id to it.label }
        // A filter on a forgotten relay would render an empty dead end —
        // fall back to "All" rather than hide the journal.
        val effectiveFilter = filter?.takeIf { id -> relays.any { it.id == id } }
        val items = buildList(events.size + activities.size) {
            events.forEach { event -> add(event.toItem()) }
            activities.forEach { activity -> add(activity.toItem(labels[activity.relayId])) }
        }
            .sortedByDescending { it.timestampEpochMs }
            .filter { effectiveFilter == null || it.relayId == effectiveFilter }
        ActivityUiState(
            items = items,
            filters = relays.map {
                RelayFilterUi(it.id, it.label, it.id == effectiveFilter)
            },
            selectedFilter = effectiveFilter,
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), ActivityUiState())

    init {
        // Pull each live relay's journal so the list isn't empty on entry.
        sessions.requestActivities()
    }

    /** Filter intent — null selects "All". */
    fun selectRelayFilter(relayId: String?) {
        relayFilter.value = relayId
    }

    /** Manual refresh — fans `get_activity` out to every session. */
    fun refresh() {
        sessions.requestActivities()
    }

    private fun ActivityJournal.Event.toItem(): ActivityItemUi {
        val kind = when (kind) {
            ActivityJournal.Kind.RELAY_ADDED -> ActivityItemKind.RELAY_ADDED
            ActivityJournal.Kind.RELAY_REMOVED -> ActivityItemKind.RELAY_REMOVED
            ActivityJournal.Kind.CONNECTING -> ActivityItemKind.CONNECTING
            ActivityJournal.Kind.CONNECTED -> ActivityItemKind.CONNECTED
            ActivityJournal.Kind.DISCONNECTED -> ActivityItemKind.DISCONNECTED
            ActivityJournal.Kind.AUTH_REJECTED -> ActivityItemKind.AUTH_REJECTED
            ActivityJournal.Kind.PAIRING_REQUIRED -> ActivityItemKind.PAIRING_REQUIRED
        }
        val headline = when (kind) {
            ActivityItemKind.RELAY_ADDED -> "Relay added"
            ActivityItemKind.RELAY_REMOVED -> "Relay removed"
            ActivityItemKind.CONNECTING -> "Connecting"
            ActivityItemKind.CONNECTED -> "Connected"
            ActivityItemKind.DISCONNECTED -> "Disconnected"
            ActivityItemKind.AUTH_REJECTED -> "Authorization rejected — re-pair required"
            ActivityItemKind.PAIRING_REQUIRED -> "Pairing required"
            ActivityItemKind.ACTION -> detail
        }
        return ActivityItemUi(
            key = id,
            relayId = relayId,
            relayLabel = relayLabel,
            headline = headline,
            detail = detail,
            timestampEpochMs = timestampEpochMs,
            timestampLabel = timestampLabel(timestampEpochMs),
            ageLabel = ageLabel(now() - timestampEpochMs),
            kind = kind,
        )
    }

    private fun RelayActivity.toItem(label: String?): ActivityItemUi =
        ActivityItemUi(
            key = key,
            relayId = relayId,
            relayLabel = label ?: relayId,
            headline = entry.summary.ifEmpty { entry.kind },
            detail = listOfNotNull(
                entry.kind.takeIf { it.isNotEmpty() },
                entry.agent.takeIf { it.isNotEmpty() },
                entry.project.takeIf { it.isNotEmpty() },
            ).distinct().joinToString(" · "),
            timestampEpochMs = entry.timestamp,
            timestampLabel = timestampLabel(entry.timestamp),
            ageLabel = ageLabel(now() - entry.timestamp),
            kind = ActivityItemKind.ACTION,
        )

    private fun timestampLabel(timestamp: Long): String {
        val pattern = if (now() - timestamp < DAY_MS) "HH:mm" else "MMM d, HH:mm"
        return SimpleDateFormat(pattern, Locale.US).format(Date(timestamp))
    }

    private fun ageLabel(ms: Long): String {
        val seconds = max(0, ms) / 1_000
        return when {
            seconds < 60 -> "${seconds}s ago"
            seconds < 3_600 -> "${seconds / 60}m ago"
            else -> "${seconds / 3_600}h ago"
        }
    }

    private companion object {
        const val DAY_MS = 24L * 60 * 60 * 1_000
    }
}
