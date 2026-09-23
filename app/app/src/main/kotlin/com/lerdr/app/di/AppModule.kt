package com.lerdr.app.di

import android.content.Context
import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.preferencesDataStoreFile
import com.lerdr.app.home.HomeRepository
import com.lerdr.app.home.RealHomeRepository
import com.lerdr.app.session.PaneBudget
import com.lerdr.app.session.RelaySessionFactory
import com.lerdr.app.session.SessionRepository
import dagger.Binds
import dagger.Module
import dagger.Provides
import dagger.hilt.InstallIn
import dagger.hilt.android.qualifiers.ApplicationContext
import dagger.hilt.components.SingletonComponent
import javax.inject.Singleton
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import lerdr.core.data.CredentialStore
import lerdr.core.data.DraftStore
import lerdr.core.data.KeystoreCredentialStore
import lerdr.core.data.RelayRegistry
import lerdr.core.store.AgentStore
import lerdr.core.store.ConnectionStore
import lerdr.core.store.WorkspaceStore

/**
 * App-level bindings — the real graph. Screens stay ViewModel-driven;
 * everything below is the singleton store/transport/data seam.
 */
@Module
@InstallIn(SingletonComponent::class)
abstract class AppModule {

    @Binds
    @Singleton
    abstract fun bindHomeRepository(impl: RealHomeRepository): HomeRepository

    @Binds
    @Singleton
    abstract fun bindCredentialStore(impl: KeystoreCredentialStore): CredentialStore

    companion object {
        @Provides
        @Singleton
        @AppScope
        fun appScope(): CoroutineScope =
            CoroutineScope(SupervisorJob() + Dispatchers.Default)

        @Provides
        @Singleton
        fun dataStore(@ApplicationContext context: Context): DataStore<Preferences> =
            PreferenceDataStoreFactory.create(
                produceFile = { context.preferencesDataStoreFile("lerdr") },
            )

        @Provides
        @Singleton
        fun keystoreCredentialStore(
            @ApplicationContext context: Context,
            @AppScope scope: CoroutineScope,
        ): KeystoreCredentialStore = KeystoreCredentialStore.create(context, scope)

        @Provides
        @Singleton
        fun relayRegistry(
            dataStore: DataStore<Preferences>,
            @AppScope scope: CoroutineScope,
        ): RelayRegistry = RelayRegistry(dataStore, scope)

        @Provides
        @Singleton
        fun draftStore(dataStore: DataStore<Preferences>): DraftStore =
            DraftStore(dataStore)

        @Provides
        @Singleton
        fun agentStore(@AppScope scope: CoroutineScope): AgentStore = AgentStore(scope)

        @Provides
        @Singleton
        fun workspaceStore(): WorkspaceStore = WorkspaceStore()

        /** Wall clock — injectable so repositories stay testable. */
        @Provides
        fun clock(): () -> Long = System::currentTimeMillis

        @Provides
        @Singleton
        fun connectionStore(clock: () -> Long): ConnectionStore = ConnectionStore(clock = clock)

        @Provides
        @Singleton
        fun relaySessionFactory(): RelaySessionFactory = RelaySessionFactory.websocket

        @Provides
        fun paneBudget(): PaneBudget = PaneBudget()
    }
}
