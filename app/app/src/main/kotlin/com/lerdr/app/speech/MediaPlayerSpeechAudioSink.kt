package com.lerdr.app.speech

import android.media.MediaPlayer
import java.io.File
import kotlinx.coroutines.CompletableDeferred

/**
 * `MediaPlayer`-backed [SpeechAudioSink] — the Android analogue of the
 * oracle's persistent `<audio>` element: relay WAV fragments are written to
 * a short-lived cache file and played as ordinary media, which keeps
 * reading with the screen off.
 *
 * [play] suspends until the clip completes, errors, or [interrupt] runs —
 * the same contract `playRelayBlob` gives the oracle loop (`onended` and
 * `onpause` both resolve it).
 */
class MediaPlayerSpeechAudioSink(
    private val cacheDir: File,
) : SpeechAudioSink {

    private val lock = Any()
    private var current: Playback? = null

    private class Playback(
        val player: MediaPlayer,
        val file: File,
        val done: CompletableDeferred<Unit>,
    )

    override suspend fun play(wav: ByteArray) {
        val file = File.createTempFile("lerdr-speech-", ".wav", cacheDir)
        file.writeBytes(wav)
        val done = CompletableDeferred<Unit>()
        val player = MediaPlayer()
        val playback = Playback(player, file, done)
        synchronized(lock) { current = playback }
        // MediaPlayer delivers these on the main looper when the calling
        // thread has none — the player runs on the app scope either way.
        player.setOnCompletionListener { finish(playback, null) }
        player.setOnErrorListener { _, _, _ ->
            finish(
                playback,
                SpeechPlaybackException("This phone could not play the relay audio."),
            )
            true
        }
        try {
            player.setDataSource(file.absolutePath)
            player.prepare()
            player.start()
        } catch (failure: Exception) {
            finish(
                playback,
                SpeechPlaybackException("This phone could not play the relay audio."),
            )
        }
        try {
            done.await()
        } finally {
            synchronized(lock) { if (current === playback) current = null }
            runCatching { player.stop() }
            runCatching { player.release() }
            file.delete()
        }
    }

    /**
     * `stopPlayback` — pause whatever is playing and resolve its [play], so
     * the chunk loop wakes up, observes the bumped generation, and exits.
     */
    override fun interrupt() {
        val playback = synchronized(lock) { current } ?: return
        runCatching { playback.player.pause() }
        finish(playback, null)
    }

    private fun finish(playback: Playback, failure: SpeechPlaybackException?) {
        synchronized(lock) { if (current === playback) current = null }
        if (failure == null) {
            playback.done.complete(Unit)
        } else {
            playback.done.completeExceptionally(failure)
        }
    }
}
