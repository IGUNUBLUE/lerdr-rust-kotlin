package com.lerdr.app.pairing

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.provider.Settings
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.QrCodeScanner
import androidx.compose.material3.Button
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.LocalLifecycleOwner
import com.google.mlkit.vision.barcode.BarcodeScanning
import com.google.mlkit.vision.barcode.BarcodeScanner
import com.google.mlkit.vision.barcode.BarcodeScannerOptions
import com.google.mlkit.vision.barcode.common.Barcode
import com.lerdr.core.designsystem.theme.LerdrTheme
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors

/**
 * QR entry point for pairing: a CameraX [PreviewView] hosting an
 * `ImageAnalysis` pipeline with an ML Kit barcode analyzer. The first
 * decoded `lerdr://pair` / oracle-style setup link is reported via
 * [onSetupLink] — [QrScanGate] latches, so it fires at most once and
 * analysis stops there.
 *
 * When the camera can't run — permission denied, or no usable camera at
 * all (`android.hardware.camera.any` is declared not-required) — an
 * explanatory [QrScannerFallback] renders instead of crashing.
 */
@Composable
fun QrScanner(
    onSetupLink: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val context = LocalContext.current
    var permissionGranted by remember {
        mutableStateOf(context.isCameraPermissionGranted())
    }
    val permissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted -> permissionGranted = granted }

    if (permissionGranted) {
        CameraPreview(onSetupLink = onSetupLink, modifier = modifier)
    } else {
        // Entering scan mode asks once up front; a denial leaves the
        // fallback below with explicit actions.
        LaunchedEffect(Unit) {
            permissionLauncher.launch(Manifest.permission.CAMERA)
        }
        QrScannerFallback(
            title = "Camera access needed",
            body = "Lerdr uses the camera to scan the setup QR code on your " +
                "computer. You can still paste the setup link instead.",
            primaryActionLabel = "Allow camera access",
            onPrimaryAction = {
                permissionLauncher.launch(Manifest.permission.CAMERA)
            },
            secondaryActionLabel = "Open settings",
            onSecondaryAction = { context.openAppSettings() },
            modifier = modifier,
        )
    }
}

@Composable
private fun CameraPreview(
    onSetupLink: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val lifecycleOwner = LocalLifecycleOwner.current
    val gate = remember { QrScanGate() }
    val session = remember { CameraSession() }
    var cameraUnavailable by remember { mutableStateOf(false) }

    DisposableEffect(Unit) {
        onDispose { session.release() }
    }

    if (cameraUnavailable) {
        QrScannerFallback(
            title = "Camera unavailable",
            body = "This device has no usable camera. " +
                "Paste the setup link instead.",
            modifier = modifier,
        )
        return
    }

    AndroidView(
        factory = { context ->
            val previewView = PreviewView(context).apply {
                // TextureView-backed so Compose size/clip bounds apply.
                implementationMode = PreviewView.ImplementationMode.COMPATIBLE
            }
            val providerFuture = ProcessCameraProvider.getInstance(context)
            providerFuture.addListener(
                {
                    val provider = try {
                        providerFuture.get()
                    } catch (failure: Exception) {
                        cameraUnavailable = true
                        return@addListener
                    }
                    if (session.disposed) {
                        // Composable left while the provider future was
                        // in flight — don't bind a camera nobody sees.
                        provider.unbindAll()
                        return@addListener
                    }
                    session.provider = provider

                    val executor = Executors.newSingleThreadExecutor()
                    session.analysisExecutor = executor
                    val scanner = BarcodeScanning.getClient(
                        BarcodeScannerOptions.Builder()
                            .setBarcodeFormats(Barcode.FORMAT_QR_CODE)
                            .build(),
                    )
                    session.scanner = scanner

                    val analysis = ImageAnalysis.Builder()
                        .setBackpressureStrategy(
                            ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST,
                        )
                        .build()
                    analysis.setAnalyzer(
                        executor,
                        QrCodeAnalyzer(scanner, gate) { link ->
                            // Gate latched — stop frames before reporting.
                            analysis.clearAnalyzer()
                            onSetupLink(link)
                        },
                    )

                    val preview = Preview.Builder().build()
                    preview.setSurfaceProvider(previewView.surfaceProvider)
                    try {
                        provider.unbindAll()
                        provider.bindToLifecycle(
                            lifecycleOwner,
                            CameraSelector.DEFAULT_BACK_CAMERA,
                            preview,
                            analysis,
                        )
                    } catch (failure: Exception) {
                        cameraUnavailable = true
                    }
                },
                ContextCompat.getMainExecutor(context),
            )
            previewView
        },
        modifier = modifier,
    )
}

/**
 * Explanation rendered in place of the viewfinder when scanning can't
 * run. Always reminds that the paste path still works.
 */
@Composable
private fun QrScannerFallback(
    title: String,
    body: String,
    modifier: Modifier = Modifier,
    primaryActionLabel: String? = null,
    onPrimaryAction: (() -> Unit)? = null,
    secondaryActionLabel: String? = null,
    onSecondaryAction: (() -> Unit)? = null,
) {
    val spacing = LerdrTheme.spacing
    Surface(
        shape = MaterialTheme.shapes.large,
        color = MaterialTheme.colorScheme.surfaceContainerLow,
        modifier = modifier.fillMaxWidth(),
    ) {
        Column(
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(spacing.small),
            modifier = Modifier.padding(spacing.medium),
        ) {
            Icon(
                Icons.Default.QrCodeScanner,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Text(title, style = MaterialTheme.typography.titleSmall)
            Text(
                body,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
            )
            if (primaryActionLabel != null || secondaryActionLabel != null) {
                Row(horizontalArrangement = Arrangement.spacedBy(spacing.small)) {
                    secondaryActionLabel?.let { label ->
                        TextButton(onClick = { onSecondaryAction?.invoke() }) {
                            Text(label)
                        }
                    }
                    primaryActionLabel?.let { label ->
                        Button(onClick = { onPrimaryAction?.invoke() }) {
                            Text(label)
                        }
                    }
                }
            }
        }
    }
}

/** Bound camera resources released when the scanner leaves composition. */
private class CameraSession {
    var disposed = false
        private set
    var provider: ProcessCameraProvider? = null
    var scanner: BarcodeScanner? = null
    var analysisExecutor: ExecutorService? = null

    fun release() {
        disposed = true
        provider?.unbindAll()
        scanner?.close()
        analysisExecutor?.shutdown()
        provider = null
        scanner = null
        analysisExecutor = null
    }
}

private fun Context.isCameraPermissionGranted(): Boolean =
    ContextCompat.checkSelfPermission(this, Manifest.permission.CAMERA) ==
        PackageManager.PERMISSION_GRANTED

private fun Context.openAppSettings() {
    runCatching {
        startActivity(
            Intent(
                Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
                Uri.fromParts("package", packageName, null),
            ),
        )
    }
}
