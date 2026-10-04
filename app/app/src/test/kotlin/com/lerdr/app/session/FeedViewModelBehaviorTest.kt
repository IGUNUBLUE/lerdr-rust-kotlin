package com.lerdr.app.session

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import androidx.datastore.preferences.core.Preferences
import androidx.lifecycle.ViewModelStore
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.input.TextFieldValue
import com.google.common.truth.Truth.assertThat
import com.lerdr.app.session.feed.QuestionDraft
import java.io.File
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.onEach
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
import lerdr.core.data.composerDraftIdentity
import lerdr.core.data.RelayEndpoint
import lerdr.core.data.RelayRegistry
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
        draftReads: Channel<Unit>? = null,
        draftReadStarted: CompletableDeferred<Unit>? = null,
        beforeDraftWrite: (suspend () -> Unit)? = null,
        completedDraftWrites: Channel<Unit>? = null,
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
        val drafts = DraftStore(
            object : DataStore<Preferences> {
                override val data = if (draftReads == null) draftStore.data else {
                    draftStore.data.onEach {
                        draftReadStarted?.complete(Unit)
                        draftReads.receive()
                    }
                }

                override suspend fun updateData(
                    transform: suspend (Preferences) -> Preferences,
                ): Preferences {
                    beforeDraftWrite?.invoke()
                    return draftStore.updateData(transform).also {
                        completedDraftWrites?.trySend(Unit)
                    }
                }
            },
        )
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

        suspend fun emitBlockedQuestion(total: Int = 1) {
            handle().emit(
                feedJson(
                    """{"type":"blocked","pane_id":"%1","attention_kind":"question","prompt":"Pick one","interaction":{"id":"q1","kind":"single_select","question":"Pick one","options":[{"index":0,"label":"A"},{"index":1,"label":"B"}],"other":{"hidden":true},"submit_label":"${if (total > 1) "Next" else "Submit"}","question_index":1,"question_total":$total},"event_id":"ev1","server_session_id":"ss1","terminal_id":"t1","generation":3}""",
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
    fun `question advances and resolves through multiple results before pane catches up`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady(capabilities = listOf("attention_classification"))
        h.emitBlockedQuestion(total = 3)
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.settleDraft()

        for (next in 2..3) {
            vm.updateQuestionDraft(QuestionDraft(selected = setOf(0)))
            vm.submitQuestion()
            awaitState(this) { h.rawFramesOf("answer_question").size == next - 1 }
            val frame = h.rawFramesOf("answer_question").last()
            h.emitCommandResult(
                frame["request_id"]!!.jsonPrimitive.content,
                phase = "advanced",
                data = """{"interaction":{"id":"q$next","kind":"single_select","question":"Question $next","options":[{"index":0,"label":"X"}],"other":{"hidden":true},"submit_label":"${if (next < 3) "Next" else "Submit"}","question_index":$next,"question_total":3}}""",
            )
            awaitState(this) { vm.uiState.value.blockedInteraction?.id == "q$next" }
            assertThat(vm.uiState.value.questionDraft).isEqualTo(QuestionDraft())
        }

        vm.updateQuestionDraft(QuestionDraft(selected = setOf(0)))
        vm.submitQuestion()
        awaitState(this) { h.rawFramesOf("answer_question").size == 3 }
        h.emitCommandResult(
            h.rawFramesOf("answer_question").last()["request_id"]!!.jsonPrimitive.content,
            phase = "confirmed",
        )
        awaitState(this) { vm.uiState.value.blocked == null }
        assertThat(vm.uiState.value.blockedInteraction).isNull()

        // A delayed intermediate pane frame cannot resurrect a resolved form.
        h.handle().emit(
            feedJson(
                """{"type":"blocked","pane_id":"%1","attention_kind":"question","interaction":{"id":"q2","kind":"single_select","question":"Question 2","options":[{"index":0,"label":"X"}],"other":{"hidden":true}},"event_id":"ev2","server_session_id":"ss1","terminal_id":"t1","generation":3}""",
            ),
        )
        h.pump()
        assertThat(vm.uiState.value.blockedInteraction).isNull()

        h.handle().emit(
            feedJson(
                """{"type":"blocked","pane_id":"%1","attention_kind":"question","interaction":{"id":"q4","kind":"single_select","question":"New question","options":[{"index":0,"label":"Y"}],"other":{"hidden":true}},"event_id":"ev4","server_session_id":"ss1","terminal_id":"t1","generation":3}""",
            ),
        )
        awaitState(this) { vm.uiState.value.blockedInteraction?.id == "q4" }

        // Two snapshots clear the store's transient blocked-state flicker guard.
        repeat(2) {
            h.handle().emit(
                feedJson(
                    """{"type":"agents","agents":[{"pane_id":"%1","raw_pane_id":"%1","terminal_id":"t1","server_session_id":"ss1","generation":3,"agent":"claude","status":"idle","cwd":"/home/u/lerdr","workspace_id":"w1","updated_at":102}]}""",
                ),
            )
        }
        awaitState(this) { vm.uiState.value.blocked == null }
        h.emitBlockedQuestion()
        // The same content-derived id can legitimately be asked again.
        awaitState(this) { vm.uiState.value.blockedInteraction?.id == "q1" }
        assertThat(vm.uiState.value.questionDraft.selected).isEmpty()
    }

    @Test
    fun `a refused question applies the returned current interaction`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady(capabilities = listOf("attention_classification"))
        h.emitBlockedQuestion()
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.settleDraft()
        vm.updateQuestionDraft(QuestionDraft(selected = setOf(0)))
        vm.submitQuestion()
        val frame = h.awaitRawFrame("answer_question")
        h.handle().emit(
            feedJson(
                """{"type":"command_result","request_id":"${frame["request_id"]!!.jsonPrimitive.content}","ok":false,"phase":"failed","error":"Question changed","data":{"interaction":{"id":"replacement","kind":"single_select","question":"Current question","options":[{"index":0,"label":"New answer"}],"other":{"hidden":true}}}}""",
            ),
        )
        awaitState(this) { vm.uiState.value.blockedInteraction?.id == "replacement" }
        assertThat(vm.uiState.value.blockedInteraction?.question).isEqualTo("Current question")
        assertThat(vm.uiState.value.questionDraft.selected).isEmpty()
        assertThat(vm.uiState.value.lastError).isNotNull()
    }

    @Test
    fun `successful old prompt does not erase a replacement terminal draft`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady()
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.settleDraft()
        val completion = CompletableDeferred<Unit>()
        h.handle().responder = { message ->
            if (message.type == "submit_prompt") completion.await()
            lerdr.core.model.CommandResultMessage(
                action = message.type,
                ok = true,
                phase = lerdr.core.model.CommandResultMessage.PHASE_COMPLETED,
                requestId = message.requestId,
            )
        }
        val replacementIdentity = composerDraftIdentity("r1", "t2", "claude", "/home/u/lerdr")
        h.drafts.save(replacementIdentity, "replacement terminal draft")
        vm.onDraftChange("old terminal prompt")
        vm.sendPrompt()
        awaitState(this) { vm.uiState.value.responding }
        h.handle().emit(
            feedJson(
                """{"type":"agents","agents":[{"pane_id":"%1","raw_pane_id":"%1","terminal_id":"t2","server_session_id":"ss1","generation":4,"agent":"claude","status":"idle","cwd":"/home/u/lerdr","workspace_id":"w1","updated_at":101}]}""",
            ),
        )
        awaitState(this) { vm.composerValue.text == "replacement terminal draft" }
        completion.complete(Unit)
        awaitState(this) { !vm.uiState.value.responding }
        assertThat(vm.composerValue.text).isEqualTo("replacement terminal draft")
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

    // ── blocked composer send ─────────────────────────────────────────

    @Test
    fun `sendPrompt is refused locally while the agent waits at a question`() = runTest {
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

        vm.onDraftChange("answer from feed")
        vm.sendPrompt()
        h.pump()

        // `submit_prompt` is refused upstream (`agent_blocked`) — the wire
        // call never happens and the draft survives for after the unblock.
        assertThat(h.handle().requests.any { it.type == "submit_prompt" }).isFalse()
        assertThat(vm.composerValue.text).isEqualTo("answer from feed")
    }

    @Test
    fun `a mid-flight agent_blocked refusal preserves the draft`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady()
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.pump()
        h.settleDraft()

        // Agent still reads idle at guard time; the block lands on the wire.
        h.handle().responder = { message ->
            if (message.type == "submit_prompt") {
                throw CommandException(
                    message = "Herdr rejected the command before it was sent",
                    code = "agent_blocked",
                )
            }
            lerdr.core.model.CommandResultMessage(
                action = message.type,
                ok = true,
                phase = lerdr.core.model.CommandResultMessage.PHASE_COMPLETED,
                requestId = message.requestId,
            )
        }
        vm.onDraftChange("queued while blocked")
        vm.sendPrompt()
        h.pump()

        assertThat(h.handle().requests.any { it.type == "submit_prompt" }).isTrue()
        assertThat(vm.composerValue.text).isEqualTo("queued while blocked")
    }

    @Test
    fun `a delayed persisted prefix cannot roll back an active composer edit`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady()
        val identity = composerDraftIdentity("r1", "t1", "claude", "/home/u/lerdr")
        h.drafts.save(identity, "saved prefix")
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        awaitState(this) { vm.composerValue.text == "saved prefix" }

        vm.onComposerChange(
            TextFieldValue("saved prefix with new typing", TextRange(4), TextRange(0, 5)),
        )
        h.pump()
        h.drafts.save(identity, "saved prefix")
        h.pump()

        assertThat(vm.composerValue.text).isEqualTo("saved prefix with new typing")
        assertThat(vm.composerValue.selection).isEqualTo(TextRange(4))
        assertThat(vm.composerValue.composition).isEqualTo(TextRange(0, 5))
    }

    @Test
    fun `a delayed initial draft read cannot replace text already typed`() = runTest {
        val reads = Channel<Unit>()
        val started = CompletableDeferred<Unit>()
        val h = Harness(this, tmp.root, reads, started)
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady()
        h.drafts.save(
            composerDraftIdentity("r1", "t1", "claude", "/home/u/lerdr"),
            "previous saved text",
        )
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        awaitState(this) { started.isCompleted }

        vm.onDraftChange("new text typed while restoration is pending")
        h.pump()
        reads.send(Unit)
        h.pump()

        assertThat(vm.composerValue.text)
            .isEqualTo("new text typed while restoration is pending")
    }

    @Test
    fun `a delayed draft save cannot restore a sent prompt after teardown`() = runTest {
        val started = CompletableDeferred<Unit>()
        val release = CompletableDeferred<Unit>()
        val completedWrites = Channel<Unit>(Channel.UNLIMITED)
        var delayNextWrite = false
        val h = Harness(this, tmp.root, completedDraftWrites = completedWrites, beforeDraftWrite = {
            if (delayNextWrite) {
                delayNextWrite = false
                started.complete(Unit)
                release.await()
            }
        })
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady()
        val identity = composerDraftIdentity("r1", "t1", "claude", "/home/u/lerdr")
        val otherIdentity = composerDraftIdentity("r1", "t2", "codex", "/home/u/other")
        h.drafts.save(identity, "initial draft")
        h.drafts.save(otherIdentity, "other terminal draft")
        repeat(2) { completedWrites.receive() }
        val vm = h.viewModel()
        val viewModels = ViewModelStore().apply { put("feed", vm) }
        backgroundScope.launch { vm.uiState.collect { } }
        awaitState(this) { vm.composerValue.text == "initial draft" }

        delayNextWrite = true
        vm.onDraftChange("sent prompt")
        started.await()
        vm.sendPrompt()
        awaitState(this) { h.handle().requests.any { it.type == "submit_prompt" } }
        viewModels.clear()
        release.complete(Unit)
        repeat(2) { completedWrites.receive() }

        assertThat(h.drafts.current(identity)).isNull()
        assertThat(h.drafts.current(otherIdentity)!!.text).isEqualTo("other terminal draft")
    }

    @Test
    fun `newest queued edit remains durable after the feed is torn down`() = runTest {
        val started = CompletableDeferred<Unit>()
        val release = CompletableDeferred<Unit>()
        val completedWrites = Channel<Unit>(Channel.UNLIMITED)
        var delayNextWrite = false
        val h = Harness(this, tmp.root, completedDraftWrites = completedWrites, beforeDraftWrite = {
            if (delayNextWrite) {
                delayNextWrite = false
                started.complete(Unit)
                release.await()
            }
        })
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady()
        val identity = composerDraftIdentity("r1", "t1", "claude", "/home/u/lerdr")
        h.drafts.save(identity, "initial draft")
        completedWrites.receive()
        val vm = h.viewModel()
        val viewModels = ViewModelStore().apply { put("feed", vm) }
        backgroundScope.launch { vm.uiState.collect { } }
        awaitState(this) { vm.composerValue.text == "initial draft" }

        delayNextWrite = true
        vm.onDraftChange("older prefix")
        started.await()
        vm.onDraftChange("newest complete draft")
        viewModels.clear()
        release.complete(Unit)
        repeat(2) { completedWrites.receive() }

        assertThat(h.drafts.current(identity)!!.text).isEqualTo("newest complete draft")
    }

    @Test
    fun `successful submit preserves edits typed while the prompt was in flight`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady()
        val identity = composerDraftIdentity("r1", "t1", "claude", "/home/u/lerdr")
        h.drafts.save(identity, "initial draft")
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        awaitState(this) { vm.composerValue.text == "initial draft" }
        val completion = CompletableDeferred<Unit>()
        h.handle().responder = { message ->
            if (message.type == "submit_prompt") completion.await()
            lerdr.core.model.CommandResultMessage(
                action = message.type,
                ok = true,
                phase = lerdr.core.model.CommandResultMessage.PHASE_COMPLETED,
                requestId = message.requestId,
            )
        }

        vm.onDraftChange("submitted prompt")
        vm.sendPrompt()
        awaitState(this) { vm.uiState.value.responding }
        vm.onComposerChange(TextFieldValue("next prompt typed during send", TextRange(5)))
        completion.complete(Unit)
        awaitState(this) { !vm.uiState.value.responding }
        h.drafts.prune()

        assertThat(vm.composerValue.text).isEqualTo("next prompt typed during send")
        assertThat(h.drafts.current(identity)!!.text).isEqualTo("next prompt typed during send")
        assertThat(vm.composerValue.selection).isEqualTo(TextRange(5))
    }

    @Test
    fun `editing back to submitted text is still a new unsent draft`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady()
        val identity = composerDraftIdentity("r1", "t1", "claude", "/home/u/lerdr")
        h.drafts.save(identity, "initial draft")
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        awaitState(this) { vm.composerValue.text == "initial draft" }
        val completion = CompletableDeferred<Unit>()
        h.handle().responder = { message ->
            if (message.type == "submit_prompt") completion.await()
            lerdr.core.model.CommandResultMessage(
                action = message.type,
                ok = true,
                phase = lerdr.core.model.CommandResultMessage.PHASE_COMPLETED,
                requestId = message.requestId,
            )
        }

        vm.onDraftChange("submitted prompt")
        vm.sendPrompt()
        awaitState(this) { vm.uiState.value.responding }
        vm.onComposerChange(TextFieldValue("revised prompt", TextRange(2)))
        vm.onComposerChange(TextFieldValue("submitted prompt", TextRange(3)))
        completion.complete(Unit)
        awaitState(this) { !vm.uiState.value.responding }
        h.drafts.prune()

        assertThat(vm.composerValue.text).isEqualTo("submitted prompt")
        assertThat(h.drafts.current(identity)!!.text).isEqualTo("submitted prompt")
        assertThat(vm.composerValue.selection).isEqualTo(TextRange(3))
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
