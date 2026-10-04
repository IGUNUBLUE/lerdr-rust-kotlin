package com.lerdr.app.speech

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.media.MediaPlayer
import android.os.Handler
import android.os.Looper
import java.io.File
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.isActive
import kotlinx.coroutines.withContext

/**
 * `MediaPlayer`-backed [SpeechAudioSink] — the Android analogue of the
 * Lerdr's persistent `<audio>` element: relay WAV fragments are written to
 * a short-lived cache file and played as ordinary media, which keeps
 * reading with the screen off.
 *
 * [play] suspends until the clip completes, errors, or [interrupt] runs —
 * the same contract `playRelayBlob` gives Lerdr loop (`onended` and
 * `onpause` both resolve it).
 */
class MediaPlayerSpeechAudioSink(
    context: Context,
) : SpeechAudioSink {
    private val cacheDir = context.cacheDir
    private val audioManager = checkNotNull(context.getSystemService(AudioManager::class.java))
    private val mediaHandler = Handler(Looper.getMainLooper())
    private val attributes = AudioAttributes.Builder()
        .setUsage(AudioAttributes.USAGE_MEDIA)
        .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
        .build()
    private val lock = Any()
    private var current: Playback? = null
    private var focusRequest: AudioFocusRequest? = null
    private var focusToken: Any? = null
    private var focusFailure: SpeechPlaybackException? = null

    private class Playback(
        val player: MediaPlayer,
        val file: File,
        val done: CompletableDeferred<Unit> = CompletableDeferred(),
        var released: Boolean = false,
    )

    override suspend fun play(wav: ByteArray) {
        val coroutine = currentCoroutineContext()
        coroutine.ensureActive()
        val file = File.createTempFile("lerdr-speech-", ".wav", cacheDir)
        var playback: Playback? = null
        try {
            file.writeBytes(wav)
            val clip = withContext(Dispatchers.Main.immediate) {
                synchronized(lock) {
                    // Stop/replacement cancels the caller before interrupting us.
                    coroutine.ensureActive()
                    focusFailure?.let { throw it }
                    current?.let { finish(it, null) }
                    Playback(MediaPlayer(), file).also { clip ->
                        playback = clip
                        current = clip
                        clip.player.setAudioAttributes(attributes)
                        clip.player.setOnCompletionListener { finish(clip, null) }
                        clip.player.setOnErrorListener { _, _, _ ->
                            finish(clip, playbackFailure())
                            true
                        }
                        clip.player.setOnPreparedListener {
                            synchronized(lock) {
                                if (!clip.released && current === clip) {
                                    if (!coroutine.isActive) {
                                        finish(clip, null)
                                    } else {
                                        try {
                                            focusFailure?.let { throw it }
                                            acquireFocus()
                                            clip.player.start()
                                        } catch (failure: Exception) {
                                            finish(
                                                clip,
                                                if (failure is SpeechPlaybackException) failure
                                                else playbackFailure(failure),
                                            )
                                        }
                                    }
                                }
                            }
                        }
                        clip.player.setDataSource(file.absolutePath)
                        clip.player.prepareAsync()
                    }
                }
            }
            clip.done.await()
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (failure: Exception) {
            throw if (failure is SpeechPlaybackException) failure else playbackFailure(failure)
        } finally {
            synchronized(lock) {
                playback?.let { finish(it, null) } ?: file.delete()
            }
        }
    }

    /** Ends the reading run, including focus retained between its fragments. */
    override fun interrupt() = synchronized(lock) {
        current?.let { finish(it, null) }
        abandonFocus()
        focusFailure = null
    }

    /** Called under [lock]; one focus grant covers the whole chunk pipeline. */
    private fun acquireFocus() {
        if (focusRequest != null) return
        val token = Any()
        val request = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT)
            .setAudioAttributes(attributes)
            .setWillPauseWhenDucked(true)
            .setOnAudioFocusChangeListener { change ->
                if (change == AudioManager.AUDIOFOCUS_LOSS ||
                    change == AudioManager.AUDIOFOCUS_LOSS_TRANSIENT ||
                    change == AudioManager.AUDIOFOCUS_LOSS_TRANSIENT_CAN_DUCK
                ) {
                    synchronized(lock) {
                        if (focusToken === token) {
                            val failure = SpeechPlaybackException("Reading stopped because audio focus was lost.")
                            focusFailure = failure
                            current?.let { finish(it, failure) }
                            abandonFocus()
                        }
                    }
                }
            }
            .build()
        focusToken = token
        focusRequest = request
        // Target 35+ may deny a fresh grant after the app backgrounds.
        // Never start playback on denial or wait for a delayed grant.
        if (audioManager.requestAudioFocus(request) != AudioManager.AUDIOFOCUS_REQUEST_GRANTED) {
            abandonFocus()
            throw SpeechPlaybackException("This phone cannot start reading audio right now.")
        }
    }

    /** Invalidates callbacks before abandoning this exact focus request. */
    private fun abandonFocus() {
        val request = focusRequest
        focusRequest = null
        focusToken = null
        if (request != null) audioManager.abandonAudioFocusRequest(request)
    }

    private fun finish(playback: Playback, failure: SpeechPlaybackException?) = synchronized(lock) {
        if (playback.released) return@synchronized
        playback.released = true
        if (current === playback) current = null
        // MediaPlayer is thread-confined, including cancellation from the
        // process scope's Default dispatcher. Late callbacks see released.
        val release = Runnable {
            runCatching { playback.player.release() }
            playback.file.delete()
        }
        if (Looper.myLooper() == mediaHandler.looper) {
            release.run()
        } else {
            mediaHandler.post(release)
        }
        if (failure == null) {
            playback.done.complete(Unit)
        } else {
            playback.done.completeExceptionally(failure)
        }
        Unit
    }

    private fun playbackFailure(cause: Throwable? = null) =
        SpeechPlaybackException("This phone could not play the relay audio.", cause)
}
