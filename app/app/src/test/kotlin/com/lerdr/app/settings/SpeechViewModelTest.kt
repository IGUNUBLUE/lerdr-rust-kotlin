package com.lerdr.app.settings

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import com.google.common.truth.Truth.assertThat
import com.lerdr.app.session.FakeCredentialStore
import com.lerdr.app.session.FakeRelaySessionFactory
import com.lerdr.app.session.FakeRelaySessionHandle
import com.lerdr.app.session.SessionRepository
import com.lerdr.app.speech.RelaySpeechPlayer
import com.lerdr.app.speech.SpeechAudioSink
import com.lerdr.app.speech.SpeechExchange
import com.lerdr.app.speech.SpeechPhase
import com.lerdr.app.speech.SpeechPlaybackException
import com.lerdr.app.speech.SpeechSender
import java.io.File
import java.util.Base64
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonPrimitive
import lerdr.core.data.RelayEndpoint
import lerdr.core.data.RelayRegistry
import lerdr.core.data.RelayTransport
import lerdr.core.model.CommandResultMessage
import lerdr.core.protocol.LerdrJson
import lerdr.core.store.AgentStore
import lerdr.core.store.ConnectionStore
import lerdr.core.store.WorkspaceStore
import org.junit.After
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

private fun json(raw: String): JsonObject =
    LerdrJson.parseToJsonElement(raw) as JsonObject

private fun sentFrames(h: FakeRelaySessionHandle): List<JsonObject> =
    h.sentRaw.map { json(it) }

private fun sentTypes(h: FakeRelaySessionHandle) =
    sentFrames(h).map { it["type"]?.jsonPrimitive?.content }

/**
 * `SpeechViewModel` — settings intents, `adoptRelaySpeech` onboarding, the
 * voice catalog round trip (`speech_voices_list`/`speech_voice_*` →
 * `command_result.data`, plus unsolicited `speech_voices` broadcasts) and
 * player state surfacing. Session plumbing is the same fake harness
 * `SessionRepositoryTest` uses; the player's network/media seams are faked.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class SpeechViewModelTest {

    @get:Rule
    val tmp = TemporaryFolder()

    private val mainDispatcher = UnconfinedTestDispatcher()

    /** viewModelScope rides Dispatchers.Main — redirect it into the test. */
    @Before
    fun setMain() = Dispatchers.setMain(mainDispatcher)

    @After
    fun resetMain() = Dispatchers.resetMain()

    private class FakeExchange(
        private val result: CommandResultMessage? = null,
        private val failure: Throwable? = null,
        val gate: CompletableDeferred<Unit>? = null,
    ) : SpeechExchange {
        var cancelled = false
            private set

        override suspend fun await(): CommandResultMessage {
            gate?.await()
            failure?.let { throw it }
            return result ?: WAV_RESULT
        }

        override fun cancel() {
            cancelled = true
        }
    }

    private class FakeSink : SpeechAudioSink {
        val played = mutableListOf<ByteArray>()
        var interruptCount = 0
            private set

        override suspend fun play(wav: ByteArray) {
            played += wav
        }

        override fun interrupt() {
            interruptCount++
        }
    }

    private class Harness(
        private val testScope: TestScope,
        tmpDir: File,
    ) {
        private val scope = testScope.backgroundScope
        val credentials = FakeCredentialStore()
        private val dataStore = PreferenceDataStoreFactory.create(scope = scope) {
            File(tmpDir, "app.preferences_pb")
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
        val preferences = SpeechPreferences(dataStore).apply {
            deviceLanguage = { "en" }
        }

        // Player fakes — scripted exchanges, recording sink.
        val sends = mutableListOf<Triple<String, String, String>>()
        val exchanges = mutableListOf<FakeExchange>()
        val script = ArrayDeque<FakeExchange>()
        val sink = FakeSink()
        val player = RelaySpeechPlayer(
            scope = scope,
            enabled = preferences.enabled,
            language = preferences.language,
            sender = SpeechSender { relayId, text, language ->
                sends += Triple(relayId, text, language)
                (script.removeFirstOrNull() ?: FakeExchange()).also {
                    exchanges += it
                }
            },
            sink = sink,
        )

        val endpoint = RelayEndpoint(
            id = "r1",
            label = "workstation",
            host = "192.168.1.5",
            port = 7474,
            transport = RelayTransport.WEBSOCKET,
        )
        val origin = "ws://192.168.1.5:7474"

        fun pump() = testScope.runCurrent()

        fun handle(): FakeRelaySessionHandle =
            factory.handleFor(origin) ?: error("no session for $origin")

        fun viewModel() = SpeechViewModel("r1", repository, preferences, player)

        /** Poll on a real clock — DataStore IO is off the test scheduler. */
        fun await(condition: () -> Boolean) {
            val deadline = System.currentTimeMillis() + 5_000
            while (!condition() && System.currentTimeMillis() < deadline) {
                testScope.runCurrent()
                Thread.sleep(5)
            }
            check(condition()) { "condition not met within deadline" }
        }

        /**
         * Connect + `push_config` carrying speech capability flags and the
         * relay's speakable languages.
         */
        suspend fun connectSpeech(
            capabilities: List<String> =
                listOf("speech_synthesis", "speech_voice_management"),
            speechLanguages: List<String> = listOf("en", "fr"),
        ) {
            repository.connect(endpoint)
            val handle = handle()
            handle.connect()
            val caps = capabilities.joinToString(",") { "\"$it\"" }
            val langs = speechLanguages.joinToString(",") { "\"$it\"" }
            handle.emit(
                json(
                    """{"type":"push_config","version":"0.4.2","protocol":3,""" +
                        """"capabilities":[$caps],"speech_languages":[$langs],""" +
                        """"inventory":{"state":"ready"}}""",
                ),
            )
            pump()
        }

        /** The voice catalog payload the relay answers `speech_voices_list` with. */
        fun catalogJson(frInstalled: Boolean = false, deBytes: Long = 0L): String =
            """{"cache_dir":"/home/u/.cache/lerdr/voices","engine_installed":true,""" +
                """"languages":["en","fr"],"management_supported":true,""" +
                """"voices":[""" +
                """{"language":"en","name":"amy","engine":"piper","installed":true,"bytes":63206179},""" +
                """{"language":"fr","name":"siwis","engine":"piper","installed":$frInstalled,"bytes":65000000},""" +
                """{"language":"de","name":"thorsten","engine":"piper","installed":false,"bytes":$deBytes}""" +
                """]}"""

        /** Resolve the newest frame of [type] with a command_result. */
        suspend fun answer(type: String, data: String? = null, error: String? = null) {
            val frame = sentFrames(handle())
                .last { it["type"]?.jsonPrimitive?.content == type }
            val requestId = frame["request_id"]!!.jsonPrimitive.content
            val dataField = data?.let { ""","data":$it""" } ?: ""
            val errorField = error?.let { ""","error":"$it"""" } ?: ""
            val ok = error == null
            handle().emit(
                json(
                    """{"type":"command_result","request_id":"$requestId",""" +
                        """"action":"$type","ok":$ok,"phase":"completed"""" +
                        """$dataField$errorField}""",
                ),
            )
            pump()
        }
    }

    companion object {
        val WAV_RESULT = CommandResultMessage(
            action = "speak_text",
            ok = true,
            phase = CommandResultMessage.PHASE_COMPLETED,
            data = JsonObject(
                mapOf(
                    "format" to JsonPrimitive("wav"),
                    "audio" to JsonPrimitive(
                        Base64.getEncoder().encodeToString("RIFF".encodeToByteArray()),
                    ),
                ),
            ),
        )
    }

    // ── toggle & language intents ───────────────────────────────────────

    @Test
    fun `enable toggle persists and lifts the player to idle`() = runTest {
        val h = Harness(this, tmp.root)
        val viewModel = h.viewModel()
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.pump()
        assertThat(viewModel.uiState.value.phase).isEqualTo(SpeechPhase.OFF)

        viewModel.setEnabled(true)
        h.await { viewModel.uiState.value.phase == SpeechPhase.IDLE }
        assertThat(viewModel.uiState.value.enabled).isTrue()
        assertThat(h.preferences.enabled.first()).isTrue()
    }

    @Test
    fun `language change persists and stops playback`() = runTest {
        val h = Harness(this, tmp.root)
        val viewModel = h.viewModel()
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.connectSpeech()
        h.await { viewModel.uiState.value.enabled }

        // Park the speak_text exchange so the run stays mid-flight.
        val gate = CompletableDeferred<Unit>()
        h.script += FakeExchange(gate = gate)
        viewModel.toggleSpeakTest()
        h.await { viewModel.uiState.value.speaking }

        viewModel.setLanguage("fr")
        h.await { viewModel.uiState.value.language == "fr" }
        assertThat(h.exchanges.last().cancelled).isTrue()
        assertThat(viewModel.uiState.value.phase).isEqualTo(SpeechPhase.IDLE)
        assertThat(h.preferences.language.first()).isEqualTo("fr")
    }

    // ── adoption ────────────────────────────────────────────────────────

    @Test
    fun `first speech-capable relay adopts language and enables`() = runTest {
        val h = Harness(this, tmp.root)
        val viewModel = h.viewModel()
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.connectSpeech(speechLanguages = listOf("fr", "en"))
        h.await { viewModel.uiState.value.enabled }
        // Device language is en — the oracle prefers it when the relay
        // speaks it too.
        assertThat(viewModel.uiState.value.language).isEqualTo("en")
        assertThat(viewModel.uiState.value.speakableLanguages)
            .containsExactly("fr", "en")
    }

    @Test
    fun `a relay without speech languages never adopts`() = runTest {
        val h = Harness(this, tmp.root)
        val viewModel = h.viewModel()
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.connectSpeech(speechLanguages = emptyList())
        h.pump()
        assertThat(viewModel.uiState.value.enabled).isFalse()
        assertThat(viewModel.uiState.value.phase).isEqualTo(SpeechPhase.OFF)
    }

    // ── voice catalog ───────────────────────────────────────────────────

    @Test
    fun `catalog auto-loads once speech is on and the relay manages voices`() =
        runTest {
            val h = Harness(this, tmp.root)
            val viewModel = h.viewModel()
            backgroundScope.launch { viewModel.uiState.collect { } }
            h.connectSpeech()
            h.await { sentTypes(h.handle()).contains("speech_voices_list") }

            h.answer("speech_voices_list", data = h.catalogJson())
            h.await { viewModel.uiState.value.catalog != null }

            val catalog = viewModel.uiState.value.catalog!!
            assertThat(catalog.engineInstalled).isTrue()
            assertThat(catalog.cacheDir).isEqualTo("/home/u/.cache/lerdr/voices")
            // One row per offered language — oracle parity.
            assertThat(catalog.rows.map { it.language })
                .containsExactly("en", "fr", "de", "es", "zh")
                .inOrder()
            val en = catalog.rows.single { it.language == "en" }
            assertThat(en.installed).isTrue()
            assertThat(en.stateLabel).isEqualTo("Neural voice cached, 63 MB")
            val fr = catalog.rows.single { it.language == "fr" }
            assertThat(fr.installed).isFalse()
            assertThat(fr.stateLabel).isEqualTo("Not downloaded - 65 MB download")
            val es = catalog.rows.single { it.language == "es" }
            assertThat(es.stateLabel).isEqualTo("No voice on this computer")
            assertThat(viewModel.uiState.value.showCatalog).isTrue()
        }

    @Test
    fun `catalog never loads without the management capability`() = runTest {
        val h = Harness(this, tmp.root)
        val viewModel = h.viewModel()
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.connectSpeech(
            capabilities = listOf("speech_synthesis"),
            speechLanguages = listOf("en"),
        )
        h.await { viewModel.uiState.value.enabled }
        h.pump()
        assertThat(sentTypes(h.handle())).doesNotContain("speech_voices_list")
        assertThat(viewModel.uiState.value.managementCapable).isFalse()
        assertThat(viewModel.uiState.value.showCatalog).isFalse()
    }

    @Test
    fun `install sends the language extra and refreshes the catalog`() = runTest {
        val h = Harness(this, tmp.root)
        val viewModel = h.viewModel()
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.connectSpeech()
        h.await { sentTypes(h.handle()).contains("speech_voices_list") }
        h.answer("speech_voices_list", data = h.catalogJson())
        h.await { viewModel.uiState.value.catalog != null }

        viewModel.installVoice("fr")
        h.pump()
        val install = sentFrames(h.handle())
            .last { it["type"]?.jsonPrimitive?.content == "speech_voice_install" }
        assertThat(install["language"]?.jsonPrimitive?.content).isEqualTo("fr")
        // The row is busy while the download runs.
        h.await {
            viewModel.uiState.value.catalog!!
                .rows.single { it.language == "fr" }.busy
        }

        // Relay broadcasts the new catalog, then resolves the command.
        h.handle().emit(
            json("""{"type":"speech_voices",""" + h.catalogJson(frInstalled = true).removePrefix("{")),
        )
        h.answer("speech_voice_install", data = h.catalogJson(frInstalled = true))
        h.await {
            viewModel.uiState.value.catalog!!
                .rows.single { it.language == "fr" }.installed
        }
        assertThat(
            viewModel.uiState.value.catalog!!
                .rows.single { it.language == "fr" }.busy,
        ).isFalse()
    }

    @Test
    fun `remove sends speech_voice_remove`() = runTest {
        val h = Harness(this, tmp.root)
        val viewModel = h.viewModel()
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.connectSpeech()
        h.await { sentTypes(h.handle()).contains("speech_voices_list") }
        h.answer("speech_voices_list", data = h.catalogJson())
        h.await { viewModel.uiState.value.catalog != null }

        viewModel.removeVoice("en")
        h.pump()
        val remove = sentFrames(h.handle())
            .last { it["type"]?.jsonPrimitive?.content == "speech_voice_remove" }
        assertThat(remove["language"]?.jsonPrimitive?.content).isEqualTo("en")
        h.answer("speech_voice_remove", data = h.catalogJson())
    }

    @Test
    fun `voice op failure surfaces the relay error and clears busy`() = runTest {
        val h = Harness(this, tmp.root)
        val viewModel = h.viewModel()
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.connectSpeech()
        h.await { sentTypes(h.handle()).contains("speech_voices_list") }
        h.answer("speech_voices_list", data = h.catalogJson())
        h.await { viewModel.uiState.value.catalog != null }

        viewModel.installVoice("de")
        h.pump()
        h.answer("speech_voice_install", error = "disk full")
        h.await { viewModel.uiState.value.lastError == "disk full" }
        assertThat(
            viewModel.uiState.value.catalog!!
                .rows.single { it.language == "de" }.busy,
        ).isFalse()

        viewModel.dismissError()
        h.pump()
        assertThat(viewModel.uiState.value.lastError).isNull()
    }

    @Test
    fun `unsolicited speech_voices broadcast refreshes the catalog`() = runTest {
        val h = Harness(this, tmp.root)
        val viewModel = h.viewModel()
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.connectSpeech()
        h.await { sentTypes(h.handle()).contains("speech_voices_list") }
        h.answer("speech_voices_list", data = h.catalogJson())
        h.await { viewModel.uiState.value.catalog != null }

        h.handle().emit(
            json(
                """{"type":"speech_voices",""" +
                    h.catalogJson(deBytes = 81_000_000).removePrefix("{"),
            ),
        )
        h.await {
            viewModel.uiState.value.catalog!!
                .rows.single { it.language == "de" }.stateLabel ==
                "Not downloaded - 81 MB download"
        }
    }

    // ── speak test ──────────────────────────────────────────────────────

    @Test
    fun `speak test reads the sample through the relay`() = runTest {
        val h = Harness(this, tmp.root)
        val viewModel = h.viewModel()
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.connectSpeech()
        h.await { viewModel.uiState.value.enabled }

        viewModel.toggleSpeakTest()
        h.await { h.sends.isNotEmpty() }
        assertThat(h.sends.single().first).isEqualTo("r1")
        assertThat(h.sends.single().second)
            .isEqualTo(SpeechViewModel.SPEAK_TEST_TEXT)
        assertThat(h.sends.single().third).isEqualTo("en")
        h.await { viewModel.uiState.value.phase == SpeechPhase.IDLE }
        assertThat(h.sink.played).isNotEmpty()
    }

    @Test
    fun `speak test stops an active reading`() = runTest {
        val h = Harness(this, tmp.root)
        val viewModel = h.viewModel()
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.connectSpeech()
        h.await { viewModel.uiState.value.enabled }

        val gate = CompletableDeferred<Unit>()
        h.script += FakeExchange(gate = gate)
        viewModel.toggleSpeakTest()
        h.await { viewModel.uiState.value.speaking }

        viewModel.toggleSpeakTest() // now the Stop button
        h.await { viewModel.uiState.value.phase == SpeechPhase.IDLE }
        assertThat(h.exchanges.last().cancelled).isTrue()
        assertThat(h.sink.interruptCount).isAtLeast(1)
    }

    @Test
    fun `speak test without a speakable voice explains what is missing`() =
        runTest {
            val h = Harness(this, tmp.root)
            val viewModel = h.viewModel()
            backgroundScope.launch { viewModel.uiState.collect { } }
            // Relay offers no speech languages at all.
            h.connectSpeech(
                capabilities = listOf("speech_synthesis", "speech_voice_management"),
                speechLanguages = emptyList(),
            )
            viewModel.setEnabled(true)
            h.await { viewModel.uiState.value.enabled }

            viewModel.toggleSpeakTest()
            h.await { viewModel.uiState.value.lastError != null }
            assertThat(viewModel.uiState.value.lastError)
                .contains("no English voice")
            assertThat(h.sends).isEmpty()
        }

    @Test
    fun `player failures surface as the section error`() = runTest {
        val h = Harness(this, tmp.root)
        val viewModel = h.viewModel()
        backgroundScope.launch { viewModel.uiState.collect { } }
        h.connectSpeech()
        h.await { viewModel.uiState.value.enabled }

        h.script += FakeExchange(
            failure = SpeechPlaybackException("Speech synthesis failed on this computer"),
        )
        viewModel.toggleSpeakTest()
        h.await { viewModel.uiState.value.phase == SpeechPhase.ERROR }
        assertThat(viewModel.uiState.value.lastError)
            .isEqualTo("Speech synthesis failed on this computer")
        assertThat(viewModel.uiState.value.playerIssue)
            .isEqualTo("Speech synthesis failed on this computer")
    }
}
