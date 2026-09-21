package com.lerdr.app

import android.app.Application

/**
 * Application used by Robolectric tests — the real `LerdrApp` eagerly
 * resolves the AndroidKeyStore cipher via Hilt, which does not exist on
 * the local JVM, so tests swap in this no-op application via
 * `@Config(application = TestApp::class)`.
 */
class TestApp : Application()
