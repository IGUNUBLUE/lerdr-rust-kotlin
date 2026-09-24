package com.lerdr.app.session

import android.content.Context
import android.net.Uri
import android.provider.OpenableColumns
import android.webkit.MimeTypeMap
import com.lerdr.app.di.AppScope
import dagger.hilt.android.qualifiers.ApplicationContext
import java.io.InputStream
import java.security.MessageDigest
import java.time.OffsetDateTime
import java.util.UUID
import javax.inject.Inject
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import lerdr.core.model.UploadAttachment
import lerdr.core.model.UploadBeginResult
import lerdr.core.protocol.BinaryUploadChunk
import lerdr.core.transport.CommandException

/**
 * `DEFAULT_ATTACHMENT_MIME_TYPES` — the oracle's allowed media-type set
 * (`attachments.ts`). Anything outside it is rejected client-side with
 * `attachment_unknown_mime` before a byte crosses the wire.
 */
val DEFAULT_ATTACHMENT_MIME_TYPES: Set<String> = setOf(
    "application/json",
    "application/pdf",
    "application/vnd.oasis.opendocument.presentation",
    "application/vnd.oasis.opendocument.spreadsheet",
    "application/vnd.oasis.opendocument.text",
    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "image/gif",
    "image/jpeg",
    "image/heic",
    "image/heif",
    "image/png",
    "image/webp",
    "text/csv",
    "text/markdown",
    "text/plain",
)

/**
 * `AttachmentLimits` — the client-side pre-checks the oracle passes into
 * `AttachmentBatchController` (`maxFiles: 8, maxFileBytes: 20 MiB,
 * maxBatchBytes: 50 MiB, maxChunkBytes: 256 KiB`); the relay's own
 * `upload_begin_result.limits` re-validates server-side.
 */
data class AttachmentLimits(
    val maxFiles: Int = 8,
    val maxFileBytes: Long = 20L * 1024 * 1024,
    val maxBatchBytes: Long = 50L * 1024 * 1024,
    val maxChunkBytes: Int = 256 * 1024,
)

/** `AttachmentItemState` — the per-file lifecycle the chip row renders. */
enum class AttachmentItemState {
    SELECTED,
    UPLOADING,
    READY,
    REJECTED,
    INTERRUPTED,
}

/**
 * `AttachmentIssue` — a public `attachment_*` failure code plus the bounded
 * args the relay (or local validation) attached. [attachmentIssueText]
 * renders it.
 */
data class AttachmentIssue(
    val code: String,
    val args: Map<String, JsonElement>? = null,
)

/** One row of the composer's attachment tray — the oracle's `publicItem`. */
data class AttachmentItem(
    val clientId: String,
    val name: String,
    val mediaType: String,
    val bytes: Long,
    val order: Int,
    val state: AttachmentItemState,
    val uploadedBytes: Long = 0,
    /** 0..1 — `item.progress`. */
    val progress: Float = 0f,
    val issue: AttachmentIssue? = null,
    /** Set once [state] is READY — carries the relay `ref`. */
    val attachment: UploadAttachment? = null,
)

/** `AttachmentBatchSnapshot` — the immutable view the composer renders. */
data class AttachmentBatch(
    val items: List<AttachmentItem> = emptyList(),
    /** Batch-level issue (e.g. `attachment_batch_limit` on over-selection). */
    val issue: AttachmentIssue? = null,
    val uploading: Boolean = false,
    /** `canUpload` — idle, no staged session, something selected. */
    val canUpload: Boolean = false,
) {
    /** `canRestart` — interrupted items the source can still open. */
    val canRestart: Boolean
        get() = !uploading && items.any { it.state == AttachmentItemState.INTERRUPTED }

    val rejectedCount: Int
        get() = items.count { it.state == AttachmentItemState.REJECTED }
}

/** Failure whose [issue] is already a public `attachment_*` code. */
class AttachmentIssueException(
    val issue: AttachmentIssue,
) : Exception(issue.code)

/**
 * SAF read seam — the batch state machine stays JVM-testable while the
 * production binding wraps `ContentResolver`. `uri` is the opaque
 * `Uri.toString()` form the picker returns.
 */
interface AttachmentSource {
    /** DISPLAY_NAME + SIZE + declared MIME; null when the provider won't answer. */
    fun probe(uri: String): AttachmentProbe?

    /** Fresh full-content stream — the caller closes it; null on refusal. */
    fun open(uri: String): InputStream?
}

data class AttachmentProbe(
    val name: String? = null,
    val mediaType: String? = null,
    val bytes: Long = -1L,
)

/** `ContentResolver` attachment source — `openInputStream` per pass. */
class ContentResolverAttachmentSource(
    private val resolver: android.content.ContentResolver,
) : AttachmentSource {
    override fun probe(uri: String): AttachmentProbe? {
        val parsed = Uri.parse(uri)
        val mediaType = resolver.getType(parsed)
            ?: mimeFromExtension(nameFromUri(parsed))
        var name: String? = null
        var bytes = -1L
        runCatching {
            resolver.query(
                parsed,
                arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE),
                null,
                null,
                null,
            )?.use { cursor ->
                if (cursor.moveToFirst()) {
                    val nameIndex = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                    if (nameIndex >= 0 && !cursor.isNull(nameIndex)) {
                        name = cursor.getString(nameIndex)
                    }
                    val sizeIndex = cursor.getColumnIndex(OpenableColumns.SIZE)
                    if (sizeIndex >= 0 && !cursor.isNull(sizeIndex)) {
                        bytes = cursor.getLong(sizeIndex)
                    }
                }
            }
        }
        if (bytes < 0) {
            bytes = runCatching {
                resolver.openAssetFileDescriptor(parsed, "r")?.use { it.length } ?: -1L
            }.getOrDefault(-1L)
        }
        if (name.isNullOrEmpty()) name = nameFromUri(parsed)
        return AttachmentProbe(name, mediaType, bytes)
    }

    override fun open(uri: String): InputStream? =
        runCatching { resolver.openInputStream(Uri.parse(uri)) }.getOrNull()

    /** Browser `file.type` equivalent — MIME guessed from the name's extension. */
    private fun mimeFromExtension(name: String?): String? {
        val extension = name?.substringAfterLast('.', "")?.lowercase()
            ?.takeIf { it.isNotEmpty() } ?: return null
        return MimeTypeMap.getSingleton().getMimeTypeFromExtension(extension)
    }

    private fun nameFromUri(uri: Uri): String? = uri.lastPathSegment?.substringAfterLast('/')
}

/**
 * `AttachmentBatchController` port — one upload batch per pane. Selection
 * validates against [AttachmentLimits] (the oracle's client-side constants),
 * [upload] streams each file through `upload_begin`/`upload_chunk`/
 * `upload_finish` with per-chunk and whole-file SHA-256, and [cancel]
 * dismisses the batch with `upload_cancel` when a session was staged.
 *
 * Lifetime is bound to the screen like the oracle's per-component
 * controller: [FeedViewModel] owns one instance per pane and calls
 * [discard] from `onCleared`. Constructed per-screen in `AgentFeedScreen`
 * (`AppEntryPoint` does not expose it); the `@Inject` constructor keeps the
 * DI graph honest for future providers.
 */
class AttachmentUploads internal constructor(
    private val scope: CoroutineScope,
    private val sessions: SessionRepository,
    private val source: AttachmentSource,
    private val clock: () -> Long = System::currentTimeMillis,
    private val limits: AttachmentLimits = AttachmentLimits(),
    private val allowedMimes: Set<String> = DEFAULT_ATTACHMENT_MIME_TYPES,
) {

    @Inject
    constructor(
        @AppScope scope: CoroutineScope,
        sessions: SessionRepository,
        @ApplicationContext context: Context,
    ) : this(
        scope,
        sessions,
        ContentResolverAttachmentSource(context.contentResolver),
        System::currentTimeMillis,
        AttachmentLimits(),
        DEFAULT_ATTACHMENT_MIME_TYPES,
    )

    private val batches = java.util.concurrent.ConcurrentHashMap<String, Batch>()

    /** `subscribe` — the snapshot stream the composer collects. */
    fun state(paneId: String): StateFlow<AttachmentBatch> = batchFor(paneId).flow.asStateFlow()

    /** Current items — for callers that need a synchronous peek. */
    fun itemsNow(paneId: String): List<AttachmentItem> = batchFor(paneId).flow.value.items

    /**
     * `select` — replaces the batch with the probed picker result. Probing
     * (`ContentResolver` queries) runs off the caller's dispatcher. Throws
     * `attachment_batch_locked` while an upload is live.
     */
    suspend fun select(paneId: String, uris: List<String>) {
        val batch = batchFor(paneId)
        synchronized(batch) {
            if (batch.uploading || batch.activeUploadId != null) {
                throw AttachmentIssueException(AttachmentIssue(BATCH_LOCKED))
            }
        }
        val probed = withContext(Dispatchers.IO) {
            uris.map { uri -> uri to source.probe(uri) }
        }
        synchronized(batch) {
            if (batch.uploading || batch.activeUploadId != null) {
                throw AttachmentIssueException(AttachmentIssue(BATCH_LOCKED))
            }
            batch.items.clear()
            batch.issue = null
            if (probed.size > limits.maxFiles) {
                batch.issue = AttachmentIssue(
                    BATCH_LIMIT,
                    mapOf(
                        "selected" to JsonPrimitive(probed.size),
                        "max" to JsonPrimitive(limits.maxFiles),
                    ),
                )
            }
            var batchBytes = 0L
            for ((order, entry) in probed.withIndex()) {
                val (uri, probe) = entry
                val name = probe?.name.orEmpty()
                val mediaType = normalizedMime(probe?.mediaType.orEmpty())
                val bytes = probe?.bytes ?: -1L
                val issue = when {
                    order >= limits.maxFiles ->
                        AttachmentIssue(BATCH_LIMIT, mapOf("max" to JsonPrimitive(limits.maxFiles)))
                    !validFilename(name) -> AttachmentIssue(INVALID_NAME)
                    mediaType.isEmpty() || mediaType !in allowedMimes ->
                        AttachmentIssue(UNKNOWN_MIME, mapOf("mime" to JsonPrimitive(mediaType.ifEmpty { "unknown" })))
                    bytes == 0L -> AttachmentIssue(FILE_EMPTY)
                    bytes < 0 -> AttachmentIssue(UPLOAD_FAILED)
                    bytes > limits.maxFileBytes ->
                        AttachmentIssue(
                            FILE_TOO_LARGE,
                            mapOf(
                                "bytes" to JsonPrimitive(bytes),
                                "max" to JsonPrimitive(limits.maxFileBytes),
                            ),
                        )
                    batchBytes + bytes > limits.maxBatchBytes ->
                        AttachmentIssue(
                            BATCH_TOO_LARGE,
                            mapOf("max" to JsonPrimitive(limits.maxBatchBytes)),
                        )
                    else -> null
                }
                if (issue == null) batchBytes += bytes
                batch.items += InternalItem(
                    clientId = "${batch.epoch}-$order-${UUID.randomUUID()}",
                    name = name,
                    mediaType = mediaType,
                    bytes = bytes,
                    order = order,
                    state = if (issue == null) {
                        AttachmentItemState.SELECTED
                    } else {
                        AttachmentItemState.REJECTED
                    },
                    issue = issue,
                    uri = if (issue == null) uri else null,
                )
            }
            batch.epoch += 1
            batch.publish()
        }
    }

    /** `remove` — drops one item; locked while an upload is live. */
    fun remove(paneId: String, clientId: String) {
        val batch = batches[paneId] ?: return
        synchronized(batch) {
            if (batch.uploading || batch.activeUploadId != null) {
                throw AttachmentIssueException(AttachmentIssue(BATCH_LOCKED))
            }
            batch.items.removeAll { it.clientId == clientId }
            batch.items.forEachIndexed { index, item -> item.order = index }
            batch.publish()
        }
    }

    /** `clear` — empties the batch without a wire cancel (post-send cleanup). */
    fun clear(paneId: String) {
        val batch = batches[paneId] ?: return
        synchronized(batch) {
            if (batch.uploading || batch.activeUploadId != null) {
                throw AttachmentIssueException(AttachmentIssue(BATCH_LOCKED))
            }
            batch.items.clear()
            batch.issue = null
            batch.publish()
        }
    }

    /**
     * `upload` — begin → stream chunks → finish. Runs on the caller's
     * coroutine; [cancel] kills it through [Batch.job]. Returns the ordered
     * ready refs — empty when the run was superseded or nothing was selected.
     */
    suspend fun upload(paneId: String): List<UploadAttachment> {
        val batch = batchFor(paneId)
        val uploadItems: List<InternalItem>
        synchronized(batch) {
            if (batch.uploading || batch.activeUploadId != null) {
                throw AttachmentIssueException(AttachmentIssue(BATCH_LOCKED))
            }
            uploadItems = batch.items.filter { it.state == AttachmentItemState.SELECTED && it.uri != null }
            if (uploadItems.isEmpty()) return emptyList()
        }
        return runUpload(paneId, batch, uploadItems)
    }

    /**
     * `restart` — re-stages items stuck INTERRUPTED. A staged-but-failed
     * session gets `upload_cancel` first like the oracle's `restart()`.
     */
    suspend fun restart(paneId: String): List<UploadAttachment> {
        val batch = batchFor(paneId)
        val uploadItems: List<InternalItem>
        val staleUploadId: String?
        synchronized(batch) {
            if (batch.uploading) {
                throw AttachmentIssueException(AttachmentIssue(BATCH_NOT_RESTARTABLE))
            }
            uploadItems = batch.items.filter {
                it.state == AttachmentItemState.INTERRUPTED && it.uri != null
            }
            if (uploadItems.isEmpty()) {
                throw AttachmentIssueException(AttachmentIssue(BATCH_NOT_RESTARTABLE))
            }
            staleUploadId = batch.activeUploadId
            batch.activeUploadId = null
        }
        batch.job?.cancel()
        if (staleUploadId != null) {
            try {
                sessions.uploadCancel(paneId, staleUploadId)
            } catch (failure: Exception) {
                val issue = AttachmentIssue(CANCEL_FAILED)
                markInterrupted(batch, uploadItems, issue)
                throw AttachmentIssueException(issue)
            }
        }
        synchronized(batch) {
            for (item in uploadItems) {
                item.state = AttachmentItemState.SELECTED
                item.uploadedBytes = 0
                item.progress = 0f
                item.issue = null
                item.digest = null
            }
            batch.publish()
        }
        return runUpload(paneId, batch, uploadItems)
    }

    /**
     * `cancel` — dismiss-all: clears the batch, kills the upload coroutine,
     * and sends `upload_cancel` for a staged session — including the
     * still-pending `upload_begin` race the oracle's `pendingBegin` covers.
     */
    suspend fun cancel(paneId: String) {
        val batch = batches[paneId] ?: return
        val job: Job?
        val pending: CompletableDeferred<UploadBeginResult>?
        var uploadId: String?
        synchronized(batch) {
            batch.epoch += 1
            uploadId = batch.activeUploadId
            batch.activeUploadId = null
            batch.uploading = false
            batch.items.clear()
            batch.issue = null
            job = batch.job
            pending = batch.pendingBegin
            batch.publish()
        }
        job?.cancel()
        if (uploadId == null && pending != null) {
            uploadId = try {
                pending.await()
            } catch (failure: Exception) {
                null
            }?.uploadId
            synchronized(batch) {
                if (batch.pendingBegin === pending) batch.pendingBegin = null
            }
        }
        if (uploadId.isNullOrEmpty()) return
        sessions.uploadCancel(paneId, uploadId)
    }

    /**
     * `onDestroy` parity — fire-and-forget [cancel] on the manager scope,
     * then drop the batch entry so a future screen starts empty.
     */
    fun discard(paneId: String) {
        val batch = batches[paneId] ?: return
        scope.launch {
            runCatching { cancel(paneId) }
            batches.remove(paneId, batch)
        }
    }

    // ── upload run ────────────────────────────────────────────────────

    /**
     * `startUpload` — the begin/chunk/finish pipeline. Each chunk is a
     * `chunk_bytes` slice streamed from [AttachmentSource]; the whole-file
     * digest folds into the same pass (the oracle's hash-worker computes the
     * identical digest in parallel — one streaming pass keeps SAF content
     * out of memory twice over).
     */
    private suspend fun runUpload(
        paneId: String,
        batch: Batch,
        uploadItems: List<InternalItem>,
    ): List<UploadAttachment> {
        val run: Int
        synchronized(batch) {
            run = ++batch.epoch
            batch.uploading = true
            batch.issue = null
            for (item in uploadItems) {
                item.state = AttachmentItemState.UPLOADING
                item.uploadedBytes = 0
                item.progress = 0f
                item.issue = null
            }
            batch.publish()
        }
        batch.job = currentJob()
        var began = false
        val pendingBegin = CompletableDeferred<UploadBeginResult>()
        batch.pendingBegin = pendingBegin
        try {
            val begin = try {
                sessions.uploadBegin(
                    paneId,
                    uploadItems.map { UploadFileSpec(it.name, it.mediaType, it.bytes) },
                ).also { pendingBegin.complete(it) }
            } catch (failure: Exception) {
                pendingBegin.completeExceptionally(failure)
                throw failure
            }
            if (run != batch.epoch) return emptyList()
            synchronized(batch) {
                if (batch.pendingBegin === pendingBegin) batch.pendingBegin = null
            }
            val expiresAtMs = parseExpiry(begin.expiresAt)
            if (begin.uploadId.isNullOrEmpty() || expiresAtMs == null || expiresAtMs <= clock()) {
                val issue = AttachmentIssue(
                    if (expiresAtMs != null && expiresAtMs <= clock()) {
                        UPLOAD_EXPIRED
                    } else {
                        INVALID_RESPONSE
                    },
                )
                markInterrupted(batch, uploadItems, issue)
                throw AttachmentIssueException(issue)
            }
            val chunkBytes = begin.chunkBytes ?: -1
            val beginLimits = begin.limits
            val limitsValid = chunkBytes > 0 && chunkBytes <= limits.maxChunkBytes &&
                beginLimits != null &&
                uploadItems.size <= (beginLimits.maxFiles ?: -1) &&
                uploadItems.all { it.bytes <= (beginLimits.maxFileBytes ?: -1) } &&
                uploadItems.sumOf { it.bytes } <= (beginLimits.maxBatchBytes ?: -1)
            val uploadId = begin.uploadId.orEmpty()
            if (!limitsValid) {
                // The session exists relay-side — keep the id so a later
                // cancel() can still discard it (the oracle does the same).
                synchronized(batch) { batch.activeUploadId = uploadId }
                val issue = AttachmentIssue(INVALID_RESPONSE)
                markInterrupted(batch, uploadItems, issue)
                throw AttachmentIssueException(issue)
            }
            began = true
            synchronized(batch) { batch.activeUploadId = uploadId }
            // §2.4 — a negotiated `chunk_encoding:"binary"` moves chunks
            // onto the `0x03` carrier; JSON/base64 otherwise. The stamp is
            // the negotiated truth — a mid-flight retraction still answers
            // `capability_unsupported`, which `issueFrom` maps like any
            // command failure.
            val binaryChunks = begin.chunkEncoding == BinaryUploadChunk.ENCODING
            // `sequence` is global across the batch's files, like the oracle.
            var sequence = 0
            for (fileIndex in uploadItems.indices) {
                val item = uploadItems[fileIndex]
                sequence = uploadFile(
                    paneId, batch, item, fileIndex,
                    uploadId, chunkBytes, expiresAtMs, run, sequence, binaryChunks,
                )
                if (sequence < 0 || run != batch.epoch) return emptyList()
            }
            if (clock() >= expiresAtMs) {
                throw AttachmentIssueException(AttachmentIssue(UPLOAD_EXPIRED))
            }
            val finish = sessions.uploadFinish(
                paneId,
                uploadId,
                uploadItems.mapIndexed { index, item -> UploadFileDigest(index, item.digest.orEmpty()) },
            )
            if (run != batch.epoch) return emptyList()
            val attachments = finish.attachments
                ?: throw AttachmentIssueException(AttachmentIssue(INVALID_RESPONSE))
            if (attachments.size != uploadItems.size) {
                throw AttachmentIssueException(AttachmentIssue(INVALID_RESPONSE))
            }
            synchronized(batch) {
                for (index in uploadItems.indices) {
                    val item = uploadItems[index]
                    val attachment = attachments[index]
                    if (attachment.ref.isNullOrEmpty() ||
                        attachment.name != item.name ||
                        normalizedMime(attachment.mediaType.orEmpty()) != item.mediaType ||
                        attachment.bytes != item.bytes ||
                        attachment.sha256 != item.digest
                    ) {
                        throw AttachmentIssueException(AttachmentIssue(INVALID_RESPONSE))
                    }
                    item.state = AttachmentItemState.READY
                    item.progress = 1f
                    item.uploadedBytes = item.bytes
                    item.attachment = attachment
                    item.issue = null
                    item.uri = null
                    item.digest = null
                }
                batch.activeUploadId = null
                batch.uploading = false
                batch.publish()
            }
            return orderedAttachments(batch)
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (failure: Exception) {
            synchronized(batch) {
                if (batch.pendingBegin === pendingBegin) batch.pendingBegin = null
            }
            if (run != batch.epoch) return emptyList()
            val issue = issueFrom(
                failure,
                if (began) UPLOAD_STATE_UNKNOWN else UPLOAD_FAILED,
            )
            markInterrupted(batch, uploadItems, issue)
            throw failure as? AttachmentIssueException ?: AttachmentIssueException(issue)
        }
    }

    /**
     * One file's chunk loop — [sequence] counts across the whole batch like
     * the oracle; returns the next value, or -1 when the run was superseded.
     * Plain IO failures propagate; the caller maps them through the
     * `began` fallback.
     */
    private suspend fun uploadFile(
        paneId: String,
        batch: Batch,
        item: InternalItem,
        fileIndex: Int,
        uploadId: String?,
        chunkBytes: Int,
        expiresAtMs: Long,
        run: Int,
        sequence: Int,
        binaryChunks: Boolean = false,
    ): Int {
        var nextSequence = sequence
        var offset = 0L
        val uri = item.uri ?: throw AttachmentIssueException(AttachmentIssue(UPLOAD_FAILED))
        val digest = MessageDigest.getInstance("SHA-256")
        val chunkDigest = MessageDigest.getInstance("SHA-256")
        withContext(Dispatchers.IO) { source.open(uri) }?.use { stream ->
            val buffer = ByteArray(chunkBytes)
            while (offset < item.bytes) {
                if (clock() >= expiresAtMs) {
                    throw AttachmentIssueException(AttachmentIssue(UPLOAD_EXPIRED))
                }
                val filled = withContext(Dispatchers.IO) {
                    var total = 0
                    while (total < buffer.size) {
                        val read = stream.read(buffer, total, buffer.size - total)
                        if (read < 0) break
                        total += read
                    }
                    total
                }
                if (filled == 0) break
                val chunk = if (filled == buffer.size) buffer else buffer.copyOf(filled)
                val response = if (binaryChunks) {
                    // The `0x03` carrier drops `sha256` — the relay measures
                    // it on receipt; the AES-GCM envelope authenticates bytes.
                    sessions.uploadBinaryChunk(
                        paneId,
                        uploadId.orEmpty(),
                        fileIndex,
                        sequence = nextSequence,
                        data = chunk,
                    )
                } else {
                    sessions.uploadChunk(
                        paneId,
                        uploadId.orEmpty(),
                        fileIndex,
                        sequence = nextSequence,
                        data = chunk,
                        sha256 = hex(chunkDigest.digest(chunk)),
                    )
                }
                if (run != batch.epoch) return -1
                val expectedBytes = offset + filled
                if (response.fileIndex != fileIndex ||
                    response.nextSequence != nextSequence + 1 ||
                    response.receivedBytes != expectedBytes
                ) {
                    throw AttachmentIssueException(AttachmentIssue(UPLOAD_STATE_UNKNOWN))
                }
                offset = expectedBytes
                nextSequence += 1
                digest.update(chunk, 0, filled)
                synchronized(batch) {
                    item.uploadedBytes = offset
                    item.progress = if (item.bytes == 0L) 1f else offset.toFloat() / item.bytes
                    batch.publish()
                }
            }
        } ?: throw java.io.IOException("Could not open attachment content")
        if (offset != item.bytes) {
            // SAF provider shrank the content mid-read — the staged body can
            // never match the declared `bytes`; fail before a doomed finish.
            throw java.io.IOException("Attachment content ended before its declared size")
        }
        item.digest = digest.hex()
        return nextSequence
    }

    private fun orderedAttachments(batch: Batch): List<UploadAttachment> =
        synchronized(batch) {
            batch.items
                .filter { it.state == AttachmentItemState.READY && it.attachment != null }
                .sortedBy { it.order }
                .map { it.attachment!! }
        }

    private fun markInterrupted(batch: Batch, items: List<InternalItem>, issue: AttachmentIssue) {
        synchronized(batch) {
            batch.uploading = false
            for (item in items) {
                if (item.state == AttachmentItemState.READY) continue
                item.state = AttachmentItemState.INTERRUPTED
                item.issue = issue
            }
            batch.publish()
        }
    }

    private fun batchFor(paneId: String): Batch = batches.getOrPut(paneId) { Batch() }

    // ── issue mapping ─────────────────────────────────────────────────

    /**
     * `issueFrom` — `ApiError`/`CommandException` codes that already carry an
     * `attachment_*` public code pass through (with args); everything else
     * collapses to the fallback like the oracle.
     */
    private fun issueFrom(error: Throwable, fallback: String): AttachmentIssue {
        val command = error as? CommandException
        val issue = error as? AttachmentIssueException
        val code = command?.apiError?.code ?: command?.code ?: issue?.issue?.code
        if (code != null && code.startsWith("attachment_")) {
            return AttachmentIssue(code, command?.apiError?.args ?: issue?.issue?.args)
        }
        return AttachmentIssue(fallback)
    }

    // ── state ─────────────────────────────────────────────────────────

    /** `InternalItem` — the public [AttachmentItem] plus uri + digest. */
    private class InternalItem(
        val clientId: String,
        val name: String,
        val mediaType: String,
        val bytes: Long,
        var order: Int,
        var state: AttachmentItemState,
        var uploadedBytes: Long = 0,
        var progress: Float = 0f,
        var issue: AttachmentIssue? = null,
        var attachment: UploadAttachment? = null,
        var uri: String? = null,
        var digest: String? = null,
    )

    /** Per-pane controller state — list mutations under `synchronized(batch)`. */
    private class Batch {
        val items = mutableListOf<InternalItem>()

        @Volatile
        var issue: AttachmentIssue? = null

        @Volatile
        var uploading = false

        /** `active.uploadId` — staged session id awaiting finish/cancel. */
        @Volatile
        var activeUploadId: String? = null

        /** `pendingBegin` — an `upload_begin` still awaiting its result. */
        @Volatile
        var pendingBegin: CompletableDeferred<UploadBeginResult>? = null

        /** The coroutine running [runUpload] — cancelled by [cancel]. */
        @Volatile
        var job: Job? = null

        /** `epoch` — stale-run guard; bumps on select/cancel/upload. */
        @Volatile
        var epoch = 0

        val flow = MutableStateFlow(AttachmentBatch())

        /** `snapshot` + `notify` — publishes the immutable public view. */
        fun publish() {
            flow.value = AttachmentBatch(
                items = items.map { it.publicItem() },
                issue = issue,
                uploading = uploading,
                canUpload = !uploading && activeUploadId == null &&
                    items.any { it.state == AttachmentItemState.SELECTED },
            )
        }

        private fun InternalItem.publicItem() = AttachmentItem(
            clientId = clientId,
            name = name,
            mediaType = mediaType,
            bytes = bytes,
            order = order,
            state = state,
            uploadedBytes = uploadedBytes,
            progress = progress,
            issue = issue,
            attachment = attachment,
        )
    }

    companion object {
        // `AttachmentIssue['code']` values.
        const val BATCH_LIMIT = "attachment_batch_limit"
        const val BATCH_TOO_LARGE = "attachment_batch_too_large"
        const val CANCEL_FAILED = "attachment_cancel_failed"
        const val FILE_EMPTY = "attachment_file_empty"
        const val FILE_TOO_LARGE = "attachment_file_too_large"
        const val INVALID_NAME = "attachment_invalid_name"
        const val INVALID_RESPONSE = "attachment_invalid_response"
        const val UNKNOWN_MIME = "attachment_unknown_mime"
        const val UPLOAD_EXPIRED = "attachment_upload_expired"
        const val UPLOAD_FAILED = "attachment_upload_failed"
        const val UPLOAD_BUSY = "attachment_upload_busy"
        const val UPLOAD_STATE_UNKNOWN = "attachment_upload_state_unknown"

        /** The oracle's local lock failures — never a public `attachment_*` relay code. */
        const val BATCH_LOCKED = "attachment_batch_locked"
        const val BATCH_NOT_RESTARTABLE = "attachment_batch_not_restartable"

        /** `normalizedMime` — `type.split(';', 1)[0].trim().toLowerCase()`. */
        internal fun normalizedMime(value: String): String =
            value.split(';', limit = 2)[0].trim().lowercase()

        /** `validFilename` — 1..255 chars, no `.`/`..`, no controls or slashes. */
        internal fun validFilename(value: String): Boolean =
            value.isNotEmpty() && value.length <= 255 && value != "." && value != ".." &&
                !value.any { it.code <= 0x1f || it == '/' || it == '\\' }

        /** RFC3339Nano → epoch millis; `Instant`/`OffsetDateTime` covers `Z` and `±HH:MM`. */
        internal fun parseExpiry(value: String?): Long? =
            value?.let { raw ->
                runCatching { OffsetDateTime.parse(raw).toInstant().toEpochMilli() }.getOrNull()
            }

        internal fun hex(bytes: ByteArray): String {
            val out = StringBuilder(bytes.size * 2)
            for (byte in bytes) out.append("%02x".format(byte))
            return out.toString()
        }

        private fun MessageDigest.hex(): String = hex(digest())
    }

    /** `coroutineContext.job` without leaking the Job type into callers. */
    private suspend fun currentJob(): Job? = kotlinx.coroutines.currentCoroutineContext()[Job]
}

/**
 * `attachmentIssueText` — the oracle's code → user string map
 * (`attachments.ts`); unknown codes fall through to the generic line.
 */
fun attachmentIssueText(issue: AttachmentIssue): String = when (issue.code) {
    AttachmentUploads.BATCH_LIMIT -> "Select at most ${
        issue.args?.get("max")?.let { (it as? JsonPrimitive)?.contentOrNull } ?: "the allowed number of"
    } files."
    AttachmentUploads.BATCH_TOO_LARGE -> "These files exceed the attachment batch limit."
    AttachmentUploads.FILE_TOO_LARGE -> "This file exceeds the attachment size limit."
    AttachmentUploads.FILE_EMPTY -> "Empty files cannot be attached."
    AttachmentUploads.UNKNOWN_MIME -> "This file type is not supported."
    AttachmentUploads.INVALID_NAME -> "This file name is not supported."
    AttachmentUploads.UPLOAD_EXPIRED -> "The upload expired. Restart it to upload from the beginning."
    AttachmentUploads.UPLOAD_BUSY ->
        "This device already has the maximum number of active uploads. Finish or cancel one, then retry."
    AttachmentUploads.UPLOAD_STATE_UNKNOWN ->
        "Upload progress is uncertain. Restart it from the beginning; it will not resume automatically."
    AttachmentUploads.CANCEL_FAILED ->
        "The local upload was cleared, but the relay could not confirm cancellation."
    AttachmentUploads.INVALID_RESPONSE -> "The relay returned an invalid upload response."
    else -> "The attachment could not be uploaded."
}
