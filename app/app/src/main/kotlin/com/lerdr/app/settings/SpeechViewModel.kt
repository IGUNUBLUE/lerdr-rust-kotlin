package com.lerdr.app.settings

import androidx.compose.runtime.Immutable
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.lerdr.app.session.SessionRepository
import com.lerdr.app.speech.RelaySpeechPlayer
import com.lerdr.app.speech.SPEECH_LANGUAGES
import com.lerdr.app.speech.SpeechPhase
import com.lerdr.app.speech.SpeechPlayerState
import com.lerdr.app.speech.isSpeechLanguage
import com.lerdr.app.speech.speechLanguageLabel
import kotlin.math.roundToInt
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import lerdr.core.model.SpeechVoice
import lerdr.core.model.SpeechVoicesMessage
import lerdr.core.model.Inbound
import lerdr.core.protocol.LerdrJson
import lerdr.core.protocol.Protocol
import lerdr.core.store.RelayConnection
import lerdr.core.store.RelayStatus
import lerdr.core.transport.CommandException
import lerdr.core.transport.ReconnectPolicy

/** One voice-catalog row — every offered language, installed or not. */
@Immutable
data class SpeechVoiceRowUi(
    val language: String,
    val label: String,
    /** `speechVoiceState` — "Neural voice cached, 63 MB" style status text. */
    val stateLabel: String,
    val installed: Boolean,
    val busy: Boolean,
)

/** `speechVoicePayload` view — cache dir, engine flag, one row per language. */
@Immutable
data class SpeechCatalogUi(
    val cacheDir: String = "",
    val engineInstalled: Boolean = false,
    val rows: List<SpeechVoiceRowUi> = emptyList(),
)

/** Everything the per-relay Speech section renders. */
@Immutable
data class SpeechUiState(
    val relayId: String,
    val relayLabel: String = "",
    val enabled: Boolean = false,
    val language: String = SpeechPreferences.DEFAULT_LANGUAGE,
    val phase: SpeechPhase = SpeechPhase.OFF,
    val connected: Boolean = false,
    /** `speech_synthesis` — the relay can synthesize and stream audio. */
    val synthesisCapable: Boolean = false,
    /** `speech_voice_management` — the relay accepts phone-driven installs. */
    val managementCapable: Boolean = false,
    /** `speech_languages` ∩ `SPEECH_LANGUAGES` — voices the relay can speak. */
    val speakableLanguages: List<String> = emptyList(),
    /** Voice catalog card content; null until the first payload lands. */
    val catalog: SpeechCatalogUi? = null,
    /** Transient failure from an action — rendered once, then dismissed. */
    val lastError: String? = null,
    /** Playback failure detail from the player — shown under the picker. */
    val playerIssue: String? = null,
) {
    /** The selected language has a voice on this relay. */
    val languageSpeakable: Boolean get() = language in speakableLanguages

    /** `toggleSpeech`'s gate — Speak test can actually produce audio. */
    val canSpeakTest: Boolean
        get() = enabled && connected && synthesisCapable && languageSpeakable

    /** `speechVoiceRelays` membership — the catalog card's gate. */
    val showCatalog: Boolean
        get() = enabled && connected && managementCapable

    val speaking: Boolean get() = phase == SpeechPhase.SPEAKING
}

/**
 * Speech settings for one relay — port of the oracle's `SettingsView`
 * Speech section (`SettingsView.svelte` §Speech).
 *
 * - Toggle/language intents persist through [SpeechPreferences]; language
 *   changes and disabling both stop playback (`setSpeechLanguage` and
 *   `setSpeechEnabled` call `stopSpeech` in the oracle — disabling is
 *   observed by the player itself, language switches stop here).
 * - `adoptRelaySpeech` runs whenever the relay advertises speakable
 *   languages (`push_config.speech_languages`, `speech_voices`, and the
 *   `speech_voices_list`/`speech_voice_*` payloads all carry them).
 * - The voice catalog auto-loads once when speech is on and the relay is
 *   connected + `speech_voice_management` capable, re-arming on reconnect —
 *   the oracle's `speechVoiceRequested` effect.
 * - `speech_voices` broadcasts and voice-op `command_result` payloads share
 *   one merge path (`adoptSpeechVoices` parity).
 */
class SpeechViewModel(
    private val relayId: String,
    private val sessions: SessionRepository,
    private val preferences: SpeechPreferences,
    private val player: RelaySpeechPlayer,
) : ViewModel() {

    /** `speechVoices` view — freshest catalog payload seen for this relay. */
    private val catalogPayload = MutableStateFlow<SpeechVoicesMessage?>(null)

    /** `speechVoiceBusy` — languages with an in-flight install/remove. */
    private val busyVoices = MutableStateFlow<Set<String>>(emptySet())

    private val lastError = MutableStateFlow<String?>(null)

    private class PrefBits(
        val enabled: Boolean,
        val language: String,
        val player: SpeechPlayerState,
    )

    private class RelayBits(
        val connection: RelayConnection?,
        val catalog: SpeechVoicesMessage?,
        val busy: Set<String>,
        val error: String?,
    )

    val uiState: StateFlow<SpeechUiState> = combine(
        combine(
            preferences.enabled,
            preferences.language,
            player.state,
            ::PrefBits,
        ),
        combine(
            sessions.connection(relayId),
            catalogPayload,
            busyVoices,
            lastError,
            ::RelayBits,
        ),
        ::Pair,
    ).map { (prefs, relay) -> toUiState(prefs, relay) }
        .stateIn(
            viewModelScope,
            SharingStarted.WhileSubscribed(5_000),
            SpeechUiState(relayId = relayId),
        )

    init {
        // `adoptRelaySpeech` — a relay saying "I can speak" turns the
        // feature on once; after that the stored prefs decide.
        viewModelScope.launch {
            sessions.connection(relayId).collect { connection ->
                connection?.speechLanguages
                    ?.takeIf { it.isNotEmpty() }
                    ?.let { preferences.adoptRelaySpeech(it) }
            }
        }
        // `speech_voices` broadcast — keep the catalog fresh without a poll.
        viewModelScope.launch {
            sessions.frames.collect { frame ->
                if (frame.relayId != relayId) return@collect
                (frame.message as? SpeechVoicesMessage)?.let { adoptCatalog(it) }
            }
        }
        // The oracle's `$effect`: pull the catalog once per capable stretch
        // while speech is on; a disconnect or lost capability re-arms it.
        viewModelScope.launch {
            var requested = false
            combine(
                sessions.connection(relayId),
                preferences.enabled,
                ::Pair,
            ).collect { (connection, enabled) ->
                val capable = connection?.status == RelayStatus.CONNECTED &&
                    Protocol.SPEECH_VOICE_MANAGEMENT_CAPABILITY in connection.capabilities
                if (!capable) {
                    requested = false
                    return@collect
                }
                if (!enabled || requested) return@collect
                requested = true
                try {
                    refreshVoices()
                } catch (cancelled: CancellationException) {
                    throw cancelled
                } catch (failure: Exception) {
                    lastError.value =
                        failure.message ?: "Could not load the voice catalog."
                }
            }
        }
        // Player failures surface like the oracle's `onIssue` toast.
        viewModelScope.launch {
            player.state.collect { state ->
                if (state.phase == SpeechPhase.ERROR && state.issue != null) {
                    lastError.value = state.issue
                }
            }
        }
    }

    /** `setSpeechEnabled` — persist; the player observes and stops/arms. */
    fun setEnabled(enabled: Boolean) {
        viewModelScope.launch { preferences.setEnabled(enabled) }
    }

    /** `setSpeechLanguage` — persist a speakable code, then stop reading. */
    fun setLanguage(code: String) {
        if (!isSpeechLanguage(code)) return
        viewModelScope.launch {
            preferences.setLanguage(code)
            player.stop()
        }
    }

    /**
     * `toggleSpeech` for the section's Speak test: while speaking this is
     * the Stop button; otherwise it guards on a speakable voice and plays
     * a short sample through [RelaySpeechPlayer].
     */
    fun toggleSpeakTest() {
        if (player.state.value.phase == SpeechPhase.SPEAKING) {
            player.stop()
            return
        }
        viewModelScope.launch {
            val language = preferences.language.first()
            if (language !in speakableNow()) {
                lastError.value =
                    "This relay has no ${speechLanguageLabel(language)} voice; " +
                        "install a Piper voice for it on that computer."
                return@launch
            }
            player.speak(relayId, SPEAK_TEST_TEXT)
        }
    }

    /** `listSpeechVoices` — pull the relay's voice catalog now. */
    suspend fun refreshVoices() {
        requireVoiceManagement()
        val result = sessions.request(relayId, Inbound(type = VOICES_LIST))
        adoptCatalog(decodeCatalog(result.data))
    }

    /** `changeSpeechVoice(relayId, language, true)` — download the voice. */
    fun installVoice(language: String) = changeSpeechVoice(language, install = true)

    /** `changeSpeechVoice(relayId, language, false)` — delete the voice. */
    fun removeVoice(language: String) = changeSpeechVoice(language, install = false)

    /**
     * `changeSpeechVoice` — one install/remove per language at a time; the
     * reply payload refreshes the catalog (the broadcast lands too — the
     * merge is idempotent). Voice downloads get the oracle's 5-minute budget.
     */
    private fun changeSpeechVoice(language: String, install: Boolean) {
        if (!isSpeechLanguage(language) || language in busyVoices.value) return
        busyVoices.value = busyVoices.value + language
        viewModelScope.launch {
            try {
                requireVoiceManagement()
                val result = sessions.request(
                    relayId,
                    Inbound(type = if (install) VOICE_INSTALL else VOICE_REMOVE),
                    extras = mapOf("language" to JsonPrimitive(language)),
                    timeoutMs = if (install) {
                        VOICE_INSTALL_TIMEOUT_MS
                    } else {
                        ReconnectPolicy.COMMAND_TIMEOUT_MS
                    },
                )
                adoptCatalog(decodeCatalog(result.data))
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: Exception) {
                lastError.value =
                    failure.message ?: "The relay could not change the voice."
            } finally {
                busyVoices.value = busyVoices.value - language
            }
        }
    }

    /** `speechVoiceConnection` — the capability gate behind every op. */
    private fun requireVoiceManagement() {
        val connection = sessions.connectionNow(relayId)
        if (connection?.capabilities
                ?.contains(Protocol.SPEECH_VOICE_MANAGEMENT_CAPABILITY) != true
        ) {
            throw CommandException(
                "This relay does not support phone-managed speech voices yet",
            )
        }
    }

    /** `adoptSpeechVoices` — store the payload and adopt its languages. */
    private suspend fun adoptCatalog(message: SpeechVoicesMessage) {
        catalogPayload.value = message
        message.languages
            ?.takeIf { it.isNotEmpty() }
            ?.let { preferences.adoptRelaySpeech(it) }
    }

    /** Current speakable set — catalog payload wins, else the connection. */
    private fun speakableNow(): List<String> =
        catalogPayload.value?.languages?.filter(::isSpeechLanguage)
            ?: sessions.connectionNow(relayId)?.speechLanguages.orEmpty()

    /** Snackbar consumed the error. */
    fun dismissError() {
        lastError.value = null
    }

    private fun toUiState(prefs: PrefBits, relay: RelayBits): SpeechUiState {
        val connection = relay.connection
        val capabilities = connection?.capabilities.orEmpty()
        val speakable = relay.catalog?.languages?.filter(::isSpeechLanguage)
            ?: connection?.speechLanguages.orEmpty()
        val catalog = relay.catalog?.let { catalog ->
            SpeechCatalogUi(
                cacheDir = catalog.cacheDir.orEmpty(),
                engineInstalled = catalog.engineInstalled == true,
                rows = voiceRows(catalog.voices, relay.busy),
            )
        } ?: connection?.takeIf { it.speechVoices.isNotEmpty() }?.let {
            SpeechCatalogUi(
                cacheDir = it.speechCacheDir,
                engineInstalled = it.speechEngineInstalled,
                rows = voiceRows(it.speechVoices, relay.busy),
            )
        }
        return SpeechUiState(
            relayId = relayId,
            relayLabel = connection?.relayLabel ?: relayId,
            enabled = prefs.enabled,
            language = prefs.language,
            phase = prefs.player.phase,
            connected = connection?.status == RelayStatus.CONNECTED,
            synthesisCapable =
                Protocol.SPEECH_SYNTHESIS_CAPABILITY in capabilities,
            managementCapable =
                Protocol.SPEECH_VOICE_MANAGEMENT_CAPABILITY in capabilities,
            speakableLanguages = speakable,
            catalog = catalog,
            lastError = relay.error,
            playerIssue = prefs.player.issue,
        )
    }

    /**
     * One row per offered language — the oracle renders all five whether
     * or not the relay has a voice for them; missing rows read "No voice".
     */
    private fun voiceRows(
        voices: List<SpeechVoice>?,
        busy: Set<String>,
    ): List<SpeechVoiceRowUi> {
        val byLanguage = voices.orEmpty()
            .filter { isSpeechLanguage(it.language) }
            .associateBy { it.language }
        return SPEECH_LANGUAGES.map { language ->
            val voice = byLanguage[language.code]
            SpeechVoiceRowUi(
                language = language.code,
                label = language.label,
                stateLabel = voiceStateLabel(voice),
                installed = voice?.installed == true,
                busy = language.code in busy,
            )
        }
    }

    private fun voiceStateLabel(voice: SpeechVoice?): String = when {
        voice?.installed == true ->
            "Neural voice cached, ${voiceSizeLabel(voice.bytes)}"
        (voice?.bytes ?: 0) > 0 ->
            "Not downloaded - ${voiceSizeLabel(voice?.bytes)} download"
        else -> "No voice on this computer"
    }

    /** `speechVoiceSize` — 63,206,179 bytes reads as "63 MB". */
    private fun voiceSizeLabel(bytes: Long?): String =
        "${(bytes ?: 0).let { (it / 1_000_000.0).roundToInt().coerceAtLeast(1) }} MB"

    /**
     * The `command_result.data` payload is the same object `speech_voices`
     * broadcasts — decode it into the shared message type.
     */
    private fun decodeCatalog(data: JsonElement?): SpeechVoicesMessage =
        runCatching {
            LerdrJson.decodeFromJsonElement(
                SpeechVoicesMessage.serializer(),
                data ?: JsonObject(emptyMap()),
            )
        }.getOrDefault(SpeechVoicesMessage())

    companion object {
        const val VOICES_LIST = "speech_voices_list"
        const val VOICE_INSTALL = "speech_voice_install"
        const val VOICE_REMOVE = "speech_voice_remove"

        /** `installSpeechVoice`'s 300 s — first install fetches the engine too. */
        const val VOICE_INSTALL_TIMEOUT_MS = 300_000L

        /** Short sample the Speak test reads aloud. */
        const val SPEAK_TEST_TEXT = "This is Lerdr reading a response aloud."
    }
}
