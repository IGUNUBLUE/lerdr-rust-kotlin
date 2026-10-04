package com.lerdr.app.update

import android.app.DownloadManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.util.Log
import com.lerdr.app.di.AppScope
import com.lerdr.app.notify.LerdrNotifier
import dagger.hilt.EntryPoint
import dagger.hilt.InstallIn
import dagger.hilt.android.EntryPointAccessors
import dagger.hilt.components.SingletonComponent
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

/** Process-death completion bridge. Notification taps return to the validated update row. */
class UpdateDownloadReceiver : BroadcastReceiver() {
    @EntryPoint
    @InstallIn(SingletonComponent::class)
    interface Seams {
        fun notifier(): LerdrNotifier
        fun updates(): AppUpdateManager
        @AppScope fun appScope(): CoroutineScope
    }

    internal var seams: ((Context) -> Seams)? = null

    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != DownloadManager.ACTION_DOWNLOAD_COMPLETE) return
        val id = intent.getLongExtra(DownloadManager.EXTRA_DOWNLOAD_ID, -1L)
        val prefs = context.getSharedPreferences(AppUpdateManager.PREFS_FILE, Context.MODE_PRIVATE)
        if (id < 0 || id != prefs.getLong(AppUpdateManager.KEY_DOWNLOAD_ID, -1L)) return
        val dependencies = seams?.invoke(context) ?: EntryPointAccessors.fromApplication(
            context.applicationContext, Seams::class.java,
        )
        val pending = goAsync()
        dependencies.appScope().launch {
            try {
                val manager = dependencies.updates()
                val version = manager.completeDownload(id) ?: return@launch
                dependencies.notifier().postUpdateDownloaded(
                    version = version,
                    body = "Tap to review and install the update",
                    tap = PendingIntent.getActivity(
                        context,
                        REQUEST_REVIEW,
                        manager.updateSettingsIntent(),
                        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
                    ),
                )
            } catch (failure: Exception) {
                Log.w("AppUpdate", "Download completion failed", failure)
            }
        }.invokeOnCompletion {
            pending?.finish()
        }
    }

    companion object {
        private const val REQUEST_REVIEW = 77_002
    }
}
