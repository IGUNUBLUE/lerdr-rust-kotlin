package com.lerdr.app.security

import android.app.Activity
import android.app.KeyguardManager
import android.content.Intent
import android.os.Looper
import androidx.biometric.BiometricManager
import androidx.fragment.app.FragmentActivity
import com.google.common.truth.Truth.assertThat
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.annotation.Implementation
import org.robolectric.annotation.Implements
import org.robolectric.shadows.ShadowBiometricPrompt
import org.robolectric.shadows.ShadowKeyguardManager

@RunWith(RobolectricTestRunner::class)
@Config(
    sdk = [28, 29],
    application = com.lerdr.app.TestApp::class,
    shadows = [BiometricPromptHelperTest.CredentialKeyguard::class],
)
class BiometricPromptHelperTest {
    @Implements(KeyguardManager::class)
    class CredentialKeyguard : ShadowKeyguardManager() {
        @Implementation
        protected fun createConfirmDeviceCredentialIntent(
            title: CharSequence?,
            description: CharSequence?,
        ): Intent? = confirmationIntent

        companion object {
            var confirmationIntent: Intent? = null
        }
    }

    @Before
    fun resetChallenge() {
        CredentialKeyguard.confirmationIntent = Intent("audit.confirm_device_credentials")
    }

    private fun secure(host: FragmentActivity, value: Boolean = true) {
        shadowOf(host.getSystemService(KeyguardManager::class.java)).setIsDeviceSecure(value)
    }

    @Test
    fun `legacy no-authenticator fallback opens without constructing unsupported prompt`() {
        val controller = Robolectric.buildActivity(FragmentActivity::class.java).setup()
        val host = controller.get()
        secure(host, false)
        val helper = AndroidBiometricPromptHelper()
        val results = mutableListOf<Boolean>()

        assertThat(helper.canPrompt(host)).isFalse()
        helper.authenticate(host, results::add)

        assertThat(results).containsExactly(true)
        assertThat(shadowOf(host).nextStartedActivityForResult).isNull()
        controller.pause().stop().destroy()
    }

    @Test
    fun `legacy cancellation stays locked and retry accepts only verified result`() {
        val controller = Robolectric.buildActivity(FragmentActivity::class.java).setup()
        val host = controller.get()
        secure(host)
        val helper = AndroidBiometricPromptHelper()
        val results = mutableListOf<Boolean>()

        assertThat(helper.canPrompt(host)).isTrue()
        helper.authenticate(host, results::add)
        val canceled = shadowOf(host).nextStartedActivityForResult
        host.activityResultRegistry.dispatchResult(canceled.requestCode, Activity.RESULT_CANCELED, null)
        assertThat(results).containsExactly(false)
        shadowOf(Looper.getMainLooper()).idle()

        helper.authenticate(host, results::add)
        val verified = shadowOf(host).nextStartedActivityForResult
        assertThat(
            host.activityResultRegistry.dispatchResult(canceled.requestCode, Activity.RESULT_OK, null),
        ).isFalse()
        assertThat(results).containsExactly(false)
        host.activityResultRegistry.dispatchResult(verified.requestCode, Activity.RESULT_OK, null)
        host.activityResultRegistry.dispatchResult(verified.requestCode, Activity.RESULT_OK, null)
        assertThat(results).containsExactly(false, true).inOrder()
        controller.pause().stop().destroy()
    }

    @Test
    fun `overlapping retry neither launches another challenge nor accepts arbitrary result`() {
        val controller = Robolectric.buildActivity(FragmentActivity::class.java).setup()
        val host = controller.get()
        secure(host)
        val helper = AndroidBiometricPromptHelper()
        val first = mutableListOf<Boolean>()
        val retry = mutableListOf<Boolean>()
        helper.authenticate(host, first::add)
        val launched = shadowOf(host).nextStartedActivityForResult

        helper.authenticate(host, retry::add)
        assertThat(shadowOf(host).nextStartedActivityForResult).isNull()
        assertThat(retry).containsExactly(false)
        host.activityResultRegistry.dispatchResult(launched.requestCode, 42, null)
        assertThat(first).containsExactly(false)
        controller.pause().stop().destroy()
    }

    @Test
    fun `secure legacy device with missing confirmation intent fails closed and can retry`() {
        val controller = Robolectric.buildActivity(FragmentActivity::class.java).setup()
        val host = controller.get()
        secure(host)
        val helper = AndroidBiometricPromptHelper()
        val results = mutableListOf<Boolean>()
        CredentialKeyguard.confirmationIntent = null

        assertThat(helper.canPrompt(host)).isTrue()
        helper.authenticate(host, results::add)
        assertThat(results).containsExactly(false)
        CredentialKeyguard.confirmationIntent = Intent("audit.confirm_device_credentials")
        helper.authenticate(host, results::add)
        val launched = shadowOf(host).nextStartedActivityForResult
        host.activityResultRegistry.dispatchResult(launched.requestCode, Activity.RESULT_OK, null)
        assertThat(results).containsExactly(false, true).inOrder()
        controller.pause().stop().destroy()
    }

    @Test
    fun `rotation reconnects existing challenge without relaunching or invoking old callback`() {
        val controller = Robolectric.buildActivity(FragmentActivity::class.java).setup()
        val original = controller.get()
        secure(original)
        val helper = AndroidBiometricPromptHelper()
        val oldResults = mutableListOf<Boolean>()
        val newResults = mutableListOf<Boolean>()
        helper.authenticate(original, oldResults::add)
        val launched = shadowOf(original).nextStartedActivityForResult

        controller.recreate()
        val recreated = controller.get()
        helper.authenticate(recreated, newResults::add)
        assertThat(shadowOf(recreated).nextStartedActivityForResult).isNull()
        recreated.activityResultRegistry.dispatchResult(launched.requestCode, Activity.RESULT_OK, null)
        assertThat(oldResults).isEmpty()
        assertThat(newResults).containsExactly(true)
        controller.pause().stop().destroy()
    }

    @Test
    fun `result received during rotation is delivered once when gate reattaches`() {
        val controller = Robolectric.buildActivity(FragmentActivity::class.java).setup()
        val original = controller.get()
        secure(original)
        val helper = AndroidBiometricPromptHelper()
        val oldResults = mutableListOf<Boolean>()
        val newResults = mutableListOf<Boolean>()
        helper.authenticate(original, oldResults::add)
        val launched = shadowOf(original).nextStartedActivityForResult
        controller.recreate()
        val recreated = controller.get()
        recreated.activityResultRegistry.dispatchResult(launched.requestCode, Activity.RESULT_CANCELED, null)

        helper.authenticate(recreated, newResults::add)
        assertThat(oldResults).isEmpty()
        assertThat(newResults).containsExactly(false)
        helper.authenticate(recreated, newResults::add)
        val retry = shadowOf(recreated).nextStartedActivityForResult
        recreated.activityResultRegistry.dispatchResult(retry.requestCode, Activity.RESULT_OK, null)
        assertThat(newResults).containsExactly(false, true).inOrder()
        controller.pause().stop().destroy()
    }

    @Test
    fun `destroyed host cannot launch or receive successful verification`() {
        val controller = Robolectric.buildActivity(FragmentActivity::class.java).setup()
        val host = controller.get()
        secure(host)
        val helper = AndroidBiometricPromptHelper()
        val results = mutableListOf<Boolean>()
        helper.authenticate(host, results::add)
        val launched = shadowOf(host).nextStartedActivityForResult
        controller.pause().stop().destroy()

        host.activityResultRegistry.dispatchResult(launched.requestCode, Activity.RESULT_OK, null)
        assertThat(results).isEmpty()
        helper.authenticate(host, results::add)
        assertThat(results).containsExactly(false)
    }

    @Test
    @Config(sdk = [30, 34])
    fun `modern prompt allows strong biometric and credential and cancellation stays locked`() {
        val controller = Robolectric.buildActivity(FragmentActivity::class.java).setup()
        val host = controller.get()
        secure(host)
        val helper = AndroidBiometricPromptHelper()
        val results = mutableListOf<Boolean>()
        assertThat(helper.canPrompt(host)).isTrue()
        helper.authenticate(host, results::add)
        shadowOf(Looper.getMainLooper()).idle()

        assertThat(ShadowBiometricPrompt.getCurrentPrompt()!!.allowedAuthenticators).isEqualTo(
            BiometricManager.Authenticators.BIOMETRIC_STRONG or
                BiometricManager.Authenticators.DEVICE_CREDENTIAL,
        )
        ShadowBiometricPrompt.authenticateCurrentSessionWithError(
            android.hardware.biometrics.BiometricPrompt.BIOMETRIC_ERROR_USER_CANCELED,
            "Canceled",
        )
        shadowOf(Looper.getMainLooper()).idle()
        assertThat(results).containsExactly(false)
        helper.authenticate(host, results::add)
        shadowOf(Looper.getMainLooper()).idle()
        ShadowBiometricPrompt.authenticateCurrentSessionSuccessfully()
        shadowOf(Looper.getMainLooper()).idle()
        assertThat(results).containsExactly(false, true).inOrder()
        controller.pause().stop().destroy()
    }
}
