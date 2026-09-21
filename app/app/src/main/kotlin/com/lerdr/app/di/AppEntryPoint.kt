package com.lerdr.app.di

import com.lerdr.app.home.HomeRepository
import com.lerdr.app.pairing.PairingManager
import com.lerdr.app.session.SessionRepository
import dagger.hilt.EntryPoint
import dagger.hilt.InstallIn
import dagger.hilt.components.SingletonComponent
import kotlinx.coroutines.CoroutineScope
import lerdr.core.data.DraftStore

/**
 * Service-locator seam for Nav3 entry scopes — `hilt-navigation-compose`
 * is absent, so `viewModel {}` initializer factories pull their
 * dependencies from here via `EntryPointAccessors.fromApplication`.
 */
@EntryPoint
@InstallIn(SingletonComponent::class)
interface AppEntryPoint {
    fun homeRepository(): HomeRepository
    fun sessionRepository(): SessionRepository
    fun pairingManager(): PairingManager
    fun draftStore(): DraftStore

    @AppScope
    fun appScope(): CoroutineScope
}
