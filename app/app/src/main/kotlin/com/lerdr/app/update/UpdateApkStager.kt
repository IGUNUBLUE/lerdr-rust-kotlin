package com.lerdr.app.update

import android.content.Context
import android.content.pm.PackageInfo
import android.content.pm.PackageManager
import android.net.Uri
import androidx.core.content.FileProvider
import java.io.File
import java.io.FileOutputStream

/** Copies first, then verifies the exact private, read-only bytes shared with the installer. */
internal class UpdateApkStager(private val context: Context) {
    private val directory get() = File(context.filesDir, AppUpdateManager.UPDATES_DIR)

    // PackageManager is the signature-verifying archive parser; tests supply its platform result.
    internal var archiveInfo: (File) -> PackageInfo? = { file ->
        context.packageManager.getPackageArchiveInfo(
            file.absolutePath,
            PackageManager.GET_SIGNING_CERTIFICATES,
        )
    }

    fun stage(source: Uri): File {
        check(directory.isDirectory || directory.mkdirs()) { "Cannot create private update directory" }
        val file = File.createTempFile("update-", ".apk", directory)
        try {
            val input = context.contentResolver.openInputStream(source)
                ?: error("Downloaded APK is absent")
            input.use {
                FileOutputStream(file).use { output ->
                    it.copyTo(output)
                    output.fd.sync()
                }
            }
            check(file.setReadOnly()) { "Cannot protect staged APK" }
            validate(file)
            return file
        } catch (failure: Exception) {
            file.delete()
            throw failure
        }
    }

    fun saved(name: String): File {
        check(name == File(name).name) { "Invalid staged APK path" }
        return File(directory, name)
    }

    fun validate(file: File) {
        check(file.isFile && file.length() > 0) { "Staged APK is absent" }
        val archive = archiveInfo(file) ?: error("Invalid APK archive")
        check(archive.packageName == context.packageName) { "Update package identity mismatch" }
        val installed = context.packageManager.getPackageInfo(
            context.packageName,
            PackageManager.GET_SIGNING_CERTIFICATES,
        )
        check(archive.longVersionCode > installed.longVersionCode) { "APK is not a newer update" }
        val current = installed.signingInfo ?: error("Installed signing identity is absent")
        val candidate = archive.signingInfo ?: error("APK signing identity is absent")
        val currentSigners = current.apkContentsSigners.orEmpty()
        val candidateSigners = candidate.apkContentsSigners.orEmpty()
        check(currentSigners.isNotEmpty() && candidateSigners.isNotEmpty()) {
            "APK signing identity is absent"
        }
        val compatible = if (current.hasMultipleSigners() || candidate.hasMultipleSigners()) {
            currentSigners.toSet() == candidateSigners.toSet()
        } else {
            // Only forward rotation: the APK must prove authorization from the installed
            // *current* signer, never merely share an old ancestor with the installed app.
            candidate.signingCertificateHistory.orEmpty().contains(currentSigners.single())
        }
        check(compatible) { "Update signing identity mismatch" }
    }

    fun uri(file: File): Uri = FileProvider.getUriForFile(
        context,
        "${context.packageName}.updates",
        file,
    )

    fun clear() {
        directory.listFiles()?.forEach(File::delete)
    }
}
