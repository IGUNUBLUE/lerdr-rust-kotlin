package com.lerdr.app.home

import androidx.compose.runtime.Immutable

/**
 * UI models for the mission-control Home screen (docs/04 §Home).
 * Deliberately decoupled from `core:store`'s [lerdr.core.store.Agent] —
 * feature rounds will map store rows onto these; the fake repository already
 * speaks this language so screens stay untouched when the real store lands.
 */

/** What a blocked agent needs — drives card color + iconography. */
enum class AttentionKind { APPROVAL, QUESTION, CHAT }

/** A card on the "needs you" rail — one blocked agent, answer inline. */
@Immutable
data class AttentionCardUi(
    val paneId: String,
    /** "claude · lerdr" — agent + workspace label. */
    val agentLabel: String,
    val kind: AttentionKind,
    /** "approval · 40s" — kind + age. */
    val metaLabel: String,
    /** The question/command preview. */
    val prompt: String,
    /** Top choices rendered as buttons (e.g. "Allow", "Deny"). */
    val options: List<String>,
    /** Normalized agent identity (e.g. "claude") — drives the avatar logo. */
    val provider: String? = null,
)

/** One agent row in the working/idle groups. */
@Immutable
data class AgentListItemUi(
    val paneId: String,
    /** "hermes · api-server" — agent + workspace. */
    val title: String,
    /** "Editing handler.go" — last known activity line. */
    val statusLine: String,
    /** Working agents show a wavy strip with this caption ("running tests…"). */
    val activityLabel: String?,
    /** "1:24" elapsed chip, or "idle". */
    val elapsedLabel: String,
    val working: Boolean,
    /** Normalized agent identity (e.g. "claude") — drives the avatar logo. */
    val provider: String? = null,
)

/** One computer card on the relays strip. */
@Immutable
data class RelayCardUi(
    val relayId: String,
    val label: String,
    /** "direct" / "tls" — the socket transport in use. */
    val transport: String,
    /** "connected" / "connecting…" / "offline" — live session status. */
    val statusLabel: String,
    val agentCount: Int,
    val connected: Boolean,
)

@Immutable
data class HomeUiState(
    /** All-relays-live rollup — the "live" chip on the app bar. */
    val live: Boolean = false,
    /** "2 computers · tailscale" — subtitle under the title. */
    val relaySummary: String = "",
    val needsYou: List<AttentionCardUi> = emptyList(),
    val working: List<AgentListItemUi> = emptyList(),
    val idle: List<AgentListItemUi> = emptyList(),
    val relays: List<RelayCardUi> = emptyList(),
)
