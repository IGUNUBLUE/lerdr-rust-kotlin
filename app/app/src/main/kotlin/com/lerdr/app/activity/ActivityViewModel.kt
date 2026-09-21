package com.lerdr.app.activity

import androidx.compose.runtime.Immutable
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.lerdr.app.session.SessionRepository
import kotlin.math.max
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.delay

/** One journal row — label + relay + age. */
@Immutable
data class ActivityItemUi(
    val key: String,
    val relayLabel: String,
    val label: String,
    val detail: String,
    val ageLabel: String,
)

@Immutable
data class ActivityUiState(
    val items: List<ActivityItemUi> = emptyList(),
)

/** Reads the session journal; refreshes the relay's history on entry. */
class ActivityViewModel(
    sessions: SessionRepository,
    private val now: () -> Long = System::currentTimeMillis,
) : ViewModel() {

    private val ticker = flow<Unit> {
        while (true) {
            emit(Unit)
            delay(30_000)
        }
    }

    val uiState: StateFlow<ActivityUiState> = combine(
        sessions.activities,
        ticker,
    ) { activities, _ ->
        ActivityUiState(
            items = activities.map { item ->
                ActivityItemUi(
                    key = item.key,
                    relayLabel = item.relayId,
                    label = item.entry.summary.ifEmpty { item.entry.kind },
                    detail = item.entry.kind,
                    ageLabel = ageLabel(now() - item.entry.timestamp),
                )
            },
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), ActivityUiState())

    init {
        sessions.requestActivities()
    }

    private fun ageLabel(ms: Long): String {
        val seconds = max(0, ms) / 1_000
        return when {
            seconds < 60 -> "${seconds}s ago"
            seconds < 3_600 -> "${seconds / 60}m ago"
            else -> "${seconds / 3_600}h ago"
        }
    }
}
