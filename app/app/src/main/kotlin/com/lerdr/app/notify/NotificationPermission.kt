package com.lerdr.app.notify

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.core.content.edit
import androidx.compose.ui.platform.LocalContext
import androidx.core.content.ContextCompat

/** POST_NOTIFICATIONS is a runtime permission only on Android 13+ (API 33). */
fun Context.hasPostNotificationsPermission(): Boolean =
    Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
        ContextCompat.checkSelfPermission(
            this,
            Manifest.permission.POST_NOTIFICATIONS,
        ) == PackageManager.PERMISSION_GRANTED

/** Ask in context, after pairing and unlock, and only after an explicit tap. */
@Composable
fun RequestPostNotificationsPermission(
    hasPairedComputer: Boolean,
    onResult: (granted: Boolean) -> Unit = {},
) {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return
    val context = LocalContext.current
    val preferences = remember(context) {
        context.getSharedPreferences("notification_permission", Context.MODE_PRIVATE)
    }
    var offered by remember { mutableStateOf(preferences.getBoolean("offered", false)) }
    var granted by remember { mutableStateOf(context.hasPostNotificationsPermission()) }
    val launcher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { result ->
        granted = result
        onResult(result)
    }
    if (!hasPairedComputer || offered || granted) return

    fun dismissOffer() {
        offered = true
        preferences.edit { putBoolean("offered", true) }
    }
    NotificationPermissionPrompt(
        onEnable = {
            dismissOffer()
            if (!context.hasPostNotificationsPermission()) {
                launcher.launch(Manifest.permission.POST_NOTIFICATIONS)
            }
        },
        onDismiss = ::dismissOffer,
    )
}

@Composable
internal fun NotificationPermissionPrompt(onEnable: () -> Unit, onDismiss: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Know when an agent needs you") },
        text = {
            Text(
                "Get alerts for approvals, questions and completed work while you're away. " +
                    "You can change notifications in Settings at any time.",
            )
        },
        confirmButton = {
            TextButton(onClick = onEnable) { Text("Enable notifications") }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) { Text("Not now") }
        },
    )
}
