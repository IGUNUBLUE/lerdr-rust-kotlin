package com.lerdr.app.update

import android.app.DownloadManager
import android.content.Intent
import android.net.Uri
import android.content.pm.Signature
import android.content.pm.SigningInfo
import android.content.pm.PackageInfo
import java.io.ByteArrayInputStream
import androidx.core.content.edit
import com.google.common.truth.Truth.assertThat
import com.lerdr.app.BuildConfig
import com.lerdr.app.notify.LerdrNotifier
import com.lerdr.app.update.AppUpdateManager.Companion.isNewerVersion
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = com.lerdr.app.TestApp::class)
@OptIn(ExperimentalCoroutinesApi::class)
class AppUpdateManagerTest {

    private fun manager(scope: TestScope): AppUpdateManager {
        val context = RuntimeEnvironment.getApplication()
        // Each test gets a clean bookkeeping file.
        context.getSharedPreferences(AppUpdateManager.PREFS_FILE, 0)
            .edit { clear() }
        return AppUpdateManager(context, scope.backgroundScope, LerdrNotifier(context))
            .also { it.pollMs = 0 }
    }

    private fun release(tag: String) = ReleaseInfo(
        tag = tag,
        apkUrl = "https://example.com/lerdr_${tag.removePrefix("v")}_universal.apk",
        apkBytes = 21_000_000L,
    )

    /** Real-clock poll — the manager's scope rides the test scheduler. */
    private fun TestScope.await(condition: () -> Boolean) {
        val deadline = System.currentTimeMillis() + 5_000
        while (!condition() && System.currentTimeMillis() < deadline) {
            runCurrent()
            Thread.sleep(5)
        }
        check(condition()) { "condition not met within deadline" }
    }

    // ── version compare ─────────────────────────────────────────────

    @Test
    fun `newer tag beats current version`() {
        assertThat(isNewerVersion("0.0.11", "v0.0.12")).isTrue()
        assertThat(isNewerVersion("0.0.11", "0.0.12")).isTrue()
        assertThat(isNewerVersion("0.0.11", "v0.1.0")).isTrue()
        assertThat(isNewerVersion("0.0.11", "v1.0.0")).isTrue()
        assertThat(isNewerVersion("0.0.11", "v0.0.11.1")).isTrue()
    }

    @Test
    fun `same or older tag is not newer`() {
        assertThat(isNewerVersion("0.0.11", "v0.0.11")).isFalse()
        assertThat(isNewerVersion("0.0.11", "v0.0.10")).isFalse()
        assertThat(isNewerVersion("0.0.11", "v0.0")).isFalse()
        assertThat(isNewerVersion("0.0.11", "garbage")).isFalse()
    }

    // ── checkNow ────────────────────────────────────────────────────

    @Test
    fun `checkNow marks up-to-date for current tag`() = runTest {
        val m = manager(this)
        m.releaseFetcher = { release("v${BuildConfig.VERSION_NAME}") }
        m.checkNow()
        await { m.state.value.phase == UpdatePhase.UP_TO_DATE }
    }

    @Test
    fun `checkNow surfaces a newer release and notifies once per tag`() = runTest {
        val m = manager(this)
        val newer = bump(BuildConfig.VERSION_NAME)
        m.releaseFetcher = { release("v$newer") }

        m.checkNow()
        await { m.state.value.phase == UpdatePhase.AVAILABLE }
        assertThat(m.state.value.latestVersion).isEqualTo(newer)
        assertThat(updateNotifications()).hasSize(1)

        // A second probe on the same tag does not re-notify.
        m.checkNow()
        await { m.state.value.phase == UpdatePhase.AVAILABLE }
        assertThat(updateNotifications()).hasSize(1)
    }

    @Test
    fun `checkNow failure lands in FAILED`() = runTest {
        val m = manager(this)
        m.releaseFetcher = { error("boom") }
        m.checkNow()
        await { m.state.value.phase == UpdatePhase.FAILED }
    }

    // ── download → install ──────────────────────────────────────────

    @Test
    fun `update without install permission gates on the grant`() = runTest {
        val m = manager(this)
        m.releaseFetcher = { release("v${bump(BuildConfig.VERSION_NAME)}") }
        m.canInstallPackages = { false }
        m.checkNow()
        await { m.state.value.phase == UpdatePhase.AVAILABLE }

        m.startUpdate()
        assertThat(m.state.value.phase)
            .isEqualTo(UpdatePhase.NEEDS_INSTALL_PERMISSION)

        // Returning denied drops back so the row offers the gate again.
        m.resumeAfterPermission()
        assertThat(m.state.value.phase).isEqualTo(UpdatePhase.AVAILABLE)
    }

    @Test
    fun `update downloads then fires the installer`() = runTest {
        val m = manager(this)
        m.releaseFetcher = { release("v${bump(BuildConfig.VERSION_NAME)}") }
        m.canInstallPackages = { true }
        val apk = Uri.parse("content://downloads/my_downloads/42")
        val bytes = "trusted update bytes".toByteArray()
        val context = RuntimeEnvironment.getApplication()
        shadowOf(context.contentResolver).registerInputStream(apk, ByteArrayInputStream(bytes))
        val signing = SigningInfo().also {
            shadowOf(it).setSignatures(arrayOf(Signature("1234")))
        }
        shadowOf(context.packageManager).getInternalMutablePackageInfo(context.packageName)
            .signingInfo = signing
        m.apkStager.archiveInfo = {
            PackageInfo().apply {
                packageName = context.packageName
                longVersionCode = BuildConfig.VERSION_CODE.toLong() + 1
                signingInfo = signing
            }
        }
        val statuses = mutableListOf(
            DownloadManager.STATUS_RUNNING,
            DownloadManager.STATUS_SUCCESSFUL,
        )
        m.enqueueDownload = { _, _ -> 42L }
        m.downloadStatus = {
            statuses.removeFirstOrNull() ?: DownloadManager.STATUS_SUCCESSFUL
        }
        m.downloadedUri = { apk }

        m.checkNow()
        await { m.state.value.phase == UpdatePhase.AVAILABLE }
        m.startUpdate()
        assertThat(m.state.value.phase).isEqualTo(UpdatePhase.DOWNLOADING)

        await { shadowOf(context).peekNextStartedActivity() != null }

        val started = shadowOf(RuntimeEnvironment.getApplication())
            .nextStartedActivity
        assertThat(started.action).isEqualTo(Intent.ACTION_VIEW)
        assertThat(started.data!!.authority).isEqualTo("${context.packageName}.updates")
        assertThat(context.contentResolver.openInputStream(started.data!!)!!.use { it.readBytes() })
            .isEqualTo(bytes)
        assertThat(started.type).isEqualTo(AppUpdateManager.APK_MIME)
    }

    @Test
    fun `failed download returns to FAILED`() = runTest {
        val m = manager(this)
        m.releaseFetcher = { release("v${bump(BuildConfig.VERSION_NAME)}") }
        m.canInstallPackages = { true }
        m.enqueueDownload = { _, _ -> 7L }
        m.downloadStatus = { DownloadManager.STATUS_FAILED }

        m.checkNow()
        await { m.state.value.phase == UpdatePhase.AVAILABLE }
        m.startUpdate()
        await { m.state.value.phase == UpdatePhase.FAILED }
    }

    @Test
    fun `cancelled download clears pending state and permits another check`() = runTest {
        val m = manager(this)
        val newer = bump(BuildConfig.VERSION_NAME)
        m.releaseFetcher = { release("v$newer") }
        m.canInstallPackages = { true }
        m.enqueueDownload = { _, _ -> 7L }
        val statuses = mutableListOf(DownloadManager.STATUS_RUNNING, -1)
        m.downloadStatus = { statuses.removeFirstOrNull() ?: -1 }

        m.checkNow()
        await { m.state.value.phase == UpdatePhase.AVAILABLE }
        m.startUpdate()
        await { m.state.value.phase == UpdatePhase.FAILED }

        val prefs = RuntimeEnvironment.getApplication()
            .getSharedPreferences(AppUpdateManager.PREFS_FILE, 0)
        assertThat(m.state.value.detail).contains("cancelled or removed")
        assertThat(prefs.contains(AppUpdateManager.KEY_DOWNLOAD_ID)).isFalse()
        assertThat(prefs.contains(AppUpdateManager.KEY_PENDING_TAG)).isFalse()
        m.checkNow()
        await { m.state.value.phase == UpdatePhase.AVAILABLE }
        assertThat(m.state.value.latestVersion).isEqualTo(newer)
    }

    @Test
    fun `restored missing download does not resume polling on another cold start`() = runTest {
        val context = RuntimeEnvironment.getApplication()
        val prefs = context.getSharedPreferences(AppUpdateManager.PREFS_FILE, 0)
        prefs.edit(commit = true) {
            clear()
            putLong(AppUpdateManager.KEY_DOWNLOAD_ID, 7L)
            putString(AppUpdateManager.KEY_PENDING_TAG, bump(BuildConfig.VERSION_NAME))
            putBoolean(AppUpdateManager.KEY_PROTECTED_DOWNLOAD, true)
        }
        val m = AppUpdateManager(context, backgroundScope, LerdrNotifier(context))
            .also {
                it.pollMs = 0
                it.downloadStatus = { -1 }
            }
        await { m.state.value.phase == UpdatePhase.FAILED }

        val restored = AppUpdateManager(context, backgroundScope, LerdrNotifier(context))
            .also {
                it.pollMs = 0
                it.downloadStatus = { error("Cancelled download must not be polled again") }
            }
        await { restored.state.value.phase == UpdatePhase.FAILED }
        assertThat(restored.state.value.detail).contains("cancelled or removed")
        assertThat(prefs.contains(AppUpdateManager.KEY_PENDING_TAG)).isFalse()
    }

    // ── helpers ─────────────────────────────────────────────────────

    /** Next patch version — the release check resolves against it. */
    private fun bump(version: String): String {
        val parts = version.split('.').map(String::toInt).toMutableList()
        parts[parts.lastIndex] += 1
        return parts.joinToString(".")
    }

    private fun updateNotifications() =
        shadowOf(
            RuntimeEnvironment.getApplication()
                .getSystemService(android.app.NotificationManager::class.java),
        ).allNotifications
            .filter { it.channelId == "app_update" }
}
