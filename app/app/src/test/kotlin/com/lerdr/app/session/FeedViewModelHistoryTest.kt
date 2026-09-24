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
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import lerdr.core.conversation.ConversationBrowseState
import lerdr.core.data.DeviceRole
import lerdr.core.data.DraftStore
import lerdr.core.data.RelayDeviceCredential
import lerdr.core.data.RelayEndpoint
import lerdr.core.data.RelayRegistry
import lerdr.core.model.CommandResultMessage
import lerdr.core.model.Inbound
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

private fun historyJson(raw: String): JsonObject =
    LerdrJson.parseToJsonElement(raw) as JsonObject

private fun historyEntry(id: String, text: String) =
    """{"id":"$id","timestamp":"2026-01-01T10:00:00Z","role":"user","text":"$text"}"""

private fun readyPage(
    entries: List<String> = emptyList(),
    nextCursor: String = "",
    hasMore: Boolean = false,
    diagnostics: String? = null,
    available: Boolean = true,
    reasonCode: String = "",
    reason: String = "",
): JsonObject = historyJson(
    """{"available":$available,"has_more":$hasMore,"entries":[${entries.joinToString(",")}],"next_cursor":"$nextCursor","state":"ready","mode":"recent","reason_code":"$reasonCode","reason":"$reason"${diagnostics?.let { ",\"diagnostics\":$it" } ?: ""}}""",
)

private fun preparingPage(
    cursor: String = "prep-1",
    phase: String = "indexing",
    scanned: Long = 512,
    source: Long = 2_048,
): JsonObject = historyJson(
    """{"available":true,"has_more":false,"entries":[],"next_cursor":"$cursor","state":"preparing","mode":"recent","reason_code":"","reason":"","progress":{"phase":"$phase","scanned_bytes":$scanned,"source_bytes":$source}}""",
)

private fun failedPage(
    code: String,
    message: String,
    retryable: Boolean,
    nextCursor: String = "",
): JsonObject = historyJson(
    """{"available":true,"has_more":false,"entries":[],"next_cursor":"$nextCursor","state":"failed","mode":"recent","reason_code":"","reason":"","error":{"code":"$code","message":"$message","retryable":$retryable}}""",
)

/**
 * Conversation-history demand loop — the oracle's `get_conversation_history`
 * controller ported: diagnostics surface, preparing-page polling, stall
 * pause, cancel/continue, cursorless reload, and `retry` wire re-issues.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class FeedViewModelHistoryTest {

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

        suspend fun connectReady() {
            repository.connect(endpoint)
            handle().connect()
            handle().emit(
                historyJson(
                    """{"type":"push_config","capabilities":[],"inventory":{"state":"ready"}}""",
                ),
            )
            handle().emit(
                historyJson(
                    """{"type":"agents","agents":[{"pane_id":"%1","raw_pane_id":"%1","terminal_id":"t1","server_session_id":"ss1","generation":3,"agent":"claude","name":"claude","status":"idle","cwd":"/home/u/lerdr","project":"lerdr","workspace_id":"w1","conversation_history_available":true,"updated_at":100}]}""",
                ),
            )
            pump()
        }

        /** Typed `request` frames — `get_conversation_history` lands here. */
        fun historyRequests(): List<Inbound> =
            handle().requests.filter { it.type == "get_conversation_history" }

        /**
         * Answers `get_conversation_history` from [pages] in order; once the
         * queue drains the last page repeats (preparation polls ask forever).
         */
        fun answerHistory(vararg pages: JsonObject) {
            val queue = ArrayDeque(pages.toList())
            var last: JsonObject? = null
            handle().responder = { message ->
                if (message.type == "get_conversation_history") {
                    val page = queue.removeFirstOrNull() ?: last ?: readyPage()
                    last = page
                    CommandResultMessage(
                        action = message.type,
                        ok = true,
                        phase = CommandResultMessage.PHASE_COMPLETED,
                        requestId = message.requestId,
                        data = page,
                    )
                } else {
                    CommandResultMessage(
                        action = message.type,
                        ok = true,
                        phase = CommandResultMessage.PHASE_COMPLETED,
                        requestId = message.requestId,
                    )
                }
            }
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

    // ── pages + diagnostics ─────────────────────────────────────────

    @Test
    fun `ready page applies entries and paging fields`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady()
        h.answerHistory(
            readyPage(
                entries = listOf(historyEntry("e1", "one"), historyEntry("e2", "two")),
                nextCursor = "c1",
                hasMore = true,
            ),
        )
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.pump()

        val state = vm.uiState.value
        assertThat(state.entries.map { it.id }).containsExactly("e1", "e2").inOrder()
        assertThat(state.hasMoreHistory).isTrue()
        assertThat(state.historyLoading).isFalse()
        assertThat(state.historyError).isNull()
        assertThat(state.browseState).isEqualTo(ConversationBrowseState.READY)
        assertThat(state.preparationPaused).isFalse()
        // The cursorless head request carries the wire page size.
        val request = h.historyRequests().single()
        assertThat(request.cursor).isNull()
        assertThat(request.limit).isEqualTo(200)
        assertThat(request.retry).isFalse()
    }

    @Test
    fun `page diagnostics surface on the feed state`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady()
        h.answerHistory(
            readyPage(
                entries = listOf(historyEntry("e1", "one")),
                diagnostics = """{"oversized_records":2,"corrupt_records":1,"omitted_tools":3,"omitted_payloads":4,"plan_corrupt":true,"source_truncated":true,"continuation_incomplete":true,"continuation_reason":"invalid_link"}""",
            ),
        )
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.pump()

        val diagnostics = vm.uiState.value.historyDiagnostics
        assertThat(diagnostics.oversizedRecords).isEqualTo(2)
        assertThat(diagnostics.corruptRecords).isEqualTo(1)
        assertThat(diagnostics.omittedTools).isEqualTo(3)
        assertThat(diagnostics.omittedPayloads).isEqualTo(4)
        assertThat(diagnostics.planCorrupt).isTrue()
        assertThat(diagnostics.sourceTruncated).isTrue()
        assertThat(diagnostics.continuationIncomplete).isTrue()
        assertThat(diagnostics.continuationReason).isEqualTo("invalid_link")
    }

    @Test
    fun `older page prepends entries deduplicated by id`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady()
        h.answerHistory(
            readyPage(
                entries = listOf(historyEntry("e2", "two"), historyEntry("e3", "three")),
                nextCursor = "c1",
                hasMore = true,
            ),
            // The overlap row e2 is dropped by the merge.
            readyPage(
                entries = listOf(historyEntry("e1", "one"), historyEntry("e2", "two")),
                nextCursor = "c0",
                hasMore = true,
                diagnostics = """{"oversized_records":1}""",
            ),
        )
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.pump()

        vm.loadOlderHistory()
        h.pump()

        val state = vm.uiState.value
        assertThat(state.entries.map { it.id }).containsExactly("e1", "e2", "e3").inOrder()
        // Cursorful pages merge diagnostics into the window report.
        assertThat(state.historyDiagnostics.oversizedRecords).isEqualTo(1)
        val older = h.historyRequests().last()
        assertThat(older.cursor).isEqualTo(JsonPrimitive("c1"))
        assertThat(older.retry).isFalse()
    }

    @Test
    fun `loadOlderHistory refuses while an error is set`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady()
        h.answerHistory(
            failedPage("query_failed", "History query failed.", retryable = true),
        )
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.pump()

        vm.loadOlderHistory()
        h.pump()
        assertThat(h.historyRequests()).hasSize(1)
    }

    // ── preparation ─────────────────────────────────────────────────

    @Test
    fun `preparing page polls until the ready page resolves`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady()
        h.answerHistory(
            preparingPage(scanned = 512),
            preparingPage(scanned = 1_536),
            readyPage(entries = listOf(historyEntry("e1", "one"))),
        )
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.pump()

        // First preparing page applied — the poll loop owns the cursor.
        var state = vm.uiState.value
        assertThat(state.browseState)
            .isEqualTo(ConversationBrowseState.PREPARING)
        assertThat(state.browseProgress?.scannedBytes).isEqualTo(512)
        assertThat(state.browseProgress?.sourceBytes).isEqualTo(2_048)
        assertThat(state.hasMoreHistory).isTrue()
        assertThat(state.preparationPaused).isFalse()

        advanceTimeBy(1_000)
        runCurrent()
        state = vm.uiState.value
        assertThat(state.browseProgress?.scannedBytes).isEqualTo(1_536)
        // Progress changed — the stall counter restarts.
        assertThat(state.preparationPaused).isFalse()

        advanceTimeBy(1_000)
        runCurrent()
        state = vm.uiState.value
        assertThat(state.browseState)
            .isEqualTo(ConversationBrowseState.READY)
        assertThat(state.entries.map { it.id }).containsExactly("e1")

        // The polls re-issued the preparation cursor on the wire.
        val requests = h.historyRequests()
        assertThat(requests).hasSize(3)
        assertThat(requests.map { it.cursor })
            .containsExactly(null, JsonPrimitive("prep-1"), JsonPrimitive("prep-1")).inOrder()
    }

    @Test
    fun `unchanged preparation progress stalls after the poll cap`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady()
        // The same progress snapshot repeats — the responder keeps serving it.
        h.answerHistory(preparingPage())
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.pump()

        advanceUntilIdle()

        val state = vm.uiState.value
        assertThat(state.preparationPaused).isTrue()
        assertThat(state.historyErrorCode).isEqualTo("preparation_stalled")
        assertThat(state.historyErrorRetryable).isTrue()
        assertThat(state.historyError).contains("Preparation is paused")
        // 30 identical-progress polls, then the loop stops.
        assertThat(h.historyRequests()).hasSize(30)
    }

    @Test
    fun `cancelPreparation stops polling and continuePreparation resumes it`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady()
        h.answerHistory(
            preparingPage(),
            readyPage(entries = listOf(historyEntry("e1", "one"))),
        )
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.pump()
        assertThat(vm.uiState.value.browseState)
            .isEqualTo(ConversationBrowseState.PREPARING)

        vm.cancelPreparation()
        h.pump()

        var state = vm.uiState.value
        assertThat(state.preparationPaused).isTrue()
        assertThat(state.historyError).isNull()

        // No wire command — the paused loop stays dead across virtual time.
        advanceTimeBy(10_000)
        runCurrent()
        assertThat(h.historyRequests()).hasSize(1)

        vm.continuePreparation()
        h.pump()

        state = vm.uiState.value
        // The Continue request re-issued the stored preparation cursor.
        val requests = h.historyRequests()
        assertThat(requests).hasSize(2)
        assertThat(requests.last().cursor).isEqualTo(JsonPrimitive("prep-1"))
        assertThat(requests.last().retry).isFalse()
        assertThat(state.browseState)
            .isEqualTo(ConversationBrowseState.READY)
        assertThat(state.preparationPaused).isFalse()
        assertThat(state.entries.map { it.id }).containsExactly("e1")
    }

    @Test
    fun `recoverHistory continues after a preparation stall`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady()
        // Thirty unchanged preparing pages reach the stall cap; the Continue
        // then consumes the queued ready page.
        h.answerHistory(
            *Array(31) { i ->
                if (i < 30) preparingPage()
                else readyPage(entries = listOf(historyEntry("e1", "one")))
            },
        )
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.pump()

        advanceUntilIdle()
        assertThat(vm.uiState.value.historyErrorCode).isEqualTo("preparation_stalled")
        assertThat(vm.uiState.value.preparationPaused).isTrue()

        vm.recoverHistory()
        h.pump()

        val state = vm.uiState.value
        assertThat(state.browseState)
            .isEqualTo(ConversationBrowseState.READY)
        assertThat(state.historyError).isNull()
        assertThat(state.preparationPaused).isFalse()
        assertThat(h.historyRequests().last().cursor).isEqualTo(JsonPrimitive("prep-1"))
        assertThat(h.historyRequests().last().retry).isFalse()
    }

    // ── errors, retry, reload ───────────────────────────────────────

    @Test
    fun `failed page preserves the error code and retryability`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady()
        h.answerHistory(
            failedPage("cursor_expired", "The history cursor expired.", retryable = true),
        )
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.pump()

        val state = vm.uiState.value
        assertThat(state.historyError).isEqualTo("The history cursor expired.")
        assertThat(state.historyErrorCode).isEqualTo("cursor_expired")
        assertThat(state.historyErrorRetryable).isTrue()
        assertThat(state.browseState)
            .isEqualTo(ConversationBrowseState.FAILED)
    }

    @Test
    fun `recoverHistory re-issues the failed request with retry true`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady()
        h.answerHistory(
            readyPage(
                entries = listOf(historyEntry("e1", "one"), historyEntry("e2", "two")),
                nextCursor = "c1",
                hasMore = true,
            ),
            failedPage("query_failed", "History query failed.", retryable = true),
            readyPage(
                entries = listOf(historyEntry("e0", "zero")),
                nextCursor = "c0",
                hasMore = true,
            ),
        )
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.pump()

        vm.loadOlderHistory()
        h.pump()
        assertThat(vm.uiState.value.historyErrorCode).isEqualTo("query_failed")

        vm.recoverHistory()
        h.pump()

        val requests = h.historyRequests()
        assertThat(requests).hasSize(3)
        val retried = requests.last()
        assertThat(retried.cursor).isEqualTo(JsonPrimitive("c1"))
        assertThat(retried.retry).isTrue()
        val state = vm.uiState.value
        assertThat(state.historyError).isNull()
        assertThat(state.entries.map { it.id }).containsExactly("e0", "e1", "e2").inOrder()
    }

    @Test
    fun `recoverHistory ignores non-retryable errors`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady()
        h.answerHistory(
            failedPage("index_failed", "History indexing failed.", retryable = false),
        )
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.pump()

        vm.recoverHistory()
        h.pump()
        assertThat(h.historyRequests()).hasSize(1)
        assertThat(vm.uiState.value.historyErrorCode).isEqualTo("index_failed")
    }

    @Test
    fun `reload re-browses cursorless and replaces the window`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady()
        h.answerHistory(
            readyPage(
                entries = listOf(historyEntry("e2", "two")),
                nextCursor = "c1",
                hasMore = true,
            ),
            readyPage(entries = listOf(historyEntry("e1", "one"))),
            readyPage(entries = listOf(historyEntry("e5", "five"), historyEntry("e6", "six"))),
        )
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.pump()

        vm.loadOlderHistory()
        h.pump()
        assertThat(vm.uiState.value.entries.map { it.id })
            .containsExactly("e1", "e2").inOrder()

        // The oracle's reloadHistory/returnToLatest — a fresh head demand.
        vm.loadHistory()
        h.pump()

        val requests = h.historyRequests()
        assertThat(requests).hasSize(3)
        assertThat(requests.last().cursor).isNull()
        assertThat(requests.last().retry).isFalse()
        val state = vm.uiState.value
        assertThat(state.entries.map { it.id }).containsExactly("e5", "e6").inOrder()
        assertThat(state.hasMoreHistory).isFalse()
        assertThat(state.historyError).isNull()
    }

    @Test
    fun `cursorful unavailable page surfaces a retryable error`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady()
        h.answerHistory(
            readyPage(
                entries = listOf(historyEntry("e1", "one")),
                nextCursor = "c1",
                hasMore = true,
            ),
            readyPage(
                available = false,
                reasonCode = "source_unavailable",
                reason = "The older source is gone.",
            ),
        )
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.pump()

        vm.loadOlderHistory()
        h.pump()

        val state = vm.uiState.value
        // The loaded window stays; the failure is the recoverable error row.
        assertThat(state.entries.map { it.id }).containsExactly("e1")
        assertThat(state.historyError).isEqualTo("The older source is gone.")
        assertThat(state.historyErrorCode).isEqualTo("source_unavailable")
        assertThat(state.historyErrorRetryable).isTrue()
        assertThat(state.historyPageAvailable).isTrue()
    }

    @Test
    fun `cursorless unavailable page is the authoritative empty state`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady()
        h.answerHistory(
            readyPage(
                available = false,
                reasonCode = "invalid_provider",
                reason = "Conversation history is not available for this agent.",
            ),
        )
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.pump()

        val state = vm.uiState.value
        assertThat(state.historyPageAvailable).isFalse()
        assertThat(state.historyUnavailableReason)
            .isEqualTo("Conversation history is not available for this agent.")
        assertThat(state.entries).isEmpty()
        assertThat(state.historyError).isNull()
        assertThat(state.hasMoreHistory).isFalse()
    }

    @Test
    fun `transport failure becomes a retryable history_failed error`() = runTest {
        val h = Harness(this, tmp.root)
        h.credentials.seed("r1", credential(DeviceRole.CONTROLLER))
        h.repository.start()
        h.pump()
        h.connectReady()
        h.handle().responder = { message ->
            if (message.type == "get_conversation_history") {
                throw RuntimeException("socket went away")
            }
            CommandResultMessage(
                action = message.type,
                ok = true,
                phase = CommandResultMessage.PHASE_COMPLETED,
                requestId = message.requestId,
            )
        }
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.pump()

        val state = vm.uiState.value
        assertThat(state.historyError).isEqualTo("socket went away")
        assertThat(state.historyErrorCode).isEqualTo("history_failed")
        assertThat(state.historyErrorRetryable).isTrue()
    }
}
