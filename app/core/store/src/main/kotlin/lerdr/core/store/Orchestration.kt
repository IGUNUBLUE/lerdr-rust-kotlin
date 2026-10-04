package lerdr.core.store

/**
 * Orchestration cohorts — Herdr workspaces linked by `worktree.repo_root`:
 * the root checkout hosts the orchestrator pane and linked worktrees host
 * its dispatched children. An agent integration supplies session identity
 * and real lifecycle; without it Herdr's omp idle fallback cannot see
 * in-process tasks. `agent start` is not required for lifecycle reporting.
 * The worktree link is a display-only fallback for separate child panes:
 * the pane state stays unclaimed and nothing on the relay asserts into a
 * foreign pane. Busy children inside the same omp process require the
 * Herdr omp integration in that process's active profile.
 */

/** Status groups that count as live cohort work — busy or awaiting input. */
private val COHORT_BUSY = setOf(
    AgentStatusGroup.WORKING,
    AgentStatusGroup.BLOCKED,
    AgentStatusGroup.ATTENTION,
)

/**
 * Busy children of [agent]'s cohort — agents in linked-worktree
 * workspaces sharing the root's `repo_root` on the same relay. Only
 * hook-less panes in the non-linked (repo-root) workspace derive: a
 * hook-bound pane already reports real status, and a worktree member is
 * a child, not the orchestrator. Anything else scores 0.
 */
fun cohortBusyCount(
    agent: Agent?,
    agents: List<Agent>,
    workspaces: List<RelayWorkspace>,
): Int {
    if (agent == null || agent.workspaceId.isEmpty() ||
        !agent.agentSessionId.isNullOrEmpty()
    ) {
        return 0
    }
    val mine = workspaces.firstOrNull {
        it.relayId == agent.relayId && it.workspaceId == agent.workspaceId
    }?.worktree ?: return 0
    if (mine.isLinkedWorktree || mine.repoRoot.isEmpty()) return 0
    val siblings = HashSet<String>()
    for (other in workspaces) {
        val worktree = other.worktree
        if (other.relayId == agent.relayId && worktree != null &&
            worktree.isLinkedWorktree && worktree.repoRoot == mine.repoRoot
        ) {
            siblings.add(other.workspaceId)
        }
    }
    if (siblings.isEmpty()) return 0
    return agents.count {
        it.relayId == agent.relayId &&
            it.workspaceId in siblings &&
            agentStatusGroup(it) in COHORT_BUSY
    }
}

/**
 * Display status for a cohort-derivable pane — `"orchestrating"` while
 * its linked worktrees churn, else null and the wire status stands.
 * Working/blocked/attention panes already carry an informative status.
 */
fun Agent?.orchestratingStatus(cohortBusy: Int): String? {
    if (cohortBusy <= 0) return null
    val group = agentStatusGroup(this)
    return if (group == AgentStatusGroup.READY ||
        group == AgentStatusGroup.DONE ||
        group == AgentStatusGroup.OTHER
    ) {
        "orchestrating"
    } else {
        null
    }
}
