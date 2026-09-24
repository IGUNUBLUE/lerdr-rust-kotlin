//! Phase-5 §2.2 `frame_zstd` — negotiated zstd compression of pane frame
//! payloads.
//!
//! Wire shape (`docs/13-phase5-wire-spec.md` §2.2): a negotiated
//! `pane_content` keeps its full envelope in plaintext — `type`, `pane_id`,
//! `target`, `content_fingerprint`, `ack_required`, `format`, `viewport_*`
//! and the semantic fields all stay visible so routing, send-buffer
//! coalescing, and the ack/fingerprint chain work unchanged — while the
//! bulky `content` member folds into
//!
//! ```json
//! {"encoding":"zstd","payload":"<base64 zstd>"}
//! ```
//!
//! `payload` is standard base64 of a zstd frame whose plaintext is the JSON
//! object of the compressed members — today `{"content":"…"}`. An absent
//! `encoding` always means a plaintext frame (the pre-Phase-5 shape).
//!
//! Only `pane_content` qualifies: `pane_delta` already compresses well
//! (spec) and `pane_resync` carries no payload member at all — it is never
//! emitted compressed, only listed in §2.2 because it shares the pane-frame
//! family.

use serde::{Deserialize, Serialize};

use crate::protocol::PaneContent;

/// The capability name — live only while present on BOTH lists
/// (docs/13 §0): `push_config.capabilities`/`caps_update` on the server
/// side, `client_caps`/inbound `caps_update` on the client side.
pub const CAPABILITY: &str = "frame_zstd";

/// The `encoding` value marking a compressed `payload` member.
pub const ENCODING: &str = "zstd";

/// Compression level — pane frames are latency-sensitive pushes (a few per
/// second per watch), not archival blobs; level 1 is zstd's fast end.
const LEVEL: i32 = 1;

/// Inflate bound — a plaintext frame can never exceed the outbound byte
/// cap, so a `payload` inflating past it is malformed, not merely large.
/// Keeps `decompress_payload` safe on untrusted input (no zip bombs).
const MAX_INFLATED: usize = crate::sendbuffer::MAX_OUTBOUND_MESSAGE_BYTES;

/// The compressed member set — the bulky payload fields that move out of
/// the plaintext envelope into `payload`. Unknown members reject loudly:
/// a `payload` carrying anything else is a contract drift the decoder
/// should not silently drop.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content: Option<String>,
}

/// Why a compressed `pane_content` could not be restored to plaintext.
#[derive(Debug, thiserror::Error)]
pub enum FrameZstdError {
    /// `encoding` names a codec this build does not speak.
    #[error("unknown pane frame encoding: {0}")]
    UnknownEncoding(String),
    /// `encoding:"zstd"` without the `payload` member.
    #[error("pane frame declares zstd encoding without payload")]
    MissingPayload,
    /// `payload` is not standard base64.
    #[error("pane frame payload is not valid base64: {0}")]
    BadBase64(#[source] base64::DecodeError),
    /// Inflation failed — truncated, corrupt, or larger than the outbound
    /// byte cap.
    #[error("pane frame payload does not inflate: {0}")]
    Inflate(#[source] std::io::Error),
    /// The inflated bytes were not the payload object.
    #[error("pane frame payload is not the payload object: {0}")]
    BadPayload(#[source] serde_json::Error),
}

impl PaneContent {
    /// docs/13 §2.2 — fold `content` into the compressed `payload` member,
    /// stamping `encoding:"zstd"`. `true` when the frame was transformed.
    /// No-ops on a frame without `content` (nothing to compress — e.g. the
    /// bare error variant) or with an `encoding` already declared (never
    /// double-wrap).
    pub fn compress_payload(&mut self) -> bool {
        if self.encoding.is_some() {
            return false;
        }
        let Some(content) = self.content.take() else {
            return false;
        };
        let json = crate::json::to_vec(&Payload {
            content: Some(content),
        })
        .expect("payload serialization cannot fail");
        let compressed = zstd::bulk::compress(&json, LEVEL)
            .expect("zstd compression of an in-memory buffer cannot fail");
        self.encoding = Some(ENCODING.to_owned());
        self.payload = Some(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            compressed,
        ));
        true
    }

    /// The §2.2 inverse — restore `content` from `payload` when `encoding`
    /// is `"zstd"`, clearing both negotiated fields so the frame re-encodes
    /// to its plaintext form. `Ok(false)` for plaintext frames. The frame
    /// is left untouched on error. [`Outbound::decode`](crate::protocol::Outbound)
    /// deliberately stays wire-faithful (compressed members pass through so
    /// decode + encode round-trips byte-exact); this is the explicit
    /// normalization for tooling, tests, and clients.
    pub fn decompress_payload(&mut self) -> Result<bool, FrameZstdError> {
        match self.encoding.as_deref() {
            None => return Ok(false),
            Some(ENCODING) => {}
            Some(other) => return Err(FrameZstdError::UnknownEncoding(other.to_owned())),
        }
        let payload = self
            .payload
            .as_deref()
            .ok_or(FrameZstdError::MissingPayload)?;
        let compressed =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, payload)
                .map_err(FrameZstdError::BadBase64)?;
        let json =
            zstd::bulk::decompress(&compressed, MAX_INFLATED).map_err(FrameZstdError::Inflate)?;
        let payload: Payload = serde_json::from_slice(&json).map_err(FrameZstdError::BadPayload)?;
        self.content = payload.content;
        self.encoding = None;
        self.payload = None;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::MaybeNull;
    use crate::protocol::{Outbound, TargetRef};

    /// A watch-shaped `pane_content` — the envelope variety `full_frame`
    /// emits, so the assertions cover the realistic field mix.
    fn watch_frame(content: &str) -> PaneContent {
        PaneContent {
            r#type: "pane_content".to_owned(),
            pane_id: Some("wE:p1".to_owned()),
            content: Some(content.to_owned()),
            ack_required: Some(true),
            content_fingerprint: Some("0123456789abcdef".to_owned()),
            format: Some("text".to_owned()),
            truncated: Some(false),
            viewport_only: Some(false),
            attention_kind: Some("idle".to_owned()),
            prompt: Some(String::new()),
            command: Some(String::new()),
            options: Some(MaybeNull::Null),
            interaction: Some(MaybeNull::Null),
            question_layout: Some(false),
            no_echo: Some(false),
            target: Some(MaybeNull::Value(TargetRef {
                server_session_id: "primary".to_owned(),
                pane_id: "wE:p1".to_owned(),
                terminal_id: "term-1".to_owned(),
                generation: 2,
                agent_session_id: "sess-1".to_owned(),
                ..TargetRef::default()
            })),
            ..PaneContent::default()
        }
    }

    /// Repetitive multi-KB pane text — ANSI output compresses well.
    fn big_content() -> String {
        "The quick brown fox jumps over the lazy dog. $ cargo test --workspace\n".repeat(80)
    }

    #[test]
    fn compress_folds_content_into_payload() {
        let mut frame = watch_frame("line one\nline two\n");
        assert!(frame.compress_payload());
        assert_eq!(frame.encoding.as_deref(), Some(ENCODING));
        let payload = frame.payload.expect("payload stamped");
        assert!(frame.content.is_none(), "content moved out");
        // The payload inflates to the member object — `{"content":"…"}`.
        let bytes =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &payload).unwrap();
        let json = zstd::bulk::decompress(&bytes, 4096).unwrap();
        assert_eq!(json, br#"{"content":"line one\nline two\n"}"#.to_vec());
        // Envelope fields are untouched — routing/coalescing sees them.
        assert_eq!(
            frame.content_fingerprint.as_deref(),
            Some("0123456789abcdef")
        );
        assert_eq!(frame.ack_required, Some(true));
        assert!(frame.target.is_some());
    }

    #[test]
    fn compress_noops_without_content_or_when_encoded() {
        let mut bare = PaneContent {
            r#type: "pane_content".to_owned(),
            pane_id: Some("wE:p1".to_owned()),
            error: Some("gone".to_owned()),
            ..PaneContent::default()
        };
        assert!(!bare.compress_payload());
        assert!(bare.encoding.is_none() && bare.payload.is_none());

        let mut encoded = watch_frame("x");
        assert!(encoded.compress_payload());
        assert!(!encoded.compress_payload(), "never double-wrap");
    }

    #[test]
    fn compress_decompress_restores_plaintext_frame() {
        let original = watch_frame(&big_content());
        let mut compressed = original.clone();
        assert!(compressed.compress_payload());
        assert!(compressed.decompress_payload().expect("inflates"));
        assert_eq!(compressed, original);
        // A plaintext frame is a decompress no-op.
        let mut plain = watch_frame("hi");
        assert!(matches!(plain.decompress_payload(), Ok(false)));
    }

    #[test]
    fn negotiated_encode_matches_wire_shape_and_round_trips() {
        let frame = Outbound::PaneContent(Box::new(watch_frame(&big_content())));
        let encoded = frame.encode_negotiated(true);
        let json: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(json["type"], "pane_content");
        assert_eq!(json["encoding"], "zstd");
        assert!(json["payload"].is_string());
        assert!(json.get("content").is_none(), "content folded into payload");
        assert_eq!(json["content_fingerprint"], "0123456789abcdef");
        assert_eq!(json["pane_id"], "wE:p1");
        // `target` keeps struct order — the typed transform does not
        // re-sort nested objects.
        assert_eq!(
            json["target"],
            serde_json::json!({"server_session_id":"primary","pane_id":"wE:p1","terminal_id":"term-1","generation":2,"agent_session_id":"sess-1"})
        );

        // Wire-faithful decode: compressed members pass through and
        // re-encode byte-exact; decompress restores the logical view.
        let decoded = Outbound::decode(&encoded).unwrap();
        assert_eq!(decoded.encode(), encoded);
        let Outbound::PaneContent(mut message) = decoded else {
            panic!("expected PaneContent");
        };
        assert!(message.decompress_payload().expect("inflates"));
        assert_eq!(message.content.as_deref(), Some(big_content().as_str()));
        let Outbound::PaneContent(original) = frame else {
            panic!("same variant");
        };
        assert_eq!(*message, *original);
    }

    #[test]
    fn negotiated_off_encodes_plaintext_identically() {
        let frame = Outbound::PaneContent(Box::new(watch_frame("content")));
        assert_eq!(frame.encode_negotiated(false), frame.encode());
        // Non-pane frames never transform either.
        let resync = Outbound::PaneResync(crate::protocol::PaneResync {
            r#type: "pane_resync".to_owned(),
            pane_id: Some("wE:p1".to_owned()),
            ..Default::default()
        });
        assert_eq!(resync.encode_negotiated(true), resync.encode());
        // `pane_delta` stays uncompressed by spec.
        let delta = Outbound::PaneDelta(Box::default());
        assert_eq!(delta.encode_negotiated(true), delta.encode());
    }

    #[test]
    fn compression_wins_on_a_realistic_frame() {
        let frame = watch_frame(&big_content());
        let raw_len = crate::json::to_vec(&frame).unwrap().len();
        assert!(raw_len > 1024, "test frame should exceed 1KB: {raw_len}");
        let mut compressed = frame.clone();
        assert!(compressed.compress_payload());
        let wire_len = crate::json::to_vec(&compressed).unwrap().len();
        assert!(
            wire_len < raw_len,
            "compressed {wire_len} should beat raw {raw_len}"
        );
        // Even the payload member alone beats the raw content — the +33%
        // base64 tax still wins on terminal text.
        assert!(compressed.payload.unwrap().len() < big_content().len());
    }

    #[test]
    fn decompress_rejects_malformed_frames() {
        let mut unknown = watch_frame("x");
        unknown.encoding = Some("gzip".to_owned());
        assert!(matches!(
            unknown.decompress_payload(),
            Err(FrameZstdError::UnknownEncoding(_))
        ));

        let mut missing = watch_frame("x");
        missing.encoding = Some(ENCODING.to_owned());
        assert!(matches!(
            missing.decompress_payload(),
            Err(FrameZstdError::MissingPayload)
        ));

        let mut bad64 = watch_frame("x");
        bad64.encoding = Some(ENCODING.to_owned());
        bad64.payload = Some("!!!not-base64!!!".to_owned());
        assert!(matches!(
            bad64.decompress_payload(),
            Err(FrameZstdError::BadBase64(_))
        ));

        let mut garbage = watch_frame("x");
        garbage.encoding = Some(ENCODING.to_owned());
        garbage.payload = Some(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            b"not a zstd frame",
        ));
        assert!(matches!(
            garbage.decompress_payload(),
            Err(FrameZstdError::Inflate(_))
        ));

        // Wrong member set — a foreign payload rejects rather than
        // silently dropping fields.
        let mut foreign = watch_frame("x");
        foreign.encoding = Some(ENCODING.to_owned());
        foreign.payload = Some(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            zstd::bulk::compress(br#"{"lines":["a","b"]}"#, LEVEL).unwrap(),
        ));
        assert!(matches!(
            foreign.decompress_payload(),
            Err(FrameZstdError::BadPayload(_))
        ));
    }
}
