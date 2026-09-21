package com.lerdr.app.home

import javax.inject.Inject
import javax.inject.Singleton
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow

/**
 * Home data seam — repositories expose `Flow`, never suspend-gets
 * (see .devin/skills/android-app). The real implementation will fold
 * `core:store`'s AgentStore + ConnectionStore into [HomeUiState]; until that
 * lands, [FakeHomeRepository] feeds the shell.
 */
interface HomeRepository {
    val uiState: Flow<HomeUiState>
}

/** Preview-quality fixture matching docs/mockup.png. */
@Singleton
class FakeHomeRepository @Inject constructor() : HomeRepository {

    override val uiState: Flow<HomeUiState> = MutableStateFlow(
        HomeUiState(
            live = true,
            relaySummary = "2 computers · tailscale",
            needsYou = listOf(
                AttentionCardUi(
                    paneId = "sd::%1",
                    agentLabel = "claude · lerdr",
                    kind = AttentionKind.APPROVAL,
                    metaLabel = "approval · 40s",
                    prompt = "Run go test ./internal/… ?",
                    options = listOf("Allow", "Deny"),
                ),
                AttentionCardUi(
                    paneId = "sd::%2",
                    agentLabel = "devin · herdr",
                    kind = AttentionKind.QUESTION,
                    metaLabel = "question · 3 options",
                    prompt = "Which module should own the delta cache?",
                    options = listOf("Answer →"),
                ),
            ),
            working = listOf(
                AgentListItemUi(
                    paneId = "sd::%3",
                    title = "hermes · api-server",
                    statusLine = "Editing handler.go",
                    activityLabel = "running tests…",
                    elapsedLabel = "1:24",
                    working = true,
                ),
                AgentListItemUi(
                    paneId = "sd::%4",
                    title = "pi · dotfiles",
                    statusLine = "Bash: git rebase",
                    activityLabel = "writing migration.sql",
                    elapsedLabel = "0:37",
                    working = true,
                ),
            ),
            idle = listOf(
                AgentListItemUi(
                    paneId = "sd::%5",
                    title = "codex · web",
                    statusLine = "ready · 12m ago",
                    activityLabel = null,
                    elapsedLabel = "idle",
                    working = false,
                ),
            ),
            relays = listOf(
                RelayCardUi(
                    relayId = "sd",
                    label = "sd",
                    transport = "tailscale",
                    latencyLabel = "12ms",
                    agentCount = 4,
                    connected = true,
                ),
                RelayCardUi(
                    relayId = "workstation",
                    label = "workstation",
                    transport = "gateway",
                    latencyLabel = "81ms",
                    agentCount = 0,
                    connected = true,
                ),
            ),
        ),
    )
}
