package com.lerdr.app.session

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import com.google.common.truth.Truth.assertThat
import java.io.ByteArrayInputStream
import java.io.File
import java.io.InputStream
import java.security.MessageDigest
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import kotlinx.coroutines.Dispatchers
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonPrimitive
import lerdr.core.data.DraftStore
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

private fun frames(h: FakeRelaySessionHandle): List<JsonObject> =
    h.sentRaw.map { json(it) }

private fun sha256Hex(bytes: ByteArray): String =
    MessageDigest.getInstance("SHA-256").digest(bytes)
        .joinToString("") { "%02x".format(it) }

@OptIn(ExperimentalCoroutinesApi::class)
class FeedViewModelAttachmentTest {

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
        files: Map<String, ByteArray>,
    ) {
        private val scope = testScope.backgroundScope
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
        val uploads = AttachmentUploads(scope, repository, FakeAttachmentSource(files))

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

        suspend fun connectReady() {
            repository.connect(endpoint)
            handle().connect()
            handle().emit(
                json(
                    """{"type":"push_config","capabilities":[],"inventory":{"state":"ready"}}""",
                ),
            )
            handle().emit(
                json(
                    """{"type":"agents","agents":[{"pane_id":"%1","raw_pane_id":"%1","terminal_id":"t1","server_session_id":"ss1","generation":3,"agent":"claude","name":"claude","status":"idle","cwd":"/home/u/lerdr","project":"lerdr","workspace_id":"w1","updated_at":100}]}""",
                ),
            )
            pump()
        }

        fun framesOf(type: String) = frames(handle()).filter {
            it["type"]?.jsonPrimitive?.content == type
        }

        fun awaitFrameCount(type: String, count: Int): List<JsonObject> {
            val deadline = System.currentTimeMillis() + 5_000
            while (framesOf(type).size < count && System.currentTimeMillis() < deadline) {
                testScope.runCurrent()
                Thread.sleep(5)
            }
            val found = framesOf(type)
            check(found.size >= count) { "expected $count '$type' frames, saw ${found.size}" }
            return found
        }

        /** Answers the full upload run for a single file. */
        suspend fun answerUpload(name: String, bytes: ByteArray, uploadId: String = "up-1") {
            val begin = awaitFrameCount("upload_begin", 1).last()
            emitResult(
                "upload_begin_result",
                begin["request_id"]!!.jsonPrimitive.content,
                """{"upload_id":"$uploadId","chunk_bytes":262144,"expires_at":"2999-01-01T00:00:00Z","limits":{"max_files":8,"max_file_bytes":20971520,"max_batch_bytes":52428800}}""",
            )
            val chunk = awaitFrameCount("upload_chunk", 1).last()
            emitResult(
                "upload_chunk_result",
                chunk["request_id"]!!.jsonPrimitive.content,
                """{"file_index":0,"next_sequence":1,"received_bytes":${bytes.size}}""",
            )
            val finish = awaitFrameCount("upload_finish", 1).last()
            emitResult(
                "upload_finish_result",
                finish["request_id"]!!.jsonPrimitive.content,
                """{"attachments":[{"ref":"r_${sha256Hex(bytes).take(31)}","name":"$name","media_type":"text/plain","bytes":${bytes.size},"sha256":"${sha256Hex(bytes)}","expires_at":"2999-01-01T00:00:00Z"}]}""",
            )
        }

        /** Typed `session.request` frames land on `requests`, not `sentRaw`. */
        fun awaitRequest(type: String) {
            val deadline = System.currentTimeMillis() + 5_000
            while (handle().requests.none { it.type == type } &&
                System.currentTimeMillis() < deadline
            ) {
                testScope.runCurrent()
                Thread.sleep(5)
            }
            check(handle().requests.any { it.type == type }) {
                "no typed request '$type' sent"
            }
        }

        /** Let the VM's initial DataStore draft read settle before driving. */
        fun settleDraft() {
            Thread.sleep(50)
            testScope.runCurrent()
        }

        suspend fun emitResult(type: String, requestId: String, resultJson: String) {
            handle().emit(
                json("""{"type":"$type","request_id":"$requestId","result":$resultJson}"""),
            )
            pump()
        }
    }

    private fun Harness.viewModel(): FeedViewModel =
        FeedViewModel(paneId, repository, drafts, uploads)

    @Test
    fun `picked files upload and Attachment refs land in the composer draft`() = runTest {
        val body = "attachment body".toByteArray()
        val h = Harness(this, tmp.root, mapOf("content://docs/notes.txt" to body))
        h.connectReady()
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.pump()
        h.settleDraft()
        assertThat(vm.uiState.value.canAttach).isTrue()

        vm.onDraftChange("look at this")
        vm.selectAttachments(listOf("content://docs/notes.txt"))
        h.answerUpload("notes.txt", body)
        // Let the upload coroutine finish appending refs.
        val expectedRef = "r_${sha256Hex(body).take(31)}"
        val deadline = System.currentTimeMillis() + 5_000
        while (!vm.uiState.value.composerDraft.contains("Attachment:") &&
            System.currentTimeMillis() < deadline
        ) {
            runCurrent()
            Thread.sleep(5)
        }
        assertThat(vm.uiState.value.composerDraft)
            .isEqualTo("look at this\nAttachment: $expectedRef\n")
        // Clean batch cleared itself — chips disappear once refs landed.
        assertThat(vm.uiState.value.attachments.items).isEmpty()
        assertThat(vm.uiState.value.uploadStatus).contains("Attached notes.txt")
        assertThat(vm.uiState.value.uploadError).isFalse()
    }

    @Test
    fun `sendPrompt submits the draft with Attachment ref lines`() = runTest {
        val body = "payload".toByteArray()
        val h = Harness(this, tmp.root, mapOf("content://docs/a.txt" to body))
        h.connectReady()
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.pump()
        h.settleDraft()

        vm.onDraftChange("check this file")
        vm.selectAttachments(listOf("content://docs/a.txt"))
        h.answerUpload("a.txt", body)
        val deadline = System.currentTimeMillis() + 5_000
        while (!vm.uiState.value.composerDraft.contains("Attachment:") &&
            System.currentTimeMillis() < deadline
        ) {
            runCurrent()
            Thread.sleep(5)
        }

        vm.sendPrompt()
        // submit_prompt resolves through the typed `request` path — the fake
        // answers ok immediately, so the draft clears without extra driving.
        h.awaitRequest("submit_prompt")
        val submitted = h.handle().requests.last { it.type == "submit_prompt" }
        val expectedRef = "r_${sha256Hex(body).take(31)}"
        assertThat(submitted.text)
            .isEqualTo("check this file\nAttachment: $expectedRef")
        assertThat(vm.uiState.value.composerDraft).isEmpty()
        assertThat(h.uploads.itemsNow(h.paneId)).isEmpty()
    }

    @Test
    fun `clearAttachments during a staged upload sends upload_cancel`() = runTest {
        val body = ByteArray(64) { it.toByte() }
        val h = Harness(this, tmp.root, mapOf("content://docs/a.txt" to body))
        h.connectReady()
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.pump()

        vm.selectAttachments(listOf("content://docs/a.txt"))
        val begin = h.awaitFrameCount("upload_begin", 1).last()
        h.emitResult(
            "upload_begin_result",
            begin["request_id"]!!.jsonPrimitive.content,
            """{"upload_id":"up-live","chunk_bytes":8,"expires_at":"2999-01-01T00:00:00Z","limits":{"max_files":8,"max_file_bytes":20971520,"max_batch_bytes":52428800}}""",
        )
        h.awaitFrameCount("upload_chunk", 1)
        vm.clearAttachments()
        val cancel = h.awaitFrameCount("upload_cancel", 1).last()
        assertThat(cancel["upload_id"]?.jsonPrimitive?.content).isEqualTo("up-live")
        h.emitResult(
            "upload_cancel_result",
            cancel["request_id"]!!.jsonPrimitive.content,
            "{}",
        )
        val deadline = System.currentTimeMillis() + 5_000
        while (vm.uiState.value.attachments.items.isNotEmpty() &&
            System.currentTimeMillis() < deadline
        ) {
            runCurrent()
            Thread.sleep(5)
        }
        assertThat(vm.uiState.value.attachments.items).isEmpty()
    }

    @Test
    fun `session loss marks the tray item interrupted and reports state_unknown`() = runTest {
        val body = ByteArray(32) { it.toByte() }
        val h = Harness(this, tmp.root, mapOf("content://docs/a.txt" to body))
        h.connectReady()
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.pump()

        vm.selectAttachments(listOf("content://docs/a.txt"))
        val begin = h.awaitFrameCount("upload_begin", 1).last()
        h.emitResult(
            "upload_begin_result",
            begin["request_id"]!!.jsonPrimitive.content,
            """{"upload_id":"up-loss","chunk_bytes":8,"expires_at":"2999-01-01T00:00:00Z","limits":{"max_files":8,"max_file_bytes":20971520,"max_batch_bytes":52428800}}""",
        )
        h.awaitFrameCount("upload_chunk", 1)
        h.handle().disconnect()
        h.pump()
        val deadline = System.currentTimeMillis() + 5_000
        while (vm.uiState.value.attachments.items.firstOrNull()?.state !=
            AttachmentItemState.INTERRUPTED && System.currentTimeMillis() < deadline
        ) {
            runCurrent()
            Thread.sleep(5)
        }
        val item = vm.uiState.value.attachments.items.single()
        assertThat(item.state).isEqualTo(AttachmentItemState.INTERRUPTED)
        assertThat(item.issue?.code).isEqualTo("attachment_upload_state_unknown")
        assertThat(vm.uiState.value.uploadError).isTrue()
        assertThat(vm.uiState.value.uploadStatus)
            .isEqualTo("Upload progress is uncertain. Restart it from the beginning; it will not resume automatically.")
        // The attach affordance disappears while the relay is down.
        assertThat(vm.uiState.value.canAttach).isFalse()
    }

    @Test
    fun `a blocked agent cannot attach`() = runTest {
        val h = Harness(this, tmp.root, mapOf("content://docs/a.txt" to ByteArray(4)))
        h.connectReady()
        h.handle().emit(
            json(
                """{"type":"blocked","pane_id":"%1","attention_kind":"approval","prompt":"Allow?","options":["Allow","Deny"],"event_id":"ev1","approval_fingerprint":"afp","server_session_id":"ss1","terminal_id":"t1","generation":3}""",
            ),
        )
        h.pump()
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.pump()
        assertThat(vm.uiState.value.canAttach).isFalse()
    }
}
