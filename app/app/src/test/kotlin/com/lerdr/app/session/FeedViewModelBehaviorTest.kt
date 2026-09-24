package com.lerdr.app.session

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import com.google.common.truth.Truth.assertThat
import com.lerdr.app.session.feed.QuestionDraft
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
import kotlinx.serialization.json.jsonPrimitive
import lerdr.core.data.DeviceRole
import lerdr.core.data.DraftStore
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

private fun feedJson(raw: String): JsonObject =
    LerdrJson.parseToJsonElement(raw) as JsonObject

/**
 * Feed depth behaviors — slash catalog fetch/cache, question submit +
 * command-result overrides, copy_agent_response, reader gating, and the
 * transient error/notice channels.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class FeedViewModelBehaviorTest {

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
        val scope = testScope.backgroundScope
        val credentials = FakeCredentialStore()
        private val relayStore = PreferenceDataStoreFactory.create(scope = scope) {
            File(tmpDir, "relays.preferences_pb")
        }
        private val draftStore = PreferenceDataStoreFactory.create(scope = scope) {
            File(tmpDir, "drafts.preferences_pb")
        }
        val registry = RelayRegistry(relayStore, scope)
        val drafts = DraftStore(draftStore)
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
        val uploads = AttachmentUploads(scope, repository, FakeAttachmentSource(emptyMap()))

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

        suspend fun connectReady(capabilities: List<String> = emptyList()) {
            repository.connect(endpoint)
            handle().connect()
            handle().emit(
                feedJson(
                    """{"type":"push_config","capabilities":${capabilities.joinToString(",", "[", "]") { "\"$it\"" }},"inventory":{"state":"ready"}}""",
                ),
            )
            handle().emit(
                feedJson(
                    """{"type":"agents","agents":[{"pane_id":"%1","raw_pane_id":"%1","terminal_id":"t1","server_session_id":"ss1","generation":3,"agent":"claude","name":"claude","status":"idle","cwd":"/home/u/lerdr","project":"lerdr","workspace_id":"w1","updated_at":100}]}""",
                ),
            )
            pump()
        }

        suspend fun emitBlockedQuestion() {
            handle().emit(
                feedJson(
                    """{"type":"blocked","pane_id":"%1","attention_kind":"question","prompt":"Pick one","interaction":{"id":"q1","kind":"single_select","question":"Pick one","options":[{"index":0,"label":"A"},{"index":1,"label":"B"}],"other":{"hidden":true},"submit_label":"Submit","question_index":1,"question_total":1},"event_id":"ev1","server_session_id":"ss1","terminal_id":"t1","generation":3}""",
                ),
            )
            pump()
        }

        /** Raw `requestRaw` frames — the catalog fetch + question submits land here. */
        fun rawFramesOf(type: String) = handle().sentRaw
            .map { feedJson(it) }
            .filter { it["type"]?.jsonPrimitive?.content == type }

        fun awaitRawFrame(type: String): JsonObject {
            val deadline = System.currentTimeMillis() + 5_000
            while (rawFramesOf(type).isEmpty() && System.currentTimeMillis() < deadline) {
                testScope.runCurrent()
                Thread.sleep(5)
            }
            return rawFramesOf(type).lastOrNull()
                ?: error("no raw frame '$type' sent")
        }

        /** Resolve a pending `requestRaw` with a `command_result`. */
        suspend fun emitCommandResult(
            requestId: String,
            phase: String = "completed",
            data: String = "{}",
        ) {
            handle().emit(
                feedJson(
                    """{"type":"command_result","request_id":"$requestId","ok":true,"phase":"$phase","data":$data}""",
                ),
            )
            pump()
        }

        fun settleDraft() {
            Thread.sleep(50)
            testScope.runCurrent()
        }
    }

    private fun Harness.viewModel(): FeedViewModel =
        FeedViewModel(paneId, repository, drafts, uploads, scope)

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

    private fun awaitState(
        scope: TestScope,
        timeoutMs: Long = 5_000,
        predicate: () -> Boolean,
    ) {
        val deadline = System.currentTimeMillis() + timeoutMs
        while (!predicate() && System.currentTimeMillis() < deadline) {
            scope.runCurrent()
            Thread.sleep(5)
        }
        check(predicate()) { "condition not met within ${timeoutMs}ms" }
    }

    // ── slash commands ────────────────────────────────────────────────

    @Test
    fun `slash catalog fetches on capability and parses the catalog`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady(capabilities = listOf("slash_commands"))
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.pump()

        h.settleDraft()
        assertThat(vm.uiState.value.canControl).isTrue()
        val frame = h.awaitRawFrame("list_slash_commands")
        assertThat(frame["pane_id"]?.jsonPrimitive?.content).isEqualTo("%1")
        h.emitCommandResult(
            frame["request_id"]!!.jsonPrimitive.content,
            data = """{"commands":[{"command":"/clear","description":"Clear it","source":"project"},{"command":"/zap","source":"vendor"},{"command":"bogus"}]}""",
        )
        awaitState(this) { vm.uiState.value.slashCommands.isNotEmpty() }
        val commands = vm.uiState.value.slashCommands
        assertThat(commands.map { it.command }).containsExactly("/clear", "/zap").inOrder()
        assertThat(commands.first { it.command == "/zap" }.source).isEqualTo("builtin")
        assertThat(vm.uiState.value.slashLoading).isFalse()
        assertThat(vm.uiState.value.slashUnavailable).isFalse()
    }

    @Test
    fun `slash catalog stays silent without the capability`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady()
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.pump()
        h.settleDraft()

        assertThat(h.rawFramesOf("list_slash_commands")).isEmpty()
        assertThat(vm.uiState.value.slashLoading).isFalse()
    }

    @Test
    fun `slash catalog stays silent for readers`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.seed("r1", credential(DeviceRole.READER))
        h.repository.start()
        h.pump()
        h.connectReady(capabilities = listOf("slash_commands"))
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.pump()
        h.settleDraft()

        assertThat(h.rawFramesOf("list_slash_commands")).isEmpty()
        assertThat(vm.uiState.value.canControl).isFalse()
    }

    // ── questions ─────────────────────────────────────────────────────

    @Test
    fun `question submit sends answer_question and applies the advanced interaction`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady(capabilities = listOf("attention_classification"))
        h.emitBlockedQuestion()
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.pump()
        h.settleDraft()

        assertThat(vm.uiState.value.blockedInteraction?.id).isEqualTo("q1")
        vm.updateQuestionDraft(QuestionDraft(selected = setOf(1)))
        vm.submitQuestion()

        val frame = h.awaitRawFrame("answer_question")
        assertThat(frame["interaction_id"]?.jsonPrimitive?.content).isEqualTo("q1")
        // Selected indices ride as a sorted array on the wire.
        assertThat(frame["selected_indices"]?.toString()).isEqualTo("[1]")
        h.emitCommandResult(
            frame["request_id"]!!.jsonPrimitive.content,
            phase = "advanced",
            data = """{"interaction":{"id":"q2","kind":"single_select","question":"Second?","options":[{"index":0,"label":"X"}],"other":{"hidden":true},"submit_label":"Submit","question_index":2,"question_total":2}}""",
        )
        awaitState(this) { vm.uiState.value.blockedInteraction?.id == "q2" }
        assertThat(vm.uiState.value.questionDraft).isEqualTo(QuestionDraft())
        assertThat(vm.uiState.value.notice).isEqualTo("Answer saved.")
    }

    @Test
    fun `confirmed final submit clears the card`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady(capabilities = listOf("attention_classification"))
        h.emitBlockedQuestion()
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.pump()
        h.settleDraft()

        vm.updateQuestionDraft(QuestionDraft(selected = setOf(0)))
        vm.submitQuestion()
        val frame = h.awaitRawFrame("answer_question")
        h.emitCommandResult(
            frame["request_id"]!!.jsonPrimitive.content,
            phase = "confirmed",
        )
        awaitState(this) { vm.uiState.value.blockedInteraction == null }
        // The question card hides even though the agent row still reads blocked.
        assertThat(vm.uiState.value.blocked).isNull()
        assertThat(vm.uiState.value.notice).isEqualTo("Answers submitted.")
    }

    @Test
    fun `submit without a valid draft is refused locally`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady(capabilities = listOf("attention_classification"))
        h.emitBlockedQuestion()
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.pump()
        h.settleDraft()

        vm.submitQuestion()
        h.pump()
        assertThat(h.rawFramesOf("answer_question")).isEmpty()
        assertThat(vm.uiState.value.lastError).isEqualTo("Complete the question first.")
    }

    // ── copy response ─────────────────────────────────────────────────

    @Test
    fun `copyAgentResponse drives the relay transaction for capable controllers`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady(capabilities = listOf("agent_response_copy"))
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.pump()
        h.settleDraft()
        assertThat(vm.uiState.value.canCopyResponse).isTrue()

        h.handle().responder = { message ->
            lerdr.core.model.CommandResultMessage(
                action = message.type,
                ok = true,
                phase = lerdr.core.model.CommandResultMessage.PHASE_COMPLETED,
                requestId = message.requestId,
                data = feedJson("""{"text":"the agent reply"}"""),
            )
        }
        var copied: String? = null
        vm.copyAgentResponse("entry fallback") { copied = it }
        h.pump()

        assertThat(h.handle().requests.any { it.type == "copy_agent_response" }).isTrue()
        assertThat(copied).isEqualTo("the agent reply")
        assertThat(vm.uiState.value.notice).isEqualTo("Agent response copied.")
    }

    @Test
    fun `copyAgentResponse falls back to the entry text for readers`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.seed("r1", credential(DeviceRole.READER))
        h.repository.start()
        h.pump()
        h.connectReady(capabilities = listOf("agent_response_copy"))
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.pump()
        h.settleDraft()
        assertThat(vm.uiState.value.canCopyResponse).isFalse()

        var copied: String? = null
        vm.copyAgentResponse("reader fallback") { copied = it }
        h.pump()

        // The relay-side transaction must NOT run for readers.
        assertThat(h.handle().requests.any { it.type == "copy_agent_response" }).isFalse()
        assertThat(copied).isEqualTo("reader fallback")
        assertThat(vm.uiState.value.notice).isEqualTo("Agent response copied.")
    }

    @Test
    fun `copyAgentResponse errors when nothing is available`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.pump()

        var copied: String? = null
        vm.copyAgentResponse("") { copied = it }
        h.pump()
        assertThat(copied).isNull()
        assertThat(vm.uiState.value.lastError)
            .isEqualTo("No completed agent response is available to copy.")
    }

    // ── transient channels ────────────────────────────────────────────

    @Test
    fun `clearError drains the error and notice channels`() = runTest {
        val h = Harness(this, tmp.root)
        h.connectReady()
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.pump()

        vm.copyAgentResponse("") { }
        h.pump()
        assertThat(vm.uiState.value.lastError).isNotNull()
        vm.clearError()
        assertThat(vm.uiState.value.lastError).isNull()
        assertThat(vm.uiState.value.notice).isNull()
    }
}
