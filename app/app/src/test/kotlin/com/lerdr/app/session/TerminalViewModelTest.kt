package com.lerdr.app.session

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import app.cash.turbine.test
import com.google.common.truth.Truth.assertThat
import com.lerdr.app.ui.terminal.TerminalCursorUi
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
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
        val viewModel = TerminalViewModel(h.paneId, h.repository, backgroundScope)
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
    fun `metadata-only delta reuses the parsed row list`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val viewModel = TerminalViewModel(h.paneId, h.repository, backgroundScope)
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
    fun `send hooks emit send_text, send_keys and send_input frames`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val viewModel = TerminalViewModel(h.paneId, h.repository, backgroundScope)
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.pump()

        viewModel.sendLiteralText("ls -la")
        h.pump()
        val literal = sentFrames(h.handle()).last {
            it["type"]?.jsonPrimitive?.content == "send_text"
        }
        assertThat(literal["text"]?.jsonPrimitive?.content).isEqualTo("ls -la")
        // send_text carries no keys — the pane injects the bytes verbatim.
        assertThat("keys" !in literal).isTrue()
        h.resolveRequest("send_text")

        viewModel.sendKeys(listOf("Ctrl+C"))
        h.pump()
        val chord = sentFrames(h.handle()).last {
            it["type"]?.jsonPrimitive?.content == "send_keys"
        }
        assertThat(chord["keys"].toString()).contains("Ctrl+C")
        h.resolveRequest("send_keys")

        viewModel.sendText("cargo test")
        h.pump()
        val submitted = sentFrames(h.handle()).last {
            it["type"]?.jsonPrimitive?.content == "send_input"
        }
        assertThat(submitted["text"]?.jsonPrimitive?.content).isEqualTo("cargo test")
        assertThat(submitted["keys"].toString()).contains("Enter")
        h.resolveRequest("send_input")
    }

    @Test
    fun `a failed send surfaces as lastError`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val viewModel = TerminalViewModel(h.paneId, h.repository, backgroundScope)
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.pump()

        viewModel.sendLiteralText("x")
        h.pump()
        h.resolveRequest("send_text", ok = false, error = "dispatch failed")

        assertThat(viewModel.uiState.value.lastError).isEqualTo("dispatch failed")
    }

    @Test
    fun `viewport measurement negotiates the pane-size lease`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
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
        val viewModel = TerminalViewModel(h.paneId, h.repository, backgroundScope)
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.pump()
        h.emitPaneContent("prompt$ ")

        viewModel.onViewportMeasured(columns = 92, rows = 42)
        h.pump()

        val lease = h.handle().requests.single { it.type == "lease_pane_size" }
        assertThat(lease.columns).isEqualTo(92)
        assertThat(lease.rows).isEqualTo(42)

        // The resize committed a snapshot — the chip reads the lease grid.
        val state = viewModel.uiState.value
        assertThat(state.leaseColumns).isEqualTo(92)
        assertThat(state.leaseRows).isEqualTo(42)
        assertThat(state.statusLabel).isEqualTo("lease 92×42")
    }

    @Test
    fun `state before the first frame keeps waitingForContent`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val viewModel = TerminalViewModel(h.paneId, h.repository, backgroundScope)
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
        val viewModel = TerminalViewModel(h.paneId, h.repository, backgroundScope)
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
}
