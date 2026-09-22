package com.lerdr.app.speech

import com.lerdr.app.session.SessionRepository
import com.lerdr.app.settings.SpeechPreferences
import dagger.hilt.EntryPoint
import dagger.hilt.InstallIn
import dagger.hilt.components.SingletonComponent

/**
 * Singleton seams `SpeechSection` pulls through `EntryPointAccessors` —
 * same pattern as `SettingsEntryPoint` (`hilt-navigation-compose` is
 * absent, so the `viewModel { }` factory resolves dependencies here).
 */
@EntryPoint
@InstallIn(SingletonComponent::class)
interface SpeechEntryPoint {
    fun sessionRepository(): SessionRepository
    fun speechPreferences(): SpeechPreferences
    fun relaySpeechPlayer(): RelaySpeechPlayer
}
