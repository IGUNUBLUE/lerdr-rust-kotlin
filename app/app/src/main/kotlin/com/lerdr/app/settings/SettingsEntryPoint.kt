package com.lerdr.app.settings

import com.lerdr.app.session.SessionRepository
import dagger.hilt.EntryPoint
import dagger.hilt.InstallIn
import dagger.hilt.components.SingletonComponent

/**
 * Singleton seams the Settings screen pulls through
 * `EntryPointAccessors` — `hilt-navigation-compose` is absent, so the
 * `viewModel { }` factory in [SettingsScreen] resolves its dependencies
 * here instead of a `@HiltViewModel` graph.
 */
@EntryPoint
@InstallIn(SingletonComponent::class)
interface SettingsEntryPoint {
    fun sessionRepository(): SessionRepository
    fun appPreferences(): AppPreferences
}
