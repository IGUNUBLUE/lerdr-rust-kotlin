package com.lerdr.app.home

import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import lerdr.core.model.Interaction
import lerdr.core.model.Option
import lerdr.core.model.Other

/**
 * Preview-quality fixture matching docs/mockup.png — test/double only;
 * production injects [RealHomeRepository].
 */
class FakeHomeRepository : HomeRepository {

    override val uiState: Flow<HomeUiState> = MutableStateFlow(
        HomeUiState(
            live = true,
            relaySummary = "2 computers · tailscale",
            needsYou = listOf(
                AttentionCardUi(
                    paneId = "sd::%1",
                    relayId = "sd",
                    agentLabel = "claude · lerdr",
                    kind = AttentionKind.APPROVAL,
                    metaLabel = "approval · 40s",
                    prompt = "Run go test ./internal/… ?",
                    options = listOf("Allow", "Deny"),
                    controllable = true,
                    provider = "claude",
                ),
                AttentionCardUi(
                    paneId = "sd::%2",
                    relayId = "sd",
                    agentLabel = "devin · herdr",
                    kind = AttentionKind.QUESTION,
                    metaLabel = "question · 3 options",
                    prompt = "Which module should own the delta cache?",
                    interaction = Interaction(
                        id = "q1",
                        kind = "single_select",
                        question = "Which module should own the delta cache?",
                        options = listOf(
                            Option(index = 0, label = "core:store"),
                            Option(index = 1, label = "session"),
                            Option(index = 2, label = "relay"),
                        ),
                        other = Other(hidden = true),
                        questionTotal = 1,
                    ),
                    controllable = true,
                    provider = "devin",
                ),
            ),
            working = listOf(
                AgentGroupUi(
                    key = "sd\u0000lerdr",
                    relayLabel = "sd",
                    label = "lerdr",
                    agents = listOf(
                        AgentListItemUi(
                            paneId = "sd::%3",
                            relayId = "sd",
                            title = "hermes · api-server",
                            statusLine = "Editing handler.go",
                            activityLabel = "running tests…",
                            elapsedLabel = "1:24",
                            working = true,
                            controllable = true,
                        ),
                        AgentListItemUi(
                            paneId = "sd::%4",
                            relayId = "sd",
                            title = "pi · dotfiles",
                            statusLine = "Bash: git rebase",
                            activityLabel = "writing migration.sql",
                            elapsedLabel = "0:37",
                            working = true,
                            controllable = true,
                            provider = "pi",
                        ),
                    ),
                ),
            ),
            idle = listOf(
                AgentGroupUi(
                    key = "sd\u0000web",
                    relayLabel = "sd",
                    label = "web",
                    agents = listOf(
                        AgentListItemUi(
                            paneId = "sd::%5",
                            relayId = "sd",
                            title = "codex · web",
                            statusLine = "ready · 12m ago",
                            activityLabel = null,
                            elapsedLabel = "idle",
                            working = false,
                            controllable = true,
                            provider = "codex",
                        ),
                    ),
                ),
            ),
            relays = listOf(
                RelayCardUi(
                    relayId = "sd",
                    label = "sd",
                    transport = "tls",
                    statusLabel = "12ms",
                    agentCount = 4,
                    connected = true,
                    rttMs = 12,
                ),
                RelayCardUi(
                    relayId = "workstation",
                    label = "workstation",
                    transport = "direct",
                    statusLabel = "connected",
                    agentCount = 0,
                    connected = true,
                ),
            ),
        ),
    )
}
