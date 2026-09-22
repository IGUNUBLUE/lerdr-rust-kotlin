package com.lerdr.app.activity

import com.lerdr.app.session.SessionRepository
import dagger.hilt.EntryPoint
import dagger.hilt.InstallIn
import dagger.hilt.components.SingletonComponent

/**
 * Singleton seams the Activity screen pulls through
 * `EntryPointAccessors` — `hilt-navigation-compose` is absent, so the
 * `viewModel { }` factory in [ActivityScreen] resolves its dependencies
 * here instead of a `@HiltViewModel` graph.
 */
@EntryPoint
@InstallIn(SingletonComponent::class)
interface ActivityEntryPoint {
    fun sessionRepository(): SessionRepository
    fun activityJournal(): ActivityJournal
}
