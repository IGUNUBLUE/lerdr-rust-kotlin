package com.lerdr.app.di

import com.lerdr.app.notify.AgentAttentionNotifier
import com.lerdr.app.session.PaneBudget
import com.lerdr.app.session.RelaySessionFactory
import com.lerdr.app.session.SessionRepository
import dagger.Module
import dagger.Provides
import dagger.hilt.InstallIn
import dagger.hilt.components.SingletonComponent
import javax.inject.Singleton
import kotlinx.coroutines.CoroutineScope
import lerdr.core.data.CredentialStore
import lerdr.core.data.RelayRegistry
import lerdr.core.store.AgentStore
import lerdr.core.store.ConnectionStore
import lerdr.core.store.WorkspaceStore

/**
 * Notification startup wiring.
 *
 * [SessionRepository] is normally JIT-bound via its `@Inject` constructor.
 * This explicit binding exists solely to attach
 * [AgentAttentionNotifier.start] to that same singleton creation:
 * `LerdrApp` injects the repository in `onCreate`, so the notifier boots
 * with the session hub without the Application class (or the repository)
 * knowing notifications exist.
 *
 * The constructor call mirrors the injected signature exactly — if the
 * repository's parameters change, this file fails to compile, loudly. The
 * notifier itself depends only on the stores, so the graph stays a DAG.
 */
@Module
@InstallIn(SingletonComponent::class)
object NotifyModule {

    @Provides
    @Singleton
    fun sessionRepository(
        @AppScope scope: CoroutineScope,
        credentialStore: CredentialStore,
        relayRegistry: RelayRegistry,
        agentStore: AgentStore,
        workspaceStore: WorkspaceStore,
        connectionStore: ConnectionStore,
        sessionFactory: RelaySessionFactory,
        paneBudget: PaneBudget,
        attentionNotifier: AgentAttentionNotifier,
    ): SessionRepository {
        attentionNotifier.start()
        return SessionRepository(
            scope = scope,
            credentialStore = credentialStore,
            relayRegistry = relayRegistry,
            agentStore = agentStore,
            workspaceStore = workspaceStore,
            connectionStore = connectionStore,
            sessionFactory = sessionFactory,
            budget = paneBudget,
        )
    }
}
