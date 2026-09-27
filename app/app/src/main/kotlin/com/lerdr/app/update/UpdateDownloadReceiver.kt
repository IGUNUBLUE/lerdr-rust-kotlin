package com.lerdr.app.update

import android.app.DownloadManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.core.content.edit
import com.lerdr.app.notify.LerdrNotifier
import dagger.hilt.EntryPoint
import dagger.hilt.InstallIn
import dagger.hilt.android.EntryPointAccessors
import dagger.hilt.components.SingletonComponent

/**
 * `ACTION_DOWNLOAD_COMPLETE` bridge — the manifest registration keeps the
 * install hand-off alive when the download finishes while the app process
 * is dead ([AppUpdateManager]'s poll loop only covers the live case).
 *
 * On success it posts a "tap to install" notification whose PendingIntent
 * is either the package-installer VIEW (when `REQUEST_INSTALL_PACKAGES`
 * is already granted) or the permission-grant settings screen. Foreign
 * download completions are ignored by matching the persisted id.
 */
class UpdateDownloadReceiver : BroadcastReceiver() {

    @EntryPoint
    @InstallIn(SingletonComponent::class)
    interface Seams {
        fun notifier(): LerdrNotifier
    }

    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != DownloadManager.ACTION_DOWNLOAD_COMPLETE) return
        val id = intent.getLongExtra(DownloadManager.EXTRA_DOWNLOAD_ID, -1L)
        val prefs = context.getSharedPreferences(
            AppUpdateManager.PREFS_FILE,
            Context.MODE_PRIVATE,
        )
        if (id < 0 || id != prefs.getLong(AppUpdateManager.KEY_DOWNLOAD_ID, -1L)) return

        val dm = context.getSystemService(DownloadManager::class.java) ?: return
        val status = queryStatus(dm, id)
        val tag = prefs.getString(AppUpdateManager.KEY_PENDING_TAG, null)
        if (status != DownloadManager.STATUS_SUCCESSFUL || tag == null) return

        prefs.edit {
            putString(AppUpdateManager.KEY_STAGED_TAG, tag)
            remove(AppUpdateManager.KEY_PENDING_TAG)
        }

        val granted = context.packageManager.canRequestPackageInstalls()
        val notifier = EntryPointAccessors
            .fromApplication(context.applicationContext, Seams::class.java)
            .notifier()
        notifier.postUpdateDownloaded(
            version = tag,
            body = if (granted) {
                "Tap to install the update"
            } else {
                "Tap to allow installing the update"
            },
            tap = PendingIntent.getActivity(
                context,
                REQUEST_INSTALL,
                if (granted) {
                    installIntent(dm.getUriForDownloadedFile(id))
                } else {
                    permissionIntent(context)
                },
                PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
            ),
        )
    }

    private fun queryStatus(dm: DownloadManager, id: Long): Int {
        val cursor = dm.query(DownloadManager.Query().setFilterById(id))
        cursor.use {
            if (!it.moveToFirst()) return -1
            return it.getInt(it.getColumnIndexOrThrow(DownloadManager.COLUMN_STATUS))
        }
    }

    companion object {
        private const val REQUEST_INSTALL = 77_001

        fun installIntent(apk: Uri): Intent =
            Intent(Intent.ACTION_VIEW)
                .setDataAndType(apk, AppUpdateManager.APK_MIME)
                .addFlags(
                    Intent.FLAG_GRANT_READ_URI_PERMISSION or
                        Intent.FLAG_ACTIVITY_NEW_TASK,
                )

        fun permissionIntent(context: Context): Intent =
            Intent(android.provider.Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES)
                .setData(Uri.parse("package:${context.packageName}"))
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
    }
}
