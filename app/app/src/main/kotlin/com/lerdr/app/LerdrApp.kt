package com.lerdr.app

import android.app.Application
import com.lerdr.app.session.SessionRepository
import dagger.hilt.android.HiltAndroidApp
import javax.inject.Inject

@HiltAndroidApp
class LerdrApp : Application() {

    @Inject
    lateinit var sessions: SessionRepository

    override fun onCreate() {
        super.onCreate()
        // Boot the relay reconcile loops — registry diffs drive sessions.
        sessions.start()
    }
}
