package lerdr.core.data

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import androidx.datastore.preferences.core.edit
import app.cash.turbine.test
import com.google.common.truth.Truth.assertThat
import java.io.File
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runTest
import lerdr.core.model.AgentState
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

@OptIn(ExperimentalCoroutinesApi::class)
class DraftStoreTest {

    @get:Rule
    val tmp: TemporaryFolder = TemporaryFolder()

    private var now = 1_000_000L

    private fun TestScope.store(): DraftStore {
        val dataStore = PreferenceDataStoreFactory.create(scope = backgroundScope) {
            File(tmp.root, "drafts.preferences_pb")
        }
        return DraftStore(dataStore, { now })
    }

    // ── save / load ──────────────────────────────────────────────────

    @Test
    fun `saved draft reads back with its timestamp`() = runTest {
        val store = store()
        assertThat(store.save("pane-1", "hello")).isEqualTo(DraftSaveResult.SAVED)
        val draft = store.current("pane-1")!!
        assertThat(draft.text).isEqualTo("hello")
        assertThat(draft.updatedAtEpochMs).isEqualTo(now)
        assertThat(draft.identity).isEqualTo("pane-1")
    }

    @Test
    fun `draft flow emits on save and clears on empty text`() = runTest {
        val store = store()
        store.draft("pane-1").test {
            assertThat(awaitItem()).isNull()
            store.save("pane-1", "wip")
            assertThat(awaitItem()!!.text).isEqualTo("wip")
            store.save("pane-1", "")
            assertThat(awaitItem()).isNull()
            cancelAndIgnoreRemainingEvents()
        }
    }

    @Test
    fun `empty save reports cleared`() = runTest {
        val store = store()
        assertThat(store.save("pane-1", "")).isEqualTo(DraftSaveResult.CLEARED)
    }

    @Test
    fun `oversize draft clears the stored record`() = runTest {
        val store = store()
        store.save("pane-1", "short")
        val oversize = "x".repeat(DraftStore.MAX_BYTES + 1)
        assertThat(store.save("pane-1", oversize)).isEqualTo(DraftSaveResult.TOO_LARGE)
        assertThat(store.current("pane-1")).isNull()
    }

    @Test
    fun `draft at exactly the byte cap saves`() = runTest {
        val store = store()
        assertThat(store.save("pane-1", "x".repeat(DraftStore.MAX_BYTES)))
            .isEqualTo(DraftSaveResult.SAVED)
    }

    @Test
    fun `clear removes the record`() = runTest {
        val store = store()
        store.save("pane-1", "wip")
        store.clear("pane-1")
        assertThat(store.current("pane-1")).isNull()
    }

    // ── TTL & eviction ───────────────────────────────────────────────

    @Test
    fun `expired draft reads absent and is evicted`() = runTest {
        val store = store()
        store.save("pane-1", "old")
        now += DraftStore.MAX_AGE_MS + 1
        assertThat(store.current("pane-1")).isNull()
        // Physical eviction — a second read doesn't re-see it, and prune has nothing to do.
        assertThat(store.prune()).isEmpty()
    }

    @Test
    fun `draft at the TTL boundary survives`() = runTest {
        val store = store()
        store.save("pane-1", "fresh")
        now += DraftStore.MAX_AGE_MS
        assertThat(store.current("pane-1")!!.text).isEqualTo("fresh")
    }

    @Test
    fun `prune evicts oldest beyond the entry cap`() = runTest {
        val store = store()
        for (i in 0 until DraftStore.MAX_ENTRIES) {
            store.save("pane-$i", "d$i")
            now += 1
        }
        // Every save prunes inline; at 64 entries nothing is evicted yet.
        assertThat(store.prune()).isEmpty()
        // The 65th save evicts the oldest draft during its own prune.
        store.save("pane-new", "latest")
        assertThat(store.current("pane-0")).isNull()
        assertThat(store.current("pane-new")!!.text).isEqualTo("latest")
        assertThat(store.prune()).isEmpty()
    }

    @Test
    fun `unparseable stored value reads absent and prunes`() = runTest {
        val dataStore = PreferenceDataStoreFactory.create(scope = backgroundScope) {
            File(tmp.root, "drafts.preferences_pb")
        }
        val store = DraftStore(dataStore, { now })
        dataStore.edit {
            it[DraftStore.draftKey("pane-1")] = "garbage"
            it[DraftStore.draftKey("pane-2")] = """{"version":99,"identity":"pane-2","text":"x","updatedAt":1}"""
        }
        assertThat(store.current("pane-1")).isNull()
        val removed = store.prune()
        assertThat(removed).containsExactly(DraftStore.draftKey("pane-2").name)
        assertThat(store.prune()).isEmpty()
    }

    @Test
    fun `identity mismatch is not served under another pane's key`() = runTest {
        val dataStore = PreferenceDataStoreFactory.create(scope = backgroundScope) {
            File(tmp.root, "drafts.preferences_pb")
        }
        val store = DraftStore(dataStore, { now })
        dataStore.edit {
            it[DraftStore.draftKey("pane-1")] =
                """{"version":1,"identity":"other-pane","text":"x","updatedAt":$now}"""
        }
        assertThat(store.current("pane-1")).isNull()
    }

    // ── identity ─────────────────────────────────────────────────────

    @Test
    fun `composerDraftIdentity matches the oracle shape`() {
        val agent = AgentState(
            rawPaneId = "p1",
            workspaceId = "w1",
            tabId = "t1",
            agent = "claude",
            cwd = "/repo",
        )
        assertThat(composerDraftIdentity("r1", agent))
            .isEqualTo("""["r1","w1:t1:p1","claude","/repo"]""")

        // terminal_id wins when present.
        val withTerminal = agent.copy(terminalId = "term-9")
        assertThat(composerDraftIdentity("r1", withTerminal))
            .isEqualTo("""["r1","term-9","claude","/repo"]""")
    }
}
