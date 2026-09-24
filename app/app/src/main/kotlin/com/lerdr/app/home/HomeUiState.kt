package com.lerdr.app.home

import androidx.compose.runtime.Immutable
import lerdr.core.model.Interaction
import lerdr.core.model.Option

/**
 * UI models for the mission-control Home screen (docs/04 §Home).
 * Deliberately decoupled from `core:store`'s [lerdr.core.store.Agent] —
 * feature rounds will map store rows onto these; the fake repository already
 * speaks this language so screens stay untouched when the real store lands.
 */

/** What a blocked agent needs — drives card color + iconography. */
enum class AttentionKind { APPROVAL, QUESTION, CHAT }

/**
 * A card on the "needs you" rail — one blocked agent. Inline answers mirror
 * the oracle's AgentList actions: approvals send `respond`, single-question
 * `single_select` interactions send `answer_question`; anything bigger
 * (multi-select, multi-question, free-text Other) navigates to the session's
 * full form via the "Choose answer/options" button.
 */
@Immutable
data class AttentionCardUi(
    val paneId: String,
    /** Owning relay — groups + `canControl` gating. */
    val relayId: String = "",
    /** "claude · lerdr" — agent + workspace label. */
    val agentLabel: String,
    val kind: AttentionKind,
    /** "approval · 40s" — kind + age. */
    val metaLabel: String,
    /** The question/command preview. */
    val prompt: String,
    /**
     * Approval choices — the oracle's `approvalOptions` (non-empty labels,
     * ≥ 2 required) with their real indices; `respond(index, choice)` uses
     * the position. Empty for other kinds.
     */
    val options: List<String> = emptyList(),
    /**
     * The oracle's `questionInteraction` — a validated `single_select` /
     * `multi_select` payload, or null when the question is not answerable
     * through the structured API.
     */
    val interaction: Interaction? = null,
    /** `agentStore.responding` — an answer is in flight; hide the buttons. */
    val responding: Boolean = false,
    /**
     * `SessionRepository.canControl(relayId)` — readers keep the card and its
     * navigation affordance but no mutating buttons.
     */
    val controllable: Boolean = false,
    /** Normalized agent identity (e.g. "claude") — drives the avatar logo. */
    val provider: String? = null,
) {
    /**
     * Quick-answer options — a lone `single_select` question without a
     * free-text Other. Each chip sends `answer_question` with `Option.index`
     * (the wire answer index, not the list position).
     */
    val quickOptions: List<Option>
        get() = interaction?.takeIf {
            kind == AttentionKind.QUESTION &&
                it.kindOrNull == Interaction.Kind.SINGLE_SELECT &&
                it.questionTotal <= 1 &&
                it.other.hidden
        }?.options.orEmpty()

    /**
     * "Choose answer (N)" / "Choose options (N)" — the oracle's label for a
     * question that needs the session's full form (multi-select,
     * multi-question, or a free-text Other). Null when the question is
     * quick-answerable or absent.
     */
    val chooseLabel: String?
        get() = interaction?.takeIf {
            kind == AttentionKind.QUESTION && quickOptions.isEmpty()
        }?.let {
            val verb = if (it.kindOrNull == Interaction.Kind.MULTI_SELECT) {
                "Choose options"
            } else {
                "Choose answer"
            }
            "$verb (${it.options.size})"
        }
}

/** One agent row in a [AgentGroupUi]. */
@Immutable
data class AgentListItemUi(
    val paneId: String,
    /** Owning relay — drives the reader-mode swipe gate. */
    val relayId: String = "",
    /** "hermes · api-server" — agent + workspace. */
    val title: String,
    /** "Editing handler.go" — last known activity line. */
    val statusLine: String,
    /** Working agents show a wavy strip with this caption ("running tests…"). */
    val activityLabel: String?,
    /** "1:24" elapsed chip, or "idle". */
    val elapsedLabel: String,
    val working: Boolean,
    /**
     * `SessionRepository.canControl(relayId)` — gates the swipe-to-stop
     * affordance; readers can only swipe to open.
     */
    val controllable: Boolean = false,
    /** Normalized agent identity (e.g. "claude") — drives the avatar logo. */
    val provider: String? = null,
    /** `lerdr_watching` token — a lerdr device is watching this pane. */
    val watching: Boolean = false,
    /** Herdr `pane.report_metadata` state label values — rendered as chips. */
    val stateLabels: List<String> = emptyList(),
)

/**
 * "relay ▸ workspace" grouping inside a status section (docs/04 §Home).
 * [key] is `relayId + \u0000 + workspaceIdentity` — stable across reorders.
 */
@Immutable
data class AgentGroupUi(
    val key: String,
    /** The computer name — "workstation". */
    val relayLabel: String,
    /**
     * Workspace label from the `workspaces` snapshot, else the oracle's
     * `groupLabel` fallback: sole project → sole cwd basename → first tab
     * label → "Workspace".
     */
    val label: String,
    val agents: List<AgentListItemUi>,
    /** `lerdr_devices` workspace token — lerdr devices on this workspace. */
    val watchingDevices: Int? = null,
)

/** One computer card on the relays strip / Computers tab. */
@Immutable
data class RelayCardUi(
    val relayId: String,
    val label: String,
    /** "direct" / "tls" — the socket transport in use. */
    val transport: String,
    /** "connected" / "connecting…" / "offline" / "12ms" — live status. */
    val statusLabel: String,
    val agentCount: Int,
    val connected: Boolean,
    /** Last keepalive round-trip in ms; -1 while unmeasured or down. */
    val rttMs: Long = -1,
)

@Immutable
data class HomeUiState(
    /** All-relays-live rollup — the "live" chip on the app bar. */
    val live: Boolean = false,
    /** "2 computers · tailscale" — subtitle under the title. */
    val relaySummary: String = "",
    val needsYou: List<AttentionCardUi> = emptyList(),
    /** Working agents grouped by relay ▸ workspace. */
    val working: List<AgentGroupUi> = emptyList(),
    /** Idle/ready/done agents grouped by relay ▸ workspace. */
    val idle: List<AgentGroupUi> = emptyList(),
    val relays: List<RelayCardUi> = emptyList(),
)
