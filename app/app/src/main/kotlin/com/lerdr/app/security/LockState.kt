package com.lerdr.app.security

import com.lerdr.app.settings.AppPreferences
import javax.inject.Inject
import javax.inject.Singleton
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.launch

/**
 * Process-scoped app-lock latch shared by the UI gate and the session
 * bootstrap. The oracle "verifies before it will connect at open"
 * (docs/security.md) — [awaitUnlocked] is what `LerdrApp` suspends
 * `sessions.start()` on, so no relay socket opens before verification.
 *
 * Fail-closed like [LockViewModel]: `unlocked` starts false and only a
 * successful `BiometricPrompt` (or a disabled setting) opens it. A
 * disable→enable cycle re-locks. This is a UX gate, not encryption —
 * stored credentials stay under the Keystore seal in `:core:data`.
 */
@Singleton
class LockState @Inject constructor(
    private val preferences: AppPreferences,
) {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    /** True after one successful verification — process-scoped by design. */
    private val sessionUnlocked = MutableStateFlow(false)

    val locked: StateFlow<Boolean> = combine(
        preferences.appLockEnabled,
        sessionUnlocked,
    ) { enabled, unlocked -> enabled && !unlocked }
        .stateIn(scope, SharingStarted.Eagerly, initialValue = true)

    init {
        // Disabling clears the latch so the next enable locks immediately.
        scope.launch {
            preferences.appLockEnabled.collect { enabled ->
                if (!enabled) sessionUnlocked.value = false
            }
        }
    }

    /** Suspend until the gate is open (setting off, or verified once). */
    suspend fun awaitUnlocked() {
        locked.first { !it }
    }

    /** Latch the session open — call after a successful prompt result. */
    fun unlock() {
        sessionUnlocked.value = true
    }

    /** Re-lock now — only observable while the setting is enabled. */
    fun lock() {
        sessionUnlocked.value = false
    }
}
