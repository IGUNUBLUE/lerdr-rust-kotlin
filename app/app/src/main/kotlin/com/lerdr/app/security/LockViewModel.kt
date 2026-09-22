package com.lerdr.app.security

import androidx.lifecycle.ViewModel
import kotlinx.coroutines.flow.StateFlow

/**
 * App-lock state for the UI (docs/04 §App "biometric lock"; oracle
 * docs/security.md "device verification"). A UX gate: it covers the UI
 * until the device verifies the user once per process — stored
 * credentials stay under the Keystore seal in `:core:data`, untouched
 * by this flag.
 *
 * The latch itself lives in [LockState], the injectable singleton
 * `LerdrApp` also gates `sessions.start()` on — verification must happen
 * before any relay socket opens, not just before content composes.
 *
 * `BiometricPrompt` itself is deliberately absent here — prompting needs
 * a `FragmentActivity`, which a ViewModel never holds. The gate
 * composable calls [BiometricPromptHelper.authenticate] with the host
 * activity and reports the outcome back through [unlock].
 */
class LockViewModel(
    private val lockState: LockState,
) : ViewModel() {

    val locked: StateFlow<Boolean> = lockState.locked

    /** Latch the session open — call after a successful prompt result. */
    fun unlock() {
        lockState.unlock()
    }

    /** Re-lock now — only observable while the setting is enabled. */
    fun lock() {
        lockState.lock()
    }
}
