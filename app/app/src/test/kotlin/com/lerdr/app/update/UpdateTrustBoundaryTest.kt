package com.lerdr.app.update

import android.app.DownloadManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInfo
import android.content.pm.PackageManager
import android.content.pm.Signature
import android.content.pm.SigningInfo
import android.net.Uri
import androidx.core.content.edit
import androidx.core.content.FileProvider
import com.google.common.truth.Truth.assertThat
import com.lerdr.app.BuildConfig
import com.lerdr.app.MainActivity
import com.lerdr.app.notify.LerdrNotifier
import java.io.File
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.After
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowContentResolver

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [28, 34], application = com.lerdr.app.TestApp::class)
@OptIn(ExperimentalCoroutinesApi::class)
class UpdateTrustBoundaryTest {
    private val context get() = RuntimeEnvironment.getApplication()
    private val prefs get() = context.getSharedPreferences(AppUpdateManager.PREFS_FILE, 0)
    private val signer = Signature("1234")
    private val other = Signature("abcd")
    private val ancestor = Signature("4567")
    private val sources = mutableListOf<File>()
    private val newer = "99.0.0"
    private val bytes = "downloaded APK bytes".toByteArray()
    @Before
    fun initializeProviderForCurrentSandbox() {
        val authority = "${context.packageName}.updates"
        val info = requireNotNull(
            context.packageManager.resolveContentProvider(authority, PackageManager.GET_META_DATA),
        )
        // Robolectric changes filesDir between tests; FileProvider caches roots by authority.
        val provider = FileProvider().also { it.attachInfo(context, info) }
        ShadowContentResolver.registerProviderInternal(authority, provider)
    }


    @After
    fun cleanup() {
        sources.forEach(File::delete)
        File(context.filesDir, AppUpdateManager.UPDATES_DIR).listFiles()?.forEach(File::delete)
    }

    private fun signing(vararg current: Signature, history: Array<Signature>? = null): SigningInfo =
        SigningInfo().also {
            shadowOf(it).setSignatures(current)
            if (history != null) shadowOf(it).setPastSigningCertificates(history)
        }

    private fun archive(
        signing: SigningInfo = signing(signer),
        name: String = context.packageName,
        version: Long = 101,
    ): PackageInfo = PackageInfo().apply {
        packageName = name
        longVersionCode = version
        signingInfo = signing
    }

    private fun manager(scope: TestScope): AppUpdateManager {
        prefs.edit(commit = true) { clear() }
        File(context.filesDir, AppUpdateManager.UPDATES_DIR).listFiles()?.forEach(File::delete)
        val installed = shadowOf(context.packageManager).getInternalMutablePackageInfo(context.packageName)
        installed.longVersionCode = 100
        installed.signingInfo = signing(signer, history = arrayOf(ancestor, signer))
        return AppUpdateManager(context, scope.backgroundScope, LerdrNotifier(context)).also {
            it.pollMs = 1
            it.canInstallPackages = { true }
            it.releaseFetcher = { ReleaseInfo(newer, "https://example.com/update.apk", 100) }
            it.enqueueDownload = { _, _ -> 42L }
            it.downloadStatus = { DownloadManager.STATUS_SUCCESSFUL }
            it.apkStager.archiveInfo = { archive() }
        }
    }

    private fun source(): File = File.createTempFile("update-source-", ".apk", context.cacheDir)
        .also { it.writeBytes(bytes); sources.add(it) }

    private fun TestScope.await(condition: () -> Boolean) {
        val deadline = System.currentTimeMillis() + 5_000
        while (!condition() && System.currentTimeMillis() < deadline) {
            runCurrent()
            testScheduler.advanceTimeBy(1)
            Thread.sleep(5)
        }
        check(condition()) {
            "update boundary did not finish: ${prefs.getString(AppUpdateManager.KEY_ERROR, null)}"
        }
    }

    private fun TestScope.available(manager: AppUpdateManager) {
        manager.checkNow()
        await { manager.state.value.phase == UpdatePhase.AVAILABLE }
    }

    private fun pending() {
        prefs.edit(commit = true) {
            putLong(AppUpdateManager.KEY_DOWNLOAD_ID, 42L)
            putString(AppUpdateManager.KEY_PENDING_TAG, newer)
            putBoolean(AppUpdateManager.KEY_PROTECTED_DOWNLOAD, true)
        }
    }

    private fun receiver(scope: TestScope, manager: AppUpdateManager): UpdateDownloadReceiver =
        UpdateDownloadReceiver().also { receiver ->
            receiver.seams = {
                object : UpdateDownloadReceiver.Seams {
                    override fun notifier() = LerdrNotifier(context)
                    override fun updates() = manager
                    override fun appScope(): CoroutineScope = scope.backgroundScope
                }
            }
        }

    private fun complete(receiver: UpdateDownloadReceiver, id: Long = 42L) {
        receiver.onReceive(
            context,
            Intent(DownloadManager.ACTION_DOWNLOAD_COMPLETE)
                .putExtra(DownloadManager.EXTRA_DOWNLOAD_ID, id),
        )
    }

    private fun notifications() = shadowOf(context.getSystemService(android.app.NotificationManager::class.java))
        .allNotifications.filter { it.channelId == "app_update" }

    @Test
    fun `untrusted and absent archives cannot reach installer from either completion path`() = runTest {
        val badArchives = listOf(
            "wrong package" to archive(name = "com.attacker.app"),
            "wrong signer" to archive(signing(other)),
            "additional signer" to archive(signing(signer, other)),
            "rollback signer" to archive(signing(ancestor)),
            "sibling lineage" to archive(signing(other, history = arrayOf(ancestor, other))),
            "missing signer" to archive(SigningInfo()),
            "same version" to archive(version = 100),
            "older version" to archive(version = 99),
            "invalid archive" to null,
        )
        for ((reason, candidate) in badArchives + listOf("absent download" to archive())) {
            for (broadcast in listOf(false, true)) {
                val manager = manager(this)
                val source = source()
                manager.downloadedUri = { if (reason == "absent download") null else Uri.fromFile(source) }
                manager.apkStager.archiveInfo = { candidate }
                available(manager)
                if (broadcast) {
                    pending()
                    complete(receiver(this, manager))
                } else {
                    manager.startUpdate()
                }
                await { manager.state.value.phase == UpdatePhase.FAILED }
                assertThat(shadowOf(context).nextStartedActivity).isNull()
                assertThat(prefs.contains(AppUpdateManager.KEY_STAGED_TAG)).isFalse()
                assertThat(File(context.filesDir, AppUpdateManager.UPDATES_DIR).listFiles().orEmpty()).isEmpty()
                assertThat(source.readBytes()).isEqualTo(bytes)
                assertThat(notifications()).isEmpty()
            }
        }
    }

    @Test
    fun `receiver stages stable private bytes and requires reviewed install after process death`() = runTest {
        val original = manager(this)
        val source = source()
        original.downloadedUri = { Uri.fromFile(source) }
        available(original)
        LerdrNotifier(context).cancelUpdateNotification()
        pending()
        val receiver = receiver(this, original)
        complete(receiver, id = 999L)
        assertThat(prefs.contains(AppUpdateManager.KEY_STAGED_FILE)).isFalse()
        complete(receiver)
        await {
            original.state.value.phase == UpdatePhase.READY_TO_INSTALL &&
                notifications().any { it.contentIntent != null }
        }
        assertThat(shadowOf(receiver).wentAsync()).isTrue()
        assertThat(shadowOf(context).nextStartedActivity).isNull()
        val notificationIntent = shadowOf(notifications().single().contentIntent).savedIntent
        assertThat(notificationIntent.component!!.className).isEqualTo(MainActivity::class.java.name)
        assertThat(notificationIntent.dataString).isEqualTo("lerdr://settings")

        source.writeText("attacker replaced external download after staging")
        val restored = AppUpdateManager(context, backgroundScope, LerdrNotifier(context))
        restored.apkStager.archiveInfo = { archive() }
        var permitted = false
        restored.canInstallPackages = { permitted }
        await { restored.state.value.phase == UpdatePhase.READY_TO_INSTALL }
        restored.startUpdate()
        assertThat(restored.state.value.phase).isEqualTo(UpdatePhase.NEEDS_INSTALL_PERMISSION)
        restored.resumeAfterPermission()
        assertThat(restored.state.value.phase).isEqualTo(UpdatePhase.READY_TO_INSTALL)
        restored.startUpdate()
        permitted = true
        restored.resumeAfterPermission()
        await { shadowOf(context).peekNextStartedActivity() != null }
        val install = shadowOf(context).nextStartedActivity
        assertThat(install.action).isEqualTo(Intent.ACTION_VIEW)
        assertThat(install.type).isEqualTo(AppUpdateManager.APK_MIME)
        assertThat(install.data!!.authority).isEqualTo("${context.packageName}.updates")
        assertThat(install.flags and Intent.FLAG_GRANT_WRITE_URI_PERMISSION).isEqualTo(0)
        assertThat(context.contentResolver.openInputStream(install.data!!)!!.use { it.readBytes() })
            .isEqualTo(bytes)
    }

    @Test
    fun `forward signing lineage and exact multisigner identity reach installer`() = runTest {
        val identities = listOf(
            signing(signer) to signing(other, history = arrayOf(ancestor, signer, other)),
            signing(signer, other) to signing(other, signer),
        )
        for ((installed, candidate) in identities) {
            val manager = manager(this)
            shadowOf(context.packageManager).getInternalMutablePackageInfo(context.packageName)
                .signingInfo = installed
            manager.apkStager.archiveInfo = { archive(candidate) }
            manager.downloadedUri = { Uri.fromFile(source()) }
            available(manager)
            manager.startUpdate()
            await { shadowOf(context).peekNextStartedActivity() != null }
            assertThat(shadowOf(context).nextStartedActivity.type).isEqualTo(AppUpdateManager.APK_MIME)
        }
    }

    @Test
    fun `staged APK is revalidated at install time and rejection can be retried`() = runTest {
        val manager = manager(this)
        manager.downloadedUri = { Uri.fromFile(source()) }
        available(manager)
        pending()
        complete(receiver(this, manager))
        await { manager.state.value.phase == UpdatePhase.READY_TO_INSTALL }
        manager.apkStager.archiveInfo = { archive(signing(other)) }
        manager.startUpdate()
        await { manager.state.value.phase == UpdatePhase.FAILED }
        assertThat(shadowOf(context).nextStartedActivity).isNull()
        manager.apkStager.archiveInfo = { archive() }
        manager.checkNow()
        await { manager.state.value.phase == UpdatePhase.AVAILABLE }
        manager.startUpdate()
        await { shadowOf(context).peekNextStartedActivity() != null }
        assertThat(shadowOf(context).nextStartedActivity.type).isEqualTo(AppUpdateManager.APK_MIME)
    }

    @Test
    fun `pending protected download resumes after death without unsolicited installer`() = runTest {
        val original = manager(this)
        available(original)
        pending()
        val source = source()
        val restored = AppUpdateManager(context, backgroundScope, LerdrNotifier(context))
        restored.pollMs = 1
        restored.downloadStatus = { DownloadManager.STATUS_SUCCESSFUL }
        restored.downloadedUri = { Uri.fromFile(source) }
        restored.apkStager.archiveInfo = { archive() }
        restored.canInstallPackages = { true }
        await { restored.state.value.phase == UpdatePhase.READY_TO_INSTALL }
        assertThat(shadowOf(context).nextStartedActivity).isNull()
        assertThat(prefs.contains(AppUpdateManager.KEY_PENDING_TAG)).isFalse()
        restored.startUpdate()
        await { shadowOf(context).peekNextStartedActivity() != null }
        assertThat(shadowOf(context).nextStartedActivity.type).isEqualTo(AppUpdateManager.APK_MIME)
    }

    @Test
    fun `archive disappearing after staging is refused before installer`() = runTest {
        val manager = manager(this)
        manager.downloadedUri = { Uri.fromFile(source()) }
        available(manager)
        pending()
        complete(receiver(this, manager))
        await { manager.state.value.phase == UpdatePhase.READY_TO_INSTALL }
        val file = manager.apkStager.saved(prefs.getString(AppUpdateManager.KEY_STAGED_FILE, null)!!)
        check(file.delete())
        manager.startUpdate()
        await { manager.state.value.phase == UpdatePhase.FAILED }
        assertThat(shadowOf(context).nextStartedActivity).isNull()
    }

    @Test
    fun `legacy external download and unchecked installer token are retired on cold start`() = runTest {
        prefs.edit(commit = true) {
            clear()
            putLong(AppUpdateManager.KEY_DOWNLOAD_ID, 42L)
            putString(AppUpdateManager.KEY_STAGED_TAG, newer)
        }
        val uri = Uri.parse("content://downloads/my_downloads/42")
        val unchecked = PendingIntent.getActivity(
            context, 77_001,
            Intent(Intent.ACTION_VIEW).setDataAndType(uri, AppUpdateManager.APK_MIME)
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val manager = AppUpdateManager(context, backgroundScope, LerdrNotifier(context))
        manager.downloadedUri = { uri }
        await { !prefs.contains(AppUpdateManager.KEY_DOWNLOAD_ID) }
        var cancelled = false
        try {
            unchecked.send()
        } catch (_: PendingIntent.CanceledException) {
            cancelled = true
        }
        assertThat(cancelled).isTrue()
        assertThat(shadowOf(context).nextStartedActivity).isNull()
        assertThat(prefs.contains(AppUpdateManager.KEY_STAGED_TAG)).isFalse()
    }
}
