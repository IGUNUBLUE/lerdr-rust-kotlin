package com.lerdr.app.speech

import com.google.common.truth.Truth.assertThat
import java.util.Base64
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import lerdr.core.model.CommandResultMessage
import org.junit.Test

/**
 * `RelaySpeechPlayer` — the `speakViaRelay`/`stopSpeech` state machine with
 * the network and media seams faked: `send` records fragments and returns
 * scripted exchanges; the sink records clips and can park playback.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class RelaySpeechPlayerTest {

    private class FakeExchange(
        private val result: CommandResultMessage? = null,
        private val failure: Throwable? = null,
        /** Set to park [await] until the test completes it. */
        val gate: CompletableDeferred<Unit>? = null,
    ) : SpeechExchange {
        var cancelled = false
            private set

        override suspend fun await(): CommandResultMessage {
            gate?.await()
            failure?.let { throw it }
            return result ?: wavResult()
        }

        override fun cancel() {
            cancelled = true
        }
    }

    private class FakeSink : SpeechAudioSink {
        val played = mutableListOf<ByteArray>()
        var interruptCount = 0
            private set
        var failWith: Throwable? = null
        /** Set to park [play] until [interrupt] or the test completes it. */
        var gate: CompletableDeferred<Unit>? = null

        override suspend fun play(wav: ByteArray) {
            played += wav
            gate?.await()
            failWith?.let { throw it }
        }

        override fun interrupt() {
            interruptCount++
            gate?.complete(Unit)
        }
    }

    private class Harness(
        private val testScope: TestScope,
    ) {
        val scope: CoroutineScope = testScope.backgroundScope
        val enabled = MutableStateFlow(false)
        val language = MutableStateFlow("en")
        val sink = FakeSink()
        var locked = false

        /** Every send, in order: (relayId, text, language) to exchange. */
        val sends = mutableListOf<Triple<String, String, String>>()
        val exchanges = mutableListOf<FakeExchange>()

        /** The script for the next send — consumed FIFO by the fake sender. */
        val script = ArrayDeque<FakeExchange>()

        val player = RelaySpeechPlayer(
            scope = scope,
            enabled = enabled,
            language = language,
            sender = SpeechSender { relayId, text, lang ->
                sends += Triple(relayId, text, lang)
                (script.removeFirstOrNull() ?: FakeExchange()).also {
                    exchanges += it
                }
            },
            sink = sink,
            locked = { locked },
        )

        fun pump() = testScope.runCurrent()

        fun wavData(tag: String = "clip"): CommandResultMessage = wavResult(tag)

        /** Queue a reply for the next send. */
        fun respondWith(exchange: FakeExchange): FakeExchange {
            script.addLast(exchange)
            return exchange
        }
    }

    companion object {
        fun wavResult(tag: String = "clip"): CommandResultMessage =
            CommandResultMessage(
                action = "speak_text",
                ok = true,
                phase = CommandResultMessage.PHASE_COMPLETED,
                data = JsonObject(
                    mapOf(
                        "format" to JsonPrimitive("wav"),
                        "audio" to JsonPrimitive(
                            Base64.getEncoder().encodeToString("RIFF-$tag".encodeToByteArray()),
                        ),
                    ),
                ),
            )

        /** 240+ chars that chunk into two fragments at a word boundary. */
        val TWO_CHUNKS = "Chunk one " + "word ".repeat(60) + ". Chunk two here."
    }

    // ── guards ──────────────────────────────────────────────────────────

    @Test
    fun `speak refuses while disabled`() = runTest {
        val h = Harness(this)
        h.pump()
        assertThat(h.player.speak("r1", "hello")).isFalse()
        assertThat(h.sends).isEmpty()
        assertThat(h.player.state.value.phase).isEqualTo(SpeechPhase.OFF)
    }

    @Test
    fun `speak refuses blank text`() = runTest {
        val h = Harness(this)
        h.enabled.value = true
        h.pump()
        assertThat(h.player.speak("r1", "   ")).isFalse()
        assertThat(h.sends).isEmpty()
    }

    @Test
    fun `speak refuses while the app is locked`() = runTest {
        val h = Harness(this)
        h.enabled.value = true
        h.locked = true
        h.pump()
        assertThat(h.player.speak("r1", "hello")).isFalse()
        assertThat(h.sends).isEmpty()
    }

    // ── state transitions ───────────────────────────────────────────────

    @Test
    fun `enable lifts off to idle, disable returns to off`() = runTest {
        val h = Harness(this)
        h.pump()
        assertThat(h.player.state.value.phase).isEqualTo(SpeechPhase.OFF)
        h.enabled.value = true
        h.pump()
        assertThat(h.player.state.value.phase).isEqualTo(SpeechPhase.IDLE)
        h.enabled.value = false
        h.pump()
        assertThat(h.player.state.value.phase).isEqualTo(SpeechPhase.OFF)
    }

    @Test
    fun `speaking a short text ends idle`() = runTest {
        val h = Harness(this)
        h.enabled.value = true
        h.pump()
        assertThat(h.player.speak("r1", "Hello.")).isTrue()
        h.pump()
        assertThat(h.player.state.value.phase).isEqualTo(SpeechPhase.IDLE)
        assertThat(h.sink.played.single()).isEqualTo("RIFF-clip".encodeToByteArray())
    }

    // ── chunk pipeline ──────────────────────────────────────────────────

    @Test
    fun `chunks send sequentially with the next prefetched during playback`() = runTest {
        val h = Harness(this)
        h.enabled.value = true
        h.pump()

        // Playback of chunk 0 parks; the loop must already hold chunk 1's
        // request — the oracle's prefetch.
        val parkedPlay = CompletableDeferred<Unit>()
        h.sink.gate = parkedPlay
        assertThat(h.player.speak("r1", TWO_CHUNKS)).isTrue()
        h.pump()

        assertThat(h.sends.size).isEqualTo(2)
        assertThat(h.sends.map { it.third }.distinct()).containsExactly("en")
        assertThat(h.sends[0].second).isNotEqualTo(h.sends[1].second)
        assertThat(h.sink.played).hasSize(1)

        parkedPlay.complete(Unit)
        h.pump()
        assertThat(h.sink.played).hasSize(2)
        assertThat(h.player.state.value.phase).isEqualTo(SpeechPhase.IDLE)
    }

    @Test
    fun `speakableText strips markdown before chunking`() = runTest {
        val h = Harness(this)
        h.enabled.value = true
        h.pump()
        h.player.speak("r1", "**Bold** `code` here.")
        h.pump()
        assertThat(h.sends.single().second).isEqualTo("Bold code here.")
    }

    @Test
    fun `language rides every chunk`() = runTest {
        val h = Harness(this)
        h.enabled.value = true
        h.language.value = "fr"
        h.pump()
        h.player.speak("r1", TWO_CHUNKS)
        h.pump()
        assertThat(h.sends.map { it.third }.distinct()).containsExactly("fr")
    }

    // ── stop / generation guard ─────────────────────────────────────────

    @Test
    fun `stop cancels the in-flight exchange and interrupts playback`() = runTest {
        val h = Harness(this)
        h.enabled.value = true
        h.pump()

        val gate = CompletableDeferred<Unit>()
        h.respondWith(FakeExchange(gate = gate))
        h.sink.gate = CompletableDeferred() // second chunk would park too
        assertThat(h.player.speak("r1", TWO_CHUNKS)).isTrue()
        h.pump()
        assertThat(h.player.state.value.phase).isEqualTo(SpeechPhase.SPEAKING)

        h.player.stop()
        h.pump()
        assertThat(h.exchanges[0].cancelled).isTrue()
        assertThat(h.sink.interruptCount).isAtLeast(1)
        assertThat(h.player.state.value.phase).isEqualTo(SpeechPhase.IDLE)
    }

    @Test
    fun `a stale reply after stop does not restart playback`() = runTest {
        val h = Harness(this)
        h.enabled.value = true
        h.pump()

        val gate = CompletableDeferred<Unit>()
        h.respondWith(FakeExchange(gate = gate, result = h.wavData()))
        h.player.speak("r1", "One. Two.")
        h.pump()
        h.player.stop()
        gate.complete(Unit) // the abandoned request resolves late
        h.pump()
        assertThat(h.sink.played).isEmpty()
        assertThat(h.player.state.value.phase).isEqualTo(SpeechPhase.IDLE)
    }

    @Test
    fun `a new speak abandons the previous run`() = runTest {
        val h = Harness(this)
        h.enabled.value = true
        h.pump()

        val gate = CompletableDeferred<Unit>()
        h.respondWith(FakeExchange(gate = gate))
        h.player.speak("r1", "First run")
        h.pump()
        h.player.speak("r1", "Second run")
        gate.complete(Unit) // run one's reply lands mid run two
        h.pump()
        // Only run two's clip plays.
        assertThat(h.sends.map { it.second }).containsExactly("First run", "Second run")
        assertThat(h.sink.played).hasSize(1)
        assertThat(h.player.state.value.phase).isEqualTo(SpeechPhase.IDLE)
    }

    @Test
    fun `disabling mid-speech stops playback and reports off`() = runTest {
        val h = Harness(this)
        h.enabled.value = true
        h.pump()

        val gate = CompletableDeferred<Unit>()
        h.respondWith(FakeExchange(gate = gate))
        h.player.speak("r1", "Reading.")
        h.pump()
        h.enabled.value = false
        h.pump()
        assertThat(h.exchanges[0].cancelled).isTrue()
        assertThat(h.player.state.value.phase).isEqualTo(SpeechPhase.OFF)
    }

    // ── failures ────────────────────────────────────────────────────────

    @Test
    fun `relay failure lands in error with the message`() = runTest {
        val h = Harness(this)
        h.enabled.value = true
        h.pump()
        h.respondWith(
            FakeExchange(
                failure = SpeechPlaybackException("Speech synthesis failed on this computer"),
            ),
        )
        h.player.speak("r1", "Hello.")
        h.pump()
        assertThat(h.player.state.value.phase).isEqualTo(SpeechPhase.ERROR)
        assertThat(h.player.state.value.issue)
            .isEqualTo("Speech synthesis failed on this computer")
    }

    @Test
    fun `a reply without audio fails like the oracle`() = runTest {
        val h = Harness(this)
        h.enabled.value = true
        h.pump()
        h.respondWith(
            FakeExchange(
                result = CommandResultMessage(
                    action = "speak_text",
                    ok = true,
                    phase = CommandResultMessage.PHASE_COMPLETED,
                    data = JsonObject(mapOf("format" to JsonPrimitive("wav"))),
                ),
            ),
        )
        h.player.speak("r1", "Hello.")
        h.pump()
        assertThat(h.player.state.value.phase).isEqualTo(SpeechPhase.ERROR)
        assertThat(h.player.state.value.issue).isEqualTo("The relay returned no audio.")
    }

    @Test
    fun `playback failure surfaces and clears on the next speak`() = runTest {
        val h = Harness(this)
        h.enabled.value = true
        h.pump()
        h.sink.failWith = SpeechPlaybackException("This phone could not play the relay audio.")
        h.player.speak("r1", "Hello.")
        h.pump()
        assertThat(h.player.state.value.phase).isEqualTo(SpeechPhase.ERROR)

        h.sink.failWith = null
        h.player.speak("r1", "Again.")
        h.pump()
        assertThat(h.player.state.value.phase).isEqualTo(SpeechPhase.IDLE)
        assertThat(h.player.state.value.issue).isNull()
    }
}
