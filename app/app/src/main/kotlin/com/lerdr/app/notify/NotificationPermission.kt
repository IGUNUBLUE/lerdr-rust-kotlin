package com.lerdr.app.notify

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.platform.LocalContext
import androidx.core.content.ContextCompat

/** POST_NOTIFICATIONS is a runtime permission only on Android 13+ (API 33). */
fun Context.hasPostNotificationsPermission(): Boolean =
    Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
        ContextCompat.checkSelfPermission(
            this,
            Manifest.permission.POST_NOTIFICATIONS,
        ) == PackageManager.PERMISSION_GRANTED

/**
 * One-shot, non-blocking POST_NOTIFICATIONS request — fires the system
 * prompt once on first composition when the permission is missing, and is
 * a permanent no-op below API 33 or once granted.
 *
 * Drop into the composition root (`MainActivity.setContent`) or the Home
 * screen; placement is a judgment call for the UI owner, so this stays a
 * leaf composable with no side conditions beyond the permission check.
 */
@Composable
fun RequestPostNotificationsPermission(
    onResult: (granted: Boolean) -> Unit = {},
) {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return
    val launcher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
        onResult,
    )
    val context = LocalContext.current
    LaunchedEffect(Unit) {
        if (!context.hasPostNotificationsPermission()) {
            launcher.launch(Manifest.permission.POST_NOTIFICATIONS)
        }
    }
}
