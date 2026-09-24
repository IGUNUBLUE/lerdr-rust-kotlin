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

use crate::json::{de_default, de_str_loose, RawJson};

/// `TargetRef` identifies the pane/tab/workspace a message is about.
/// The five original fields are always emitted (no `omitempty` in Go);
/// `workspace_id`/`tab_id` are the Phase-5 additions — emitted only when
/// non-empty so pre-Phase-5 `decoded_json` stays byte-exact.
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
    /// Phase-5 `focus_workspace` target (`docs/13` §1.1).
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub workspace_id: String,
    /// Phase-5 `focus_tab` target (`docs/13` §1.1).
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub tab_id: String,
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
/// `upload_id`, `file_index`, `sequence`, `sha256`. `action` is
/// special-cased in [`Inbound::decode_map`]; the rest stay reachable
/// through [`Inbound::raw`], [`Inbound::raw_str`], and [`Inbound::raw_int`]
/// (`content_fingerprint`/`interval_ms` have named accessors too).
///
/// Phase-5 adds `capabilities` (`client_caps`/`caps_update`, docs/13
/// §0) — a field the Go struct never carried; it appends at the tail
/// and emits only when populated.
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
    /// Pagination cursor for the legacy reads — Phase-5 `pane_search`/
    /// `pane_selection_read` overload the key with a `{row,col}` object;
    /// `de_str_loose` keeps strings verbatim and tolerates the object
    /// (handlers read the structured form through `raw("cursor")`).
    #[serde(
        default,
        deserialize_with = "de_str_loose",
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
    /// Phase-5 `client_caps`/inbound `caps_update` — the client's
    /// announced capability list (docs/13 §0).
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub capabilities: Vec<String>,
    /// Wire fields the typed view does not model — the raw decoded map,
    /// captured by [`Inbound::decode_map`] before normalization so
    /// [`Inbound::raw`]/[`Inbound::raw_str`]/[`Inbound::raw_int`] see
    /// exactly what Go's handlers see. `skip` keeps it out of both the
    /// typed decode and `encode()` — `decoded_json` parity is preserved
    /// because Go's `Inbound` never carries these fields either.
    #[serde(skip)]
    raw_fields: serde_json::Map<String, serde_json::Value>,
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
        message.raw_fields = raw.clone();
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

    /// `message[key]` — a wire field the typed view does not model
    /// (`content_fingerprint`, `interval_ms`, `sdp`, `candidate`, …).
    /// Populated by [`Inbound::decode_map`]; absent key → `None`.
    /// Messages built any other way (e.g. `Default`) see an empty map.
    pub fn raw(&self, key: &str) -> Option<&serde_json::Value> {
        self.raw_fields.get(key)
    }

    /// `message[key].(string)` — `Some` only for a present JSON string;
    /// absent, `null`, and non-string all read `None`, exactly like the
    /// failed `,ok` type-assert in the Go handlers (never a decode error).
    pub fn raw_str(&self, key: &str) -> Option<&str> {
        self.raw(key).and_then(serde_json::Value::as_str)
    }

    /// `messageInt(message[key], …)` — integers and integral floats
    /// (`250.0` reads as `250`), nothing else. `None` means the caller's
    /// fallback applies, same as Go.
    pub fn raw_int(&self, key: &str) -> Option<i64> {
        match self.raw(key)? {
            serde_json::Value::Number(n) => n.as_i64().or_else(|| {
                n.as_f64().and_then(|f| {
                    (f.fract() == 0.0 && f.abs() <= (1u64 << 53) as f64 - 1.0).then_some(f as i64)
                })
            }),
            _ => None,
        }
    }

    /// `message["content_fingerprint"].(string)` — the pane dedup field
    /// `read_pane`/`watch_pane`/`pane_applied` handlers read raw in Go.
    pub fn content_fingerprint(&self) -> Option<&str> {
        self.raw_str("content_fingerprint")
    }

    /// `messageInt(message["interval_ms"], …)` — the `watch_pane`
    /// cadence hint, `None` when absent or not an integral number.
    pub fn interval_ms(&self) -> Option<i64> {
        self.raw_int("interval_ms")
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

    #[test]
    fn raw_fields_reach_handlers() {
        let msg = Inbound::decode(
            br#"{"type":"watch_pane","pane_id":"p1","content_fingerprint":"0123456789abcdef","interval_ms":500}"#,
        )
        .unwrap();
        assert_eq!(msg.content_fingerprint(), Some("0123456789abcdef"));
        assert_eq!(msg.interval_ms(), Some(500));
        assert_eq!(
            msg.raw("pane_id").and_then(|v| v.as_str()),
            Some("p1"),
            "raw map holds every wire field, not just the dropped ones"
        );
        assert_eq!(msg.raw("missing"), None);
    }

    #[test]
    fn raw_fields_stay_out_of_decoded_json() {
        // Go's `Inbound` never types these fields, so `decoded_json` must
        // not grow them — the conformance vectors pin that.
        let msg = Inbound::decode(
            br#"{"type":"watch_pane","pane_id":"p1","content_fingerprint":"0123456789abcdef","interval_ms":500}"#,
        )
        .unwrap();
        let encoded = String::from_utf8(msg.encode()).unwrap();
        assert!(!encoded.contains("content_fingerprint"), "{encoded}");
        assert!(!encoded.contains("interval_ms"), "{encoded}");
    }

    #[test]
    fn raw_field_type_mismatch_reads_absent_not_error() {
        // `message["content_fingerprint"].(string)` — a non-string is a
        // failed type-assert in Go: absent, never a decode failure.
        let msg =
            Inbound::decode(br#"{"type":"read_pane","pane_id":"p1","content_fingerprint":42}"#)
                .unwrap();
        assert_eq!(msg.content_fingerprint(), None);
        let msg =
            Inbound::decode(br#"{"type":"read_pane","pane_id":"p1","content_fingerprint":null}"#)
                .unwrap();
        assert_eq!(msg.content_fingerprint(), None);
    }

    #[test]
    fn phase5_target_ref_fields_decode_and_emit_when_populated() {
        // `workspace_id`/`tab_id` are additive (docs/13 §1.1): absent
        // decodes to "", and "" stays out of `decoded_json` so pre-Phase-5
        // re-serialization stays byte-exact.
        let msg = Inbound::decode(
            br#"{"type":"focus_workspace","target":{"workspace_id":"wE","pane_id":"wE:p1"}}"#,
        )
        .unwrap();
        let target = msg.target.as_ref().unwrap();
        assert_eq!(target.workspace_id, "wE");
        assert_eq!(target.pane_id, "wE:p1");
        assert_eq!(target.tab_id, "");
        let encoded = String::from_utf8(msg.encode()).unwrap();
        assert!(encoded.contains("\"workspace_id\":\"wE\""), "{encoded}");
        assert!(!encoded.contains("tab_id"), "{encoded}");

        let msg = Inbound::decode(
            br#"{"type":"focus_tab","target":{"pane_id":"wE:p1","tab_id":"wE:p1:t2"}}"#,
        )
        .unwrap();
        let target = msg.target.as_ref().unwrap();
        assert_eq!(target.tab_id, "wE:p1:t2");
        assert_eq!(target.workspace_id, "");

        // A pre-Phase-5 target emits none of the new fields.
        let msg = Inbound::decode(br#"{"type":"send_text","target":{"pane_id":"p1"}}"#).unwrap();
        let encoded = String::from_utf8(msg.encode()).unwrap();
        assert!(!encoded.contains("workspace_id"), "{encoded}");
        assert!(!encoded.contains("tab_id"), "{encoded}");
    }

    #[test]
    fn client_caps_fields_decode_and_emit() {
        let msg = Inbound::decode(
            br#"{"type":"client_caps","protocol":3,"capabilities":["focus","frame_zstd"]}"#,
        )
        .unwrap();
        assert_eq!(msg.capabilities, ["focus", "frame_zstd"]);
        let encoded = String::from_utf8(msg.encode()).unwrap();
        assert!(
            encoded.contains("\"capabilities\":[\"focus\",\"frame_zstd\"]"),
            "{encoded}"
        );

        // Inbound `caps_update` re-announces through the same field.
        let msg = Inbound::decode(br#"{"type":"caps_update","capabilities":["focus"]}"#).unwrap();
        assert_eq!(msg.capabilities, ["focus"]);

        let msg = Inbound::decode(br#"{"type":"read_pane"}"#).unwrap();
        let encoded = String::from_utf8(msg.encode()).unwrap();
        assert!(!encoded.contains("capabilities"), "{encoded}");
    }

    #[test]
    fn cursor_tolerates_the_phase5_object_shape() {
        // `cursor` is a pagination string for the legacy reads; Phase-5
        // `pane_search`/`pane_selection_read` send `{row,col}` — the
        // typed field reads "" while the object stays reachable raw.
        let msg = Inbound::decode(
            br#"{"type":"pane_search","pane_id":"wE:p1","query":"panic","cursor":{"row":3,"col":4}}"#,
        )
        .unwrap();
        assert_eq!(msg.cursor, "");
        assert_eq!(
            msg.raw("cursor").cloned(),
            Some(serde_json::json!({"row": 3, "col": 4}))
        );

        // Strings still arrive verbatim; absent stays absent.
        let msg = Inbound::decode(br#"{"type":"read_pane","cursor":"page-2"}"#).unwrap();
        assert_eq!(msg.cursor, "page-2");
        let msg = Inbound::decode(br#"{"type":"read_pane"}"#).unwrap();
        assert_eq!(msg.cursor, "");
        // Null and other non-strings degrade to "" rather than failing
        // the whole request (a legacy `de_default` would have errored).
        let msg = Inbound::decode(br#"{"type":"read_pane","cursor":null}"#).unwrap();
        assert_eq!(msg.cursor, "");
    }

    #[test]
    fn interval_ms_accepts_integral_numbers_only() {
        // Go `messageInt`: int and integral float64 only.
        for (raw, want) in [
            ("500", Some(500)),
            ("250.0", Some(250)),
            ("-5", Some(-5)),
            ("250.5", None),
            ("\"250\"", None),
            ("true", None),
            ("null", None),
            ("18446744073709551616", None),
        ] {
            let json = format!(r#"{{"type":"watch_pane","pane_id":"p1","interval_ms":{raw}}}"#);
            let msg = Inbound::decode(json.as_bytes()).unwrap();
            assert_eq!(msg.interval_ms(), want, "interval_ms={raw}");
        }
        let msg = Inbound::decode(br#"{"type":"watch_pane","pane_id":"p1"}"#).unwrap();
        assert_eq!(msg.interval_ms(), None, "absent");
    }
}
