//! Wire frame codecs (`FrameCodec` in `e2ee.go`).
//!
//! - `Codec::Json`: `{"type":"e2ee","version":2,"sequence":N,"ciphertext":B64}`
//!   carried in WS text frames.
//! - `Codec::Binary`: `[0x02, 0x00, BE64 seq, ciphertext…]` (10-byte header)
//!   carried in WS binary frames.
//!
//! Decode error order is load-bearing: envelope errors precede the sequence
//! check, which precedes GCM verification (see `crypto.failures`).

use std::borrow::Cow;

use base64::{
    alphabet::URL_SAFE,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
    Engine,
};
use serde::Deserialize;

use crate::{json, E2eeError, VERSION};

const BINARY_FRAME_KIND_DATA: u8 = 0;
const BINARY_FRAME_HEADER_SIZE: usize = 1 + 1 + 8;

/// Go `base64.RawURLEncoding`: URL-safe alphabet, no padding, and (unlike
/// `base64`'s canned `URL_SAFE_NO_PAD` engine) tolerant of non-zero trailing
/// bits — matching Go's non-strict decoder.
const RAW_URL_CONFIG: GeneralPurposeConfig = GeneralPurposeConfig::new()
    .with_encode_padding(false)
    .with_decode_padding_mode(DecodePaddingMode::RequireNone)
    .with_decode_allow_trailing_bits(true);
pub(crate) const RAW_URL: GeneralPurpose = GeneralPurpose::new(&URL_SAFE, RAW_URL_CONFIG);

/// Go `e2eeFrame` decode semantics: absent or `null` fields become zero
/// values, unknown fields are ignored, type mismatches fail the whole parse.
#[derive(Deserialize)]
struct JsonFrame {
    #[serde(default, deserialize_with = "json::null_default")]
    r#type: String,
    #[serde(default, deserialize_with = "json::null_default")]
    version: i64,
    #[serde(default, deserialize_with = "json::null_default")]
    sequence: u64,
    #[serde(default, deserialize_with = "json::null_default")]
    ciphertext: String,
}

/// The frame envelope codec negotiated by the WS message type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Json,
    Binary,
}

impl Codec {
    /// Parse the fixture `codec` label (`"json"` | `"binary"`).
    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "json" => Some(Codec::Json),
            "binary" => Some(Codec::Binary),
            _ => None,
        }
    }

    /// `encodeFrame`: wrap `sequence` + `ciphertext` into wire bytes.
    ///
    /// The JSON layout fixes field order to Go's struct order
    /// (`type, version, sequence, ciphertext`); `sequence` is decimal.
    pub fn encode(self, sequence: u64, ciphertext: &[u8]) -> Vec<u8> {
        match self {
            Codec::Binary => {
                let mut frame = Vec::with_capacity(BINARY_FRAME_HEADER_SIZE + ciphertext.len());
                frame.push(VERSION);
                frame.push(BINARY_FRAME_KIND_DATA);
                frame.extend_from_slice(&sequence.to_be_bytes());
                frame.extend_from_slice(ciphertext);
                frame
            }
            Codec::Json => {
                let mut out = String::with_capacity(ciphertext.len() * 4 / 3 + 64);
                out.push_str("{\"type\":\"e2ee\",\"version\":2,\"sequence\":");
                out.push_str(&sequence.to_string());
                out.push_str(",\"ciphertext\":");
                json::escape_string(&mut out, &RAW_URL.encode(ciphertext));
                out.push('}');
                out.into_bytes()
            }
        }
    }

    /// `decodeFrame`: split wire bytes into `(sequence, ciphertext)`.
    ///
    /// Binary borrows the ciphertext slice; JSON must allocate for the base64
    /// decode — hence `Cow`.
    pub fn decode<'f>(self, raw_frame: &'f [u8]) -> Result<(u64, Cow<'f, [u8]>), E2eeError> {
        match self {
            Codec::Binary => {
                if raw_frame.len() < BINARY_FRAME_HEADER_SIZE {
                    return Err(E2eeError::InvalidFrame);
                }
                if raw_frame[0] != VERSION || raw_frame[1] != BINARY_FRAME_KIND_DATA {
                    return Err(E2eeError::UnsupportedFrame);
                }
                let sequence = u64::from_be_bytes(raw_frame[2..10].try_into().unwrap());
                Ok((
                    sequence,
                    Cow::Borrowed(&raw_frame[BINARY_FRAME_HEADER_SIZE..]),
                ))
            }
            Codec::Json => {
                let frame: JsonFrame =
                    serde_json::from_slice(raw_frame).map_err(|_| E2eeError::InvalidFrame)?;
                if frame.r#type != "e2ee" || frame.version != i64::from(VERSION) {
                    return Err(E2eeError::UnsupportedFrame);
                }
                let ciphertext = RAW_URL
                    .decode(&frame.ciphertext)
                    .map_err(|_| E2eeError::InvalidCiphertext)?;
                Ok((frame.sequence, Cow::Owned(ciphertext)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_layout() {
        let frame = Codec::Binary.encode(0x0102030405060708, b"ct");
        assert_eq!(
            frame,
            [0x02, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, b'c', b't']
        );
        let (seq, ct) = Codec::Binary.decode(&frame).unwrap();
        assert_eq!(seq, 0x0102030405060708);
        assert_eq!(&*ct, b"ct");
    }

    #[test]
    fn json_layout() {
        let frame = Codec::Json.encode(7, b"\x00\xff");
        assert_eq!(
            frame,
            br#"{"type":"e2ee","version":2,"sequence":7,"ciphertext":"AP8"}"#
        );
        let (seq, ct) = Codec::Json.decode(&frame).unwrap();
        assert_eq!(seq, 7);
        assert_eq!(&*ct, b"\x00\xff");
    }

    #[test]
    fn decode_error_order() {
        // Truncated binary header -> InvalidFrame before version/kind checks.
        assert!(matches!(
            Codec::Binary.decode(b"\x02\x00"),
            Err(E2eeError::InvalidFrame)
        ));
        assert!(matches!(
            Codec::Binary.decode(&[0x03, 0x00, 0, 0, 0, 0, 0, 0, 0, 0]),
            Err(E2eeError::UnsupportedFrame)
        ));
        // Malformed JSON -> InvalidFrame; valid JSON, wrong type -> Unsupported.
        assert!(matches!(
            Codec::Json.decode(b"{\"type\":"),
            Err(E2eeError::InvalidFrame)
        ));
        assert!(matches!(
            Codec::Json.decode(br#"{"type":"e2ee_v3_draft","version":2}"#),
            Err(E2eeError::UnsupportedFrame)
        ));
        // Missing fields behave like Go zero values: version 0 -> unsupported.
        assert!(matches!(
            Codec::Json.decode(br#"{"type":"e2ee"}"#),
            Err(E2eeError::UnsupportedFrame)
        ));
        // Explicit nulls decode as zero values too.
        assert!(matches!(
            Codec::Json
                .decode(br#"{"type":"e2ee","version":null,"sequence":null,"ciphertext":null}"#),
            Err(E2eeError::UnsupportedFrame)
        ));
        assert!(matches!(
            Codec::Json.decode(br#"{"type":"e2ee","version":2,"sequence":0,"ciphertext":"!!!"}"#),
            Err(E2eeError::InvalidCiphertext)
        ));
    }
}
