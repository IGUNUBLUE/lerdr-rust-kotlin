package com.lerdr.app.security

import android.app.KeyguardManager
import android.content.Context
import androidx.biometric.BiometricManager
import androidx.biometric.BiometricPrompt
import androidx.core.content.ContextCompat
import androidx.fragment.app.FragmentActivity
import javax.inject.Inject
import javax.inject.Singleton

/**
 * Boundary over `androidx.biometric` — the only place `BiometricPrompt`
 * and `BiometricManager` are touched. ViewModels and unit tests never see
 * the framework types: the gate composable hands in the host activity and
 * the answer comes back as a plain Boolean.
 *
 * Fallback policy (the lock is a UX gate, not encryption — oracle
 * docs/security.md "device verification"; spec-gap P2 owns the stolen-
 * phone story):
 * - The prompt always allows `BIOMETRIC_STRONG or DEVICE_CREDENTIAL`, so
 *   the system itself falls back to the lockscreen PIN/pattern/password
 *   when no biometric is enrolled or the hardware is absent.
 * - When nothing on the device can verify at all (no biometric hardware
 *   AND no secure lockscreen), [authenticate] reports success
 *   immediately rather than stranding the user out of their own app;
 *   the Settings row explains that the toggle is a no-op in that state.
 */
interface BiometricPromptHelper {

    /** True when at least one allowed authenticator can run on this device. */
    fun canPrompt(context: Context): Boolean

    /**
     * Shows the system verify prompt over [host] (must be a
     * [FragmentActivity] — `BiometricPrompt` attaches an internal
     * fragment to it). [onResult] receives `true` on successful
     * verification, and also when no authenticator exists at all (see
     * class doc); `false` on dismissal or a recoverable failure so the
     * lock surface can offer a retry.
     */
    fun authenticate(host: FragmentActivity, onResult: (unlocked: Boolean) -> Unit)
}

@Singleton
class AndroidBiometricPromptHelper @Inject constructor() : BiometricPromptHelper {

    override fun canPrompt(context: Context): Boolean {
        val probe = BiometricManager.from(context)
            .canAuthenticate(ALLOWED_AUTHENTICATORS)
        if (probe == BiometricManager.BIOMETRIC_SUCCESS) return true
        // The combined-authenticator probe predates reliable
        // DEVICE_CREDENTIAL reporting on older API levels — a secure
        // lockscreen alone is enough for the prompt to run.
        val keyguard = context.getSystemService(KeyguardManager::class.java)
        return keyguard?.isDeviceSecure == true
    }

    override fun authenticate(host: FragmentActivity, onResult: (Boolean) -> Unit) {
        if (!canPrompt(host)) {
            // Nothing to verify against — open the gate, never strand.
            onResult(true)
            return
        }
        val prompt = BiometricPrompt(
            host,
            ContextCompat.getMainExecutor(host),
            object : BiometricPrompt.AuthenticationCallback() {
                override fun onAuthenticationSucceeded(
                    result: BiometricPrompt.AuthenticationResult,
                ) = onResult(true)

                override fun onAuthenticationError(
                    errorCode: Int,
                    errString: CharSequence,
                ) {
                    // The keyguard can disappear between the probe and
                    // the prompt — same "nothing to verify with" case.
                    onResult(errorCode == BiometricPrompt.ERROR_NO_DEVICE_CREDENTIAL)
                }
            },
        )
        prompt.authenticate(PROMPT_INFO)
    }

    companion object {
        const val ALLOWED_AUTHENTICATORS: Int =
            BiometricManager.Authenticators.BIOMETRIC_STRONG or
                BiometricManager.Authenticators.DEVICE_CREDENTIAL

        /**
         * No `setNegativeButtonText` — it is illegal to combine one with
         * `DEVICE_CREDENTIAL`; the system renders its own fallback.
         */
        private val PROMPT_INFO: BiometricPrompt.PromptInfo =
            BiometricPrompt.PromptInfo.Builder()
                .setTitle("Unlock Lerdr")
                .setSubtitle("Verify it's you to continue")
                .setAllowedAuthenticators(ALLOWED_AUTHENTICATORS)
                .build()
    }
}
