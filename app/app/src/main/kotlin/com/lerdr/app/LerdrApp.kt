package com.lerdr.app

import android.app.Application
import com.lerdr.app.security.LockState
import com.lerdr.app.session.SessionRepository
import dagger.hilt.android.HiltAndroidApp
import javax.inject.Inject
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch

@HiltAndroidApp
class LerdrApp : Application() {

    @Inject
    lateinit var sessions: SessionRepository

    @Inject
    lateinit var lockState: LockState

    private val applicationScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    override fun onCreate() {
        super.onCreate()
        // The oracle verifies before it connects at open: when the app
        // lock is armed, no relay socket opens until one verification
        // succeeds. With the setting off the gate opens immediately.
        applicationScope.launch {
            lockState.awaitUnlocked()
            sessions.start()
        }
    }
}
