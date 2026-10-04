package com.lerdr.app.speech

import com.lerdr.app.session.SessionRepository
import java.util.UUID
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.async
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonPrimitive
import lerdr.core.model.CommandResultMessage
import lerdr.core.model.Inbound
import lerdr.core.protocol.Protocol
import lerdr.core.store.RelayStatus

/**
 * `SpeechSender` over [SessionRepository] — the wire shape Lerdr's
 * `speakToAgent` emits:
 *
 * - `speak_text` — `text` on the typed [Inbound], `language` and a fresh
 *   `speech_request_id` as raw extras (the flat `Inbound` declares neither).
 *   Awaits the correlated `command_result` carrying `data.audio` (base64
 *   WAV). Lerdr's 20 s command budget applies.
 * - `cancel_speech` — fire-and-forget under the same `speech_request_id`;
 *   the relay flags the in-flight synthesis cancelled (and tombstones the
 *   id, so a late `speak_text` starts cancelled). It receipts with a
 *   `confirmed` action_receipt — `request()` consumes it and the result is
 *   dropped. Unlike Lerdr's raw send this frame gets a `request_id`,
 *   which only earns the receipt — harmless parity.
 */
class SessionSpeechSender(
    private val scope: CoroutineScope,
    private val sessions: SessionRepository,
) : SpeechSender {

    override fun send(relayId: String, text: String, language: String): SpeechExchange {
        val speechRequestId = UUID.randomUUID().toString()
        val deferred: Deferred<CommandResultMessage> = scope.async {
            val connection = sessions.connectionNow(relayId)
            if (connection?.status != RelayStatus.CONNECTED ||
                Protocol.SPEECH_SYNTHESIS_CAPABILITY !in connection.capabilities
            ) {
                throw SpeechPlaybackException("This relay cannot read aloud right now.")
            }
            sessions.request(
                relayId,
                Inbound(type = SPEAK_TEXT, text = text),
                extras = mapOf(
                    "language" to JsonPrimitive(language),
                    "speech_request_id" to JsonPrimitive(speechRequestId),
                ),
                timeoutMs = SPEAK_TIMEOUT_MS,
            )
        }
        return Exchange(relayId, speechRequestId, deferred)
    }

    private inner class Exchange(
        private val relayId: String,
        private val speechRequestId: String,
        private val deferred: Deferred<CommandResultMessage>,
    ) : SpeechExchange {
        private val cancelled = AtomicBoolean(false)

        override suspend fun await(): CommandResultMessage = deferred.await()

        override fun cancel() {
            if (!cancelled.compareAndSet(false, true)) return
            deferred.cancel()
            scope.launch {
                try {
                    sessions.request(
                        relayId,
                        Inbound(type = CANCEL_SPEECH),
                        extras = mapOf(
                            "speech_request_id" to JsonPrimitive(speechRequestId),
                        ),
                    )
                } catch (cancelled: CancellationException) {
                    throw cancelled
                } catch (_: Exception) {
                    // Best effort: local playback and waiting already stopped.
                }
            }
        }
    }

    private companion object {
        const val SPEAK_TEXT = "speak_text"
        const val CANCEL_SPEECH = "cancel_speech"

        /** `speakToAgent`'s 20 s per-fragment budget. */
        const val SPEAK_TIMEOUT_MS = 20_000L
    }
}
