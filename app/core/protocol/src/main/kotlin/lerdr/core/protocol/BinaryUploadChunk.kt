package lerdr.core.protocol

import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * Phase-5 §2.4 `upload_binary` — negotiated raw-binary upload chunks
 * (`relay/crates/lerdr-core/src/uploadbinary.rs`).
 *
 * While `upload_binary` is live on both capability lists, chunks travel as
 * binary plaintext inside the e2ee channel:
 *
 * ```text
 * [0x03][upload_id: 32 ASCII bytes][chunk_seq: BE64 i64][raw bytes…]
 * ```
 *
 * The `0x03` type byte distinguishes a binary chunk from JSON (always
 * `{`); `upload_id` is the `upload_begin`-minted 32-char base64url opaque
 * id verbatim. `target`/`file_index`/`sha256` are anchored server-side;
 * acks stay JSON (`upload_chunk_result`, empty `request_id`,
 * `next_sequence` correlates).
 */
object BinaryUploadChunk {

    const val CAPABILITY = "upload_binary"
    const val TYPE_BYTE: Byte = 0x03
    const val UPLOAD_ID_BYTES = 32
    const val HEADER_BYTES = 1 + UPLOAD_ID_BYTES + 8

    /** `chunk_encoding` value a negotiated `upload_begin_result` reports. */
    const val ENCODING = "binary"

    /** `is_upload_id` — exactly 32 chars of `[A-Za-z0-9_-]` (base64url). */
    fun isUploadId(value: String): Boolean =
        value.length == UPLOAD_ID_BYTES &&
            value.all { it.isLetterOrDigit() || it == '-' || it == '_' }

    /**
     * `encode_chunk` — build one `0x03` frame; null when `uploadId` is not
     * the fixed-width opaque id the header carries verbatim (a foreign id
     * shape is a bug in the staged session, never sent).
     */
    fun encodeChunk(uploadId: String, sequence: Long, data: ByteArray): ByteArray? {
        if (!isUploadId(uploadId)) return null
        val frame = ByteBuffer.allocate(HEADER_BYTES + data.size)
            .order(ByteOrder.BIG_ENDIAN)
        frame.put(TYPE_BYTE)
        frame.put(uploadId.toByteArray(Charsets.US_ASCII))
        frame.putLong(sequence)
        frame.put(data)
        return frame.array()
    }
}
