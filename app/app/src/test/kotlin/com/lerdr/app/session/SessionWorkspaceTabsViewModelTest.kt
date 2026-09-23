package com.lerdr.app.session

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import com.google.common.truth.Truth.assertThat
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.advanceTimeBy
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
import lerdr.core.model.WorkspaceWorktree
import lerdr.core.protocol.LerdrJson
import lerdr.core.store.AgentStore
import lerdr.core.store.ConnectionStore
import lerdr.core.store.RelayWorkspace
import lerdr.core.store.WorkspaceStore
import org.junit.After
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

private fun json(raw: String): JsonObject =
    LerdrJson.parseToJsonElement(raw) as JsonObject

@OptIn(ExperimentalCoroutinesApi::class)
class SessionWorkspaceTabsViewModelTest {

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
        val paneId = "r1::%1"

        fun pump() = testScope.runCurrent()

        /** Runs the refusal-grace timeout out under virtual time. */
        fun advance(ms: Long) {
            testScope.advanceTimeBy(ms)
            testScope.runCurrent()
        }

        fun handle(): FakeRelaySessionHandle =
            factory.handleFor(origin) ?: error("no session for $origin")

        fun sentFrames(): List<JsonObject> = handle().sentRaw.map { json(it) }

        fun sentOf(type: String): List<JsonObject> = sentFrames().filter {
            it["type"]?.jsonPrimitive?.content == type
        }

        fun lastRequest(type: String): JsonObject =
            sentOf(type).lastOrNull() ?: error("no $type frame sent")

        fun credential(role: DeviceRole) = RelayDeviceCredential(
            id = "cred-1",
            version = 1,
            secret = java.util.Base64.getUrlEncoder().withoutPadding()
                .encodeToString(ByteArray(32) { it.toByte() }),
            deviceId = "dev-1",
            role = role,
            locale = "en",
            issuedAtEpochMs = 1_000L,
        )

        /**
         * Connected session: `workspace_management` + `directory_browser` +
         * `tab_reorder` capabilities, ready inventory, two tabbed agents in
         * `w1` and one workspace row.
         */
        suspend fun connectReady(
            role: DeviceRole = DeviceRole.CONTROLLER,
            capabilities: String =
                "\"workspace_management\",\"directory_browser\",\"tab_reorder\"",
            inventoryState: String = "ready",
            workspacesJson: String =
                """[{"workspace_id":"w1","number":1,"label":"lerdr","focused":true,"pane_count":2,"tab_count":2,"active_tab_id":"tabA","agent_status":"working","cwd":"/home/u/lerdr"}]""",
        ) {
            // Registry first — `start()`'s reconcile tears down sessions
            // whose endpoint is not registered.
            registry.upsert(endpoint)
            credentials.seed("r1", credential(role))
            repository.start()
            repository.connect(endpoint)
            handle().connect()
            handle().emit(
                json(
                    """{"type":"push_config","capabilities":[$capabilities],"inventory":{"state":"$inventoryState"}}""",
                ),
            )
            handle().emit(
                json(
                    """{"type":"agents","agents":[{"pane_id":"%1","raw_pane_id":"%1","terminal_id":"t1","server_session_id":"ss1","generation":3,"agent":"claude","name":"main","status":"working","cwd":"/home/u/lerdr","project":"lerdr","workspace_id":"w1","tab_id":"tabA","tab_label":"main","tab_number":1,"updated_at":100},{"pane_id":"%2","raw_pane_id":"%2","terminal_id":"t2","server_session_id":"ss1","generation":3,"agent":"codex","name":"tests","status":"idle","cwd":"/home/u/lerdr","project":"lerdr","workspace_id":"w1","tab_id":"tabB","tab_label":"tests","tab_number":2,"updated_at":100}]}""",
                ),
            )
            handle().emit(
                json("""{"type":"workspaces","workspaces":$workspacesJson}"""),
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

        suspend fun answerFailed(type: String, error: String, dataJson: String? = null) {
            val data = dataJson?.let { ""","data":$it""" }.orEmpty()
            answer(type, """"ok":false,"phase":"not_started","error":"$error"$data""")
        }

        fun viewModel(): WorkspaceTabsViewModel =
            WorkspaceTabsViewModel(paneId, repository, workspaces).also {
                testScope.backgroundScope.launch { it.uiState.collect { } }
            }
    }

    // ── tabs + menus ──────────────────────────────────────────────────

    @Test
    fun `tabs group per tab_id with the active tab marked`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        h.pump()

        val state = vm.uiState.value
        assertThat(state.tabs.map { it.tabId }).containsExactly("tabA", "tabB")
        assertThat(state.activeTabId).isEqualTo("tabA")
        assertThat(state.workspaceLabel).isEqualTo("lerdr")
        assertThat(state.canControl).isTrue()
        assertThat(state.managementAvailable).isTrue()
        assertThat(state.reorderAvailable).isTrue()
    }

    @Test
    fun `long-press opens the menu for controllers only`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        h.pump()

        vm.openMenu("tabA")
        h.pump()
        assertThat(vm.uiState.value.menuTabId).isEqualTo("tabA")
        vm.dismissMenu()
        h.pump()
        assertThat(vm.uiState.value.menuTabId).isNull()
    }

    @Test
    fun `a reader gets no menu and no mutations`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady(role = DeviceRole.READER)
        val vm = h.viewModel()
        h.pump()

        assertThat(vm.uiState.value.canControl).isFalse()
        vm.openMenu("tabA")
        vm.requestRename()
        vm.requestClose()
        vm.requestCreate()
        vm.moveTab("tabA", 1)
        h.pump()

        val state = vm.uiState.value
        assertThat(state.menuTabId).isNull()
        assertThat(state.renameOpen).isFalse()
        assertThat(state.confirmClose).isFalse()
        assertThat(state.createOpen).isFalse()
        assertThat(h.sentOf("workspace_rename")).isEmpty()
        assertThat(h.sentOf("workspace_close")).isEmpty()
        assertThat(h.sentOf("workspace_create")).isEmpty()
        assertThat(h.sentOf("tab_reorder")).isEmpty()
    }

    // ── rename ────────────────────────────────────────────────────────

    @Test
    fun `rename sends workspace_rename with the trimmed label`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        h.pump()

        vm.requestRename()
        h.pump()
        assertThat(vm.uiState.value.renameOpen).isTrue()
        assertThat(vm.uiState.value.renameDraft).isEqualTo("lerdr")

        vm.onRenameDraftChange("  renamed ws  ")
        vm.confirmRename()
        h.pump()

        val sent = h.lastRequest("workspace_rename")
        assertThat(sent["workspace_id"]?.jsonPrimitive?.content).isEqualTo("w1")
        assertThat(sent["label"]?.jsonPrimitive?.content).isEqualTo("renamed ws")

        h.answerOk("workspace_rename")
        val state = vm.uiState.value
        assertThat(state.renameOpen).isFalse()
        assertThat(state.status).isEqualTo("Renamed workspace renamed ws.")
        assertThat(h.sentOf("refresh_agents")).isNotEmpty()
    }

    @Test
    fun `rename without workspace_management sends nothing`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady(capabilities = "\"tab_reorder\"")
        val vm = h.viewModel()
        h.pump()

        assertThat(vm.uiState.value.managementAvailable).isFalse()
        vm.requestRename()
        h.pump()
        assertThat(vm.uiState.value.renameOpen).isFalse()
        assertThat(h.sentOf("workspace_rename")).isEmpty()
    }

    // ── close ─────────────────────────────────────────────────────────

    @Test
    fun `close sends workspace_close then reports closed`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        h.pump()

        vm.requestClose()
        h.pump()
        assertThat(vm.uiState.value.confirmClose).isTrue()

        vm.confirmClose()
        h.pump()

        val sent = h.lastRequest("workspace_close")
        assertThat(sent["workspace_id"]?.jsonPrimitive?.content).isEqualTo("w1")
        assertThat(sent["close_group"]).isNull()
        // The VM assigns an action_id so the receipt correlates.
        assertThat(sent["action_id"]?.jsonPrimitive?.content).isNotEmpty()

        h.answerOk("workspace_close")
        val state = vm.uiState.value
        assertThat(state.confirmClose).isFalse()
        assertThat(state.status).isEqualTo("Closed workspace lerdr.")
        assertThat(h.sentOf("refresh_agents")).isNotEmpty()
    }

    @Test
    fun `a group-close-required refusal escalates into group mode`() = runTest {
        val h = Harness(this, tmp.root)
        // Primary workspace w1 + linked worktree w2 share repo_key k1.
        h.connectReady(
            workspacesJson = """[{"workspace_id":"w1","number":1,"label":"lerdr","focused":true,"pane_count":2,"tab_count":2,"active_tab_id":"tabA","agent_status":"working","cwd":"/home/u/lerdr","worktree":{"repo_key":"k1","repo_name":"lerdr","repo_root":"/home/u/lerdr","checkout_path":"/home/u/lerdr","is_linked_worktree":false}},{"workspace_id":"w2","number":2,"label":"fix","focused":false,"pane_count":1,"tab_count":1,"active_tab_id":"","agent_status":"idle","cwd":"/home/u/worktrees/fix","worktree":{"repo_key":"k1","repo_name":"lerdr","repo_root":"/home/u/lerdr","checkout_path":"/home/u/worktrees/fix","is_linked_worktree":true}}]""",
        )
        val vm = h.viewModel()
        h.pump()

        vm.requestClose()
        vm.confirmClose()
        h.pump()

        h.answerFailed(
            "workspace_close",
            "Workspace group close requires consent",
            dataJson = """{"code":"workspace_group_close_required","workspace_ids":["w1","w2"]}""",
        )

        val state = vm.uiState.value
        assertThat(state.confirmClose).isTrue()
        assertThat(state.confirmGroup).isTrue()
        assertThat(state.groupMembers).containsExactly("lerdr", "fix").inOrder()
        assertThat(state.status).isEqualTo("Review the workspace group before closing it.")

        // Confirming again sends the group consent payload.
        vm.confirmClose()
        h.pump()
        val retry = h.lastRequest("workspace_close")
        assertThat(retry["close_group"]?.jsonPrimitive?.content).isEqualTo("true")
        val ids = retry["expected_workspace_ids"]
        assertThat(ids.toString()).contains("w1")
        assertThat(ids.toString()).contains("w2")
    }

    @Test
    fun `a group-changed refusal cancels with the review message`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        h.pump()

        vm.requestClose()
        vm.confirmClose()
        h.pump()
        h.answerFailed(
            "workspace_close",
            "Workspace group changed",
            dataJson = """{"code":"workspace_group_changed"}""",
        )

        val state = vm.uiState.value
        assertThat(state.confirmClose).isFalse()
        assertThat(state.status)
            .isEqualTo("Workspace group changed. Review the current group before closing it.")
        assertThat(state.statusError).isTrue()
    }

    @Test
    fun `a plain close failure lands as a status error`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        h.pump()

        vm.requestClose()
        vm.confirmClose()
        h.pump()
        h.answerFailed("workspace_close", "Workspace is unavailable")
        // No group code arrives — the refusal watch lapses after its grace.
        h.advance(WorkspaceTabsViewModel.RECEIPT_GRACE_MS + 1)

        val state = vm.uiState.value
        assertThat(state.confirmClose).isFalse()
        assertThat(state.status).isEqualTo("Workspace is unavailable")
        assertThat(state.statusError).isTrue()
    }

    // ── create ────────────────────────────────────────────────────────

    @Test
    fun `create opens the sheet and primes the directory browser`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        h.pump()

        vm.requestCreate()
        h.pump()

        assertThat(vm.uiState.value.createOpen).isTrue()
        // `directory_browser` advertised — the browser loads immediately.
        h.answerOk(
            "list_directories",
            """{"current":{"path":"/home/u","label":"u"},"parent":"","directories":[{"name":"lerdr","path":"/home/u/lerdr"},{"name":"tmp","path":"/home/u/tmp"}]}""",
        )

        val state = vm.uiState.value
        assertThat(state.createCwd).isEqualTo("/home/u")
        // `pathBase` seeds the untouched label.
        assertThat(state.createLabel).isEqualTo("u")
        assertThat(state.directory?.directories).hasSize(2)

        vm.toggleDirectoryBrowser()
        h.pump()
        assertThat(vm.uiState.value.directoryOpen).isTrue()

        // Descending reseeds cwd; the label stays (already filled).
        vm.loadDirectory("/home/u/lerdr")
        h.pump()
        h.answerOk(
            "list_directories",
            """{"current":{"path":"/home/u/lerdr","label":"lerdr"},"parent":"/home/u","directories":[]}""",
        )
        assertThat(vm.uiState.value.createCwd).isEqualTo("/home/u/lerdr")
        assertThat(vm.uiState.value.createLabel).isEqualTo("u")

        vm.confirmCreate()
        h.pump()
        val sent = h.lastRequest("workspace_create")
        assertThat(sent["cwd"]?.jsonPrimitive?.content).isEqualTo("/home/u/lerdr")
        assertThat(sent["label"]?.jsonPrimitive?.content).isEqualTo("u")

        h.answerOk("workspace_create")
        val done = vm.uiState.value
        assertThat(done.createOpen).isFalse()
        assertThat(done.status).isEqualTo("Created workspace u.")
        assertThat(h.sentOf("refresh_agents")).isNotEmpty()
    }

    @Test
    fun `create without directory_browser uses the typed cwd`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady(capabilities = "\"workspace_management\"")
        val vm = h.viewModel()
        h.pump()

        assertThat(vm.uiState.value.directoryBrowserAvailable).isFalse()
        vm.requestCreate()
        h.pump()

        assertThat(h.sentOf("list_directories")).isEmpty()

        vm.onCreateCwdChange("/tmp/scratch")
        vm.onCreateLabelChange("scratch")
        vm.confirmCreate()
        h.pump()

        val sent = h.lastRequest("workspace_create")
        assertThat(sent["cwd"]?.jsonPrimitive?.content).isEqualTo("/tmp/scratch")
        assertThat(sent["label"]?.jsonPrimitive?.content).isEqualTo("scratch")
    }

    @Test
    fun `create without workspace_management never opens`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady(capabilities = "\"tab_reorder\"")
        val vm = h.viewModel()
        h.pump()

        vm.requestCreate()
        h.pump()
        assertThat(vm.uiState.value.createOpen).isFalse()
        assertThat(h.sentOf("workspace_create")).isEmpty()
    }

    // ── pure helpers ──────────────────────────────────────────────────

    @Test
    fun `workspaceGroupIds orders primary first then children`() {
        val w1 = RelayWorkspace(
            relayId = "r1",
            relayLabel = "workstation",
            workspaceId = "w1",
            number = 1,
            label = "lerdr",
            worktree = WorkspaceWorktree(
                repoKey = "k1",
                isLinkedWorktree = false,
            ),
        )
        val w2 = RelayWorkspace(
            relayId = "r1",
            relayLabel = "workstation",
            workspaceId = "w2",
            number = 2,
            label = "fix",
            worktree = WorkspaceWorktree(repoKey = "k1", isLinkedWorktree = true),
        )
        val w3 = RelayWorkspace(
            relayId = "r1",
            relayLabel = "workstation",
            workspaceId = "w3",
            number = 3,
            label = "other",
        )
        val all = listOf(w1, w2, w3)
        assertThat(workspaceGroupIds(all, "r1", "w1")).containsExactly("w1", "w2").inOrder()
        assertThat(workspaceGroupIds(all, "r1", "w2")).containsExactly("w1", "w2").inOrder()
        assertThat(workspaceGroupIds(all, "r1", "w3")).containsExactly("w3")
        assertThat(workspaceGroupIds(all, "r1", "missing")).containsExactly("missing")
    }

    @Test
    fun `pathBaseOf returns the last path segment`() {
        assertThat(pathBaseOf("/home/u/lerdr")).isEqualTo("lerdr")
        assertThat(pathBaseOf("/home/u/lerdr/")).isEqualTo("lerdr")
        assertThat(pathBaseOf("")).isEqualTo("workspace")
        assertThat(pathBaseOf("C:\\Users\\u\\proj")).isEqualTo("proj")
    }
}
