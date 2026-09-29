package com.lerdr.app.session

import com.google.common.truth.Truth.assertThat
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import lerdr.core.model.WorkspaceWorktree
import lerdr.core.protocol.LerdrJson
import lerdr.core.store.RelayWorkspace
import lerdr.core.transport.CommandException
import org.junit.Test

/**
 * `closePlanFor` / close refusal mapping — the pure seams behind
 * `CloseWorkspaceSheet` (the relay's `workspaceGroupIDs` mirrored).
 */
class CloseWorkspaceSheetTest {

    private fun workspace(
        id: String,
        relayId: String = "r1",
        label: String = id,
        repoKey: String = "",
        linked: Boolean = false,
    ) = RelayWorkspace(
        relayId = relayId,
        relayLabel = "workstation",
        workspaceId = id,
        label = label,
        worktree = if (repoKey.isEmpty()) {
            null
        } else {
            WorkspaceWorktree(
                repoKey = repoKey,
                repoName = "repo",
                repoRoot = "/src/repo",
                checkoutPath = "/src/repo/$id",
                isLinkedWorktree = linked,
            )
        },
    )

    // ── closePlanFor ────────────────────────────────────────────────────

    @Test
    fun `standalone workspace plans a single close`() {
        val plan = closePlanFor(
            listOf(workspace("w1"), workspace("w2")),
            relayId = "r1",
            workspaceId = "w1",
        )
        assertThat(plan.mode).isEqualTo(ClosePlan.Mode.SINGLE)
        assertThat(plan.groupSize).isEqualTo(1)
        assertThat(plan.singleTargetId).isEqualTo("w1")
        assertThat(plan.expectedIds).containsExactly("w1")
    }

    @Test
    fun `worktree without a repo key still plans a single close`() {
        val plan = closePlanFor(
            listOf(workspace("w1", repoKey = "repo-a")),
            relayId = "r1",
            workspaceId = "w1",
        )
        assertThat(plan.mode).isEqualTo(ClosePlan.Mode.SINGLE)
        assertThat(plan.groupSize).isEqualTo(1)
    }

    @Test
    fun `primary of a linked group must close the whole group`() {
        val rows = listOf(
            workspace("main", label = "repo", repoKey = "repo-a"),
            workspace("wt-1", repoKey = "repo-a", linked = true),
            workspace("wt-2", repoKey = "repo-a", linked = true),
            workspace("other", repoKey = "repo-b"),
        )
        val plan = closePlanFor(rows, relayId = "r1", workspaceId = "main")
        assertThat(plan.mode).isEqualTo(ClosePlan.Mode.PRIMARY_GROUP)
        assertThat(plan.groupSize).isEqualTo(3)
        assertThat(plan.groupTargetId).isEqualTo("main")
        assertThat(plan.expectedIds).containsExactly("main", "wt-1", "wt-2")
    }

    @Test
    fun `linked worktree can close alone or through the primary`() {
        val rows = listOf(
            workspace("main", repoKey = "repo-a"),
            workspace("wt-1", repoKey = "repo-a", linked = true),
        )
        val plan = closePlanFor(rows, relayId = "r1", workspaceId = "wt-1")
        assertThat(plan.mode).isEqualTo(ClosePlan.Mode.LINKED_GROUP)
        assertThat(plan.isLinked).isTrue()
        assertThat(plan.singleTargetId).isEqualTo("wt-1")
        // Group closes target the primary — the relay refuses linked ids.
        assertThat(plan.groupTargetId).isEqualTo("main")
        assertThat(plan.expectedIds).containsExactly("main", "wt-1")
    }

    @Test
    fun `group membership ignores other relays with the same repo key`() {
        val rows = listOf(
            workspace("w1", relayId = "r1", repoKey = "repo-a"),
            workspace("w2", relayId = "r2", repoKey = "repo-a", linked = true),
        )
        val plan = closePlanFor(rows, relayId = "r1", workspaceId = "w1")
        assertThat(plan.mode).isEqualTo(ClosePlan.Mode.SINGLE)
        assertThat(plan.group.map { it.workspaceId }).containsExactly("w1")
    }

    @Test
    fun `missing workspace degrades to a single close`() {
        val plan = closePlanFor(
            listOf(workspace("w1")),
            relayId = "r1",
            workspaceId = "gone",
        )
        assertThat(plan.mode).isEqualTo(ClosePlan.Mode.SINGLE)
        assertThat(plan.singleTargetId).isEqualTo("gone")
    }

    @Test
    fun `consent escalation replaces expected ids with the relay set`() {
        val rows = listOf(
            workspace("main", repoKey = "repo-a"),
            workspace("wt-1", repoKey = "repo-a", linked = true),
        )
        val plan = closePlanFor(
            rows,
            relayId = "r1",
            workspaceId = "main",
            consentedGroupIds = listOf("main", "wt-1", "wt-new"),
        )
        assertThat(plan.expectedIds).containsExactly("main", "wt-1", "wt-new")
    }

    @Test
    fun `consent escalation forces the group path on a stale snapshot`() {
        // The store still shows the workspace alone — the relay's refusal
        // carried the real group; the dialog must offer the group close.
        val plan = closePlanFor(
            listOf(workspace("w1", repoKey = "repo-a")),
            relayId = "r1",
            workspaceId = "w1",
            consentedGroupIds = listOf("w1", "w2", "w3"),
        )
        assertThat(plan.mode).isEqualTo(ClosePlan.Mode.PRIMARY_GROUP)
        assertThat(plan.groupSize).isEqualTo(3)
        assertThat(plan.groupTargetId).isEqualTo("w1")
        assertThat(plan.expectedIds).containsExactly("w1", "w2", "w3")
    }

    // ── refusal extraction ──────────────────────────────────────────────

    private fun commandError(data: String? = null) = CommandException(
        message = "refused",
        data = data?.let { LerdrJson.parseToJsonElement(it) },
    )

    @Test
    fun `refusal code and ids read from command_result data`() {
        val error = commandError(
            """{"code":"workspace_group_close_required","workspace_ids":["a","b"]}""",
        )
        assertThat(closeRefusalCode(error)).isEqualTo("workspace_group_close_required")
        assertThat(closeRefusalWorkspaceIds(error)).containsExactly("a", "b")
    }

    @Test
    fun `missing data yields no code and no ids`() {
        val error = commandError()
        assertThat(closeRefusalCode(error)).isNull()
        assertThat(closeRefusalWorkspaceIds(error)).isEmpty()
    }

    // ── closeErrorMessage ───────────────────────────────────────────────

    @Test
    fun `group refusal codes map to retry-worded messages`() {
        assertThat(
            closeErrorMessage(commandError("""{"code":"workspace_group_changed"}""")),
        ).contains("changed")
        assertThat(
            closeErrorMessage(commandError("""{"code":"workspace_group_primary_required"}""")),
        ).contains("main workspace")
        assertThat(
            closeErrorMessage(commandError("""{"code":"workspace_group_consent_invalid"}""")),
        ).contains("confirm again")
        assertThat(
            closeErrorMessage(
                commandError("""{"code":"workspace_group_validation_unavailable"}"""),
            ),
        ).contains("try again")
    }

    @Test
    fun `dispatched unknown warns the close may have landed`() {
        val error = CommandException(message = "timeout", dispatchedUnknown = true)
        assertThat(closeErrorMessage(error)).contains("may have run")
    }

    @Test
    fun `plain failures pass the relay message through`() {
        val error = CommandException(message = "Workspace is unavailable")
        assertThat(closeErrorMessage(error)).isEqualTo("Workspace is unavailable")
        assertThat(closeErrorMessage(IllegalStateException("boom"))).isEqualTo("boom")
    }
}
