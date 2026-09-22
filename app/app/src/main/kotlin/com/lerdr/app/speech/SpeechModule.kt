package com.lerdr.app.speech

import android.content.Context
import com.lerdr.app.di.AppScope
import com.lerdr.app.security.LockState
import com.lerdr.app.session.SessionRepository
import com.lerdr.app.settings.SpeechPreferences
import dagger.Module
import dagger.Provides
import dagger.hilt.InstallIn
import dagger.hilt.android.qualifiers.ApplicationContext
import dagger.hilt.components.SingletonComponent
import javax.inject.Singleton
import kotlinx.coroutines.CoroutineScope

/**
 * Speech bindings — the process-wide [RelaySpeechPlayer]. The player lives
 * on [AppScope] like the oracle's module-level speech state, so reading
 * survives leaving the screen that started it.
 */
@Module
@InstallIn(SingletonComponent::class)
object SpeechModule {

    @Provides
    @Singleton
    fun relaySpeechPlayer(
        @ApplicationContext context: Context,
        @AppScope scope: CoroutineScope,
        sessions: SessionRepository,
        preferences: SpeechPreferences,
        lockState: LockState,
    ): RelaySpeechPlayer = RelaySpeechPlayer(
        scope = scope,
        enabled = preferences.enabled,
        language = preferences.language,
        sender = SessionSpeechSender(scope, sessions),
        sink = MediaPlayerSpeechAudioSink(context.cacheDir),
        locked = { lockState.locked.value },
    )
}
