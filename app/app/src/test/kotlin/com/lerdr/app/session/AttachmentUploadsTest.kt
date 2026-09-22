package com.lerdr.app.session

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import com.google.common.truth.Truth.assertThat
import java.io.ByteArrayInputStream
import java.io.File
import java.io.InputStream
import java.security.MessageDigest
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.async
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonPrimitive
import lerdr.core.data.RelayEndpoint
import lerdr.core.data.RelayRegistry
import lerdr.core.model.UploadAttachment
import lerdr.core.protocol.LerdrJson
import lerdr.core.store.AgentStore
import lerdr.core.store.ConnectionStore
import lerdr.core.store.WorkspaceStore
import lerdr.core.store.clientPaneId
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
class AttachmentUploadsTest {

    @get:Rule
    val tmp = TemporaryFolder()

    private class Harness(
        private val testScope: TestScope,
        tmpDir: File,
        source: AttachmentSource,
        limits: AttachmentLimits = AttachmentLimits(),
        clock: () -> Long = System::currentTimeMillis,
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
        val uploads = AttachmentUploads(scope, repository, source, clock, limits)

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
                    """{"type":"push_config","capabilities":["pane_realtime_delta"],"inventory":{"state":"ready"}}""",
                ),
            )
            handle().emit(
                json(
                    """{"type":"agents","agents":[{"pane_id":"%1","raw_pane_id":"%1","terminal_id":"t1","server_session_id":"ss1","generation":3,"agent":"claude","name":"claude","status":"working","cwd":"/home/u/lerdr","project":"lerdr","workspace_id":"w1","updated_at":100}]}""",
                ),
            )
            pump()
        }

        fun framesOf(type: String) = frames(handle()).filter {
            it["type"]?.jsonPrimitive?.content == type
        }

        /** Poll on a real clock — `Dispatchers.IO` stream reads hop off-scheduler. */
        fun awaitFrameCount(type: String, count: Int): List<JsonObject> {
            val deadline = System.currentTimeMillis() + 5_000
            while (framesOf(type).size < count && System.currentTimeMillis() < deadline) {
                testScope.runCurrent()
                Thread.sleep(5)
            }
            val found = framesOf(type)
            check(found.size >= count) {
                "expected $count '$type' frames, saw ${found.size}"
            }
            return found
        }

        suspend fun emitResult(type: String, requestId: String, resultJson: String) {
            handle().emit(
                json("""{"type":"$type","request_id":"$requestId","result":$resultJson}"""),
            )
            pump()
        }

        suspend fun emitError(type: String, requestId: String, code: String) {
            handle().emit(
                json("""{"type":"$type","request_id":"$requestId","error":{"code":"$code"}}"""),
            )
            pump()
        }

        /** Drives one whole upload run: begin → chunks → finish. */
        suspend fun answerUpload(
            chunkBytes: Int = 256 * 1024,
            attachments: List<Pair<String, ByteArray>>,
            uploadId: String = "upload-1",
        ) {
            val begin = awaitFrameCount("upload_begin", 1).last()
            emitResult(
                "upload_begin_result",
                begin["request_id"]!!.jsonPrimitive.content,
                """{"upload_id":"$uploadId","chunk_bytes":$chunkBytes,"expires_at":"2999-01-01T00:00:00Z","limits":{"max_files":8,"max_file_bytes":20971520,"max_batch_bytes":52428800}}""",
            )
            var chunkCount = 0
            var finished = false
            var guard = 0
            while (!finished && guard++ < 512) {
                val sent = framesOf("upload_chunk")
                if (sent.size > chunkCount) {
                    val chunk = sent[chunkCount]
                    chunkCount += 1
                    emitResult(
                        "upload_chunk_result",
                        chunk["request_id"]!!.jsonPrimitive.content,
                        """{"file_index":${chunk["file_index"]},"next_sequence":${chunk["sequence"]!!.jsonPrimitive.content.toInt() + 1},"received_bytes":${receivedFor(attachments, sent.take(chunkCount))}}""",
                    )
                    continue
                }
                val finishes = framesOf("upload_finish")
                if (finishes.isNotEmpty()) {
                    val finish = finishes.last()
                    emitResult(
                        "upload_finish_result",
                        finish["request_id"]!!.jsonPrimitive.content,
                        """{"attachments":${attachmentsJson(attachments)}}""",
                    )
                    finished = true
                    continue
                }
                testScope.runCurrent()
                Thread.sleep(5)
            }
        }

        private fun receivedFor(
            files: List<Pair<String, ByteArray>>,
            chunks: List<JsonObject>,
        ): Long {
            val last = chunks.last()
            val fileIndex = last["file_index"]!!.jsonPrimitive.content.toInt()
            val chunkBytes = java.util.Base64.getDecoder()
                .decode(last["data"]!!.jsonPrimitive.content).size
            // received_bytes is cumulative within the file — the sum of all
            // decoded bytes this file index has received so far.
            return chunks
                .filter { it["file_index"]!!.jsonPrimitive.content.toInt() == fileIndex }
                .sumOf {
                    java.util.Base64.getDecoder()
                        .decode(it["data"]!!.jsonPrimitive.content).size.toLong()
                }
                .also {
                    check(chunkBytes > 0)
                    check(fileIndex < files.size)
                }
        }

        private fun attachmentsJson(files: List<Pair<String, ByteArray>>): String =
            files.joinToString(",", "[", "]") { (name, bytes) ->
                """{"ref":"ref_${sha256Hex(bytes).take(28)}","name":"$name","media_type":"text/plain","bytes":${bytes.size},"sha256":"${sha256Hex(bytes)}","expires_at":"2999-01-01T00:00:00Z"}"""
            }
    }

    private fun sourceOf(vararg files: Pair<String, ByteArray>) = FakeAttachmentSource(
        files.toMap(),
        names = files.associate { it.first to it.first.substringAfterLast('/') },
    )

    @Test
    fun `begin chunk finish writes oracle wire fields`() = runTest {
        val body = "hello attachment".toByteArray()
        val source = sourceOf("content://docs/a.txt" to body)
        val h = Harness(this, tmp.root, source)
        h.connectReady()
        h.uploads.select(h.paneId, listOf("content://docs/a.txt"))

        val pending = backgroundScope.async { h.uploads.upload(h.paneId) }
        val begin = h.awaitFrameCount("upload_begin", 1).last()
        assertThat(begin["protocol"]?.jsonPrimitive?.content).isEqualTo("3")
        val target = begin["target"] as JsonObject
        assertThat(target["server_session_id"]?.jsonPrimitive?.content).isEqualTo("ss1")
        assertThat(target["pane_id"]?.jsonPrimitive?.content).isEqualTo("%1")
        assertThat(target["terminal_id"]?.jsonPrimitive?.content).isEqualTo("t1")
        assertThat(target["generation"]?.jsonPrimitive?.content).isEqualTo("3")
        val spec = begin["files"]!!.jsonArray.single() as JsonObject
        assertThat(spec["name"]?.jsonPrimitive?.content).isEqualTo("a.txt")
        assertThat(spec["media_type"]?.jsonPrimitive?.content).isEqualTo("text/plain")
        assertThat(spec["bytes"]?.jsonPrimitive?.content).isEqualTo("${body.size}")

        h.answerUpload(attachments = listOf("a.txt" to body))
        val attachments: List<UploadAttachment> = pending.await()

        assertThat(attachments).hasSize(1)
        assertThat(attachments[0].ref).startsWith("ref_")
        assertThat(attachments[0].sha256).isEqualTo(sha256Hex(body))

        val chunk = h.framesOf("upload_chunk").single()
        assertThat(chunk["upload_id"]?.jsonPrimitive?.content).isEqualTo("upload-1")
        assertThat(chunk["file_index"]?.jsonPrimitive?.content).isEqualTo("0")
        assertThat(chunk["sequence"]?.jsonPrimitive?.content).isEqualTo("0")
        assertThat(
            java.util.Base64.getDecoder().decode(chunk["data"]!!.jsonPrimitive.content),
        ).isEqualTo(body)
        assertThat(chunk["sha256"]?.jsonPrimitive?.content).isEqualTo(sha256Hex(body))

        val finish = h.framesOf("upload_finish").single()
        val digest = finish["files"]!!.jsonArray.single() as JsonObject
        assertThat(digest["file_index"]?.jsonPrimitive?.content).isEqualTo("0")
        assertThat(digest["sha256"]?.jsonPrimitive?.content).isEqualTo(sha256Hex(body))
        assertThat(digest["file_index"]).isNotNull()
        assertThat("upload_id" in finish).isTrue()

        val state = h.uploads.state(h.paneId).value
        assertThat(state.items.single().state).isEqualTo(AttachmentItemState.READY)
        assertThat(state.items.single().progress).isEqualTo(1f)
    }

    @Test
    fun `server chunk_bytes drives slice size and a global sequence`() = runTest {
        // Two files, 10 + 6 bytes, server chunk_bytes 4 → 3 + 2 chunks,
        // sequence counts across files: 0..4.
        val a = ByteArray(10) { it.toByte() }
        val b = ByteArray(6) { (it + 40).toByte() }
        val source = sourceOf(
            "content://docs/a.bin" to a,
            "content://docs/b.bin" to b,
        )
        val h = Harness(this, tmp.root, source)
        h.connectReady()
        // The batch's items must be selected before `upload` runs.
        h.uploads.select(h.paneId, listOf("content://docs/a.bin", "content://docs/b.bin"))

        val pending = backgroundScope.async { h.uploads.upload(h.paneId) }
        h.answerUpload(chunkBytes = 4, attachments = listOf("a.bin" to a, "b.bin" to b))
        val attachments = pending.await()

        val chunks = h.framesOf("upload_chunk")
        assertThat(chunks).hasSize(5)
        assertThat(chunks.map { it["sequence"]!!.jsonPrimitive.content })
            .containsExactly("0", "1", "2", "3", "4").inOrder()
        assertThat(chunks.map { it["file_index"]!!.jsonPrimitive.content })
            .containsExactly("0", "0", "0", "1", "1").inOrder()
        val sizes = chunks.map {
            java.util.Base64.getDecoder().decode(it["data"]!!.jsonPrimitive.content).size
        }
        assertThat(sizes).containsExactly(4, 4, 2, 4, 2).inOrder()
        assertThat(attachments.map { it.sha256 })
            .containsExactly(sha256Hex(a), sha256Hex(b)).inOrder()
    }

    @Test
    fun `validation rejects oversize empty unknown-mime and over-limit picks`() = runTest {
        val files = linkedMapOf(
            "content://docs/ok.txt" to ByteArray(10),
            "content://docs/empty.txt" to ByteArray(0),
            "content://docs/big.bin" to ByteArray(30),
            "content://docs/weird.xyz" to ByteArray(5),
        )
        val source = FakeAttachmentSource(
            files,
            names = files.keys.associateWith { it.substringAfterLast('/') },
            mimes = mapOf("content://docs/weird.xyz" to "application/x-weird"),
        )
        val h = Harness(
            this, tmp.root, source,
            limits = AttachmentLimits(
                maxFiles = 4,
                maxFileBytes = 20,
                maxBatchBytes = 40,
                maxChunkBytes = 256 * 1024,
            ),
        )
        h.connectReady()
        h.uploads.select(h.paneId, files.keys.toList())
        val state = h.uploads.state(h.paneId).value
        // ok(10) + empty + big(30 over 20) + weird-mime → 4 items; batch limit
        // never trips (4 == maxFiles).
        assertThat(state.items.map { it.state }).containsExactly(
            AttachmentItemState.SELECTED,
            AttachmentItemState.REJECTED,
            AttachmentItemState.REJECTED,
            AttachmentItemState.REJECTED,
        ).inOrder()
        assertThat(state.items[1].issue?.code).isEqualTo("attachment_file_empty")
        assertThat(state.items[2].issue?.code).isEqualTo("attachment_file_too_large")
        assertThat(state.items[3].issue?.code).isEqualTo("attachment_unknown_mime")
    }

    @Test
    fun `over-selection marks the batch issue and rejects the tail`() = runTest {
        val files = (1..5).associate { "content://docs/f$it.txt" to ByteArray(4) }
        val h = Harness(
            this, tmp.root, FakeAttachmentSource(files),
            limits = AttachmentLimits(maxFiles = 3, maxFileBytes = 100, maxBatchBytes = 1000),
        )
        h.connectReady()
        h.uploads.select(h.paneId, files.keys.toList())
        val state = h.uploads.state(h.paneId).value
        assertThat(state.issue?.code).isEqualTo("attachment_batch_limit")
        assertThat(state.items.take(3).map { it.state })
            .containsExactly(
                AttachmentItemState.SELECTED,
                AttachmentItemState.SELECTED,
                AttachmentItemState.SELECTED,
            )
        assertThat(state.items.drop(3).map { it.state })
            .containsExactly(AttachmentItemState.REJECTED, AttachmentItemState.REJECTED)
        assertThat(state.items[3].issue?.code).isEqualTo("attachment_batch_limit")
    }

    @Test
    fun `over-batch files are rejected after earlier sizes accumulate`() = runTest {
        val files = linkedMapOf(
            "content://docs/a.txt" to ByteArray(30),
            "content://docs/b.txt" to ByteArray(30),
        )
        val h = Harness(
            this, tmp.root, FakeAttachmentSource(files),
            limits = AttachmentLimits(maxFiles = 8, maxFileBytes = 100, maxBatchBytes = 50),
        )
        h.connectReady()
        h.uploads.select(h.paneId, files.keys.toList())
        val state = h.uploads.state(h.paneId).value
        assertThat(state.items[0].state).isEqualTo(AttachmentItemState.SELECTED)
        assertThat(state.items[1].state).isEqualTo(AttachmentItemState.REJECTED)
        assertThat(state.items[1].issue?.code).isEqualTo("attachment_batch_too_large")
    }

    @Test
    fun `invalid begin limits interrupt with attachment_invalid_response`() = runTest {
        val body = ByteArray(8) { it.toByte() }
        val source = sourceOf("content://docs/a.txt" to body)
        val h = Harness(this, tmp.root, source)
        h.connectReady()
        h.uploads.select(h.paneId, listOf("content://docs/a.txt"))

        val pending = backgroundScope.async {
            runCatching { h.uploads.upload(h.paneId) }
        }
        val begin = h.awaitFrameCount("upload_begin", 1).last()
        // chunk_bytes above the client cap — the oracle rejects the session.
        h.emitResult(
            "upload_begin_result",
            begin["request_id"]!!.jsonPrimitive.content,
            """{"upload_id":"up-bad","chunk_bytes":99999999,"expires_at":"2999-01-01T00:00:00Z","limits":{"max_files":8,"max_file_bytes":20971520,"max_batch_bytes":52428800}}""",
        )
        val result = pending.await()
        assertThat(result.isFailure).isTrue()
        val item = h.uploads.state(h.paneId).value.items.single()
        assertThat(item.state).isEqualTo(AttachmentItemState.INTERRUPTED)
        assertThat(item.issue?.code).isEqualTo("attachment_invalid_response")
        // The staged session id is kept so a later cancel() can discard it.
        // `cancel` suspends until the relay answers — drive it async.
        val cancelJob = backgroundScope.async { h.uploads.cancel(h.paneId) }
        val cancel = h.awaitFrameCount("upload_cancel", 1).last()
        assertThat(cancel["upload_id"]?.jsonPrimitive?.content).isEqualTo("up-bad")
        val requestId = cancel["request_id"]!!.jsonPrimitive.content
        h.emitResult("upload_cancel_result", requestId, "{}")
        cancelJob.await()
        assertThat(h.uploads.state(h.paneId).value.items).isEmpty()
    }

    @Test
    fun `cancel during an in-flight chunk sends upload_cancel and clears`() = runTest {
        val body = ByteArray(32) { it.toByte() }
        val source = sourceOf("content://docs/a.txt" to body)
        val h = Harness(this, tmp.root, source)
        h.connectReady()
        h.uploads.select(h.paneId, listOf("content://docs/a.txt"))

        val pending = backgroundScope.async {
            runCatching { h.uploads.upload(h.paneId) }
        }
        val begin = h.awaitFrameCount("upload_begin", 1).last()
        h.emitResult(
            "upload_begin_result",
            begin["request_id"]!!.jsonPrimitive.content,
            """{"upload_id":"up-live","chunk_bytes":8,"expires_at":"2999-01-01T00:00:00Z","limits":{"max_files":8,"max_file_bytes":20971520,"max_batch_bytes":52428800}}""",
        )
        h.awaitFrameCount("upload_chunk", 1)
        // `cancel` suspends until `upload_cancel_result` — drive it async.
        val cancelJob = backgroundScope.async { h.uploads.cancel(h.paneId) }
        val cancel = h.awaitFrameCount("upload_cancel", 1).last()
        assertThat(cancel["upload_id"]?.jsonPrimitive?.content).isEqualTo("up-live")
        h.emitResult(
            "upload_cancel_result",
            cancel["request_id"]!!.jsonPrimitive.content,
            "{}",
        )
        cancelJob.await()
        // The upload coroutine was cancelled — its deferred may complete
        // cancelled or with the caught failure; either is "not a success".
        val outcome = runCatching { pending.await() }
        assertThat(outcome.isFailure || outcome.getOrThrow().isFailure).isTrue()
        assertThat(h.uploads.state(h.paneId).value.items).isEmpty()
    }

    @Test
    fun `session loss marks the batch interrupted with state_unknown`() = runTest {
        val body = ByteArray(16) { it.toByte() }
        val source = sourceOf("content://docs/a.txt" to body)
        val h = Harness(this, tmp.root, source)
        h.connectReady()
        h.uploads.select(h.paneId, listOf("content://docs/a.txt"))

        val pending = backgroundScope.async {
            runCatching { h.uploads.upload(h.paneId) }
        }
        val begin = h.awaitFrameCount("upload_begin", 1).last()
        h.emitResult(
            "upload_begin_result",
            begin["request_id"]!!.jsonPrimitive.content,
            """{"upload_id":"up-loss","chunk_bytes":8,"expires_at":"2999-01-01T00:00:00Z","limits":{"max_files":8,"max_file_bytes":20971520,"max_batch_bytes":52428800}}""",
        )
        h.awaitFrameCount("upload_chunk", 1)
        h.handle().disconnect()
        h.pump()
        assertThat(pending.await().isFailure).isTrue()
        val item = h.uploads.state(h.paneId).value.items.single()
        assertThat(item.state).isEqualTo(AttachmentItemState.INTERRUPTED)
        assertThat(item.issue?.code).isEqualTo("attachment_upload_state_unknown")
        assertThat(h.uploads.state(h.paneId).value.canRestart).isTrue()
    }

    @Test
    fun `begin-side attachment error code maps through`() = runTest {
        val body = ByteArray(4)
        val source = sourceOf("content://docs/a.txt" to body)
        val h = Harness(this, tmp.root, source)
        h.connectReady()
        h.uploads.select(h.paneId, listOf("content://docs/a.txt"))

        val pending = backgroundScope.async {
            runCatching { h.uploads.upload(h.paneId) }
        }
        val begin = h.awaitFrameCount("upload_begin", 1).last()
        h.emitError(
            "upload_begin_result",
            begin["request_id"]!!.jsonPrimitive.content,
            "attachment_upload_busy",
        )
        assertThat(pending.await().isFailure).isTrue()
        val item = h.uploads.state(h.paneId).value.items.single()
        assertThat(item.state).isEqualTo(AttachmentItemState.INTERRUPTED)
        assertThat(item.issue?.code).isEqualTo("attachment_upload_busy")
    }

    @Test
    fun `finish attachment mismatch interrupts with invalid_response`() = runTest {
        val body = ByteArray(6) { it.toByte() }
        val source = sourceOf("content://docs/a.txt" to body)
        val h = Harness(this, tmp.root, source)
        h.connectReady()
        h.uploads.select(h.paneId, listOf("content://docs/a.txt"))

        val pending = backgroundScope.async {
            runCatching { h.uploads.upload(h.paneId) }
        }
        val begin = h.awaitFrameCount("upload_begin", 1).last()
        h.emitResult(
            "upload_begin_result",
            begin["request_id"]!!.jsonPrimitive.content,
            """{"upload_id":"up-mismatch","chunk_bytes":256,"expires_at":"2999-01-01T00:00:00Z","limits":{"max_files":8,"max_file_bytes":20971520,"max_batch_bytes":52428800}}""",
        )
        val chunk = h.awaitFrameCount("upload_chunk", 1).last()
        h.emitResult(
            "upload_chunk_result",
            chunk["request_id"]!!.jsonPrimitive.content,
            """{"file_index":0,"next_sequence":1,"received_bytes":6}""",
        )
        val finish = h.awaitFrameCount("upload_finish", 1).last()
        h.emitResult(
            "upload_finish_result",
            finish["request_id"]!!.jsonPrimitive.content,
            """{"attachments":[{"ref":"ref_bad","name":"a.txt","media_type":"text/plain","bytes":6,"sha256":"deadbeef","expires_at":"2999-01-01T00:00:00Z"}]}""",
        )
        assertThat(pending.await().isFailure).isTrue()
        val item = h.uploads.state(h.paneId).value.items.single()
        assertThat(item.state).isEqualTo(AttachmentItemState.INTERRUPTED)
        assertThat(item.issue?.code).isEqualTo("attachment_invalid_response")
    }

    @Test
    fun `remove drops one item and reorders the batch`() = runTest {
        val files = linkedMapOf(
            "content://docs/a.txt" to ByteArray(2),
            "content://docs/b.txt" to ByteArray(2),
        )
        val h = Harness(this, tmp.root, FakeAttachmentSource(files))
        h.connectReady()
        h.uploads.select(h.paneId, files.keys.toList())
        val first = h.uploads.state(h.paneId).value.items.first()
        h.uploads.remove(h.paneId, first.clientId)
        val state = h.uploads.state(h.paneId).value
        assertThat(state.items).hasSize(1)
        assertThat(state.items[0].name).isEqualTo("b.txt")
        assertThat(state.items[0].order).isEqualTo(0)
    }

    @Test
    fun `canAttachTo requires the exact target tuple`() = runTest {
        val h = Harness(this, tmp.root, sourceOf("content://docs/a.txt" to ByteArray(4)))
        h.connectReady()
        assertThat(h.repository.canAttachTo(h.paneId)).isTrue()
        assertThat(h.repository.canAttachTo("r1::%9")).isFalse()
        // An agent without a complete target tuple cannot attach.
        h.handle().emit(
            json(
                """{"type":"agents","agents":[{"pane_id":"%2","raw_pane_id":"%2","agent":"claude","name":"partial","status":"working","updated_at":101}]}""",
            ),
        )
        h.pump()
        val partial = clientPaneId("r1", "%2")
        assertThat(h.repository.canAttachTo(partial)).isFalse()
    }

    @Test
    fun `upload to a targetless agent fails before the wire`() = runTest {
        val h = Harness(this, tmp.root, sourceOf("content://docs/a.txt" to ByteArray(4)))
        h.connectReady()
        h.handle().emit(
            json(
                """{"type":"agents","agents":[{"pane_id":"%2","raw_pane_id":"%2","agent":"claude","name":"partial","status":"working","updated_at":101}]}""",
            ),
        )
        h.pump()
        val partial = clientPaneId("r1", "%2")
        h.uploads.select(partial, listOf("content://docs/a.txt"))
        val result = runCatching { h.uploads.upload(partial) }
        assertThat(result.isFailure).isTrue()
        assertThat(h.framesOf("upload_begin")).isEmpty()
    }
}
