package lerdr.core.store

import com.google.common.truth.Truth.assertThat
import lerdr.core.model.WorkspaceWorktree
import org.junit.Test

/**
 * Cohort derivation — hook-less panes in repo-root workspaces surface
 * busy linked-worktree siblings as "orchestrating" (see Orchestration.kt).
 */
class OrchestrationTest {

    private fun agent(
        paneId: String,
        workspaceId: String = "",
        status: String? = "idle",
        agentSessionId: String? = null,
    ) = Agent(
        relayId = "r1",
        relayLabel = "relay",
        rawPaneId = paneId,
        paneId = "r1::$paneId",
        agent = "omp",
        status = status,
        workspaceId = workspaceId,
        agentSessionId = agentSessionId,
    )

    private fun workspace(id: String, linked: Boolean, repoRoot: String = ROOT) =
        RelayWorkspace(
            relayId = "r1",
            relayLabel = "relay",
            workspaceId = id,
            worktree = WorkspaceWorktree(
                repoRoot = repoRoot,
                checkoutPath = if (linked) "$ROOT-worktrees/$id" else ROOT,
                isLinkedWorktree = linked,
            ),
        )

    private fun plainWorkspace(id: String) = RelayWorkspace(
        relayId = "r1",
        relayLabel = "relay",
        workspaceId = id,
    )

    @Test
    fun `hook-less pane in root workspace counts busy linked children`() {
        val workspaces = listOf(
            workspace("wA", linked = false),
            workspace("w1", linked = true),
            workspace("w2", linked = true),
        )
        val orchestrator = agent("%A", workspaceId = "wA")
        val children = listOf(
            agent("%1", workspaceId = "w1", status = "working"),
            agent("%2", workspaceId = "w2", status = "working"),
            agent("%3", workspaceId = "w2", status = "idle"),
        )
        assertThat(cohortBusyCount(orchestrator, children + orchestrator, workspaces))
            .isEqualTo(2)
    }

    @Test
    fun `blocked child counts toward the cohort`() {
        val workspaces = listOf(
            workspace("wA", linked = false),
            workspace("w1", linked = true),
        )
        val orchestrator = agent("%A", workspaceId = "wA")
        val children = listOf(
            agent("%1", workspaceId = "w1", status = "blocked",
                agentSessionId = "sess-1").let {
                it.copy(attentionKind = "question")
            },
        )
        assertThat(cohortBusyCount(orchestrator, children + orchestrator, workspaces))
            .isEqualTo(1)
    }

    @Test
    fun `hook-bound pane does not derive — real status stands`() {
        val workspaces = listOf(
            workspace("wA", linked = false),
            workspace("w1", linked = true),
        )
        val bound = agent("%A", workspaceId = "wA", agentSessionId = "sess-9")
        val children = listOf(agent("%1", workspaceId = "w1", status = "working"))
        assertThat(cohortBusyCount(bound, children + bound, workspaces)).isEqualTo(0)
    }

    @Test
    fun `linked-worktree member is a child, not an orchestrator`() {
        val workspaces = listOf(
            workspace("wA", linked = false),
            workspace("w1", linked = true),
            workspace("w2", linked = true),
        )
        val child = agent("%1", workspaceId = "w1")
        val others = listOf(
            agent("%2", workspaceId = "w2", status = "working"),
        )
        assertThat(cohortBusyCount(child, others + child, workspaces)).isEqualTo(0)
    }

    @Test
    fun `plain workspace without worktree cannot orchestrate`() {
        val workspaces = listOf(
            plainWorkspace("wA"),
            workspace("w1", linked = true),
        )
        val agent = agent("%A", workspaceId = "wA")
        assertThat(cohortBusyCount(agent, listOf(agent), workspaces)).isEqualTo(0)
    }

    @Test
    fun `idle cohort does not count`() {
        val workspaces = listOf(
            workspace("wA", linked = false),
            workspace("w1", linked = true),
        )
        val orchestrator = agent("%A", workspaceId = "wA")
        val children = listOf(agent("%1", workspaceId = "w1", status = "idle"))
        assertThat(cohortBusyCount(orchestrator, children + orchestrator, workspaces))
            .isEqualTo(0)
    }

    @Test
    fun `different repo root does not count`() {
        val workspaces = listOf(
            workspace("wA", linked = false),
            workspace("w1", linked = true, repoRoot = "/other/repo"),
        )
        val orchestrator = agent("%A", workspaceId = "wA")
        val children = listOf(agent("%1", workspaceId = "w1", status = "working"))
        assertThat(cohortBusyCount(orchestrator, children + orchestrator, workspaces))
            .isEqualTo(0)
    }

    @Test
    fun `different relay does not count`() {
        val workspaces = listOf(
            workspace("wA", linked = false),
            RelayWorkspace(
                relayId = "r2",
                relayLabel = "other",
                workspaceId = "w9",
                worktree = WorkspaceWorktree(
                    repoRoot = ROOT,
                    isLinkedWorktree = true,
                ),
            ),
        )
        val orchestrator = agent("%A", workspaceId = "wA")
        val childOnOtherRelay = Agent(
            relayId = "r2",
            relayLabel = "other",
            rawPaneId = "%9",
            paneId = "r2::%9",
            status = "working",
            workspaceId = "w9",
        )
        assertThat(
            cohortBusyCount(orchestrator, listOf(orchestrator, childOnOtherRelay), workspaces),
        ).isEqualTo(0)
    }

    @Test
    fun `no agent or no workspace scores zero`() {
        val workspaces = listOf(
            workspace("wA", linked = false),
            workspace("w1", linked = true),
        )
        val children = listOf(agent("%1", workspaceId = "w1", status = "working"))
        assertThat(cohortBusyCount(null, children, workspaces)).isEqualTo(0)
        assertThat(cohortBusyCount(agent("%A"), children, workspaces)).isEqualTo(0)
    }

    @Test
    fun `orchestratingStatus maps only non-busy wire states`() {
        assertThat(agent("%A", status = "idle").orchestratingStatus(2))
            .isEqualTo("orchestrating")
        assertThat(agent("%A", status = "done").orchestratingStatus(1))
            .isEqualTo("orchestrating")
        assertThat(agent("%A", status = "working").orchestratingStatus(1)).isNull()
        assertThat(agent("%A", status = "blocked").orchestratingStatus(1)).isNull()
        assertThat(agent("%A", status = "idle").orchestratingStatus(0)).isNull()
        assertThat(null.orchestratingStatus(0)).isNull()
    }

    private companion object {
        const val ROOT = "/home/u/repo"
    }
}
