//! Phase-5 §2.4 `upload_binary` — negotiated raw-binary upload chunks.
//!
//! `upload_chunk` today is base64-in-JSON (+33%). While `upload_binary`
//! is live on both capability lists (docs/13 §0), `upload_begin_result`
//! reports `"chunk_encoding":"binary"` and chunks travel as binary
//! plaintext frames inside the e2ee channel — the outer envelope
//! (AES-GCM, outer sequence discipline) is unchanged; the *decrypted*
//! payload for a chunk is:
//!
//! ```text
//! [0x03][upload_id: 32 ASCII bytes][chunk_seq: BE64 i64][raw bytes…]
//! ```
//!
//! The inner type byte `0x03` distinguishes a binary chunk from JSON,
//! which always opens with `{` (0x7B). The `upload_id` field carries the
//! 32-char base64url opaque id (`m.opaqueID` — 192 bits) **verbatim as
//! ASCII**: the begin result hands the client that exact string, so the
//! binary header is self-describing with no relay-side id mapping.
//! (The spec sketch said `upload_id:16`; a truncated id would need a
//! second lookup table on the relay for 16 bytes saved per ≤256 KiB
//! chunk — the verbatim string is the cleaner contract.)
//!
//! Fields the JSON form carries and the carrier omits are anchored
//! server-side: `target`/`file_index` come from the staged session (the
//! client cannot claim either), and `sha256` is computed over the
//! received bytes — the AES-GCM envelope already authenticates them.
//! Replies stay JSON: `upload_chunk_result` acks with an empty
//! `request_id` (the carrier has none; `next_sequence` correlates).

use crate::json::{MaybeNull, RawJson};
use crate::protocol::UploadResultMessage;

/// The capability name — live only while present on BOTH lists
/// (docs/13 §0): `push_config.capabilities`/`caps_update` on the server
/// side, `client_caps`/inbound `caps_update` on the client side.
pub const CAPABILITY: &str = "upload_binary";

/// The inner frame-type byte marking a binary upload chunk. JSON
/// plaintext never collides: a JSON object opens with `{` (0x7B).
pub const TYPE_BYTE: u8 = 0x03;

/// `upload_id` width on the wire — the 32-char base64url opaque id
/// verbatim. The relay only ever mints that shape (`m.opaqueID`), so the
/// header is fixed-width.
pub const UPLOAD_ID_BYTES: usize = 32;

/// `[0x03][upload_id][seq]` — everything before the chunk payload.
pub const HEADER_BYTES: usize = 1 + UPLOAD_ID_BYTES + 8;

/// The `chunk_encoding` result value a negotiated `upload_begin`
/// reports — tells the client to put this session's chunks on the
/// `0x03` carrier.
pub const ENCODING: &str = "binary";

/// One decoded `0x03` frame — the carrier equivalent of a JSON
/// `upload_chunk` minus the server-anchored fields (`target`,
/// `file_index`, `sha256` are supplied relay-side).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryChunk {
    /// The staged session's opaque id (32-char base64url).
    pub upload_id: String,
    /// The global chunk counter — same domain as JSON `sequence`.
    pub sequence: i64,
    /// The raw chunk payload.
    pub data: Vec<u8>,
}

/// A decrypted frame is a binary upload chunk iff its first byte is
/// [`TYPE_BYTE`] — the JSON decode's `serde_json::from_slice` arm owns
/// everything else.
pub fn is_binary_frame(plaintext: &[u8]) -> bool {
    plaintext.first() == Some(&TYPE_BYTE)
}

/// `validAttachmentReference`'s shape — exactly 32 chars of
/// `[A-Za-z0-9_-]` (base64url, no padding).
pub fn is_upload_id(value: &str) -> bool {
    value.len() == UPLOAD_ID_BYTES
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Parse one decrypted `0x03` frame. `None` means malformed — short
/// header, wrong type byte, or an `upload_id` outside the base64url
/// alphabet; the session treats it exactly like non-JSON plaintext
/// (close). Semantic failures — unknown session, out-of-order,
/// oversize — are *not* parse failures: they answer on the JSON ack
/// channel via the upload machinery.
pub fn decode_chunk(plaintext: &[u8]) -> Option<BinaryChunk> {
    let (header, data) = plaintext.split_at_checked(HEADER_BYTES)?;
    if header[0] != TYPE_BYTE {
        return None;
    }
    let upload_id = std::str::from_utf8(&header[1..1 + UPLOAD_ID_BYTES]).ok()?;
    if !is_upload_id(upload_id) {
        return None;
    }
    let sequence = i64::from_be_bytes(header[1 + UPLOAD_ID_BYTES..].try_into().ok()?);
    Some(BinaryChunk {
        upload_id: upload_id.to_owned(),
        sequence,
        data: data.to_vec(),
    })
}

/// Build one `0x03` frame — the encoder half for tests, probes, and any
/// future Rust-side uploader. `None` when `upload_id` is not the
/// fixed-width opaque id the header carries verbatim.
pub fn encode_chunk(upload_id: &str, sequence: i64, data: &[u8]) -> Option<Vec<u8>> {
    if !is_upload_id(upload_id) {
        return None;
    }
    let mut frame = Vec::with_capacity(HEADER_BYTES + data.len());
    frame.push(TYPE_BYTE);
    frame.extend_from_slice(upload_id.as_bytes());
    frame.extend_from_slice(&sequence.to_be_bytes());
    frame.extend_from_slice(data);
    Some(frame)
}

/// The negotiated transform half of §2.4 on the result channel: stamp
/// `chunk_encoding:"binary"` into a successful `upload_begin_result`
/// payload. Error and null results pass through untouched — only a
/// staged session can stream binary chunks. `true` when stamped.
pub fn mark_chunk_encoding(message: &mut UploadResultMessage) -> bool {
    let Some(MaybeNull::Value(raw)) = &message.result else {
        return false;
    };
    let Ok(serde_json::Value::Object(mut result)) =
        serde_json::from_str::<serde_json::Value>(raw.get())
    else {
        return false;
    };
    result.insert(
        "chunk_encoding".to_owned(),
        serde_json::Value::from(ENCODING),
    );
    let Ok(encoded) = crate::json::to_string(&serde_json::Value::Object(result))
        .and_then(serde_json::value::RawValue::from_string)
    else {
        return false;
    };
    message.result = Some(MaybeNull::Value(RawJson(encoded)));
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::MaybeNull;
    use crate::protocol::ApiError;
    use std::collections::BTreeMap;

    const ID: &str = "abcdefghijklmnopqrstuvwxyz123456"; // 32 base64url chars

    #[test]
    fn decode_round_trips_encode() {
        let data = b"raw chunk bytes \x00\x01\x02";
        let frame = encode_chunk(ID, 41, data).expect("encodes");
        assert_eq!(frame.len(), HEADER_BYTES + data.len());
        assert_eq!(frame[0], TYPE_BYTE);
        assert_eq!(&frame[1..33], ID.as_bytes());
        assert_eq!(
            i64::from_be_bytes(frame[33..41].try_into().unwrap()),
            41,
            "BE64 sequence"
        );
        assert_eq!(&frame[41..], data);
        let chunk = decode_chunk(&frame).expect("decodes");
        assert_eq!(
            chunk,
            BinaryChunk {
                upload_id: ID.to_owned(),
                sequence: 41,
                data: data.to_vec(),
            }
        );
    }

    #[test]
    fn decode_accepts_edge_sequences_and_empty_payload() {
        // Negative and huge sequences decode into the same i64 domain the
        // JSON form uses — ordering validation rejects them downstream,
        // not the parser.
        let frame = encode_chunk(ID, -1, b"").expect("encodes");
        let chunk = decode_chunk(&frame).expect("decodes");
        assert_eq!(chunk.sequence, -1);
        assert!(chunk.data.is_empty());
        let frame = encode_chunk(ID, i64::MAX, b"").expect("encodes");
        assert_eq!(decode_chunk(&frame).unwrap().sequence, i64::MAX);
    }

    #[test]
    fn decode_rejects_malformed_frames() {
        // Too short — every truncation of a well-formed frame.
        let frame = encode_chunk(ID, 0, b"payload").expect("encodes");
        for len in 0..HEADER_BYTES {
            assert!(
                decode_chunk(&frame[..len]).is_none(),
                "truncated to {len} must reject"
            );
        }
        // Wrong type byte (JSON starts '{').
        let mut json = frame.clone();
        json[0] = b'{';
        assert!(decode_chunk(&json).is_none());
        assert!(decode_chunk(b"{}").is_none());
        // upload_id outside the base64url alphabet.
        for bad in [b'!', b'=', b' ', b'+', 0xFF] {
            let mut frame = encode_chunk(ID, 0, b"").expect("encodes");
            frame[5] = bad;
            assert!(decode_chunk(&frame).is_none(), "id byte {bad:#04x}");
        }
        // Header alone — a zero-length chunk still decodes (the staged
        // session's size check rejects it like an empty JSON `data`).
        let bare = encode_chunk(ID, 0, b"").expect("encodes");
        assert_eq!(bare.len(), HEADER_BYTES);
        assert!(decode_chunk(&bare).is_some());
    }

    #[test]
    fn encode_rejects_foreign_id_shapes() {
        for id in [
            "",
            "short",
            "x".repeat(33).as_str(),
            "+=+/+=+/+=+/+=+/+=+/+=+/+=+/+=+=",
        ] {
            assert!(encode_chunk(id, 0, b"").is_none(), "id {id:?}");
        }
    }

    #[test]
    fn is_binary_frame_and_is_upload_id_predicates() {
        assert!(is_binary_frame(&encode_chunk(ID, 0, b"").unwrap()));
        assert!(!is_binary_frame(b"{\"type\":\"upload_chunk\"}"));
        assert!(!is_binary_frame(&[]));
        assert!(is_upload_id(ID));
        assert!(is_upload_id(&"-_".repeat(16)));
        for bad in [
            "",
            "abc",
            &"a".repeat(33),
            &("!".to_owned() + &"a".repeat(31)),
        ] {
            assert!(!is_upload_id(bad), "id {bad:?}");
        }
    }

    #[test]
    fn mark_chunk_encoding_stamps_result_payloads_only() {
        let raw = |json: &str| {
            Some(MaybeNull::Value(RawJson(
                serde_json::value::RawValue::from_string(json.to_owned()).unwrap(),
            )))
        };
        // Success payload gains the member — sorted-map order lands it
        // right after chunk_bytes.
        let mut message = UploadResultMessage {
            r#type: "upload_begin_result".to_owned(),
            request_id: Some("r1".to_owned()),
            result: raw(r#"{"upload_id":"u","chunk_bytes":262144,"expires_at":"t","limits":{}}"#),
            ..Default::default()
        };
        assert!(mark_chunk_encoding(&mut message));
        let result = message.result.unwrap();
        let result: serde_json::Value =
            serde_json::from_str(result.value().expect("result value").get()).unwrap();
        assert_eq!(result["chunk_encoding"], "binary");
        assert_eq!(result["upload_id"], "u");
        assert_eq!(result["chunk_bytes"], 262144);

        // Error results and null results pass through.
        let mut error = UploadResultMessage {
            r#type: "upload_begin_result".to_owned(),
            error: Some(ApiError::new("attachment_upload_busy", BTreeMap::new())),
            ..Default::default()
        };
        assert!(!mark_chunk_encoding(&mut error));
        let mut null = UploadResultMessage {
            result: Some(MaybeNull::Null),
            ..Default::default()
        };
        assert!(!mark_chunk_encoding(&mut null));
        let mut absent = UploadResultMessage::default();
        assert!(!mark_chunk_encoding(&mut absent));
    }
}
