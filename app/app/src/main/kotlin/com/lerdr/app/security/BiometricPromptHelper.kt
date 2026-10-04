package com.lerdr.app.security

import android.app.Activity
import android.app.KeyguardManager
import android.content.Context
import android.os.Build
import android.os.Handler
import android.os.Looper
import androidx.activity.result.ActivityResultLauncher
import androidx.activity.result.contract.ActivityResultContracts
import androidx.biometric.BiometricManager
import androidx.biometric.BiometricPrompt
import androidx.core.content.ContextCompat
import androidx.fragment.app.FragmentActivity
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import java.util.UUID
import javax.inject.Inject
import javax.inject.Singleton

/**
 * Boundary over `androidx.biometric` — the only place `BiometricPrompt`
 * and `BiometricManager` are touched. ViewModels and unit tests never see
 * the framework types: the gate composable hands in the host activity and
 * the answer comes back as a plain Boolean.
 *
 * Fallback policy (the lock is a UX gate, not encryption —
 * docs/10-spec-gaps.md records that it gates session startup and leaves
 * the stolen-phone threat model in P2):
 * - API 30+ allows `BIOMETRIC_STRONG or DEVICE_CREDENTIAL`; API 28/29
 *   uses the secure system credential-confirmation activity because
 *   AndroidX Biometric 1.1.0 cannot combine those authenticators there.
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
        val secure = context.getSystemService(KeyguardManager::class.java)?.isDeviceSecure == true
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) return secure
        return secure || BiometricManager.from(context).canAuthenticate(
            MODERN_AUTHENTICATORS,
        ) == BiometricManager.BIOMETRIC_SUCCESS
    }

    override fun authenticate(host: FragmentActivity, onResult: (Boolean) -> Unit) {
        if (host.lifecycle.currentState == Lifecycle.State.DESTROYED) {
            onResult(false)
            return
        }
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) {
            authenticateLegacy(host, onResult)
            return
        }
        if (!canPrompt(host)) {
            // Nothing to verify against — open the gate, never strand.
            onResult(true)
            return
        }
        var completed = false
        fun finish(unlocked: Boolean) {
            if (completed) return
            completed = true
            onResult(unlocked)
        }
        val prompt = BiometricPrompt(
            host,
            ContextCompat.getMainExecutor(host),
            object : BiometricPrompt.AuthenticationCallback() {
                override fun onAuthenticationSucceeded(
                    result: BiometricPrompt.AuthenticationResult,
                ) = finish(true)

                override fun onAuthenticationError(
                    errorCode: Int,
                    errString: CharSequence,
                ) {
                    // The keyguard can disappear between the probe and
                    // the prompt — same "nothing to verify with" case.
                    finish(errorCode == BiometricPrompt.ERROR_NO_DEVICE_CREDENTIAL)
                }
            },
        )
        prompt.authenticate(
            BiometricPrompt.PromptInfo.Builder()
                .setTitle("Unlock Lerdr")
                .setSubtitle("Verify it's you to continue")
                .setAllowedAuthenticators(MODERN_AUTHENTICATORS)
                .build(),
        )
    }

    @Suppress("DEPRECATION") // Supported credential confirmation on API 28/29.
    private fun authenticateLegacy(host: FragmentActivity, onResult: (Boolean) -> Unit) {
        val keyguard = host.getSystemService(KeyguardManager::class.java)
        if (keyguard == null) {
            onResult(false)
            return
        }
        if (!keyguard.isDeviceSecure) {
            onResult(true) // Explicit no-authenticator UX fallback.
            return
        }
        val request = ViewModelProvider(host)[LegacyVerificationRequest::class.java]
        if (request.attached) {
            onResult(false) // Retry while a challenge is open does not launch another.
            return
        }
        val resumed = request.key != null
        val key = request.key ?: "lerdr.device-verification.${UUID.randomUUID()}"
        request.key = key
        request.attached = true
        val lifecycle = host.lifecycle
        var launcher: ActivityResultLauncher<android.content.Intent>? = null
        var closed = false
        lateinit var observer: LifecycleEventObserver
        fun detach(afterDispatch: Boolean = false) {
            closed = true
            val registration = launcher
            if (afterDispatch && registration != null) {
                // The registry clears its in-flight marker after the callback returns.
                Handler(Looper.getMainLooper()).post { registration.unregister() }
            } else {
                registration?.unregister()
            }
            launcher = null
            lifecycle.removeObserver(observer)
            request.attached = false
        }
        fun finish(unlocked: Boolean, afterDispatch: Boolean = false) {
            if (closed) return
            request.key = null
            detach(afterDispatch)
            onResult(unlocked)
        }
        observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_DESTROY) detach()
        }
        lifecycle.addObserver(observer)
        // The no-LifecycleOwner overload can register after STARTED (Compose).
        // Explicit detach releases the callback; the ViewModel retains only the
        // registry key across rotation, never the activity, launcher or callback.
        launcher = host.activityResultRegistry.register(
            key,
            ActivityResultContracts.StartActivityForResult(),
        ) { result -> finish(result.resultCode == Activity.RESULT_OK, afterDispatch = true) }
        if (closed) {
            // register() may synchronously deliver a result saved during rotation.
            launcher?.unregister()
            launcher = null
            return
        }
        if (resumed) return
        try {
            val intent = keyguard.createConfirmDeviceCredentialIntent(
                "Unlock Lerdr",
                "Verify it's you to continue",
            )
            if (intent == null) {
                finish(false) // A secure device with no challenge must never fail open.
                return
            }
            launcher?.launch(intent)
        } catch (_: android.content.ActivityNotFoundException) {
            finish(false)
        } catch (_: SecurityException) {
            finish(false)
        }
    }

    private companion object {
        const val MODERN_AUTHENTICATORS: Int =
            BiometricManager.Authenticators.BIOMETRIC_STRONG or
                BiometricManager.Authenticators.DEVICE_CREDENTIAL
    }
}

/** Rotation state only; no activity, registered callback, or launcher is retained. */
internal class LegacyVerificationRequest : ViewModel() {
    var key: String? = null
    var attached = false
}
