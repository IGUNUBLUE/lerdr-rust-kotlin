package com.lerdr.app.session

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import com.google.common.truth.Truth.assertThat
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
import lerdr.core.data.RelayEndpoint
import lerdr.core.data.RelayRegistry
import lerdr.core.model.CommandResultMessage
import lerdr.core.protocol.LerdrJson
import lerdr.core.store.AgentStore
import lerdr.core.store.ConnectionStore
import lerdr.core.store.WorkspaceStore
import lerdr.core.store.clientPaneId
import lerdr.core.transport.CommandException
import org.junit.After
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

private fun json(raw: String): JsonObject =
    LerdrJson.parseToJsonElement(raw) as JsonObject

@OptIn(ExperimentalCoroutinesApi::class)
class FilesViewModelTest {

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
         * Connected session + agent row. [capabilities] lets a test drop
         * `workspace_inspection` to exercise the oracle's gate.
         */
        suspend fun connectReady(
            capabilities: String = "\"pane_realtime_delta\",\"workspace_inspection\"",
            cwd: String = "/home/u/lerdr",
        ) {
            repository.connect(endpoint)
            handle().connect()
            handle().emit(
                json(
                    """{"type":"push_config","capabilities":[$capabilities],"inventory":{"state":"ready"}}""",
                ),
            )
            handle().emit(
                json(
                    """{"type":"agents","agents":[{"pane_id":"%1","raw_pane_id":"%1","terminal_id":"t1","server_session_id":"ss1","generation":3,"agent":"claude","name":"claude","status":"working","cwd":"$cwd","project":"lerdr","workspace_id":"w1","updated_at":100}]}""",
                ),
            )
            pump()
        }

        /** Responder that routes the four workspace actions by wire type. */
        fun respondWith(
            tree: String = """{"root":"/home/u/lerdr","entries":[]}""",
            git: String = """{"available":true,"branch":"main","files":[]}""",
            file: (String) -> String = { path ->
                """{"path":"$path","media_type":"text/plain","kind":"text","text":"hello","size":5}"""
            },
            diff: (String) -> String = { path ->
                """{"path":"$path","diff":"@@ -1 +1 @@\n-old\n+new"}"""
            },
            failing: Map<String, String> = emptyMap(),
        ) {
            handle().responder = { message ->
                failing[message.type]?.let { throw CommandException(it) }
                val data: JsonObject? = when (message.type) {
                    "workspace_tree" -> json(tree)
                    "workspace_git_status" -> json(git)
                    "workspace_file" -> json(file(message.path))
                    "workspace_git_diff" -> json(diff(message.path))
                    else -> null
                }
                CommandResultMessage(
                    action = message.type,
                    ok = true,
                    phase = CommandResultMessage.PHASE_COMPLETED,
                    requestId = message.requestId,
                    data = data,
                )
            }
        }

        fun viewModel(): FilesViewModel = FilesViewModel(paneId, repository).also {
            // Keep the WhileSubscribed stateIn live for assertions.
            testScope.backgroundScope.launch { it.uiState.collect { } }
        }

        fun requestTypes(): List<String> =
            handle().requests.map { it.type }
    }

    @Test
    fun `agent row triggers a parallel tree and git load`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        h.respondWith(
            tree = """{"root":"/home/u/lerdr","entries":[{"path":"app","name":"app","kind":"directory"},{"path":"README.md","name":"README.md","kind":"file","size":512}]}""",
            git = """{"available":true,"branch":"main","ahead":2,"behind":1,"files":[{"path":"app/src/f.kt","status":"M "}],"truncated":false}""",
        )
        val vm = h.viewModel()
        h.pump()

        val state = vm.uiState.value
        assertThat(state.loading).isFalse()
        assertThat(state.workspaceError).isNull()
        assertThat(state.tree?.entries).hasSize(2)
        assertThat(state.git?.available).isTrue()
        assertThat(state.git?.branch).isEqualTo("main")
        assertThat(state.git?.ahead).isEqualTo(2)
        assertThat(state.git?.behind).isEqualTo(1)
        assertThat(state.git?.files?.single()?.path).isEqualTo("app/src/f.kt")
        assertThat(h.requestTypes())
            .containsExactly("workspace_tree", "workspace_git_status")
    }

    @Test
    fun `missing workspace_inspection capability gates the load`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady(capabilities = "\"pane_realtime_delta\"")
        h.respondWith()
        val vm = h.viewModel()
        h.pump()

        val state = vm.uiState.value
        assertThat(state.loading).isFalse()
        assertThat(state.workspaceError)
            .isEqualTo("This relay does not support workspace inspection.")
        assertThat(h.requestTypes()).isEmpty()
    }

    @Test
    fun `agent without a cwd surfaces the workspace-path gate`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady(cwd = "")
        h.respondWith()
        val vm = h.viewModel()
        h.pump()

        val state = vm.uiState.value
        assertThat(state.loading).isFalse()
        assertThat(state.workspaceError)
            .isEqualTo("This agent does not report a workspace path.")
    }

    @Test
    fun `tree failure errors the browser and leaves git unset`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        h.respondWith(failing = mapOf("workspace_tree" to "Workspace is unavailable"))
        val vm = h.viewModel()
        h.pump()

        val state = vm.uiState.value
        assertThat(state.loading).isFalse()
        assertThat(state.workspaceError).isEqualTo("Workspace is unavailable")
        // Oracle parity — the throw precedes git assignment, so the Changes
        // tab falls back to its generic "unavailable" message.
        assertThat(state.git).isNull()
    }

    @Test
    fun `git failure degrades to unavailable with the relay message`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        h.respondWith(failing = mapOf("workspace_git_status" to "Git inspection timed out"))
        val vm = h.viewModel()
        h.pump()

        val state = vm.uiState.value
        assertThat(state.workspaceError).isNull()
        assertThat(state.git?.available).isFalse()
        assertThat(state.gitReason).isEqualTo("Git inspection timed out")
    }

    @Test
    fun `not-a-repo status carries the oracle's reason`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        h.respondWith(git = """{"available":false,"files":[]}""")
        val vm = h.viewModel()
        h.pump()

        val state = vm.uiState.value
        assertThat(state.git?.available).isFalse()
        assertThat(state.gitReason)
            .isEqualTo("This workspace is not inside a Git repository.")
    }

    @Test
    fun `invalid payloads map to the oracle's validation errors`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        h.respondWith(tree = """{"entries":"nope"}""")
        val vm = h.viewModel()
        h.pump()

        assertThat(vm.uiState.value.workspaceError)
            .isEqualTo("Relay returned an invalid workspace tree.")
    }

    @Test
    fun `showFile loads a text preview for the tapped path`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        h.respondWith()
        val vm = h.viewModel()
        h.pump()

        vm.showFile("src/main.kt")
        h.pump()

        val state = vm.uiState.value
        assertThat(state.previewVisible).isTrue()
        assertThat(state.previewLoading).isFalse()
        assertThat(state.selectedPath).isEqualTo("src/main.kt")
        assertThat(state.previewFile?.text).isEqualTo("hello")
        assertThat(state.previewFile?.kind).isEqualTo(WorkspacePreviewKind.TEXT)
        // The request rode the wire with the workspace-relative path.
        val request = h.handle().requests.single { it.type == "workspace_file" }
        assertThat(request.path).isEqualTo("src/main.kt")
        assertThat(request.paneId).isEqualTo("%1")
    }

    @Test
    fun `showFile surfaces preview errors without losing the listing`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        h.respondWith(
            tree = """{"root":"/home/u/lerdr","entries":[{"path":"a.txt","name":"a.txt","kind":"file","size":1}]}""",
            failing = mapOf("workspace_file" to "Workspace file was not found"),
        )
        val vm = h.viewModel()
        h.pump()

        vm.showFile("a.txt")
        h.pump()

        val state = vm.uiState.value
        assertThat(state.previewVisible).isTrue()
        assertThat(state.previewError).isEqualTo("Workspace file was not found")
        assertThat(state.tree?.entries).hasSize(1)
    }

    @Test
    fun `showDiff loads the unified diff for a changed path`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        h.respondWith()
        val vm = h.viewModel()
        h.pump()

        vm.showDiff("app/src/f.kt")
        h.pump()

        val state = vm.uiState.value
        assertThat(state.previewKind).isEqualTo(FilesPreviewKind.DIFF)
        assertThat(state.previewDiff?.diff).isEqualTo("@@ -1 +1 @@\n-old\n+new")
        val request = h.handle().requests.single { it.type == "workspace_git_diff" }
        assertThat(request.path).isEqualTo("app/src/f.kt")
    }

    @Test
    fun `selectSection clears the open preview like the oracle`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        h.respondWith()
        val vm = h.viewModel()
        h.pump()

        vm.showFile("a.txt")
        h.pump()
        assertThat(vm.uiState.value.previewVisible).isTrue()

        vm.selectSection(FilesSection.CHANGES)
        h.pump()

        val state = vm.uiState.value
        assertThat(state.section).isEqualTo(FilesSection.CHANGES)
        assertThat(state.previewVisible).isFalse()
        assertThat(state.selectedPath).isEmpty()
    }

    @Test
    fun `closePreview returns to the listing keeping the selection`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        h.respondWith()
        val vm = h.viewModel()
        h.pump()

        vm.showFile("a.txt")
        h.pump()
        vm.closePreview()
        h.pump()

        val state = vm.uiState.value
        assertThat(state.previewVisible).isFalse()
        assertThat(state.selectedPath).isEqualTo("a.txt")
    }

    @Test
    fun `a stale preview result is dropped after a newer request`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        // Manual responder — the first workspace_file call stays unresolved
        // until the test releases it, simulating a slow preview.
        var first: kotlinx.coroutines.CompletableDeferred<CommandResultMessage>? = null
        h.handle().responder = { message ->
            when (message.type) {
                "workspace_file" -> {
                    if (message.path == "slow.txt") {
                        kotlinx.coroutines.CompletableDeferred<CommandResultMessage>()
                            .also { first = it }
                            .await()
                    } else {
                        CommandResultMessage(
                            action = message.type,
                            ok = true,
                            phase = CommandResultMessage.PHASE_COMPLETED,
                            requestId = message.requestId,
                            data = json(
                                """{"path":"${message.path}","media_type":"text/plain","kind":"text","text":"fast","size":4}""",
                            ),
                        )
                    }
                }
                "workspace_tree" -> CommandResultMessage(
                    action = message.type,
                    ok = true,
                    phase = CommandResultMessage.PHASE_COMPLETED,
                    requestId = message.requestId,
                    data = json("""{"root":"/r","entries":[]}"""),
                )
                else -> CommandResultMessage(
                    action = message.type,
                    ok = true,
                    phase = CommandResultMessage.PHASE_COMPLETED,
                    requestId = message.requestId,
                    data = json("""{"available":true,"files":[]}"""),
                )
            }
        }
        val vm = h.viewModel()
        h.pump()

        vm.showFile("slow.txt")
        vm.showFile("fast.txt")
        h.pump()

        // The slow request resolves late — its payload must be dropped.
        first?.complete(
            CommandResultMessage(
                action = "workspace_file",
                ok = true,
                phase = CommandResultMessage.PHASE_COMPLETED,
                data = json(
                    """{"path":"slow.txt","media_type":"text/plain","kind":"text","text":"stale","size":5}""",
                ),
            ),
        )
        h.pump()

        val state = vm.uiState.value
        assertThat(state.selectedPath).isEqualTo("fast.txt")
        assertThat(state.previewFile?.text).isEqualTo("fast")
    }

    @Test
    fun `a cwd change reloads the workspace`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        h.respondWith()
        val vm = h.viewModel()
        h.pump()
        assertThat(h.requestTypes().count { it == "workspace_tree" }).isEqualTo(1)

        // A topology change lands as a fresh snapshot — agent_update deltas
        // carry no target tuple (normalizeAgentTargetFields clears it).
        h.handle().emit(
            json(
                """{"type":"agents","agents":[{"pane_id":"%1","raw_pane_id":"%1","terminal_id":"t1","server_session_id":"ss1","generation":3,"agent":"claude","name":"claude","status":"working","cwd":"/home/u/other","project":"other","workspace_id":"w2","updated_at":200}]}""",
            ),
        )
        h.pump()

        assertThat(h.requestTypes().count { it == "workspace_tree" }).isEqualTo(2)
    }

    @Test
    fun `directory helpers derive children, breadcrumbs and labels`() {
        val entries = listOf(
            WorkspaceTreeEntry("app", "app", WorkspaceEntryKind.DIRECTORY),
            WorkspaceTreeEntry("app/src", "src", WorkspaceEntryKind.DIRECTORY),
            WorkspaceTreeEntry("app/src/f.kt", "f.kt", WorkspaceEntryKind.FILE, 10),
            WorkspaceTreeEntry("README.md", "README.md", WorkspaceEntryKind.FILE, 5),
        )
        assertThat(childrenOf(entries, "").map { it.path })
            .containsExactly("app", "README.md")
        assertThat(childrenOf(entries, "app").map { it.path })
            .containsExactly("app/src")
        assertThat(childrenOf(entries, "app/src").map { it.path })
            .containsExactly("app/src/f.kt")

        assertThat(breadcrumbsOf("lerdr", "app/src").map { it.label to it.dir })
            .containsExactly(
                "lerdr" to "",
                "app" to "app",
                "src" to "app/src",
            ).inOrder()

        assertThat(gitStatusLabel("??")).isEqualTo("New")
        assertThat(gitStatusLabel(" M")).isEqualTo("Modified")
        assertThat(gitStatusLabel("D ")).isEqualTo("Deleted")
        assertThat(gitStatusLabel("R ")).isEqualTo("Renamed")
        assertThat(gitStatusLabel("A ")).isEqualTo("Added")
        assertThat(gitStatusLabel("UU")).isEqualTo("UU")
        assertThat(gitStatusLabel("  ")).isEqualTo("Changed")

        assertThat(diffLineToneOf("+added")).isEqualTo(DiffLineTone.ADDITION)
        assertThat(diffLineToneOf("-removed")).isEqualTo(DiffLineTone.DELETION)
        assertThat(diffLineToneOf("@@ -1 +1 @@")).isEqualTo(DiffLineTone.HUNK)
        assertThat(diffLineToneOf("--- a/f")).isEqualTo(DiffLineTone.FILE)
        assertThat(diffLineToneOf("+++ b/f")).isEqualTo(DiffLineTone.FILE)
        assertThat(diffLineToneOf("diff --git a/f b/f")).isEqualTo(DiffLineTone.META)
        assertThat(diffLineToneOf("\\ No newline at end of file")).isEqualTo(DiffLineTone.NOTE)
        assertThat(diffLineToneOf(" context")).isEqualTo(DiffLineTone.CONTEXT)

        assertThat(formatFileSize(0)).isEqualTo("0 B")
        assertThat(formatFileSize(512)).isEqualTo("512 B")
        assertThat(formatFileSize(2048)).isEqualTo("2 KB")
        assertThat(formatFileSize(3 * 1024 * 1024)).isEqualTo("3 MB")
    }

    @Test
    fun `openDir drills down and clears the filter`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        h.respondWith()
        val vm = h.viewModel()
        h.pump()

        vm.onFilterChange("main")
        vm.openDir("app/src")
        h.pump()

        val state = vm.uiState.value
        assertThat(state.currentDir).isEqualTo("app/src")
        assertThat(state.filter).isEmpty()
    }
}
