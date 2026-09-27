package com.lerdr.app

import android.app.Application
import com.lerdr.app.di.AppScope
import com.lerdr.app.security.LockState
import com.lerdr.app.session.SessionRepository
import com.lerdr.app.update.AppUpdateManager
import dagger.hilt.android.HiltAndroidApp
import javax.inject.Inject
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

@HiltAndroidApp
class LerdrApp : Application() {

    @Inject
    lateinit var sessions: SessionRepository

    @Inject
    lateinit var lockState: LockState

    @Inject
    @AppScope
    lateinit var appScope: CoroutineScope

    @Inject
    lateinit var updates: AppUpdateManager

    override fun onCreate() {
        super.onCreate()
        // Lerdr verifies before it connects at open: when the app
        // lock is armed, no relay socket opens until one verification
        // succeeds. With the setting off the gate opens immediately.
        appScope.launch {
            lockState.awaitUnlocked()
            sessions.start()
        }
        // push_viewed_pane's `unlocked` input — lock transitions clear the
        // viewed pane, unlocks republish it (Lerdr's App-level effect).
        appScope.launch {
            lockState.locked.collect { sessions.setLocked(it) }
        }
        // One GitHub latest-release probe per cold start — unrelated to the
        // lock gate and relay sessions (pure HTTPS, no credentials).
        updates.checkNow()
    }
}
