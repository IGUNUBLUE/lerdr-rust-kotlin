package com.lerdr.app.update

import android.app.DownloadManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.provider.Settings
import android.util.Log
import androidx.compose.runtime.Immutable
import androidx.core.content.edit
import com.lerdr.app.BuildConfig
import com.lerdr.app.di.AppScope
import com.lerdr.app.notify.LerdrNotifier
import com.lerdr.app.MainActivity
import dagger.hilt.android.qualifiers.ApplicationContext
import java.io.File
import java.util.concurrent.atomic.AtomicBoolean
import javax.inject.Inject
import javax.inject.Singleton
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.Serializable
import lerdr.core.protocol.LerdrJson
import okhttp3.OkHttpClient
import okhttp3.Request

/**
 * Self-update path — polls GitHub for the newest published release,
 * downloads the universal APK with [DownloadManager], and hands the file
 * to the system package installer.
 *
 * State machine: [UpdatePhase.UNCHECKED] → CHECKING → UP_TO_DATE /
 * AVAILABLE → (install-permission gate) → DOWNLOADING → READY_TO_INSTALL
 * → the user taps Install and the package installer takes over. Any
 * failure lands in FAILED with the failing step kept in [AppUpdateState]'s
 * detail so the row can offer a matching retry.
 *
 * `REQUEST_INSTALL_PACKAGES` is a one-time grant; when it is missing the
 * row routes through `Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES` and
 * [resumeAfterPermission] continues the pending step on return.
 */
@Singleton
class AppUpdateManager @Inject constructor(
    @param:ApplicationContext private val context: Context,
    @param:AppScope private val scope: CoroutineScope,
    private val notifier: LerdrNotifier,
) {

    private val prefs = context.getSharedPreferences(PREFS_FILE, Context.MODE_PRIVATE)
    private val http = OkHttpClient()
    internal val apkStager = UpdateApkStager(context)
    private val stagingLock = Mutex()
    private val installing = AtomicBoolean()

    private val _state = MutableStateFlow(AppUpdateState())
    val state: StateFlow<AppUpdateState> = _state

    /** Test seams — production values follow; unit tests swap these. */
    internal var releasesUrl: String = RELEASES_URL
    internal var releaseFetcher: suspend () -> ReleaseInfo = { fetchLatestRelease() }
    internal var enqueueDownload: (Uri, String) -> Long = { uri, name ->
        realEnqueue(uri, name)
    }
    internal var downloadStatus: (Long) -> Int = { id -> realStatus(id) }
    internal var downloadedUri: (Long) -> Uri? = { id ->
        downloadManager()?.getUriForDownloadedFile(id)
    }
    internal var canInstallPackages: () -> Boolean = {
        context.packageManager.canRequestPackageInstalls()
    }
    internal var pollMs: Long = POLL_MS

    private val restoration = scope.launch {
        stagingLock.withLock {
            withContext(Dispatchers.IO) { retireLegacyDownload() }
            val staged = prefs.getString(KEY_STAGED_TAG, null)
            val pending = prefs.getString(KEY_PENDING_TAG, null)
            if (staged != null && isNewerVersion(BuildConfig.VERSION_NAME, staged)) {
                _state.value = AppUpdateState(
                    phase = UpdatePhase.READY_TO_INSTALL,
                    latestVersion = staged,
                )
            } else if (pending != null) {
                _state.value = AppUpdateState(
                    phase = UpdatePhase.DOWNLOADING,
                    latestVersion = pending,
                )
                scope.launch {
                    watchDownload(prefs.getLong(KEY_DOWNLOAD_ID, -1L), autoInstall = false)
                }
            }
        }
    }

    /**
     * One GitHub `/releases/latest` round trip. Safe to call repeatedly —
     * in-flight checks and downloads short-circuit it.
     */
    fun checkNow() {
        if (_state.value.phase == UpdatePhase.CHECKING ||
            _state.value.phase == UpdatePhase.DOWNLOADING
        ) {
            return
        }
        _state.update { it.copy(phase = UpdatePhase.CHECKING) }
        scope.launch {
            restoration.join()
            if (_state.value.phase == UpdatePhase.DOWNLOADING) return@launch
            val outcome = runCatching { releaseFetcher() }
            val release = outcome.getOrNull()
            if (release == null) {
                Log.w(TAG, "release check failed", outcome.exceptionOrNull())
                _state.update {
                    it.copy(
                        phase = UpdatePhase.FAILED,
                        detail = outcome.exceptionOrNull()?.message.orEmpty(),
                    )
                }
                return@launch
            }
            val latest = release.tag.removePrefix("v")
            if (!isNewerVersion(BuildConfig.VERSION_NAME, latest)) {
                stagingLock.withLock {
                    withContext(Dispatchers.IO) { clearStagedFiles() }
                }
                _state.value = AppUpdateState(
                    phase = UpdatePhase.UP_TO_DATE,
                    latestVersion = latest,
                )
                notifier.cancelUpdateNotification()
                return@launch
            }
            if (prefs.getString(KEY_STAGED_TAG, null) == latest &&
                withContext(Dispatchers.IO) { downloadExists() }
            ) {
                _state.value = AppUpdateState(
                    phase = UpdatePhase.READY_TO_INSTALL,
                    latestVersion = latest,
                    apkBytes = release.apkBytes,
                )
                return@launch
            }
            _state.value = AppUpdateState(
                phase = UpdatePhase.AVAILABLE,
                latestVersion = latest,
                apkUrl = release.apkUrl,
                apkBytes = release.apkBytes,
            )
            maybeNotify(latest)
        }
    }

    /**
     * Row action — "Update" while AVAILABLE starts the download (gating on
     * the unknown-sources grant first); "Install" while READY_TO_INSTALL
     * hands the staged APK to the package installer.
     */
    fun startUpdate() {
        when (_state.value.phase) {
            UpdatePhase.READY_TO_INSTALL -> installStaged()
            UpdatePhase.AVAILABLE -> {
                if (!canInstallPackages()) {
                    _state.update { it.copy(phase = UpdatePhase.NEEDS_INSTALL_PERMISSION) }
                    return
                }
                enqueue()
            }
            else -> Unit
        }
    }

    /**
     * Called by the screen's activity-result callback after returning from the
     * install-permission settings — continues whichever step was pending.
     */
    fun resumeAfterPermission() {
        if (_state.value.phase != UpdatePhase.NEEDS_INSTALL_PERMISSION) return
        if (!canInstallPackages()) {
            // Still denied — drop back so the row offers the gate again.
            val staged = prefs.getString(KEY_STAGED_TAG, null) == _state.value.latestVersion
            _state.update {
                it.copy(phase = if (staged) UpdatePhase.READY_TO_INSTALL else UpdatePhase.AVAILABLE)
            }
            return
        }
        if (prefs.getString(KEY_STAGED_TAG, null) == _state.value.latestVersion) {
            _state.update { it.copy(phase = UpdatePhase.READY_TO_INSTALL) }
            installStaged()
        } else {
            _state.update { it.copy(phase = UpdatePhase.AVAILABLE) }
            enqueue()
        }
    }

    /** The system settings screen granting `REQUEST_INSTALL_PACKAGES`. */
    fun installPermissionIntent(): Intent =
        Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES)
            .setData(Uri.parse("package:${context.packageName}"))
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)

    // ── download ────────────────────────────────────────────────────

    private fun enqueue() {
        val version = _state.value.latestVersion
        val url = _state.value.apkUrl
        _state.update { it.copy(phase = UpdatePhase.DOWNLOADING, detail = "") }
        scope.launch {
            val outcome = runCatching {
                stagingLock.withLock {
                    withContext(Dispatchers.IO) {
                        clearStagedFiles()
                        val id = enqueueDownload(Uri.parse(url), version)
                        prefs.edit(commit = true) {
                            putLong(KEY_DOWNLOAD_ID, id)
                            putString(KEY_PENDING_TAG, version)
                            putBoolean(KEY_PROTECTED_DOWNLOAD, true)
                        }
                        id
                    }
                }
            }
            val id = outcome.getOrNull()
            if (id == null) {
                fail(outcome.exceptionOrNull())
                return@launch
            }
            watchDownload(id, autoInstall = true)
        }
    }

    private suspend fun watchDownload(id: Long, autoInstall: Boolean) {
        while (scope.isActive) {
            delay(pollMs)
            when (val status = withContext(Dispatchers.IO) { downloadStatus(id) }) {
                DownloadManager.STATUS_SUCCESSFUL -> {
                    if (completeDownload(id) != null && autoInstall) installStaged()
                    return
                }
                DownloadManager.STATUS_FAILED, -1 -> {
                    stagingLock.withLock {
                        withContext(Dispatchers.IO) { clearStagedFiles() }
                        fail(IllegalStateException(
                            if (status == -1) "APK download was cancelled or removed"
                            else "APK download failed",
                        ))
                    }
                    return
                }
            }
        }
    }

    /** Shared by the live poller and the process-death broadcast path. Never starts an activity. */
    internal suspend fun completeDownload(id: Long): String? {
        restoration.join()
        return stagingLock.withLock {
            withContext(Dispatchers.IO) {
                if (id < 0 || id != prefs.getLong(KEY_DOWNLOAD_ID, -1L)) return@withContext null
                val version = prefs.getString(KEY_PENDING_TAG, null)
                    ?: prefs.getString(KEY_STAGED_TAG, null) ?: return@withContext null
                if (downloadStatus(id) != DownloadManager.STATUS_SUCCESSFUL) return@withContext null
                val outcome = runCatching {
                    check(prefs.getBoolean(KEY_PROTECTED_DOWNLOAD, false)) { "Legacy update requires download" }
                    preparedApk()
                    prefs.edit(commit = true) {
                        putString(KEY_STAGED_TAG, version)
                        remove(KEY_PENDING_TAG)
                        remove(KEY_ERROR)
                    }
                }
                if (outcome.isFailure) {
                    clearStagedFiles()
                    fail(outcome.exceptionOrNull())
                    return@withContext null
                }
                _state.update {
                    it.copy(phase = UpdatePhase.READY_TO_INSTALL, latestVersion = version, detail = "")
                }
                version
            }
        }
    }

    private fun preparedApk(): File {
        check(prefs.getBoolean(KEY_PROTECTED_DOWNLOAD, false)) { "Legacy update requires download" }
        val name = prefs.getString(KEY_STAGED_FILE, null)
        val file = if (name != null) {
            apkStager.saved(name).also(apkStager::validate)
        } else {
            val id = prefs.getLong(KEY_DOWNLOAD_ID, -1L)
            val source = if (id >= 0) downloadedUri(id) else null
            check(source != null) { "Downloaded APK is absent" }
            apkStager.stage(source).also {
                prefs.edit(commit = true) { putString(KEY_STAGED_FILE, it.name) }
            }
        }
        return file
    }

    private fun installStaged() {
        if (!canInstallPackages()) {
            _state.update { it.copy(phase = UpdatePhase.NEEDS_INSTALL_PERMISSION) }
            return
        }
        if (!installing.compareAndSet(false, true)) return
        scope.launch {
            try {
                stagingLock.withLock {
                    val outcome = runCatching {
                        withContext(Dispatchers.IO) { apkStager.uri(preparedApk()) }
                    }
                    val uri = outcome.getOrNull()
                    if (uri == null) {
                        withContext(Dispatchers.IO) { clearStagedFiles() }
                        fail(outcome.exceptionOrNull())
                        return@withLock
                    }
                    // Permission can be revoked while the archive is being verified.
                    if (!canInstallPackages()) {
                        _state.update { it.copy(phase = UpdatePhase.NEEDS_INSTALL_PERMISSION) }
                        return@withLock
                    }
                    runCatching {
                        context.startActivity(installerIntent(uri))
                    }.onFailure { fail(it) }
                }
            } finally {
                installing.set(false)
            }
        }
    }

    private fun fail(failure: Throwable?) {
        val detail = failure?.message.orEmpty()
        prefs.edit { putString(KEY_ERROR, detail) }
        _state.update { it.copy(phase = UpdatePhase.FAILED, detail = detail) }
    }

    // ── notification ────────────────────────────────────────────────

    private fun maybeNotify(version: String) {
        if (prefs.getString(KEY_NOTIFIED_TAG, null) == version) return
        if (!notifier.postUpdateAvailable(version)) return
        prefs.edit { putString(KEY_NOTIFIED_TAG, version) }
    }

    // ── platform seams ──────────────────────────────────────────────

    private fun downloadManager(): DownloadManager? =
        context.getSystemService(DownloadManager::class.java)

    private fun realEnqueue(uri: Uri, version: String): Long {
        val request = DownloadManager.Request(uri)
            .setTitle("Lerdr $version")
            .setMimeType(APK_MIME)
            .setNotificationVisibility(
                DownloadManager.Request.VISIBILITY_VISIBLE,
            )
        // No external destination: DownloadManager owns protected system staging.
        return downloadManager()?.enqueue(request)
            ?: error("DownloadManager unavailable")
    }

    private fun realStatus(id: Long): Int {
        val cursor = downloadManager()
            ?.query(DownloadManager.Query().setFilterById(id)) ?: return -1
        cursor.use {
            if (!it.moveToFirst()) return -1
            return it.getInt(it.getColumnIndexOrThrow(DownloadManager.COLUMN_STATUS))
        }
    }

    private fun downloadExists(): Boolean =
        prefs.getString(KEY_STAGED_FILE, null)?.let { apkStager.saved(it).isFile } == true

    private fun clearStagedFiles() {
        val id = prefs.getLong(KEY_DOWNLOAD_ID, -1L)
        if (id >= 0) downloadManager()?.remove(id)
        apkStager.clear()
        context.getExternalFilesDir(UPDATES_DIR)?.listFiles()?.forEach(File::delete)
        prefs.edit(commit = true) {
            remove(KEY_DOWNLOAD_ID)
            remove(KEY_STAGED_FILE)
            remove(KEY_STAGED_TAG)
            remove(KEY_PENDING_TAG)
            remove(KEY_PROTECTED_DOWNLOAD)
            remove(KEY_ERROR)
        }
        notifier.cancelUpdateNotification()
    }

    private fun retireLegacyDownload() {
        val id = prefs.getLong(KEY_DOWNLOAD_ID, -1L)
        if (id >= 0 && !prefs.getBoolean(KEY_PROTECTED_DOWNLOAD, false)) {
            // Revoke an already-posted pre-cutover direct installer PendingIntent too.
            runCatching { downloadedUri(id) }.getOrNull()?.let {
                PendingIntent.getActivity(
                    context, 77_001, installerIntent(it),
                    PendingIntent.FLAG_NO_CREATE or PendingIntent.FLAG_IMMUTABLE,
                )?.cancel()
            }
            PendingIntent.getActivity(
                context, 77_001, installPermissionIntent(),
                PendingIntent.FLAG_NO_CREATE or PendingIntent.FLAG_IMMUTABLE,
            )?.cancel()
            clearStagedFiles()
        }
        prefs.getString(KEY_ERROR, null)?.let {
            _state.update { state -> state.copy(phase = UpdatePhase.FAILED, detail = it) }
        }
    }

    internal fun updateSettingsIntent(): Intent =
        Intent(Intent.ACTION_VIEW, Uri.parse("lerdr://settings"), context, MainActivity::class.java)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP)

    internal fun installerIntent(uri: Uri): Intent =
        Intent(Intent.ACTION_VIEW)
            .setDataAndType(uri, APK_MIME)
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)

    private suspend fun fetchLatestRelease(): ReleaseInfo = withContext(Dispatchers.IO) {
        val request = Request.Builder()
            .url(releasesUrl)
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "lerdr-android/${BuildConfig.VERSION_NAME}")
            .build()
        http.newCall(request).execute().use { response ->
            check(response.isSuccessful) { "HTTP ${response.code}" }
            val body = response.body.string()
            val release = LerdrJson.decodeFromString<GhRelease>(body)
            val asset = release.assets.firstOrNull {
                it.name.endsWith(APK_ASSET_SUFFIX)
            } ?: error("no universal APK asset")
            ReleaseInfo(
                tag = release.tagName,
                apkUrl = asset.downloadUrl,
                apkBytes = asset.size,
            )
        }
    }

    companion object {
        const val RELEASES_URL =
            "https://api.github.com/repos/IGUNUBLUE/lerdr-rust-kotlin/releases/latest"
        const val APK_MIME = "application/vnd.android.package-archive"
        const val APK_ASSET_SUFFIX = "_universal.apk"
        const val UPDATES_DIR = "updates"
        const val PREFS_FILE = "lerdr_updates"
        const val POLL_MS = 750L
        private const val TAG = "AppUpdate"

        const val KEY_NOTIFIED_TAG = "notified_tag"
        const val KEY_DOWNLOAD_ID = "download_id"
        const val KEY_PENDING_TAG = "pending_tag"
        const val KEY_STAGED_TAG = "staged_tag"
        internal const val KEY_STAGED_FILE = "staged_file"
        internal const val KEY_PROTECTED_DOWNLOAD = "protected_download"
        internal const val KEY_ERROR = "error"

        /**
         * `v0.0.12` vs `0.0.11` — numeric field compare, ignoring any
         * `v` prefix and `-suffix` (pre-release tags never appear on the
         * `/latest` endpoint, so this is defensive).
         */
        fun isNewerVersion(current: String, latestTag: String): Boolean {
            val cur = versionParts(current) ?: return false
            val latest = versionParts(latestTag) ?: return false
            val width = maxOf(cur.size, latest.size)
            for (i in 0 until width) {
                val c = cur.getOrElse(i) { 0 }
                val l = latest.getOrElse(i) { 0 }
                if (l != c) return l > c
            }
            return false
        }

        private fun versionParts(version: String): List<Int>? =
            version.trim().removePrefix("v").substringBefore('-')
                .split('.')
                .map { it.toIntOrNull() ?: return null }
    }
}

enum class UpdatePhase {
    UNCHECKED,
    CHECKING,
    UP_TO_DATE,
    AVAILABLE,
    /** Waiting on the one-time `REQUEST_INSTALL_PACKAGES` grant. */
    NEEDS_INSTALL_PERMISSION,
    DOWNLOADING,
    READY_TO_INSTALL,
    FAILED,
}

@Immutable
data class AppUpdateState(
    val phase: UpdatePhase = UpdatePhase.UNCHECKED,
    val latestVersion: String = "",
    val apkUrl: String = "",
    val apkBytes: Long = 0L,
    /** Failure detail for diagnostics — not user-facing. */
    val detail: String = "",
)

internal data class ReleaseInfo(
    val tag: String,
    val apkUrl: String,
    val apkBytes: Long,
)

@Serializable
internal data class GhRelease(
    @kotlinx.serialization.SerialName("tag_name") val tagName: String,
    val assets: List<GhAsset> = emptyList(),
)

@Serializable
internal data class GhAsset(
    val name: String,
    @kotlinx.serialization.SerialName("browser_download_url")
    val downloadUrl: String,
    val size: Long = 0L,
)
