package com.lerdr.app.session

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import androidx.lifecycle.ViewModelStore
import app.cash.turbine.test
import com.google.common.truth.Truth.assertThat
import com.lerdr.app.ui.terminal.TerminalCursorUi
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import lerdr.core.data.DeviceRole
import lerdr.core.data.RelayDeviceCredential
import lerdr.core.data.RelayEndpoint
import lerdr.core.data.RelayRegistry
import lerdr.core.protocol.LerdrJson
import lerdr.core.store.AgentStore
import lerdr.core.store.ConnectionStore
import lerdr.core.store.WorkspaceStore
import lerdr.core.store.clientPaneId
import lerdr.core.terminal.FingerprintChain
import org.junit.After
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

private fun json(raw: String): JsonObject =
    LerdrJson.parseToJsonElement(raw) as JsonObject

private fun sentFrames(h: FakeRelaySessionHandle): List<JsonObject> =
    h.sentRaw.map { json(it) }

@OptIn(ExperimentalCoroutinesApi::class)
class TerminalViewModelTest {

    @get:Rule
    val tmp = TemporaryFolder()

    private val mainDispatcher = UnconfinedTestDispatcher()

    /** viewModelScope rides Dispatchers.Main — redirect it into the test. */
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
        val preferences = com.lerdr.app.settings.AppPreferences(dataStore)
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
        val paneId = clientPaneId("r1", "%1")

        fun pump() = testScope.runCurrent()

        fun handle(): FakeRelaySessionHandle =
            factory.handleFor(origin) ?: error("no session for $origin")

        /**
         * Connected session + agent row, with the row-leasing capability so
         * [TerminalViewModel.onViewportMeasured] negotiates both dimensions.
         */
        suspend fun connectReady() {
            repository.connect(endpoint)
            handle().connect()
            handle().emit(
                json(
                    """{"type":"push_config","capabilities":["pane_realtime_delta","pane_size_lease","pane_size_lease_rows","attention_classification"],"inventory":{"state":"ready"}}""",
                ),
            )
            handle().emit(
                json(
                    """{"type":"agents","agents":[{"pane_id":"%1","raw_pane_id":"%1","terminal_id":"t1","server_session_id":"ss1","generation":3,"agent":"claude","name":"claude","status":"working","cwd":"/home/u/lerdr","project":"lerdr","workspace_id":"w1","updated_at":100}]}""",
                ),
            )
            pump()
        }

        /** `agents` upsert — flips the pane's provider mid-session. */
        suspend fun emitAgent(agent: String, status: String = "working") {
            handle().emit(
                json(
                    """{"type":"agents","agents":[{"pane_id":"%1","raw_pane_id":"%1","terminal_id":"t1","server_session_id":"ss1","generation":3,"agent":"$agent","name":"$agent","status":"$status","cwd":"/home/u/lerdr","project":"lerdr","workspace_id":"w1","updated_at":100}]}""",
                ),
            )
            pump()
        }

        /**
         * `workspaces` snapshot — a repo-root workspace plus a linked
         * worktree child sharing its `repo_root`, the shape Herdr reports
         * for an orchestrated cohort.
         */
        suspend fun emitWorktreeCohort() {
            handle().emit(
                json(
                    """{"type":"workspaces","workspaces":[{"workspace_id":"w1","label":"app","worktree":{"repo_root":"/repo/app","checkout_path":"/repo/app","is_linked_worktree":false}},{"workspace_id":"w9","label":"app-fix","worktree":{"repo_root":"/repo/app","checkout_path":"/repo/app-wt/fix","is_linked_worktree":true}}]}""",
                ),
            )
            pump()
        }

        /** `agents` snapshot — the orchestrator plus a busy cohort child. */
        suspend fun emitOrchestratorCohort() {
            handle().emit(
                json(
                    """{"type":"agents","agents":[{"pane_id":"%1","raw_pane_id":"%1","agent":"omp","name":"omp","status":"idle","workspace_id":"w1","updated_at":100},{"pane_id":"%9","raw_pane_id":"%9","agent":"omp","name":"child","status":"working","workspace_id":"w9","agent_session_id":"s-9","updated_at":100}]}""",
                ),
            )
            pump()
        }

        suspend fun emitPaneContent(content: String, fingerprint: String = "fp-1") {
            handle().emit(
                buildJsonObject {
                    put("type", "pane_content")
                    put("pane_id", "%1")
                    put("content", content)
                    put("content_fingerprint", fingerprint)
                    put("format", "ansi")
                },
            )
            pump()
        }

        /** Resolve the latest raw request frame by wire `type`. */
        suspend fun resolveRequest(type: String, ok: Boolean = true, error: String? = null) {
            val sent = sentFrames(handle()).last { it["type"]?.jsonPrimitive?.content == type }
            val requestId = sent["request_id"]!!.jsonPrimitive.content
            val result = buildJsonObject {
                put("type", "command_result")
                put("request_id", requestId)
                put("action", type)
                put("ok", ok)
                put("phase", if (ok) "completed" else "failed")
                if (error != null) put("error", error)
            }
            handle().emit(result)
            pump()
        }
    }

    @Test
    fun `pane content commits to styled rows and a write cursor`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val viewModel = TerminalViewModel(h.paneId, h.repository, backgroundScope, h.preferences)
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.pump()

        // The watch went out on init — the runtime exists before frames land.
        assertThat(
            sentFrames(h.handle()).map { it["type"]?.jsonPrimitive?.content },
        ).contains("read_pane")

        h.emitPaneContent("\u001b[31mred\u001b[0m plain\n$ ")

        val state = viewModel.uiState.value
        assertThat(state.waitingForContent).isFalse()
        assertThat(state.lines).hasSize(2)
        assertThat(state.rows).hasSize(2)
        with(state.rows[0]) {
            assertThat(spans).hasSize(2)
            assertThat(spans[0].text).isEqualTo("red")
            assertThat(spans[0].fg).isEqualTo(
                androidx.compose.ui.graphics.Color(0xFFFF5F5F),
            )
            assertThat(spans[1].text).isEqualTo(" plain")
            assertThat(spans[1].fg).isNull()
            assertThat(cells).isEqualTo(9)
        }
        // The write cursor rides the last row, one cell past its content.
        assertThat(state.cursor).isEqualTo(TerminalCursorUi(row = 1, column = 2))
        // No lease yet — the chip falls back to the agent status.
        assertThat(state.statusLabel).isEqualTo("working")
        assertThat(state.connected).isTrue()
    }

    @Test
    fun `resize_settling frames hold the last settled display`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val viewModel = TerminalViewModel(h.paneId, h.repository, backgroundScope, h.preferences)
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.pump()

        h.emitPaneContent("settled\n$ ")
        val settled = viewModel.uiState.value
        assertThat(settled.lines).containsExactly("settled", "$ ").inOrder()

        // A settling frame still commits to the surface — the delta chain
        // and `pane_applied` acks must not skip — but the display keeps
        // the last settled content instead of the mid-repaint mix.
        h.handle().emit(
            json(
                """{"type":"pane_content","pane_id":"%1","content":"mid repaint\n$ ","content_fingerprint":"fp-2","format":"ansi","resize_settling":true}""",
            ),
        )
        h.pump()

        val held = viewModel.uiState.value
        assertThat(held.lines).containsExactly("settled", "$ ").inOrder()
        assertThat(held.revision).isEqualTo(settled.revision)

        // Once the flag clears, the newest frame lands.
        h.emitPaneContent("settled again\n$ ", fingerprint = "fp-3")
        val cleared = viewModel.uiState.value
        assertThat(cleared.lines).containsExactly("settled again", "$ ").inOrder()
        assertThat(cleared.revision).isGreaterThan(held.revision)
    }

    @Test
    fun `a settling first frame still renders`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val viewModel = TerminalViewModel(h.paneId, h.repository, backgroundScope, h.preferences)
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.pump()

        // No settled frame exists yet — a lease resize landing before the
        // first read still shows the settling content rather than a blank.
        h.handle().emit(
            json(
                """{"type":"pane_content","pane_id":"%1","content":"first\n$ ","content_fingerprint":"fp-1","format":"ansi","resize_settling":true}""",
            ),
        )
        h.pump()

        val state = viewModel.uiState.value
        assertThat(state.waitingForContent).isFalse()
        assertThat(state.lines).containsExactly("first", "$ ").inOrder()
    }

    @Test
    fun `metadata-only delta reuses the parsed row list`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val viewModel = TerminalViewModel(h.paneId, h.repository, backgroundScope, h.preferences)
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.pump()
        h.emitPaneContent("hello pane\n$ ")

        val before = viewModel.uiState.value
        h.handle().emit(
            json(
                """{"type":"pane_delta","pane_id":"%1","base_fingerprint":"fp-1","content_fingerprint":"fp-1","segments":null,"truncated":true}""",
            ),
        )
        h.pump()

        val after = viewModel.uiState.value
        assertThat(after.revision).isEqualTo(before.revision + 1)
        assertThat(after.truncated).isTrue()
        // Content unchanged → the same row instance survives (Compose skips
        // re-measuring what the delta did not touch).
        assertThat(after.rows).isSameInstanceAs(before.rows)
    }

    @Test
    fun `text delivery waits for completed acknowledgement and reports rejection`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectController()
        val viewModel = TerminalViewModel(h.paneId, h.repository, backgroundScope, h.preferences)
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.pump()

        val submitted = backgroundScope.async { viewModel.sendText("cargo test") }
        h.pump()
        assertThat(submitted.isCompleted).isFalse()
        val request = sentFrames(h.handle()).last { it["type"]?.jsonPrimitive?.content == "send_input" }
        h.handle().emit(buildJsonObject {
            put("type", "command_result")
            put("request_id", request["request_id"]!!.jsonPrimitive.content)
            put("action", "send_input")
            put("ok", true)
            put("phase", "accepted")
        })
        h.pump()
        assertThat(submitted.isCompleted).isFalse()
        h.resolveRequest("send_input")
        assertThat(submitted.await()).isTrue()

        val rejected = backgroundScope.async { viewModel.sendText("next command") }
        h.pump()
        h.resolveRequest("send_input", ok = false, error = "dispatch failed")
        assertThat(rejected.await()).isFalse()
        assertThat(viewModel.uiState.value.lastError).isNotNull()
    }

    @Test
    fun `background returns viewport to desktop until foreground resumes`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectController()
        val desktopGeometry = 160 to 50
        val phoneGeometry = 84 to 31
        var remoteGeometry = desktopGeometry
        h.handle().responder = { message ->
            when (message.type) {
                "lease_pane_size" -> remoteGeometry = phoneGeometry
                "release_pane_size" -> remoteGeometry = desktopGeometry
            }
            lerdr.core.model.CommandResultMessage(
                action = message.type,
                ok = true,
                phase = lerdr.core.model.CommandResultMessage.PHASE_COMPLETED,
                data = buildJsonObject {
                    put("columns", remoteGeometry.first)
                    put("rows", remoteGeometry.second)
                },
            )
        }
        val viewModel = TerminalViewModel(h.paneId, h.repository, backgroundScope, h.preferences)
        viewModel.onViewportMeasured(92, 42)
        h.pump()
        assertThat(remoteGeometry).isEqualTo(phoneGeometry)

        h.repository.setHidden(true)
        h.pump()
        assertThat(remoteGeometry).isEqualTo(desktopGeometry)
        viewModel.onViewportMeasured(110, 51)
        h.pump()
        assertThat(remoteGeometry).isEqualTo(desktopGeometry)
        advanceTimeBy(20_000)
        h.pump()
        assertThat(remoteGeometry).isEqualTo(desktopGeometry)

        h.repository.setHidden(false)
        h.pump()
        assertThat(remoteGeometry).isEqualTo(phoneGeometry)
    }

    @Test
    fun `renewal restores requested phone geometry after a smaller peer leaves`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectController()
        var smallerPeerPresent = true
        var remoteGeometry = 160 to 50
        h.handle().responder = { message ->
            if (message.type == "lease_pane_size") {
                remoteGeometry = if (smallerPeerPresent) 40 to 10
                    else message.columns to message.rows
            }
            lerdr.core.model.CommandResultMessage(
                action = message.type,
                ok = true,
                phase = lerdr.core.model.CommandResultMessage.PHASE_COMPLETED,
                requestId = message.requestId,
                data = buildJsonObject {
                    put("columns", remoteGeometry.first)
                    put("rows", remoteGeometry.second)
                },
            )
        }
        val viewModel = TerminalViewModel(h.paneId, h.repository, backgroundScope, h.preferences)
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.pump()
        viewModel.onViewportMeasured(columns = 57, rows = 35)
        h.pump()
        assertThat(remoteGeometry).isEqualTo(40 to 10)
        smallerPeerPresent = false
        advanceTimeBy(10_000)
        h.pump()
        assertThat(remoteGeometry).isEqualTo(57 to 35)
    }

    @Test
    fun `viewport measurement negotiates the pane-size lease`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectController()
        h.handle().responder = { message ->
            lerdr.core.model.CommandResultMessage(
                action = message.type,
                ok = true,
                phase = lerdr.core.model.CommandResultMessage.PHASE_COMPLETED,
                requestId = message.requestId,
                data = buildJsonObject {
                    put("columns", message.columns)
                    put("rows", message.rows)
                },
            )
        }
        val viewModel = TerminalViewModel(h.paneId, h.repository, backgroundScope, h.preferences)
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.pump()
        h.emitPaneContent("prompt$ ")

        viewModel.onViewportMeasured(columns = 92, rows = 42)
        h.pump()

        val lease = h.handle().requests.single { it.type == "lease_pane_size" }
        assertThat(lease.columns).isEqualTo(92)
        assertThat(lease.rows).isEqualTo(42)

        // Geometry remains available without replacing the live lifecycle.
        val state = viewModel.uiState.value
        assertThat(state.leaseColumns).isEqualTo(92)
        assertThat(state.leaseRows).isEqualTo(42)
        assertThat(state.statusLabel).isEqualTo("working")

        h.emitAgent("claude", status = "idle")
        assertThat(viewModel.uiState.value.statusLabel).isEqualTo("idle")
        h.emitAgent("claude", status = "blocked")
        assertThat(viewModel.uiState.value.statusLabel).isEqualTo("blocked")
        assertThat(viewModel.uiState.value.leaseColumns).isEqualTo(92)
        assertThat(viewModel.uiState.value.leaseRows).isEqualTo(42)
    }

    @Test
    fun `queued old teardown preserves reopened terminal lease and content until final close`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectController()
        h.handle().responder = { message ->
            lerdr.core.model.CommandResultMessage(
                action = message.type,
                ok = true,
                phase = lerdr.core.model.CommandResultMessage.PHASE_COMPLETED,
                data = buildJsonObject {
                    put("columns", message.columns)
                    put("rows", message.rows)
                },
            )
        }
        val old = TerminalViewModel(h.paneId, h.repository, backgroundScope, h.preferences)
        val oldStore = ViewModelStore().apply { put("terminal", old) }
        h.emitPaneContent("live content")
        old.onViewportMeasured(92, 42)
        h.pump()
        oldStore.clear() // App-scope cleanup is queued, not yet run.
        val successor = TerminalViewModel(h.paneId, h.repository, backgroundScope, h.preferences)
        val successorStore = ViewModelStore().apply { put("terminal", successor) }
        backgroundScope.launch { successor.uiState.collect { } }
        successor.onViewportMeasured(84, 35)
        h.pump()
        assertThat(h.handle().requests.map { it.type }).doesNotContain("release_pane_size")
        assertThat(sentFrames(h.handle()).map { it["type"]?.jsonPrimitive?.content })
            .doesNotContain("unwatch_pane")
        assertThat(successor.uiState.value.lines).containsExactly("live content")
        assertThat(successor.uiState.value.leaseColumns).isEqualTo(84)
        assertThat(successor.uiState.value.leaseRows).isEqualTo(35)
        successorStore.clear()
        h.pump()
        assertThat(h.handle().requests.count { it.type == "release_pane_size" }).isEqualTo(1)
        assertThat(sentFrames(h.handle()).count { it["type"]?.jsonPrimitive?.content == "unwatch_pane" })
            .isEqualTo(1)
        assertThat(h.repository.paneSnapshot(h.paneId).first()).isNull()
    }

    @Test
    fun `close releases lease dispatched before its acquire receipt arrives`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectController()
        val acquire = CompletableDeferred<Unit>()
        h.handle().responder = { message ->
            if (message.type == "lease_pane_size") acquire.await()
            lerdr.core.model.CommandResultMessage(
                action = message.type,
                ok = true,
                phase = lerdr.core.model.CommandResultMessage.PHASE_COMPLETED,
            )
        }
        val viewModel = TerminalViewModel(h.paneId, h.repository, backgroundScope, h.preferences)
        val store = ViewModelStore().apply { put("terminal", viewModel) }
        h.emitPaneContent("leased pane")
        viewModel.onViewportMeasured(92, 42)
        h.pump()
        store.clear()
        h.pump()
        assertThat(h.handle().requests.map { it.type })
            .containsExactly("lease_pane_size", "release_pane_size").inOrder()
        assertThat(h.repository.paneSnapshot(h.paneId).first()).isNull()
        assertThat(sentFrames(h.handle()).map { it["type"]?.jsonPrimitive?.content })
            .contains("unwatch_pane")
    }

    @Test
    fun `pending old release completes before successor size acquire`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectController()
        val release = CompletableDeferred<Unit>()
        h.handle().responder = { message ->
            if (message.type == "release_pane_size") release.await()
            lerdr.core.model.CommandResultMessage(
                action = message.type,
                ok = true,
                phase = lerdr.core.model.CommandResultMessage.PHASE_COMPLETED,
                data = buildJsonObject {
                    put("columns", message.columns)
                    put("rows", message.rows)
                },
            )
        }
        val old = TerminalViewModel(h.paneId, h.repository, backgroundScope, h.preferences)
        val oldStore = ViewModelStore().apply { put("terminal", old) }
        h.emitPaneContent("before close")
        old.onViewportMeasured(92, 42)
        h.pump()
        oldStore.clear()
        h.pump()
        val successor = TerminalViewModel(h.paneId, h.repository, backgroundScope, h.preferences)
        val successorStore = ViewModelStore().apply { put("terminal", successor) }
        backgroundScope.launch { successor.uiState.collect { } }
        successor.onViewportMeasured(84, 35)
        h.pump()
        assertThat(h.handle().requests.map { it.type })
            .containsExactly("lease_pane_size", "release_pane_size").inOrder()
        release.complete(Unit)
        h.pump()
        h.emitPaneContent("after close", fingerprint = "fp-2")
        assertThat(h.handle().requests.map { it.type })
            .containsExactly("lease_pane_size", "release_pane_size", "lease_pane_size").inOrder()
        assertThat(successor.uiState.value.leaseColumns).isEqualTo(84)
        assertThat(successor.uiState.value.leaseRows).isEqualTo(35)
        assertThat(successor.uiState.value.lines).containsExactly("after close")
        successorStore.clear()
        h.pump()
        assertThat(h.handle().requests.count { it.type == "release_pane_size" }).isEqualTo(2)
    }

    @Test
    fun `late target discovery leases the phone grid and provider changes preserve it`() = runTest {
        val h = Harness(this, tmp.root)
        h.registry.upsert(h.endpoint)
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.handle().connect()
        h.handle().emit(json(
            """{"type":"push_config","capabilities":["pane_size_lease","pane_size_lease_rows"],"inventory":{"state":"ready"}}""",
        ))
        h.handle().responder = { message ->
            lerdr.core.model.CommandResultMessage(
                action = message.type,
                ok = true,
                phase = lerdr.core.model.CommandResultMessage.PHASE_COMPLETED,
                requestId = message.requestId,
                data = buildJsonObject {
                    put("columns", 57)
                    put("rows", 36)
                },
            )
        }
        val viewModel = TerminalViewModel(h.paneId, h.repository, backgroundScope, h.preferences)
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.pump()
        viewModel.onViewportMeasured(columns = 57, rows = 36)
        h.pump()
        assertThat(h.handle().requests.map { it.type }).doesNotContain("lease_pane_size")

        h.emitAgent("opencode")
        h.emitPaneContent("phone_prompt")
        assertThat(viewModel.uiState.value.leaseColumns).isEqualTo(57)
        assertThat(viewModel.uiState.value.leaseRows).isEqualTo(36)
        for (provider in listOf("omp", "codex")) {
            h.emitAgent(provider)
            assertThat(viewModel.uiState.value.leaseColumns).isEqualTo(57)
            assertThat(viewModel.uiState.value.leaseRows).isEqualTo(36)
        }
    }


    @Test
    fun `hook-less orchestrator with a busy worktree cohort reads orchestrating`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        h.emitWorktreeCohort()
        h.emitOrchestratorCohort()
        val viewModel = TerminalViewModel(h.paneId, h.repository, backgroundScope, h.preferences)
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.pump()
        h.emitPaneContent("prompt$ ")

        // The chip uses cohort-aware display status while Herdr's wire
        // status stays idle for hook-less panes.
        viewModel.onViewportMeasured(columns = 92, rows = 42)
        h.pump()

        val state = viewModel.uiState.value
        assertThat(state.leaseColumns).isEqualTo(0)
        assertThat(state.statusLabel).isEqualTo("orchestrating")
    }

    @Test
    fun `state before the first frame keeps waitingForContent`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val viewModel = TerminalViewModel(h.paneId, h.repository, backgroundScope, h.preferences)
        viewModel.uiState.test {
            val state = awaitItem()
            assertThat(state.waitingForContent).isTrue()
            assertThat(state.rows).isEmpty()
            assertThat(state.cursor).isNull()
            cancelAndIgnoreRemainingEvents()
        }
    }

    @Test
    fun `delta-applied content reparses into new rows`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val viewModel = TerminalViewModel(h.paneId, h.repository, backgroundScope, h.preferences)
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.pump()
        h.emitPaneContent("one\ntwo\n$ ", fingerprint = "fp-1")

        val nextContent = "one\nTWO\n$ "
        h.handle().emit(
            json(
                """{"type":"pane_delta","pane_id":"%1","base_fingerprint":"fp-1","content_fingerprint":"${FingerprintChain.fingerprint(nextContent)}","segments":[{"copy_lines":1},{"text":"TWO\n"},{"copy_start":2,"copy_lines":1}]}""",
            ),
        )
        h.pump()

        val state = viewModel.uiState.value
        assertThat(state.lines).containsExactly("one", "TWO", "$ ").inOrder()
        assertThat(state.rows[1].spans.single().text).isEqualTo("TWO")
        assertThat(state.cursor).isEqualTo(TerminalCursorUi(row = 2, column = 2))
    }

    @Test
    fun `unproven control cannot dispatch input or resize an observed terminal`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val viewModel = TerminalViewModel(h.paneId, h.repository, backgroundScope, h.preferences)
        assertThat(viewModel.sendText("command")).isFalse()
        assertThat(viewModel.sendSecret("password")).isFalse()
        viewModel.sendKeys(listOf("Enter"))
        viewModel.onViewportMeasured(columns = 57, rows = 12)
        h.pump()
        assertThat(sentFrames(h.handle()).map { it["type"]?.jsonPrimitive?.content })
            .containsNoneOf("send_input", "send_keys", "send_secret")
        assertThat(h.handle().requests.map { it.type }).containsNoneOf("send_secret", "lease_pane_size")
    }

    @Test
    fun `send_secret without the capability fails closed into lastError`() = runTest {
        val h = Harness(this, tmp.root)
        // connectReady's push_config does not advertise `secret_input`.
        h.connectController()
        val viewModel = TerminalViewModel(h.paneId, h.repository, backgroundScope, h.preferences)
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.pump()

        assertThat(viewModel.uiState.value.secretInputSupported).isFalse()

        assertThat(viewModel.sendSecret("hunter2")).isFalse()
        h.pump()

        assertThat(viewModel.uiState.value.lastError).isNotNull()
        // Nothing left the device — no send_secret request was framed.
        assertThat(h.handle().requests.map { it.type }).doesNotContain("send_secret")

        // The snackbar consumes the error once.
        viewModel.dismissError()
        assertThat(viewModel.uiState.value.lastError).isNull()
    }

    @Test
    fun `no_echo frame surfaces the hidden prompt in ui state`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val viewModel = TerminalViewModel(h.paneId, h.repository, backgroundScope, h.preferences)
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.pump()

        h.handle().emit(
            json(
                """{"type":"pane_content","pane_id":"%1","content":"Password:","content_fingerprint":"fp-secret","format":"text","no_echo":true,"no_echo_prompt":"Password:"}""",
            ),
        )
        h.pump()

        val state = viewModel.uiState.value
        assertThat(state.noEcho).isTrue()
        assertThat(state.noEchoPrompt).isEqualTo("Password:")
    }

    @Test
    fun `controller credential enables canControl`() = runTest {
        val h = Harness(this, tmp.root)
        // canControl reads the enrolled role — seed it, then start() so
        // the repository's records collector publishes it before the VM's
        // combine evaluates.
        h.registry.upsert(h.endpoint)
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady()
        val viewModel = TerminalViewModel(h.paneId, h.repository, backgroundScope, h.preferences)
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.pump()

        assertThat(viewModel.uiState.value.canControl).isTrue()
    }

    @Test
    fun `reader credential leaves canControl false`() = runTest {
        val h = Harness(this, tmp.root)
        h.registry.upsert(h.endpoint)
        h.credentials.seed("r1", credential(DeviceRole.READER))
        h.repository.start()
        h.pump()
        h.connectReady()
        val viewModel = TerminalViewModel(h.paneId, h.repository, backgroundScope, h.preferences)
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.pump()

        assertThat(viewModel.uiState.value.canControl).isFalse()
    }

    @Test
    fun `pane_search rides the negotiated capability and returns the scrollback count`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        h.handle().emit(
            json("""{"type":"push_config","capabilities":["pane_search"]}"""),
        )
        h.handle().responder = { message ->
            lerdr.core.model.CommandResultMessage(
                action = message.type,
                ok = true,
                phase = lerdr.core.model.CommandResultMessage.PHASE_COMPLETED,
                requestId = message.requestId,
                data = json(
                    """{"matches":[{"start":{"row":12,"col":4},"end":{"row":12,"col":9}}],"content_revision":42,"total":7,"current":3,"current_global":19}""",
                ),
            )
        }
        val viewModel = TerminalViewModel(h.paneId, h.repository, backgroundScope, h.preferences)
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.pump()

        assertThat(viewModel.uiState.value.paneSearchSupported).isTrue()

        val result = viewModel.paneSearch("panic")

        val request = h.handle().requests.single { it.type == "pane_search" }
        assertThat(request.query).isEqualTo("panic")
        assertThat(request.direction).isEqualTo("forward")
        assertThat(result?.total).isEqualTo(7)
        assertThat(result?.matches).hasSize(1)
    }

    @Test
    fun `pane_search without the capability stays local`() = runTest {
        val h = Harness(this, tmp.root)
        // connectReady's push_config does not advertise `pane_search`.
        h.connectReady()
        val viewModel = TerminalViewModel(h.paneId, h.repository, backgroundScope, h.preferences)
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.pump()

        assertThat(viewModel.uiState.value.paneSearchSupported).isFalse()

        // The bar treats null as "no server count" — nothing is sent.
        assertThat(viewModel.paneSearch("panic")).isNull()
        assertThat(h.handle().requests.map { it.type }).doesNotContain("pane_search")
        assertThat(viewModel.uiState.value.lastError).isNull()
    }

    @Test
    fun `pane_link_resolve reports a hit only when regions exist`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        h.handle().emit(
            json("""{"type":"push_config","capabilities":["pane_links"]}"""),
        )
        val viewModel = TerminalViewModel(h.paneId, h.repository, backgroundScope, h.preferences)
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.pump()

        assertThat(viewModel.uiState.value.paneLinksSupported).isTrue()

        h.handle().responder = { message ->
            lerdr.core.model.CommandResultMessage(
                action = message.type,
                ok = true,
                phase = lerdr.core.model.CommandResultMessage.PHASE_COMPLETED,
                requestId = message.requestId,
                data = json(
                    """{"regions":[{"row":2,"start_col":3,"end_col":44}]}""",
                ),
            )
        }
        assertThat(viewModel.paneLinkRegions(row = 2, col = 30)).isTrue()

        val request = h.handle().requests.single { it.type == "pane_link_resolve" }
        assertThat(request.row).isEqualTo(2)
        assertThat(request.col).isEqualTo(30)

        h.handle().responder = { message ->
            lerdr.core.model.CommandResultMessage(
                action = message.type,
                ok = true,
                phase = lerdr.core.model.CommandResultMessage.PHASE_COMPLETED,
                requestId = message.requestId,
                data = json("""{"regions":[]}"""),
            )
        }
        assertThat(viewModel.paneLinkRegions(row = 0, col = 0)).isFalse()
    }

    @Test
    fun `pane_link_activate returns the handled flag and url`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        h.handle().emit(
            json("""{"type":"push_config","capabilities":["pane_links"]}"""),
        )
        h.handle().responder = { message ->
            lerdr.core.model.CommandResultMessage(
                action = message.type,
                ok = true,
                phase = lerdr.core.model.CommandResultMessage.PHASE_COMPLETED,
                requestId = message.requestId,
                data = json("""{"handled":true,"url":"https://example.com/spec"}"""),
            )
        }
        val viewModel = TerminalViewModel(h.paneId, h.repository, backgroundScope, h.preferences)
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.pump()

        val result = viewModel.activatePaneLink(row = 2, col = 30)

        val request = h.handle().requests.single { it.type == "pane_link_activate" }
        assertThat(request.row).isEqualTo(2)
        assertThat(request.col).isEqualTo(30)
        assertThat(result?.handled).isTrue()
        assertThat(result?.url).isEqualTo("https://example.com/spec")
        assertThat(viewModel.uiState.value.lastError).isNull()
    }

    @Test
    fun `pane_link actions without the capability fail closed`() = runTest {
        val h = Harness(this, tmp.root)
        // connectReady's push_config does not advertise `pane_links`.
        h.connectReady()
        val viewModel = TerminalViewModel(h.paneId, h.repository, backgroundScope, h.preferences)
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.pump()

        assertThat(viewModel.uiState.value.paneLinksSupported).isFalse()

        // Resolve folds to "no link" — the menu item never renders.
        assertThat(viewModel.paneLinkRegions(row = 0, col = 0)).isFalse()
        // Activate surfaces the gate — the snackbar explains the miss.
        assertThat(viewModel.activatePaneLink(row = 0, col = 0)).isNull()
        assertThat(viewModel.uiState.value.lastError)
            .isEqualTo("This relay does not support pane_links")
        assertThat(h.handle().requests.map { it.type })
            .containsNoneOf("pane_link_resolve", "pane_link_activate")
    }

    @Test
    fun `font scale persists through app preferences`() = runTest {
        val h = Harness(this, tmp.root)
        val viewModel = TerminalViewModel(h.paneId, h.repository, backgroundScope, h.preferences)

        viewModel.persistFontScale(1.5f)
        viewModel.persistFontScale(1.8f)

        // The debounce is virtual-time (backgroundScope); the DataStore
        // write it releases then rides the real clock.
        advanceTimeBy(1_000)
        runCurrent()
        await { h.preferences.terminalFontScale.first() == 1.8f }
    }

    /** Real-clock poll — DataStore writes are off the test scheduler. */
    private suspend fun await(condition: suspend () -> Boolean) {
        val deadline = System.nanoTime() + 5_000_000_000L
        while (!condition()) {
            check(System.nanoTime() < deadline) { "timed out waiting" }
            kotlinx.coroutines.delay(25)
        }
    }

    private suspend fun Harness.connectController() {
        registry.upsert(endpoint)
        credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        repository.start()
        pump()
        connectReady()
    }

    private fun credential(role: DeviceRole) = RelayDeviceCredential(
        id = "cred-1",
        version = 1,
        secret = java.util.Base64.getUrlEncoder().withoutPadding()
            .encodeToString(ByteArray(32) { it.toByte() }),
        deviceId = "dev-1",
        role = role,
        locale = "en",
        issuedAtEpochMs = 1_000L,
    )
}
