package com.lerdr.app.update

import android.app.DownloadManager
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
import dagger.hilt.android.qualifiers.ApplicationContext
import java.io.File
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

    init {
        // Cold start: a download that completed while the process was dead
        // surfaces as READY_TO_INSTALL immediately; the next checkNow()
        // replaces it if the tag is no longer newer than the build.
        val staged = prefs.getString(KEY_STAGED_TAG, null)
        if (staged != null && isNewerVersion(BuildConfig.VERSION_NAME, staged)) {
            _state.value = AppUpdateState(
                phase = UpdatePhase.READY_TO_INSTALL,
                latestVersion = staged,
            )
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
                _state.value = AppUpdateState(
                    phase = UpdatePhase.UP_TO_DATE,
                    latestVersion = latest,
                )
                notifier.cancelUpdateNotification()
                return@launch
            }
            if (prefs.getString(KEY_STAGED_TAG, null) == latest &&
                downloadExists()
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
     * Called from the screen's resume effect after returning from the
     * install-permission settings — continues whichever step was pending.
     */
    fun resumeAfterPermission() {
        if (_state.value.phase != UpdatePhase.NEEDS_INSTALL_PERMISSION) return
        if (!canInstallPackages()) {
            // Still denied — drop back so the row offers the gate again.
            _state.update { it.copy(phase = UpdatePhase.AVAILABLE) }
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
        clearStagedFiles()
        val id = runCatching { enqueueDownload(Uri.parse(url), version) }
            .getOrElse {
                _state.update { it.copy(phase = UpdatePhase.FAILED) }
                return
            }
        prefs.edit {
            putLong(KEY_DOWNLOAD_ID, id)
            putString(KEY_PENDING_TAG, version)
        }
        _state.update { it.copy(phase = UpdatePhase.DOWNLOADING) }
        scope.launch { watchDownload(id, version) }
    }

    private suspend fun watchDownload(id: Long, version: String) {
        while (scope.isActive) {
            delay(pollMs)
            when (downloadStatus(id)) {
                DownloadManager.STATUS_SUCCESSFUL -> {
                    prefs.edit {
                        putString(KEY_STAGED_TAG, version)
                        remove(KEY_PENDING_TAG)
                    }
                    _state.update {
                        it.copy(phase = UpdatePhase.READY_TO_INSTALL)
                    }
                    // Foreground path — the receiver covers process death.
                    installStaged()
                    return
                }
                DownloadManager.STATUS_FAILED -> {
                    _state.update { it.copy(phase = UpdatePhase.FAILED) }
                    return
                }
            }
        }
    }

    private fun installStaged() {
        val id = prefs.getLong(KEY_DOWNLOAD_ID, -1L)
        val uri = if (id >= 0) downloadedUri(id) else null
        if (uri == null) {
            // Staged file vanished (Downloads cleared) — re-offer the row.
            prefs.edit { remove(KEY_STAGED_TAG).remove(KEY_DOWNLOAD_ID) }
            _state.update { it.copy(phase = UpdatePhase.AVAILABLE) }
            return
        }
        context.startActivity(
            Intent(Intent.ACTION_VIEW)
                .setDataAndType(uri, APK_MIME)
                .addFlags(
                    Intent.FLAG_GRANT_READ_URI_PERMISSION or
                        Intent.FLAG_ACTIVITY_NEW_TASK,
                ),
        )
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
            .setDestinationInExternalFilesDir(
                context,
                UPDATES_DIR,
                "lerdr-$version.apk",
            )
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

    private fun downloadExists(): Boolean {
        val id = prefs.getLong(KEY_DOWNLOAD_ID, -1L)
        if (id < 0) return false
        val cursor = downloadManager()
            ?.query(DownloadManager.Query().setFilterById(id)) ?: return false
        cursor.use {
            if (!it.moveToFirst()) return false
            return it.getInt(it.getColumnIndexOrThrow(DownloadManager.COLUMN_STATUS)) ==
                DownloadManager.STATUS_SUCCESSFUL
        }
    }

    private fun clearStagedFiles() {
        context.getExternalFilesDir(UPDATES_DIR)?.listFiles()
            ?.forEach(File::delete)
    }

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
