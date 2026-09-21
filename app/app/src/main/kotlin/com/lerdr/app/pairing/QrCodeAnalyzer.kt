package com.lerdr.app.pairing

import androidx.annotation.OptIn
import androidx.camera.core.ExperimentalGetImage
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import com.google.mlkit.vision.barcode.BarcodeScanner
import com.google.mlkit.vision.common.InputImage

/**
 * CameraX `ImageAnalysis` analyzer running ML Kit barcode detection.
 * Every decoded string goes through [gate]; the first accepted setup link
 * fires [onSetupLink] exactly once. Each proxy is closed on every path so
 * `STRATEGY_KEEP_ONLY_LATEST` never stalls.
 */
internal class QrCodeAnalyzer(
    private val scanner: BarcodeScanner,
    private val gate: QrScanGate,
    private val onSetupLink: (String) -> Unit,
) : ImageAnalysis.Analyzer {

    @OptIn(ExperimentalGetImage::class)
    override fun analyze(image: ImageProxy) {
        val mediaImage = image.image
        if (gate.consumed || mediaImage == null) {
            image.close()
            return
        }
        val input = InputImage.fromMediaImage(
            mediaImage,
            image.imageInfo.rotationDegrees,
        )
        scanner.process(input)
            .addOnSuccessListener { barcodes ->
                for (barcode in barcodes) {
                    val link = gate.offer(barcode.rawValue) ?: continue
                    onSetupLink(link)
                    break
                }
            }
            .addOnCompleteListener { image.close() }
    }
}
