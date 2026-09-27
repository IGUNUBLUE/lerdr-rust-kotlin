package com.lerdr.app.session

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import com.google.common.truth.Truth.assertThat
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonPrimitive
import lerdr.core.data.DeviceRole
import lerdr.core.data.RelayDeviceCredential
import lerdr.core.data.RelayEndpoint
import lerdr.core.data.RelayRegistry
import lerdr.core.protocol.LerdrJson
import lerdr.core.store.AgentStore
import lerdr.core.store.ConnectionStore
import lerdr.core.store.WorkspaceStore
import lerdr.core.store.clientPaneId
import org.junit.After
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

private fun json(raw: String): JsonObject =
    LerdrJson.parseToJsonElement(raw) as JsonObject

@OptIn(ExperimentalCoroutinesApi::class)
class WorktreesViewModelTest {

    @get:Rule
    val tmp = TemporaryFolder()

    private val mainDispatcher = UnconfinedTestDispatcher()

    @Before
    fun setMain() = Dispatchers.setMain(mainDispatcher)

    @After
    fun resetMain() = Dispatchers.resetMain()

    private class Harness(
        private val testScope: TestScope,
        tmpDir: File,
    ) {
        private val scope = testScope.backgroundScope
        val credentials = FakeCredentialStore()
        private val dataStore = PreferenceDataStoreFactory.create(scope = scope) {
            File(tmpDir, "relays.preferences_pb")
        }
        val registry = RelayRegistry(dataStore, scope)
        val agents = AgentStore(scope)
        val workspaces = WorkspaceStore()
        val connections = ConnectionStore(clock = { 0L })
        val factory = FakeRelaySessionFactory(scope)
        val repository = SessionRepository(
            scope = scope,
            credentialStore = credentials,
            relayRegistry = registry,
            agentStore = agents,
            workspaceStore = workspaces,
            connectionStore = connections,
            sessionFactory = factory,
        )

        val endpoint = RelayEndpoint(
            id = "r1",
            label = "workstation",
            host = "192.168.1.5",
            port = 7474,
            transport = lerdr.core.data.RelayTransport.WEBSOCKET,
        )
        val origin = "ws://192.168.1.5:7474"

        fun pump() = testScope.runCurrent()

        fun handle(): FakeRelaySessionHandle =
            factory.handleFor(origin) ?: error("no session for $origin")

        fun sentFrames(): List<JsonObject> = handle().sentRaw.map { json(it) }

        fun sentOf(type: String): List<JsonObject> = sentFrames().filter {
            it["type"]?.jsonPrimitive?.content == type
        }

        /** The last wire frame of [type] — `requestRaw` frames ride sendRaw. */
        fun lastRequest(type: String): JsonObject =
            sentOf(type).lastOrNull() ?: error("no $type frame sent")

        /**
         * Connected session with `worktree_management` capability, a ready
         * inventory, one agent and one (optionally linked) workspace row.
         */
        suspend fun connectReady(
            capabilities: String = "\"worktree_management\"",
            inventoryState: String = "ready",
            linkedWorktree: Boolean = true,
        ) {
            repository.connect(endpoint)
            handle().connect()
            handle().emit(
                json(
                    """{"type":"push_config","capabilities":[$capabilities],"inventory":{"state":"$inventoryState"}}""",
                ),
            )
            handle().emit(
                json(
                    """{"type":"agents","agents":[{"pane_id":"%1","raw_pane_id":"%1","terminal_id":"t1","server_session_id":"ss1","generation":3,"agent":"claude","name":"claude","status":"working","cwd":"/home/u/worktrees/fix","project":"lerdr","workspace_id":"w1","tab_id":"tabA","updated_at":100}]}""",
                ),
            )
            val worktree = if (linkedWorktree) {
                ""","worktree":{"repo_key":"k1","repo_name":"lerdr","repo_root":"/home/u/lerdr","checkout_path":"/home/u/worktrees/fix","is_linked_worktree":true}"""
            } else {
                ""
            }
            handle().emit(
                json(
                    """{"type":"workspaces","workspaces":[{"workspace_id":"w1","number":1,"label":"lerdr","focused":true,"pane_count":1,"tab_count":1,"active_tab_id":"tabA","agent_status":"working","cwd":"/home/u/worktrees/fix"$worktree}]}""",
                ),
            )
            pump()
        }

        /** Answer the last `type` request with a `command_result`. */
        suspend fun answer(type: String, resultJson: String) {
            val requestId = lastRequest(type)["request_id"]!!.jsonPrimitive.content
            handle().emit(
                json(
                    """{"type":"command_result","request_id":"$requestId","action":"$type",$resultJson}""",
                ),
            )
            pump()
        }

        suspend fun answerOk(type: String, dataJson: String? = null) {
            val data = dataJson?.let { ""","data":$it""" }.orEmpty()
            answer(type, """"ok":true,"phase":"completed"$data""")
        }

        suspend fun answerFailed(
            type: String,
            error: String,
            phase: String = "not_started",
            dataJson: String? = null,
        ) {
            val data = dataJson?.let { ""","data":$it""" }.orEmpty()
            answer(type, """"ok":false,"phase":"$phase","error":"$error"$data""")
        }

        fun viewModel(
            relayId: String = "r1",
            workspaceId: String = "w1",
        ): WorktreesViewModel =
            WorktreesViewModel(relayId, workspaceId, repository, workspaces).also {
                // Keep the WhileSubscribed stateIn live for assertions.
                testScope.backgroundScope.launch { it.uiState.collect { } }
            }
    }

    // ── parseWorktreeListing ──────────────────────────────────────────

    @Test
    fun `a populated listing parses source and rows verbatim`() {
        val listing = parseWorktreeListing(
            json(
                """{"source":{"repo_key":"k1","repo_name":"lerdr","repo_root":"/home/u/lerdr","source_checkout_path":"/home/u/lerdr","source_workspace_id":"w0"},"worktrees":[{"path":"/home/u/lerdr","branch":"main","is_bare":false,"is_detached":false,"is_prunable":false,"is_linked_worktree":false,"label":"main","open_workspace_id":"w0"},{"path":"/home/u/worktrees/fix","branch":"fix/one","is_bare":false,"is_detached":false,"is_prunable":false,"is_linked_worktree":true,"label":"fix/one","open_workspace_id":null}]}""",
            ),
        )
        assertThat(listing.source.repoKey).isEqualTo("k1")
        assertThat(listing.source.repoRoot).isEqualTo("/home/u/lerdr")
        assertThat(listing.source.sourceWorkspaceId).isEqualTo("w0")
        assertThat(listing.worktrees).hasSize(2)
        val open = listing.worktrees[0]
        assertThat(open.path).isEqualTo("/home/u/lerdr")
        assertThat(open.branch).isEqualTo("main")
        assertThat(open.openWorkspaceId).isEqualTo("w0")
        assertThat(open.openable).isFalse()
        val linked = listing.worktrees[1]
        assertThat(linked.isLinkedWorktree).isTrue()
        assertThat(linked.openable).isTrue()
        assertThat(linked.title).isEqualTo("fix/one")
    }

    @Test
    fun `an empty worktrees array parses to an empty listing`() {
        val listing = parseWorktreeListing(
            json("""{"source":{"repo_key":"k1"},"worktrees":[]}"""),
        )
        assertThat(listing.source.repoKey).isEqualTo("k1")
        assertThat(listing.worktrees).isEmpty()
    }

    @Test
    fun `a missing source or worktrees array is invalid`() {
        for (bad in listOf(
            """{"worktrees":[]}""",
            """{"source":{}}""",
            """{"source":{},"worktrees":"nope"}""",
            """null""",
        )) {
            try {
                parseWorktreeListing(
                    if (bad == "null") null else json(bad),
                )
                org.junit.Assert.fail("expected CommandException for $bad")
            } catch (expected: lerdr.core.transport.CommandException) {
                assertThat(expected.message)
                    .isEqualTo("Relay returned an invalid worktree listing")
            }
        }
    }

    @Test
    fun `bare and prunable rows are not openable`() {
        val bare = WorktreeEntry(path = "/p", isBare = true)
        val prunable = WorktreeEntry(path = "/p", isPrunable = true)
        val open = WorktreeEntry(path = "/p", openWorkspaceId = "w9")
        assertThat(bare.openable).isFalse()
        assertThat(prunable.openable).isFalse()
        assertThat(open.openable).isFalse()
    }

    // ── listing over the wire ─────────────────────────────────────────

    @Test
    fun `opening the sheet sends worktree_list with the workspace id`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        h.pump()

        val sent = h.lastRequest("worktree_list")
        assertThat(sent["workspace_id"]?.jsonPrimitive?.content).isEqualTo("w1")
        // A bare request — no agent target fields on a workspace command.
        assertThat(sent["pane_id"]).isNull()
        assertThat(sent["target"]).isNull()

        h.answerOk(
            "worktree_list",
            """{"source":{"repo_key":"k1","repo_name":"lerdr","repo_root":"/home/u/lerdr"},"worktrees":[{"path":"/home/u/worktrees/fix","branch":"fix/one","is_linked_worktree":true,"label":"fix/one","open_workspace_id":"w1"}]}""",
        )

        val state = vm.uiState.value
        assertThat(state.loading).isFalse()
        assertThat(state.error).isNull()
        assertThat(state.listing?.worktrees).hasSize(1)
        assertThat(state.listing?.worktrees?.single()?.openWorkspaceId).isEqualTo("w1")
        assertThat(state.workspaceLabel).isEqualTo("lerdr")
        assertThat(state.workspacePath).isEqualTo("/home/u/worktrees/fix")
        assertThat(state.linkedWorktree).isTrue()
        assertThat(state.managementAvailable).isTrue()
    }

    @Test
    fun `a malformed listing payload surfaces the validation error`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        h.pump()
        h.answerOk("worktree_list", """{"source":{}}""")

        val state = vm.uiState.value
        assertThat(state.loading).isFalse()
        assertThat(state.listing).isNull()
        assertThat(state.error).isEqualTo("Relay returned an invalid worktree listing")
    }

    @Test
    fun `a failed list lands as an inline error`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        h.pump()
        h.answerFailed("worktree_list", "Herdr is unreachable")

        val state = vm.uiState.value
        assertThat(state.loading).isFalse()
        assertThat(state.error).isEqualTo("Herdr is unreachable")
    }

    @Test
    fun `without the worktree_management capability nothing is sent`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady(capabilities = "\"pane_realtime_delta\"")
        val vm = h.viewModel()
        h.pump()

        assertThat(h.sentOf("worktree_list")).isEmpty()
        val state = vm.uiState.value
        assertThat(state.loading).isFalse()
        assertThat(state.error).isEqualTo("This relay does not support worktree management")
        assertThat(state.managementAvailable).isFalse()
    }

    @Test
    fun `a starting inventory gates the listing before send`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady(inventoryState = "starting")
        val vm = h.viewModel()
        h.pump()

        assertThat(h.sentOf("worktree_list")).isEmpty()
        assertThat(vm.uiState.value.error)
            .isEqualTo("Herdr agent inventory is not ready on this computer")
    }

    // ── create / open ─────────────────────────────────────────────────

    @Test
    fun `create sends Lerdr's fields then refreshes`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        h.pump()
        h.answerOk(
            "worktree_list",
            """{"source":{"repo_key":"k1"},"worktrees":[]}""",
        )

        vm.onBranchDraftChange("  fix/issue-14  ")
        vm.onBaseDraftChange(" main ")
        vm.onLabelDraftChange(" issue 14 ")
        vm.createWorktree()
        h.pump()

        val sent = h.lastRequest("worktree_create")
        assertThat(sent["workspace_id"]?.jsonPrimitive?.content).isEqualTo("w1")
        assertThat(sent["branch"]?.jsonPrimitive?.content).isEqualTo("fix/issue-14")
        assertThat(sent["base"]?.jsonPrimitive?.content).isEqualTo("main")
        assertThat(sent["label"]?.jsonPrimitive?.content).isEqualTo("issue 14")

        h.answerOk("worktree_create")
        h.pump()

        val state = vm.uiState.value
        assertThat(state.busy).isFalse()
        assertThat(state.status).isEqualTo("Created worktree fix/issue-14.")
        assertThat(state.statusError).isFalse()
        // Drafts clear and Lerdr's requestAgents() fans out.
        assertThat(state.branchDraft).isEmpty()
        assertThat(h.sentOf("refresh_agents")).isNotEmpty()
        // The listing reloads after a mutation.
        assertThat(h.sentOf("worktree_list")).hasSize(2)
    }

    @Test
    fun `create without a branch sends nothing`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        h.pump()
        h.answerOk("worktree_list", """{"source":{},"worktrees":[]}""")

        vm.onBranchDraftChange("   ")
        vm.createWorktree()
        h.pump()

        assertThat(h.sentOf("worktree_create")).isEmpty()
    }

    @Test
    fun `open sends path only and refreshes`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        h.pump()
        h.answerOk("worktree_list", """{"source":{},"worktrees":[]}""")

        vm.openWorktree("/home/u/worktrees/review", "review")
        h.pump()

        val sent = h.lastRequest("worktree_open")
        assertThat(sent["workspace_id"]?.jsonPrimitive?.content).isEqualTo("w1")
        assertThat(sent["path"]?.jsonPrimitive?.content)
            .isEqualTo("/home/u/worktrees/review")
        // Exactly one of path/branch — branch stays absent.
        assertThat(sent["branch"]).isNull()
        assertThat(sent["label"]).isNull()

        h.answerOk("worktree_open")
        h.pump()

        assertThat(vm.uiState.value.status).isEqualTo("Opened worktree review.")
        assertThat(h.sentOf("refresh_agents")).isNotEmpty()
        assertThat(h.sentOf("worktree_list")).hasSize(2)
    }

    // ── remove ────────────────────────────────────────────────────────

    @Test
    fun `remove confirmation sends workspace_id and force`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        h.pump()
        h.answerOk("worktree_list", """{"source":{},"worktrees":[]}""")

        vm.requestRemove()
        h.pump()
        assertThat(vm.uiState.value.confirmRemove).isTrue()
        assertThat(vm.uiState.value.confirmForce).isFalse()

        vm.confirmRemove()
        h.pump()

        val sent = h.lastRequest("worktree_remove")
        assertThat(sent["workspace_id"]?.jsonPrimitive?.content).isEqualTo("w1")
        // `force` is omitempty — a non-forced remove leaves it absent.
        assertThat(sent["force"]).isNull()
        // The VM assigns an action_id so the receipt correlates.
        assertThat(sent["action_id"]?.jsonPrimitive?.content).isNotEmpty()

        h.answerOk("worktree_remove")
        h.pump()

        val state = vm.uiState.value
        assertThat(state.confirmRemove).isFalse()
        assertThat(state.status).isEqualTo("Removed worktree lerdr.")
        assertThat(state.shouldDismiss).isTrue()
        assertThat(h.sentOf("refresh_agents")).isNotEmpty()
    }

    @Test
    fun `a dirty refusal keeps the dialog open in force mode`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        h.pump()
        h.answerOk("worktree_list", """{"source":{},"worktrees":[]}""")

        vm.requestRemove()
        vm.confirmRemove()
        h.pump()

        val requestId = h.lastRequest("worktree_remove")["request_id"]!!.jsonPrimitive.content
        val actionId = h.lastRequest("worktree_remove")["action_id"]!!.jsonPrimitive.content
        // The relay's real refusal pair: failed command_result then receipt.
        h.handle().emit(
            json(
                """{"type":"command_result","request_id":"$requestId","action":"worktree_remove","ok":false,"phase":"not_started","error":"Worktree has uncommitted changes; force removal is required","data":{"code":"dirty_worktree_requires_force","force_available":true}}""",
            ),
        )
        h.handle().emit(
            json(
                """{"type":"action_receipt","request_id":"$requestId","receipt":{"action_id":"$actionId","phase":"confirmed","error":{"code":"dirty_worktree_requires_force"}}}""",
            ),
        )
        h.pump()

        val state = vm.uiState.value
        assertThat(state.busy).isFalse()
        assertThat(state.confirmRemove).isTrue()
        assertThat(state.confirmForce).isTrue()
        assertThat(state.status).isNull()

        // Confirming again sends force:true.
        vm.confirmRemove()
        h.pump()
        val retry = h.lastRequest("worktree_remove")
        assertThat(retry["force"]?.jsonPrimitive?.content).isEqualTo("true")
    }

    @Test
    fun `a dirty refusal via receipt alone escalates to force`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        h.pump()
        h.answerOk("worktree_list", """{"source":{},"worktrees":[]}""")

        vm.requestRemove()
        vm.confirmRemove()
        h.pump()

        val requestId = h.lastRequest("worktree_remove")["request_id"]!!.jsonPrimitive.content
        val actionId = h.lastRequest("worktree_remove")["action_id"]!!.jsonPrimitive.content
        // Result fails without the data payload; the receipt's error code
        // still carries the force escape (the relay emits both frames).
        h.handle().emit(
            json(
                """{"type":"command_result","request_id":"$requestId","action":"worktree_remove","ok":false,"phase":"not_started","error":"Worktree has uncommitted changes; force removal is required"}""",
            ),
        )
        h.handle().emit(
            json(
                """{"type":"action_receipt","request_id":"$requestId","receipt":{"action_id":"$actionId","phase":"confirmed","error":{"code":"dirty_worktree_requires_force"}}}""",
            ),
        )
        h.pump()

        assertThat(vm.uiState.value.confirmRemove).isTrue()
        assertThat(vm.uiState.value.confirmForce).isTrue()
    }

    @Test
    fun `a plain remove failure closes the dialog with a status error`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        h.pump()
        h.answerOk("worktree_list", """{"source":{},"worktrees":[]}""")

        vm.requestRemove()
        vm.confirmRemove()
        h.pump()

        h.answerFailed("worktree_remove", "Workspace is not a removable linked worktree")
        // No receipt arrives in this test — the watch falls out through the
        // receipt grace window.
        advanceUntilIdle()

        val state = vm.uiState.value
        assertThat(state.confirmRemove).isFalse()
        assertThat(state.statusError).isTrue()
        assertThat(state.status)
            .isEqualTo("Workspace is not a removable linked worktree")
    }

    @Test
    fun `remove is only offered for a linked worktree`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady(linkedWorktree = false)
        val vm = h.viewModel()
        h.pump()

        assertThat(vm.uiState.value.linkedWorktree).isFalse()
        vm.requestRemove()
        assertThat(vm.uiState.value.confirmRemove).isFalse()
    }

    // ── refresh / staleness ───────────────────────────────────────────

    @Test
    fun `a dispatched_unknown create refreshes the listing`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        h.pump()
        h.answerOk("worktree_list", """{"source":{},"worktrees":[]}""")

        vm.onBranchDraftChange("fix/issue-14")
        vm.createWorktree()
        h.pump()
        h.answerFailed(
            "worktree_create",
            "Relay confirmation timed out",
            phase = "dispatched_unknown",
        )
        h.pump()

        val state = vm.uiState.value
        assertThat(state.statusError).isTrue()
        assertThat(state.status)
            .isEqualTo("Relay confirmation timed out Check the worktree list before retrying.")
        assertThat(h.sentOf("worktree_list")).hasSize(2)
    }

    @Test
    fun `a stale listing cannot overwrite a newer load`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        h.pump()

        // The first load stays unanswered; refresh issues a second.
        val first = h.lastRequest("worktree_list")["request_id"]!!.jsonPrimitive.content
        vm.refresh()
        h.pump()
        val second = h.lastRequest("worktree_list")["request_id"]!!.jsonPrimitive.content
        h.answerOk(
            "worktree_list",
            """{"source":{"repo_key":"k1"},"worktrees":[]}""",
        )

        // The stale reply for the superseded request is dropped.
        h.handle().emit(
            json(
                """{"type":"command_result","request_id":"$first","action":"worktree_list","ok":true,"phase":"completed","data":{"source":{"repo_key":"stale"},"worktrees":[{"path":"/stale","branch":"stale"}]}}""",
            ),
        )
        h.pump()

        val state = vm.uiState.value
        assertThat(state.loading).isFalse()
        assertThat(state.listing?.worktrees).isEmpty()
        assertThat(state.listing?.source?.repoKey).isEqualTo("k1")
        assertThat(second).isNotEqualTo(first)
    }

    @Test
    fun `the workspace leaving the store dismisses the sheet`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        h.pump()
        assertThat(vm.uiState.value.shouldDismiss).isFalse()

        h.handle().emit(json("""{"type":"workspaces","workspaces":[]}"""))
        h.pump()

        assertThat(vm.uiState.value.shouldDismiss).isTrue()
    }
}
