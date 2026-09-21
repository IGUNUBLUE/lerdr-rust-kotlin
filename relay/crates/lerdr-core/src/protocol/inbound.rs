//! Client -> server action envelope.
//!
//! Port of `Inbound` + `DecodeMap` from `internal/protocol/protocol.go`.
//! Field order mirrors the Go struct exactly so that re-serializing a decoded
//! message reproduces the canonical `decoded_json` byte-for-byte.
//!
//! `DecodeMap` first round-trips the raw map through `json.Marshal` (which
//! sorts keys and collapses duplicate keys last-wins); [`Inbound::decode_map`]
//! does the same through [`crate::json::to_vec`] so that `json.RawMessage`
//! fields (`subscription`, `policy`) hold the normalized — not wire-verbatim —
//! bytes, exactly like the Go path.

use serde::{Deserialize, Serialize};

use crate::json::{de_default, RawJson};

/// `TargetRef` identifies the pane/tab/workspace a message is about.
/// All five fields are always emitted (no `omitempty` in Go).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetRef {
    #[serde(default, deserialize_with = "de_default")]
    pub server_session_id: String,
    #[serde(default, deserialize_with = "de_default")]
    pub pane_id: String,
    #[serde(default, deserialize_with = "de_default")]
    pub terminal_id: String,
    #[serde(default, deserialize_with = "de_default")]
    pub generation: i64,
    #[serde(default, deserialize_with = "de_default")]
    pub agent_session_id: String,
}

/// Errors from decoding an inbound message.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    /// Malformed JSON, a non-object payload, or a field of the wrong type —
    /// the Go side answers with `DecodeFailureResponse` (`invalid_request`).
    #[error("invalid request: {0}")]
    Invalid(#[from] serde_json::Error),
    /// `type` ended up empty after the command-envelope rewrite.
    #[error("message type is required")]
    MissingType,
}

/// Flat client->server action message.
///
/// Mirrors `protocol.Inbound`: every field is optional on decode (missing or
/// `null` -> zero value), unknown keys are dropped, and serialization omits
/// zero values — except `protocol`, which Go emits unconditionally.
///
/// Notably absent (read from the raw map by Go handlers, never typed):
/// `action`, `content_fingerprint`, `interval_ms`, `sdp`, `candidate`,
/// `sdp_mid`, `sdp_mline_index`, `language`, `speech_request_id`, `files`,
/// `upload_id`, `file_index`, `sequence`, `sha256`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Inbound {
    #[serde(default, deserialize_with = "de_default")]
    pub r#type: String,
    /// Always emitted, even when 0 (no `omitempty` in Go).
    #[serde(default, deserialize_with = "de_default")]
    pub protocol: i64,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<TargetRef>,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub action_id: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub server_session_id: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub session_id: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub pane_id: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub text: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub name: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub device_id: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub role: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub locale: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub profile_id: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub label: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub workspace_id: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub workspace_ids: Vec<String>,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub expected_workspace_ids: Vec<String>,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub close_group: bool,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub before_workspace_id: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub branch: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub base: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub force: bool,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub cwd: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub prompt: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub event_id: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub approval_fingerprint: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub choice: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub interaction_id: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "Option::is_none"
    )]
    pub insert_index: Option<i64>,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "Option::is_none"
    )]
    pub index: Option<i64>,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "Option::is_none"
    )]
    pub total: Option<i64>,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub keys: Vec<String>,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub selected_indices: Vec<i64>,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub other_selected: bool,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub other_text: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub direction: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "is_zero"
    )]
    pub lines: i64,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub before: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub cursor: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub retry: bool,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "is_zero"
    )]
    pub limit: i64,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "is_zero"
    )]
    pub columns: i64,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "is_zero"
    )]
    pub rows: i64,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub format: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub path: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub filename: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub mime: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub data: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub client_id: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub replace_endpoints: Vec<String>,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub notify_finished: bool,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub endpoints: Vec<String>,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub origin: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub expected_origin: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub expected_version: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub expected_revision: String,
    /// `json.RawMessage` in Go: absent -> omitted, `null` -> emits `null`,
    /// value -> normalized bytes. `Some` always holds the raw text.
    #[serde(
        default,
        deserialize_with = "raw_field",
        skip_serializing_if = "Option::is_none"
    )]
    pub subscription: Option<RawJson>,
    #[serde(
        default,
        deserialize_with = "raw_field",
        skip_serializing_if = "Option::is_none"
    )]
    pub policy: Option<RawJson>,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub event_ref: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub snooze_until: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub snoozed: bool,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub visible: bool,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub unlocked: bool,
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

/// `RawMessage` field: captures `null` as literal `Some("null")` like Go does
/// (only absence maps to `None`/omitted).
fn raw_field<'de, D>(deserializer: D) -> Result<Option<RawJson>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    RawJson::deserialize(deserializer).map(Some)
}

impl Inbound {
    /// Decode from a parsed JSON object — the `DecodeMap` port.
    ///
    /// The map is re-serialized through the Go-compatible formatter (sorted
    /// keys, integral floats normalized) before the typed decode, so
    /// `json.RawMessage` fields see canonical bytes.
    pub fn decode_map(
        raw: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, DecodeError> {
        let mut normalized = raw.clone();
        normalized.values_mut().for_each(normalize_numbers);
        let data = crate::json::to_vec(&normalized).map_err(DecodeError::Invalid)?;
        let mut message: Inbound = serde_json::from_slice(&data).map_err(DecodeError::Invalid)?;
        if let Some(action) = raw.get("action").and_then(serde_json::Value::as_str) {
            if !action.is_empty() && (message.r#type.is_empty() || message.r#type == "command") {
                message.r#type = action.to_owned();
            }
        }
        if message.r#type.is_empty() {
            return Err(DecodeError::MissingType);
        }
        Ok(message)
    }

    /// Decode a wire frame (UTF-8 JSON text) into the typed view.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let raw: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(bytes).map_err(DecodeError::Invalid)?;
        Self::decode_map(&raw)
    }

    /// Serialize back to the canonical typed view (what `decoded_json` shows).
    pub fn encode(&self) -> Vec<u8> {
        crate::json::to_vec(self).expect("Inbound serialization cannot fail")
    }
}

/// `json.Marshal` renders integral `float64` values without a fraction
/// (`500.0` -> `500`), so a float that fits the int fields must be normalized
/// before the typed decode — Go accepts it, and serde would reject `500.0`
/// for an `i64` field.
fn normalize_numbers(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                if f.fract() == 0.0 && f.abs() <= (1u64 << 53) as f64 - 1.0 {
                    *value = serde_json::Value::from(f as i64);
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(normalize_numbers),
        serde_json::Value::Object(map) => map.values_mut().for_each(normalize_numbers),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_envelope_rewrites_type() {
        let msg = Inbound::decode(
            br#"{"action":"submit_prompt","pane_id":"p1","protocol":3,"type":"command"}"#,
        )
        .unwrap();
        assert_eq!(msg.r#type, "submit_prompt");
        assert_eq!(msg.protocol, 3);
    }

    #[test]
    fn action_does_not_override_real_type() {
        let msg = Inbound::decode(
            br#"{"action":"submit_prompt","pane_id":"p1","protocol":3,"type":"send_text"}"#,
        )
        .unwrap();
        assert_eq!(msg.r#type, "send_text");
    }

    #[test]
    fn missing_type_is_an_error() {
        assert!(matches!(
            Inbound::decode(br#"{"protocol":3}"#),
            Err(DecodeError::MissingType)
        ));
    }

    #[test]
    fn null_fields_decode_to_zero() {
        let msg = Inbound::decode(br#"{"type":"send_text","text":null,"force":null}"#).unwrap();
        assert_eq!(msg.text, "");
        assert!(!msg.force);
    }

    #[test]
    fn float_fits_int_fields() {
        let msg = Inbound::decode(br#"{"type":"watch_pane","lines":80.0}"#).unwrap();
        assert_eq!(msg.lines, 80);
    }

    #[test]
    fn policy_raw_message_normalized() {
        // Duplicate keys collapse last-wins; RawValue sees canonical bytes.
        let msg = Inbound::decode(br#"{"type":"push_policy_set","policy":{"b":2,"a":1}}"#).unwrap();
        assert_eq!(msg.policy.unwrap().get(), "{\"a\":1,\"b\":2}");
    }
}
