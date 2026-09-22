package com.lerdr.app.speech

import androidx.compose.runtime.Immutable
import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import lerdr.core.model.CommandResultMessage

/**
 * `SpeechState` — the oracle's `'off' | 'idle' | 'speaking' | 'error'`
 * lifecycle. `off` while the setting is disabled; `error` latches until the
 * next [RelaySpeechPlayer.speak]/[RelaySpeechPlayer.stop] or a re-enable.
 */
enum class SpeechPhase { OFF, IDLE, SPEAKING, ERROR }

/** Playback snapshot the UI renders — `speechState` plus the failure text. */
@Immutable
data class SpeechPlayerState(
    val phase: SpeechPhase = SpeechPhase.OFF,
    /** The last synthesis/playback failure, surfaced like the oracle toast. */
    val issue: String? = null,
)

/** Playback failed on the phone or the relay refused — message is UI-facing. */
class SpeechPlaybackException(message: String, cause: Throwable? = null) :
    Exception(message, cause)

/**
 * `SpeechRequest` — one in-flight `speak_text` exchange: [await] the
 * `command_result`, or [cancel] it, which sends the oracle's fire-and-forget
 * `cancel_speech` frame under the same `speech_request_id`.
 */
interface SpeechExchange {
    suspend fun await(): CommandResultMessage
    fun cancel()
}

/**
 * `SpeechSender` — issues one `speak_text` per fragment and returns the
 * cancellable exchange. The production binding is [SessionSpeechSender];
 * tests inject a fake so the state machine never touches network or media.
 */
fun interface SpeechSender {
    fun send(relayId: String, text: String, language: String): SpeechExchange
}

/**
 * The audio endpoint — port of the oracle's persistent `<audio>` element.
 * [play] suspends until the clip ends, fails, or [interrupt] cuts it short
 * (the oracle's `onpause` resolve, which is how `stopSpeech` unwinds the
 * playback loop). Implemented by `MediaPlayerSpeechAudioSink` in production
 * and by a fake in tests.
 */
interface SpeechAudioSink {
    suspend fun play(wav: ByteArray)
    fun interrupt()
}

/**
 * `speakViaRelay`/`stopSpeech` — reads text with the relay's speech engine:
 * fragments are synthesized on the computer and played here as ordinary
 * media, prefetching the next fragment while the current one speaks.
 *
 * Oracle semantics mirrored (`frontend/src/lib/speech.ts`):
 * - [speak] refuses while locked, disabled, or handed blank text, and every
 *   call abandons the previous run through a generation guard.
 * - Chunks come from [SpeechChunker.speechChunks] at 240 chars over
 *   [SpeechChunker.speakableText] (falling back to the raw text when the
 *   markdown strip empties it).
 * - The next chunk's request is issued before the current clip plays.
 * - [stop] bumps the generation, sends `cancel_speech` for the in-flight
 *   request (if any) and resolves the current clip — the parked `await`
 *   observes the bumped generation and exits, exactly like the oracle's
 *   `onpause` path.
 * - Disabling the setting or it being off at call time forces `off`;
 *   enabling flips `off` → `idle`.
 *
 * This class is pure JVM — Android lives behind [SpeechAudioSink] and the
 * sender so unit tests drive it with fakes.
 */
class RelaySpeechPlayer(
    private val scope: CoroutineScope,
    enabled: Flow<Boolean>,
    language: Flow<String>,
    private val sender: SpeechSender,
    private val sink: SpeechAudioSink,
    private val locked: () -> Boolean = { false },
) {
    private val _state = MutableStateFlow(SpeechPlayerState())

    /** `speechState` — `off`/`idle`/`speaking`/`error` for the UI. */
    val state: StateFlow<SpeechPlayerState> = _state.asStateFlow()

    /**
     * Identifies the current run — a new [speak] or [stop] bumps it so a
     * late reply or a resolved clip from the abandoned run cannot restart
     * or overwrite the fresh state.
     */
    private val generation = AtomicLong(0)

    /** Synchronous reads of the flows, like the oracle's `get(store)`. */
    @Volatile
    private var enabledNow = false

    @Volatile
    private var languageNow = DEFAULT_LANGUAGE

    /** `cancelRelaySynthesis` — the exchange awaiting its reply right now. */
    @Volatile
    private var inFlight: SpeechExchange? = null

    init {
        scope.launch {
            enabled.collect { on ->
                enabledNow = on
                if (on) {
                    // setSpeechEnabled(true) — `speechState.set('idle')`
                    // unconditionally; `speaking` cannot be live here since
                    // disabling already stopped it.
                    _state.value = SpeechPlayerState(SpeechPhase.IDLE)
                } else {
                    stopInternal()
                }
            }
        }
        scope.launch { language.collect { languageNow = it } }
    }

    /**
     * `speakViaRelay` — returns false when the app is locked, reading is
     * disabled, or the text is blank; otherwise chunks and starts the
     * synthesize→play pipeline on [scope].
     */
    fun speak(relayId: String, text: String): Boolean {
        if (locked() || !enabledNow || text.isBlank()) return false
        val gen = generation.incrementAndGet()
        val chunks = SpeechChunker.speechChunks(
            SpeechChunker.speakableText(text).ifEmpty { text },
            SpeechChunker.SPEAK_CHUNK_LIMIT,
        )
        if (chunks.isEmpty()) return false
        _state.value = SpeechPlayerState(SpeechPhase.SPEAKING)
        scope.launch { playChunks(relayId, chunks, gen) }
        return true
    }

    /** `stopSpeech` — cancel the in-flight synthesis and end the run. */
    fun stop() = stopInternal()

    private fun stopInternal() {
        generation.incrementAndGet()
        inFlight?.cancel()
        inFlight = null
        sink.interrupt()
        _state.value = SpeechPlayerState(
            if (enabledNow) SpeechPhase.IDLE else SpeechPhase.OFF,
        )
    }

    /**
     * `playRelayChunks` — the send-ahead loop: chunk `i+1` is requested
     * while clip `i` is still playing, so a relay round trip never gaps
     * the audio.
     */
    private suspend fun playChunks(relayId: String, chunks: List<String>, gen: Long) {
        val language = languageNow
        var pending = sender.send(relayId, chunks[0], language)
        inFlight = pending
        try {
            for (index in chunks.indices) {
                val request = pending
                val result = request.await()
                if (inFlight === request) inFlight = null
                if (gen != generation.get()) return
                if (index + 1 < chunks.size) {
                    pending = sender.send(relayId, chunks[index + 1], language)
                    inFlight = pending
                }
                val wav = decodeAudio(result)
                sink.play(wav)
                if (gen != generation.get()) return
            }
            inFlight = null
            _state.value = SpeechPlayerState(SpeechPhase.IDLE)
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (failure: Exception) {
            if (gen != generation.get()) return
            // Bump like the oracle so a stale completion cannot revive this run.
            generation.incrementAndGet()
            inFlight = null
            _state.value = SpeechPlayerState(
                SpeechPhase.ERROR,
                issue = failure.message?.takeIf { it.isNotEmpty() }
                    ?: "The relay could not read this aloud.",
            )
        }
    }

    /** `data.audio` — the base64 WAV payload, or the oracle's "no audio" error. */
    private fun decodeAudio(result: CommandResultMessage): ByteArray {
        val audio = (result.data as? JsonObject)?.get("audio")
            ?.let { it as? JsonPrimitive }?.contentOrNull
            ?.takeIf { it.isNotEmpty() }
            ?: throw SpeechPlaybackException("The relay returned no audio.")
        return try {
            java.util.Base64.getDecoder().decode(audio)
        } catch (invalid: IllegalArgumentException) {
            throw SpeechPlaybackException("The relay returned unplayable audio.")
        }
    }

    private companion object {
        const val DEFAULT_LANGUAGE = "en"
    }
}
