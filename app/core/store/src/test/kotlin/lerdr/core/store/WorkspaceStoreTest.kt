package lerdr.core.store

import com.google.common.truth.Truth.assertThat
import lerdr.core.model.WorkspaceInfo
import lerdr.core.model.WorkspaceWorktree
import org.junit.Test

class WorkspaceStoreTest {

    private fun ws(id: String, label: String = "ws-$id", number: Int = 1) =
        WorkspaceInfo(workspaceId = id, label = label, number = number)

    @Test
    fun `snapshot replaces the relay slice and tombstones missing workspaces`() {
        val store = WorkspaceStore()
        store.replaceForRelay("r1", "relay", listOf(ws("w1"), ws("w2")))
        assertThat(store.workspaces.value.map { it.workspaceId })
            .containsExactly("w1", "w2").inOrder()

        store.replaceForRelay("r1", "relay", listOf(ws("w2")))
        assertThat(store.workspaces.value.map { it.workspaceId }).containsExactly("w2")
    }

    @Test
    fun `other relays keep their rows and order`() {
        val store = WorkspaceStore()
        store.replaceForRelay("r1", "relay", listOf(ws("w1")))
        store.replaceForRelay("r2", "two", listOf(ws("x1")))
        store.replaceForRelay("r1", "relay", listOf(ws("w2")))
        assertThat(store.workspaces.value.map { "${it.relayId}:${it.workspaceId}" })
            .containsExactly("r2:x1", "r1:w2").inOrder()
    }

    @Test
    fun `equal rows keep their instance across snapshots`() {
        val store = WorkspaceStore()
        store.replaceForRelay("r1", "relay", listOf(ws("w1"), ws("w2")))
        val first = store.workspaces.value
        store.replaceForRelay("r1", "relay", listOf(ws("w2"), ws("w1", label = "renamed")))
        val second = store.workspaces.value
        assertThat(second.map { it.workspaceId }).containsExactly("w2", "w1").inOrder()
        assertThat(second[0]).isSameInstanceAs(first[1])
        assertThat(second[1]).isNotSameInstanceAs(first[0])
        assertThat(second[1].label).isEqualTo("renamed")
    }

    @Test
    fun `rows without a workspace id are dropped`() {
        val store = WorkspaceStore()
        store.replaceForRelay("r1", "relay", listOf(ws(""), ws("w1")))
        assertThat(store.workspaces.value.map { it.workspaceId }).containsExactly("w1")
    }

    @Test
    fun `blank label defaults and caps at 256 chars`() {
        val store = WorkspaceStore()
        store.replaceForRelay("r1", "relay", listOf(ws("w1", label = ""), ws("w2", label = "x".repeat(300))))
        assertThat(store.workspaces.value[0].label).isEqualTo("Workspace")
        assertThat(store.workspaces.value[1].label).hasLength(256)
    }

    @Test
    fun `worktree payload is carried through`() {
        val store = WorkspaceStore()
        val worktree = WorkspaceWorktree(repoKey = "k", repoName = "repo", checkoutPath = "/tmp/x")
        store.replaceForRelay("r1", "relay", listOf(ws("w1").copy(worktree = worktree)))
        assertThat(store.workspaces.value[0].worktree).isEqualTo(worktree)
    }

    @Test
    fun `removeRelay purges only that relay`() {
        val store = WorkspaceStore()
        store.replaceForRelay("r1", "relay", listOf(ws("w1")))
        store.replaceForRelay("r2", "two", listOf(ws("x1")))
        store.removeRelay("r1")
        assertThat(store.workspaces.value.map { it.workspaceId }).containsExactly("x1")
    }
}
