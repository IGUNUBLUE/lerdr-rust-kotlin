//! Push-notification actions — the `internal/push` subsystem port.
//!
//! The oracle keeps per-device push subscriptions, a notification policy
//! (which events reach which device), snooze state, a viewed-pane dedup
//! ledger (`push_viewed_pane` suppresses notifications for panes the
//! operator is already looking at), an HMAC-signed open-reference scheme,
//! and a durable delivery queue behind a Web Push sender.
//!
//! Replies are NOT `command_result` for the whole family — see each
//! handler for its exact frame sequence (mirrors `server.go`):
//!
//! - `push_policy_get` -> `push_policy` (no `command_result`)
//! - `push_policy_set` -> `command_result`, then `push_policy_result`
//! - `push_snooze` -> `push_policy_result` only
//! - `push_viewed_pane` -> `push_viewed_pane_result`
//! - `push_open_ref` -> `command_result` only
//! - `push_test_device` -> `push_test_result`
//! - `push_subscribe` / `push_unsubscribe` -> `push_subscribed` /
//!   `push_unsubscribed`
//!
//! Every handler additionally emits the relay-local terminal
//! `action_receipt` last (`confirmed` on success,
//! `failed_before_dispatch` with the failing push code otherwise) —
//! additive to the oracle's sequence, matching how `Outcome::frames`
//! terminates command actions.
//!
//! ## Identity note
//!
//! The oracle keys device state by `identity.DeviceID` /
//! `identity.Locale` from the authenticated session — `ActionContext`
//! carries it as `device_id` and handlers use it directly
//! (`pushM.Policy(identity.DeviceID, …)`, `sub.DeviceID = identity.DeviceID`,
//! `SetViewedPane(identity.DeviceID, …)`). The wire `client_id` a client
//! sends on push actions is *claimed* identity — it stays a
//! per-subscription attribute (stored on the subscription, honored as a
//! filter inside `push_unsubscribe`) and never keys device state.
//! Locale stands in as `"en"` — the oracle normalizes to its supported
//! set anyway.
//!
//! ## Persistence note
//!
//! The oracle persists `subscriptions.json`, `policies.json`,
//! `queue.json`, `action_ref.key`, and VAPID keys under the runtime
//! push directory. [`Push::new`] mirrors the handler-facing files plus
//! the durable queue (`0700` dir, `0600` files, `json.MarshalIndent` +
//! trailing newline, atomic tmp+rename); `Push::default()` stays fully
//! in-memory. The `queue.json` envelope, salvage, and quarantine live
//! in [`super::push_queue`]. The Web Push delivery worker
//! (`RunOnce`/`sendOne`/VAPID) lives in [`super::push_delivery`] and
//! drives the queue bookkeeping here through the
//! `due_entries`/`finish_entry`/`recover_pruned`/`flush_queue` seams.
//!
//! Queue durability mirrors the oracle's split: handler-path mutations
//! (`enqueueLocked`/`cancelKey`/`forgetDelivered`/`replaceSubscriptions`/
//! `removeSubscriptions`/`removeDevice`) persist immediately with
//! rollback-on-write-failure; drain-pass mutations
//! (`finishInMemory`/`rescheduleInMemory`, the prune recover/restore
//! halves) mark `state.queue_dirty` and land once per pass through
//! `flush_queue` — the oracle's `dirty` + `flush()` inside `finish`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use base64::Engine;
use serde::{Deserialize, Serialize, Serializer};
use sha2::{Digest, Sha256};
use tokio::sync::Notify;

use lerdr_core::json::{de_default, MaybeNull, RawJson};
use lerdr_core::protocol::{
    action_receipt_response, ActionReceipt, ActionReceiptPhase, ApiError, Inbound, OkMessage,
    Outbound, PushPolicyMessage, PushPolicyResultMessage, PushTestResultMessage, TargetRef,
};

use super::{ActionContext, Outcome};
use crate::topology::Topology;

const NS_PER_MS: i64 = 1_000_000;
const NS_PER_SEC: i64 = 1_000_000_000;
const TEST_EVENT_TTL_NS: i64 = 60 * NS_PER_SEC;
const PUBLISH_DEFAULT_TTL_NS: i64 = 5 * 60 * NS_PER_SEC;
const MAX_PUSH_PAYLOAD_BYTES: usize = 3993;
const MAX_IDENTIFIER_BYTES: usize = 256;
const PUSH_TEST_INTERVAL: Duration = Duration::from_secs(10);

pub(crate) const CATEGORY_ATTENTION: &str = "attention";
pub(crate) const CATEGORY_QUESTION: &str = "question";
pub(crate) const CATEGORY_BRIEF: &str = "brief";
pub(crate) const CATEGORY_FINISHED: &str = "finished";
const CATEGORY_UPDATE: &str = "update";
const CATEGORY_TEST: &str = "test";
const ALLOWED_CATEGORIES: [&str; 6] = [
    CATEGORY_ATTENTION,
    CATEGORY_QUESTION,
    CATEGORY_BRIEF,
    CATEGORY_FINISHED,
    CATEGORY_UPDATE,
    CATEGORY_TEST,
];

pub(crate) const PREVIEW_HIDDEN: &str = "hidden";
pub(crate) const PREVIEW_QUESTION: &str = "question";
pub(crate) const PREVIEW_BRIEF: &str = "brief";

const PLATFORM_OTHER: &str = "other";
const REFERENCE_KEY_SIZE: usize = 32;

/// Go `base64.RawURLEncoding`: URL-safe alphabet, no padding, and Go's
/// non-strict decoder (tolerant of non-zero trailing bits, rejects
/// `=` padding).
const RAW_URL: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::URL_SAFE,
    GeneralPurposeConfig::new()
        .with_encode_padding(false)
        .with_decode_padding_mode(DecodePaddingMode::RequireNone)
        .with_decode_allow_trailing_bits(true),
);

// ---------------------------------------------------------------------------
// Timestamp — `time.Time` semantics: civil UTC + nanoseconds, Go's
// `IsZero` sentinel, RFC3339 / RFC3339Nano serialization.
// ---------------------------------------------------------------------------

/// Seconds + nanos since the Unix epoch. `Default` is Go's zero time
/// (`0001-01-01T00:00:00Z`), so `is_zero` matches `time.Time.IsZero`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Timestamp {
    secs: i64,
    nanos: u32,
}

impl Default for Timestamp {
    /// `time.Time{}` — `0001-01-01T00:00:00Z` in Unix seconds.
    fn default() -> Self {
        Timestamp {
            secs: -62_135_596_800,
            nanos: 0,
        }
    }
}

impl Timestamp {
    pub(crate) fn now() -> Self {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => Timestamp {
                secs: d.as_secs() as i64,
                nanos: d.subsec_nanos(),
            },
            Err(e) => {
                let d = e.duration();
                if d.subsec_nanos() == 0 {
                    Timestamp {
                        secs: -(d.as_secs() as i64),
                        nanos: 0,
                    }
                } else {
                    Timestamp {
                        secs: -(d.as_secs() as i64) - 1,
                        nanos: NS_PER_SEC as u32 - d.subsec_nanos(),
                    }
                }
            }
        }
    }

    pub(crate) fn is_zero(&self) -> bool {
        *self == Timestamp::default()
    }

    /// `t.Unix()` — whole seconds; the VAPID JWT `exp` claim uses it.
    pub(crate) fn unix_seconds(&self) -> i64 {
        self.secs
    }

    /// `t.Add(d)` — nanosecond arithmetic with carry; wraps neither
    /// sign nor magnitude (i128 intermediate).
    pub(crate) fn add_ns(&self, ns: i64) -> Timestamp {
        let total = self.secs as i128 * NS_PER_SEC as i128 + self.nanos as i128 + ns as i128;
        Timestamp {
            secs: total.div_euclid(NS_PER_SEC as i128) as i64,
            nanos: total.rem_euclid(NS_PER_SEC as i128) as u32,
        }
    }

    /// `(year, month, day, hour, minute, second)` in UTC.
    fn civil(&self) -> (i64, u32, u32, u32, u32, u32) {
        let days = self.secs.div_euclid(86_400);
        let rem = self.secs.rem_euclid(86_400) as u32;
        let (y, m, d) = civil_from_days(days);
        (y, m, d, rem / 3600, (rem % 3600) / 60, rem % 60)
    }

    /// Go `Format(time.RFC3339)` — seconds precision, `Z` for UTC.
    pub(crate) fn to_rfc3339(self) -> String {
        let (y, m, d, hh, mm, ss) = self.civil();
        format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
    }

    /// Go `Format(time.RFC3339Nano)` — fractional digits only when
    /// nonzero, trailing zeros trimmed.
    pub(crate) fn to_rfc3339_nano(self) -> String {
        let (y, m, d, hh, mm, ss) = self.civil();
        let mut out = format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}");
        if self.nanos > 0 {
            let frac = format!("{:09}", self.nanos);
            out.push('.');
            out.push_str(frac.trim_end_matches('0'));
        }
        out.push('Z');
        out
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_rfc3339_nano())
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        parse_rfc3339(&text).ok_or_else(|| serde::de::Error::custom("invalid RFC3339 timestamp"))
    }
}

/// Go `json.Unmarshal` into `time.Time` + `omitempty`-on-struct (a
/// no-op): the field is always emitted; the zero time round-trips as
/// `"0001-01-01T00:00:00Z"`. Used on `Option<Timestamp>` fields in the
/// persisted file formats.
mod go_zero_time {
    use super::*;

    pub fn serialize<S: Serializer>(
        value: &Option<Timestamp>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(t) => serializer.serialize_str(&t.to_rfc3339_nano()),
            None => serializer.serialize_str("0001-01-01T00:00:00Z"),
        }
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Timestamp>, D::Error> {
        let text = String::deserialize(deserializer)?;
        let Some(t) = parse_rfc3339(&text) else {
            return Err(serde::de::Error::custom("invalid RFC3339 timestamp"));
        };
        if t.is_zero() {
            return Ok(None);
        }
        Ok(Some(t))
    }
}

/// Go `[]byte` JSON coding for `PushEvent.payload` inside `queue.json`:
/// `base64.StdEncoding` (standard alphabet, padded — *not* the URL-safe
/// `RAW_URL` codec the wire uses). `json.Unmarshal` decodes `null` and
/// `""` to an empty slice, so both map to `Vec::new()` here; serialize
/// emits the padded base64 string (`""` when empty — `Vec` cannot
/// distinguish Go's `nil` from `[]byte{}`, and payloads are never
/// empty in practice).
mod go_bytes_b64 {
    use serde::{Deserialize, Deserializer, Serializer};

    use super::*;

    pub fn serialize<S: Serializer>(value: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(value))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let Some(text) = Option::<String>::deserialize(deserializer)? else {
            return Ok(Vec::new());
        };
        base64::engine::general_purpose::STANDARD
            .decode(text)
            .map_err(serde::de::Error::custom)
    }
}

/// `days_from_civil` (Howard Hinnant's civil calendar algorithms) —
/// days since the Unix epoch for a proleptic-Gregorian date.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (m as i64 + 9) % 12; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

fn ascii_digits(bytes: &[u8], start: usize, len: usize) -> Option<i64> {
    if start + len > bytes.len() {
        return None;
    }
    let mut value: i64 = 0;
    for &b in &bytes[start..start + len] {
        if !b.is_ascii_digit() {
            return None;
        }
        value = value * 10 + (b - b'0') as i64;
    }
    Some(value)
}

/// `time.Parse(time.RFC3339, s)` — `YYYY-MM-DDTHH:MM:SS[.frac](Z|±HH:MM)`.
/// Accepts the case-insensitive `t`/`z` spellings Go's RFC3339 parser
/// allows; requires seconds; converts offsets to UTC.
fn parse_rfc3339(text: &str) -> Option<Timestamp> {
    let b = text.as_bytes();
    if b.len() < 20 {
        return None;
    }
    let year = ascii_digits(b, 0, 4)?;
    if b[4] != b'-' || b[7] != b'-' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let month = ascii_digits(b, 5, 2)? as u32;
    let day = ascii_digits(b, 8, 2)? as u32;
    if b[10] != b'T' && b[10] != b't' {
        return None;
    }
    let hour = ascii_digits(b, 11, 2)? as u32;
    let minute = ascii_digits(b, 14, 2)? as u32;
    let second = ascii_digits(b, 17, 2)? as u32;
    let mut index = 19usize;
    let mut nanos: u32 = 0;
    if index < b.len() && (b[index] == b'.' || b[index] == b',') {
        index += 1;
        let start = index;
        let mut scale = 100_000_000u32;
        while index < b.len() && b[index].is_ascii_digit() {
            if scale > 0 {
                nanos += (b[index] - b'0') as u32 * scale;
                scale /= 10;
            }
            index += 1;
        }
        if index == start {
            return None;
        }
    }
    let offset_secs: i64 = match b.get(index) {
        Some(b'Z') | Some(b'z') => {
            index += 1;
            0
        }
        Some(b'+') | Some(b'-') => {
            let sign = if b[index] == b'-' { -1i64 } else { 1 };
            let oh = ascii_digits(b, index + 1, 2)? as u32;
            if index + 3 >= b.len() || b[index + 3] != b':' {
                return None;
            }
            let om = ascii_digits(b, index + 4, 2)? as u32;
            index += 6;
            sign * ((oh as i64) * 3600 + (om as i64) * 60)
        }
        _ => return None,
    };
    if index != b.len() {
        return None;
    }
    if !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let secs = days_from_civil(year, month, day)
        .checked_mul(86_400)?
        .checked_add((hour * 3600 + minute * 60 + second) as i64)?
        .checked_sub(offset_secs)?;
    Some(Timestamp { secs, nanos })
}

// ---------------------------------------------------------------------------
// Oracle types — `PushEventKey`, `DevicePolicy`, `Subscription`, the
// signed `ReferenceClaims`, and the Web Push `Payload` shape.
// ---------------------------------------------------------------------------

/// `push.PushEventKey` — field order is load-bearing: Go marshals
/// struct fields in declaration order and `notificationTag` /
/// `deliveryID` hash those exact bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct PushEventKey {
    #[serde(default, deserialize_with = "de_default")]
    pub device_id: String,
    #[serde(default, deserialize_with = "de_default")]
    pub server_session_id: String,
    #[serde(default, deserialize_with = "de_default")]
    pub pane_id: String,
    #[serde(default, deserialize_with = "de_default")]
    pub terminal_id: String,
    #[serde(default, deserialize_with = "de_default")]
    pub agent_session_id: String,
    #[serde(default, deserialize_with = "de_default")]
    pub generation: i64,
    #[serde(default, deserialize_with = "de_default")]
    pub event_id: String,
    #[serde(default, deserialize_with = "de_default")]
    pub interaction_revision: i64,
    #[serde(default, deserialize_with = "de_default")]
    pub category: String,
}

impl PushEventKey {
    /// `PushEventKey.Validate` — `push_invalid_event_key` /
    /// `push_invalid_category`.
    fn validate(&self) -> Result<(), &'static str> {
        if !valid_push_identifier(&self.device_id, true)
            || !valid_push_identifier(&self.event_id, true)
            || !valid_push_target_identifier(&self.server_session_id)
            || !valid_push_target_identifier(&self.pane_id)
            || !valid_push_target_identifier(&self.terminal_id)
            || !valid_push_target_identifier(&self.agent_session_id)
        {
            return Err("push_invalid_event_key");
        }
        match self.category.as_str() {
            CATEGORY_ATTENTION | CATEGORY_QUESTION | CATEGORY_BRIEF | CATEGORY_FINISHED => {
                if self.server_session_id.is_empty()
                    || self.pane_id.is_empty()
                    || self.terminal_id.is_empty()
                    || self.generation < 0
                {
                    return Err("push_invalid_event_key");
                }
            }
            CATEGORY_UPDATE | CATEGORY_TEST => {}
            _ => return Err("push_invalid_category"),
        }
        Ok(())
    }

    /// `key.Target()` — the pane the notification deep-links into.
    pub(crate) fn target(&self) -> TargetRef {
        TargetRef {
            server_session_id: self.server_session_id.clone(),
            pane_id: self.pane_id.clone(),
            terminal_id: self.terminal_id.clone(),
            generation: self.generation,
            agent_session_id: self.agent_session_id.clone(),
        }
    }
}

/// `validPushIdentifier` — ≤256 bytes, valid (Rust `str` is always
/// UTF-8), no leading/trailing whitespace; `required` rejects empty.
fn valid_push_identifier(value: &str, required: bool) -> bool {
    if value.is_empty() {
        return !required;
    }
    value.len() <= MAX_IDENTIFIER_BYTES && value.trim() == value
}

/// `validPushTargetIdentifier` — opaque target ids carry no length cap
/// (the oracle binds only its own identifiers).
fn valid_push_target_identifier(value: &str) -> bool {
    value.is_empty() || value.trim() == value
}

/// `push.DevicePolicy` — `settle`/`cooldown` are `time.Duration`
/// (nanoseconds); `snooze_until`/`snoozed`/`update_versions` match the
/// Go `omitempty`/`time.Time` file encoding exactly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct DevicePolicy {
    #[serde(default, deserialize_with = "de_default")]
    device_id: String,
    #[serde(default, deserialize_with = "de_default")]
    locale: String,
    /// Go `map[Category]bool` — `null`/absent (None) differs from `{}`
    /// (Some(empty)): None gets the default set at `SetPolicy` time.
    #[serde(default)]
    categories: Option<BTreeMap<String, bool>>,
    #[serde(default, deserialize_with = "de_default")]
    settle: i64,
    #[serde(default, deserialize_with = "de_default")]
    cooldown: i64,
    #[serde(default, with = "go_zero_time")]
    snooze_until: Option<Timestamp>,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "std::ops::Not::not"
    )]
    snoozed: bool,
    #[serde(default, deserialize_with = "de_default")]
    update_once: bool,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    update_versions: BTreeMap<String, bool>,
}

fn default_categories() -> BTreeMap<String, bool> {
    BTreeMap::from([
        (CATEGORY_ATTENTION.to_owned(), true),
        (CATEGORY_QUESTION.to_owned(), true),
        (CATEGORY_BRIEF.to_owned(), true),
        (CATEGORY_FINISHED.to_owned(), false),
        (CATEGORY_UPDATE.to_owned(), true),
        (CATEGORY_TEST.to_owned(), true),
    ])
}

/// `DefaultDevicePolicy` — the policy a device gets before it ever
/// changes one: finished events opt-out, everything else on, 2s
/// settle, 30s cooldown, once-per-version update notifications.
fn default_device_policy(device_id: &str, locale: &str) -> DevicePolicy {
    DevicePolicy {
        device_id: device_id.to_owned(),
        locale: normalize_locale(locale).to_owned(),
        categories: Some(default_categories()),
        settle: 2 * NS_PER_SEC,
        cooldown: 30 * NS_PER_SEC,
        snooze_until: None,
        snoozed: false,
        update_once: true,
        update_versions: BTreeMap::new(),
    }
}

/// `localize.NormalizeLocale` — `zh-cn*` -> `zh-CN`, `en*` -> `en`,
/// everything else falls back to English.
fn normalize_locale(value: &str) -> &'static str {
    let tag = value.trim().replace('_', "-").to_lowercase();
    if tag == "zh-cn" || tag.starts_with("zh-cn-") {
        "zh-CN"
    } else {
        "en"
    }
}

/// `normalizePolicy` — the `SetPolicy` gate.
fn normalize_policy(mut policy: DevicePolicy) -> Result<DevicePolicy, &'static str> {
    if policy.device_id.trim().is_empty() {
        return Err("push_device_required");
    }
    policy.locale = normalize_locale(&policy.locale).to_owned();
    if policy.settle < 0 || policy.cooldown < 0 {
        return Err("push_invalid_duration");
    }
    if policy.categories.is_none() {
        policy.categories = Some(default_categories());
    }
    if let Some(categories) = &policy.categories {
        for category in categories.keys() {
            if !ALLOWED_CATEGORIES.contains(&category.as_str()) {
                return Err("push_invalid_category");
            }
        }
    }
    Ok(policy)
}

/// `pushPolicyWire` — the client-editable policy shape. `categories`
/// stays `Option` so `null`/absent maps to Go's nil-map (defaults at
/// normalize time) rather than `{}` (all disabled).
#[derive(Debug, Default, Deserialize)]
struct PolicyWire {
    #[serde(default)]
    categories: Option<BTreeMap<String, bool>>,
    #[serde(default, deserialize_with = "de_default")]
    settle_ms: i64,
    #[serde(default, deserialize_with = "de_default")]
    cooldown_ms: i64,
    #[serde(default, deserialize_with = "de_default")]
    snooze_until: String,
    #[serde(default, deserialize_with = "de_default")]
    snoozed: bool,
    #[serde(default, deserialize_with = "de_default")]
    update_once: bool,
}

/// `boundPushPolicy` — bind the wire patch onto the device's current
/// policy. The error codes are the oracle's internal distinctions —
/// the oracle's response frame collapses them to `push_invalid_policy`;
/// this port surfaces the specific code (task requirement).
fn bound_push_policy(
    raw: Option<&RawJson>,
    device_id: &str,
    locale: &str,
    mut current: DevicePolicy,
) -> Result<DevicePolicy, &'static str> {
    let Some(raw) = raw else {
        return Err("push_invalid_policy");
    };
    // `json.Unmarshal` on `"null"` is a no-op: `Option<PolicyWire>`
    // mirrors that (`null` -> zero wire -> still a valid patch).
    let wire: PolicyWire = serde_json::from_str::<Option<PolicyWire>>(raw.get())
        .map_err(|_| "push_invalid_policy")?
        .unwrap_or_default();
    if wire.settle_ms < 0 || wire.cooldown_ms < 0 {
        return Err("push_invalid_duration");
    }
    current.device_id = device_id.to_owned();
    current.locale = locale.to_owned();
    current.categories = wire.categories;
    // Go `time.Duration(ms) * time.Millisecond` wraps on int64
    // overflow; a wrapped-negative duration is rejected by
    // normalizePolicy as `push_invalid_duration` — replicate with
    // `wrapping_mul` + the `< 0` check downstream.
    current.settle = wire.settle_ms.wrapping_mul(NS_PER_MS);
    current.cooldown = wire.cooldown_ms.wrapping_mul(NS_PER_MS);
    current.snoozed = wire.snoozed;
    current.update_once = wire.update_once;
    current.snooze_until = None;
    if !wire.snooze_until.is_empty() {
        current.snooze_until =
            Some(parse_rfc3339(&wire.snooze_until).ok_or("push_invalid_snooze")?);
    }
    Ok(current)
}

/// `pushPolicyResponse` — the `push_policy`/`push_policy_result`
/// payload. `snooze_until` rides along only when nonzero (RFC3339
/// seconds precision), matching the oracle's `omitempty`-by-hand.
fn policy_response(policy: &DevicePolicy) -> serde_json::Value {
    let categories: serde_json::Map<String, serde_json::Value> = policy
        .categories
        .as_ref()
        .map(|c| {
            c.iter()
                .map(|(k, v)| (k.clone(), serde_json::Value::Bool(*v)))
                .collect()
        })
        .unwrap_or_default();
    let mut result = serde_json::json!({
        "categories": serde_json::Value::Object(categories),
        "cooldown_ms": policy.cooldown / NS_PER_MS,
        "device_id": policy.device_id,
        "locale": policy.locale,
        "settle_ms": policy.settle / NS_PER_MS,
        "snoozed": policy.snoozed,
        "update_once": policy.update_once,
    });
    if let Some(until) = policy.snooze_until {
        // `!SnoozeUntil.IsZero()` — a zero time carries no wire value.
        if !until.is_zero() {
            result["snooze_until"] = serde_json::Value::String(until.to_rfc3339());
        }
    }
    result
}

/// `push.Subscription` — the wire parse shape; the handler overwrites
/// every trust-bearing field after unmarshal like the oracle.
/// `pub(crate)` fields are the delivery worker's read surface (endpoint
/// + keys drive `sendOne`; `device_id`/`endpoint` key the terminal set).
///
/// `Serialize` mirrors the Go tags for the `queue.json` embedding:
/// `endpoint`/`keys` always emit, the rest are `omitempty`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Subscription {
    #[serde(default, deserialize_with = "de_default")]
    pub(crate) endpoint: String,
    #[serde(default, deserialize_with = "de_default")]
    pub(crate) keys: SubscriptionKeys,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "str::is_empty"
    )]
    pub(crate) device_id: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "str::is_empty"
    )]
    pub(crate) locale: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "str::is_empty"
    )]
    pub(crate) platform: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "str::is_empty"
    )]
    pub(crate) user_agent: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub(crate) notify_finished: bool,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "str::is_empty"
    )]
    pub(crate) client_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct SubscriptionKeys {
    #[serde(default, deserialize_with = "de_default")]
    pub(crate) p256dh: String,
    #[serde(default, deserialize_with = "de_default")]
    pub(crate) auth: String,
}

/// `pythonFile`/`pythonSubscription` — the on-disk subscriptions
/// envelope the oracle reads and writes (`subscriptions.json`).
#[derive(Deserialize)]
struct SubscriptionsFile {
    #[serde(default)]
    subscriptions: Option<Vec<FileSubscription>>,
}

#[derive(Deserialize)]
struct FileSubscription {
    #[serde(default, deserialize_with = "de_default")]
    subscription: FileSubscriptionInner,
    #[serde(default, deserialize_with = "de_default")]
    device_id: String,
    #[serde(default, deserialize_with = "de_default")]
    locale: String,
    #[serde(default, deserialize_with = "de_default")]
    platform: String,
    #[serde(default, deserialize_with = "de_default")]
    client_id: String,
    #[serde(default, deserialize_with = "de_default")]
    user_agent: String,
    #[serde(default, deserialize_with = "de_default")]
    notify_finished: bool,
}

#[derive(Debug, Default, Deserialize)]
struct FileSubscriptionInner {
    #[serde(default, deserialize_with = "de_default")]
    endpoint: String,
    #[serde(default, deserialize_with = "de_default")]
    keys: SubscriptionKeys,
}

/// `validPushEndpoint` — the allowlist of Web Push service hosts.
/// HTTPS only, no userinfo, no fragment, no non-443 port.
fn valid_push_endpoint(raw: &str) -> bool {
    let Ok(endpoint) = url::Url::parse(raw) else {
        return false;
    };
    if endpoint.scheme() != "https" {
        return false;
    }
    // Go `endpoint.User != nil`: any userinfo (even empty) rejects —
    // the url crate normalizes "https://@host" to an empty username,
    // so scan the authority for '@' too.
    if !endpoint.username().is_empty() || endpoint.password().is_some() {
        return false;
    }
    let authority = &raw[endpoint.scheme().len() + 3..];
    let authority = &authority[..authority.find('/').unwrap_or(authority.len())];
    if authority.contains('@') {
        return false;
    }
    let Some(host) = endpoint.host_str() else {
        return false;
    };
    if host.is_empty() {
        return false;
    }
    if endpoint.fragment().is_some_and(|f| !f.is_empty()) {
        return false;
    }
    if endpoint.port().is_some_and(|p| p != 443) {
        return false;
    }
    let host = host.trim_end_matches('.').to_lowercase();
    match host.as_str() {
        "fcm.googleapis.com"
        | "android.googleapis.com"
        | "updates.push.services.mozilla.com"
        | "push.services.mozilla.com"
        | "web.push.apple.com" => true,
        _ => host == "notify.windows.com" || host.ends_with(".notify.windows.com"),
    }
}

/// `push.PushEvent` — one queued notification; serializes into
/// `queue.json` with the Go field names (`payload` is the `[]byte`
/// `base64.StdEncoding` string, `null`/`""` decode empty; `retract`
/// omits when false).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct PushEvent {
    #[serde(default, deserialize_with = "de_default")]
    pub(crate) key: PushEventKey,
    #[serde(default, with = "go_bytes_b64")]
    pub(crate) payload: Vec<u8>,
    #[serde(default, deserialize_with = "de_default")]
    pub(crate) created_at: Timestamp,
    #[serde(default, deserialize_with = "de_default")]
    pub(crate) expires_at: Timestamp,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub(crate) retract: bool,
}

impl PushEvent {
    /// `PushEvent.Validate` — `push_invalid_event` /
    /// `push_event_expired`.
    fn validate(&self, now: Timestamp) -> Result<(), &'static str> {
        self.key.validate()?;
        if self.payload.is_empty()
            || self.payload.len() > MAX_PUSH_PAYLOAD_BYTES
            || self.created_at.is_zero()
            || self.expires_at.is_zero()
            || self.expires_at <= self.created_at
        {
            return Err("push_invalid_event");
        }
        if now >= self.expires_at {
            return Err("push_event_expired");
        }
        Ok(())
    }
}

/// `queueEntry` — a pending delivery and the `queue.json` `entries`
/// row (all five Go fields always emit). `PartialEq` is the worker's
/// `sameQueueEntry` guard: a snapshot raced by `resolve`/`subscribe`
/// no longer matches and finish/reschedule declines.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct QueueEntry {
    #[serde(default, deserialize_with = "de_default")]
    pub(crate) id: String,
    #[serde(default, deserialize_with = "de_default")]
    pub(crate) event: PushEvent,
    #[serde(default, deserialize_with = "de_default")]
    pub(crate) subscription: Subscription,
    #[serde(default, deserialize_with = "de_default")]
    pub(crate) due_at: Timestamp,
    #[serde(default, deserialize_with = "de_default")]
    pub(crate) attempts: u32,
}

/// `deliveredRecord` — an accepted delivery kept for retraction; the
/// `queue.json` `delivered` row (`key`/`subscription`/`tag`/
/// `accepted_at` all emit — no `omitempty` in the oracle).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct DeliveredRecord {
    #[serde(default, deserialize_with = "de_default")]
    pub(crate) key: PushEventKey,
    #[serde(default, deserialize_with = "de_default")]
    pub(crate) subscription: Subscription,
    #[serde(default, deserialize_with = "de_default")]
    pub(crate) tag: String,
    #[serde(default, deserialize_with = "de_default")]
    pub(crate) accepted_at: Timestamp,
}

/// `deliveryID` — sha256 of the canonical `{"key":…,"endpoint":…}`.
fn delivery_id(key: &PushEventKey, endpoint: &str) -> String {
    #[derive(Serialize)]
    struct DeliveryKey<'a> {
        key: &'a PushEventKey,
        endpoint: &'a str,
    }
    let data = lerdr_core::json::to_vec(&DeliveryKey { key, endpoint }).unwrap_or_default();
    hex::encode(Sha256::digest(&data))
}

/// `notificationTag` — `herdr-` + first 16 sha256 bytes of the key.
fn notification_tag(key: &PushEventKey) -> String {
    let data = lerdr_core::json::to_vec(key).unwrap_or_default();
    let digest = Sha256::digest(&data);
    format!("herdr-{}", hex::encode(&digest[..16]))
}

/// `push.PublishRequest` — `created_at`/`expires_at` map the Go zero
/// times onto `Option`.
#[derive(Debug)]
pub(crate) struct PublishRequest {
    pub key: PushEventKey,
    pub preview: &'static str,
    pub created_at: Option<Timestamp>,
    pub expires_at: Option<Timestamp>,
}

/// `push.PublishResult`.
#[derive(Debug, Default)]
pub(crate) struct PublishResult {
    pub queued: usize,
    pub suppressed: usize,
}

/// `push.PolicyDecision` — `Deliver` + `DueAt`/`Code`.
#[derive(Debug)]
struct PolicyDecision {
    deliver: bool,
    due_at: Timestamp,
    #[allow(dead_code)] // logged/inspected by the delivery worker port
    code: &'static str,
}

/// `policySlot` — the cooldown slot key.
fn policy_slot(key: &PushEventKey) -> String {
    format!(
        "{}\x00{}\x00{}\x00{}\x00{}\x00{}\x00{}",
        key.device_id,
        key.server_session_id,
        key.pane_id,
        key.terminal_id,
        key.generation,
        key.agent_session_id,
        key.category
    )
}

// ---------------------------------------------------------------------------
// ReferenceSigner — HMAC-SHA256 over the canonical claims JSON,
// `action_ref.key` (32 raw bytes) when persisted.
// ---------------------------------------------------------------------------

/// `push.ReferenceClaims` — `{key, expires_at}`; field order matches
/// Go's marshal output (the signed bytes).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReferenceClaims {
    #[serde(default, deserialize_with = "de_default")]
    key: PushEventKey,
    #[serde(default, deserialize_with = "de_default")]
    expires_at: Timestamp,
}

/// `ErrInvalidReference` / `ErrExpiredReference` / `ErrStaleReference`
/// — folded to one enum; `code()` keeps the wire vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefError {
    Invalid,
    Expired,
    Stale,
}

impl RefError {
    fn code(&self) -> &'static str {
        match self {
            RefError::Invalid => "push_invalid_reference",
            RefError::Expired => "push_expired_reference",
            RefError::Stale => "push_stale_reference",
        }
    }
}

/// `push.ReferenceSigner` — sign/verify `payload.signature` tokens.
#[derive(Debug, Clone)]
struct ReferenceSigner {
    key: [u8; REFERENCE_KEY_SIZE],
}

impl ReferenceSigner {
    fn ephemeral() -> Self {
        let mut key = [0u8; REFERENCE_KEY_SIZE];
        use rand::TryRngCore;
        rand::rngs::OsRng
            .try_fill_bytes(&mut key)
            .expect("OS RNG failure is unrecoverable");
        ReferenceSigner { key }
    }

    /// `loadOrCreateReferenceSigner` — 32 raw bytes, `0600`, atomic
    /// create; a short/long key fails the manager like the oracle.
    fn load_or_create(path: &Path) -> io::Result<Self> {
        match std::fs::read(path) {
            Ok(key) => {
                if key.len() != REFERENCE_KEY_SIZE {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "push_invalid_reference_key",
                    ));
                }
                set_private_permissions(path)?;
                let mut fixed = [0u8; REFERENCE_KEY_SIZE];
                fixed.copy_from_slice(&key);
                Ok(ReferenceSigner { key: fixed })
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let signer = ReferenceSigner::ephemeral();
                atomic_write(path, &signer.key, 0o600)?;
                Ok(signer)
            }
            Err(e) => Err(e),
        }
    }

    /// `signer.Sign` — `b64url(claims) + "." + b64url(hmac)`.
    fn sign(&self, claims: &ReferenceClaims) -> Result<String, &'static str> {
        if claims.key.validate().is_err() || claims.expires_at.is_zero() {
            return Err("push_invalid_reference");
        }
        let data = lerdr_core::json::to_vec(claims).map_err(|_| "push_invalid_reference")?;
        let mac = hmac_sha256(&self.key, &data);
        Ok(format!("{}.{}", RAW_URL.encode(&data), RAW_URL.encode(mac)))
    }

    /// `signer.Verify` — format, signature, decode, key validity,
    /// expiry — in the oracle's order.
    fn verify(&self, token: &str, now: Timestamp) -> Result<ReferenceClaims, RefError> {
        let mut parts = token.split('.');
        let (Some(payload_b64), Some(sig_b64), None) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(RefError::Invalid);
        };
        let data = RAW_URL.decode(payload_b64).map_err(|_| RefError::Invalid)?;
        let signature = RAW_URL.decode(sig_b64).map_err(|_| RefError::Invalid)?;
        let expected = hmac_sha256(&self.key, &data);
        if !constant_time_eq(&signature, &expected) {
            return Err(RefError::Invalid);
        }
        let claims: ReferenceClaims =
            serde_json::from_slice(&data).map_err(|_| RefError::Invalid)?;
        if claims.key.validate().is_err() {
            return Err(RefError::Invalid);
        }
        if now >= claims.expires_at {
            return Err(RefError::Expired);
        }
        Ok(claims)
    }
}

/// `hmac.Equal` — fixed-length constant-time compare.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// RFC 2104 HMAC over SHA-256 (64-byte block).
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut block_key = [0u8; BLOCK];
    if key.len() > BLOCK {
        block_key[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block_key[..key.len()].copy_from_slice(key);
    }
    let mut hasher = Sha256::new();
    for byte in &block_key {
        hasher.update([byte ^ 0x36]);
    }
    hasher.update(message);
    let inner = hasher.finalize();
    let mut hasher = Sha256::new();
    for byte in &block_key {
        hasher.update([byte ^ 0x5c]);
    }
    hasher.update(inner);
    hasher.finalize().into()
}

// ---------------------------------------------------------------------------
// Payload — `push.BuildPayload`: signed deep-link reference + localized
// title/body; the bytes a Web Push service would receive.
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct NotificationAction<'a> {
    action: &'a str,
    title: &'a str,
}

/// `push.Payload` — Go field order; `actions`/`action_refs` always
/// emit (`[]`/`{}` today — the oracle's action buttons are unused).
#[derive(Serialize)]
struct Payload<'a> {
    v: u32,
    category: &'a str,
    key: &'a PushEventKey,
    title: &'a str,
    body: &'a str,
    tag: String,
    url: String,
    actions: Vec<NotificationAction<'a>>,
    action_refs: BTreeMap<String, String>,
    event_ref: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    retract: bool,
}

/// `BuildPayload` — validate the key, sign the reference, localize,
/// pick the URL (test/update land on `#settings`, retractions on `./`).
fn build_payload(
    signer: &ReferenceSigner,
    key: &PushEventKey,
    locale: &str,
    preview: &str,
    expires_at: Option<Timestamp>,
    retract: bool,
) -> Result<Vec<u8>, &'static str> {
    key.validate()?;
    let Some(expires_at) = expires_at else {
        return Err("push_expiry_required");
    };
    let event_ref = signer.sign(&ReferenceClaims {
        key: key.clone(),
        expires_at,
    })?;
    let (title, body) = localized_payload_text(locale, &key.category, preview, retract)?;
    let mut url = format!("./#push={event_ref}");
    if key.category == CATEGORY_UPDATE || key.category == CATEGORY_TEST {
        url = "./#settings".to_owned();
    }
    let mut payload = Payload {
        v: 1,
        category: &key.category,
        key,
        title,
        body,
        tag: notification_tag(key),
        url,
        actions: Vec::new(),
        action_refs: BTreeMap::new(),
        event_ref,
        retract,
    };
    if retract {
        payload.url = "./".to_owned();
        payload.event_ref = String::new();
    }
    lerdr_core::json::to_vec(&payload).map_err(|_| "push_invalid_event")
}

/// `localizedPayloadText` — retracts carry no text; hidden/question/
/// brief are the only supported preview modes.
fn localized_payload_text(
    locale: &str,
    category: &str,
    preview: &str,
    retract: bool,
) -> Result<(&'static str, &'static str), &'static str> {
    if retract {
        return Ok(("", ""));
    }
    let zh = normalize_locale(locale) == "zh-CN";
    match preview {
        PREVIEW_HIDDEN => {
            if zh {
                Ok(("Lerdr 通知", "打开应用查看详情"))
            } else {
                Ok(("Lerdr notification", "Open the app to view details"))
            }
        }
        PREVIEW_QUESTION => {
            if zh {
                Ok(("需要回复", "打开应用查看问题并回复"))
            } else {
                Ok(("Response needed", "Open the app to review and respond"))
            }
        }
        PREVIEW_BRIEF => {
            if category == CATEGORY_FINISHED {
                if zh {
                    Ok(("任务已完成", "打开应用查看已完成的任务"))
                } else {
                    Ok(("Agent finished", "Open the app to view the completed task"))
                }
            } else if zh {
                Ok(("简报已就绪", "打开应用查看简报"))
            } else {
                Ok(("Brief ready", "Open the app to view the brief"))
            }
        }
        _ => Err("push_invalid_preview"),
    }
}

// ---------------------------------------------------------------------------
// Push — the shared subsystem handle (oracle's `push.Manager` +
// `PolicyEngine` + the server's `pushTestLast` ledger).
// ---------------------------------------------------------------------------

struct State {
    /// `None` — fully in-memory; `Some(dir)` — `subscriptions.json`,
    /// `policies.json`, `action_ref.key` persist under `dir`.
    dir: Option<PathBuf>,
    subscriptions: Vec<Subscription>,
    policies: BTreeMap<String, DevicePolicy>,
    global_snooze_until: Option<Timestamp>,
    last_accepted: BTreeMap<String, Timestamp>,
    viewed_panes: HashMap<String, TargetRef>,
    /// `pushTestLast` — in-memory 10s test-notification rate limit.
    test_last: HashMap<String, Instant>,
    signer: ReferenceSigner,
    /// `durableQueue.state` — pending entries + delivered records,
    /// persisted to `queue.json` (`BTreeMap` keeps Go's sorted-key
    /// `MarshalIndent` deterministic). `queue_dirty` is the pass's
    /// `dirty` flag: drain-time mutations mark it and `flush_queue`
    /// writes once per pass, like the oracle's `finish`.
    entries: BTreeMap<String, QueueEntry>,
    delivered: BTreeMap<String, DeliveredRecord>,
    queue_dirty: bool,
    /// The manager's `active`/`retracting` key sets — in-memory like
    /// the oracle (`NewManager` rebuilds `active` from the loaded
    /// queue's `activeKeys`).
    active: HashSet<PushEventKey>,
    retracting: HashSet<PushEventKey>,
    reconciled: bool,
}

impl State {
    fn in_memory() -> Self {
        State {
            dir: None,
            subscriptions: Vec::new(),
            policies: BTreeMap::new(),
            global_snooze_until: None,
            last_accepted: BTreeMap::new(),
            viewed_panes: HashMap::new(),
            test_last: HashMap::new(),
            signer: ReferenceSigner::ephemeral(),
            entries: BTreeMap::new(),
            delivered: BTreeMap::new(),
            queue_dirty: false,
            active: HashSet::new(),
            retracting: HashSet::new(),
            reconciled: true,
        }
    }
}

/// Shared push state — one per relay (the oracle's push registry:
/// policy, subscriptions, snooze, viewed-pane ledger, reference
/// signer, delivery queue bookkeeping). `wake` is `m.wake` — the
/// buffered (single-permit) signal that kicks the delivery worker.
#[derive(Clone)]
pub(crate) struct Push {
    inner: Arc<Mutex<State>>,
    wake: Arc<Notify>,
}

impl Default for Push {
    /// In-memory push state — `Push::new(dir)` is the persistent form
    /// (wired once the router passes the runtime directory).
    fn default() -> Self {
        Push {
            inner: Arc::new(Mutex::new(State::in_memory())),
            wake: Arc::new(Notify::new()),
        }
    }
}

impl Push {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.inner.lock().expect("push poisoned")
    }

    /// `push.NewManager(pushDir)` — the handler-facing subset: creates
    /// the dir (`0700`), loads `subscriptions.json` + `queue.json` +
    /// `policies.json`, loads or creates `action_ref.key`. Recovered
    /// queue keys seed `active` and gate delivery behind `reconcile`
    /// like the oracle (`m.reconciled = len(recovered) == 0`) — an
    /// entry due in the past is eligible the moment reconciliation
    /// opens the gate. The VAPID key pair is loaded or generated by
    /// `super::push_delivery::spawn_push_worker` at worker start (the
    /// oracle's `loadOrGenerateVAPIDKeys`).
    pub(crate) fn new(dir: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        set_dir_permissions(dir)?;
        let mut state = State::in_memory();
        state.dir = Some(dir.to_path_buf());
        state.subscriptions = load_subscriptions(&dir.join("subscriptions.json"))?;
        let queue = super::push_queue::load_queue(dir)?;
        state.entries = queue.entries;
        state.delivered = queue.delivered;
        let file = load_policies(&dir.join("policies.json"))?;
        state.policies = file.policies;
        state.global_snooze_until = file.global_snooze_until;
        state.last_accepted = file.last_accepted;
        state.signer = ReferenceSigner::load_or_create(&dir.join("action_ref.key"))?;
        // `recovered := m.queue.activeKeys()` — queue keys reactivate so
        // `reconcile` can retract the stale half; `reconciled` opens
        // only when nothing survived the restart.
        for entry in state.entries.values() {
            state.active.insert(entry.event.key.clone());
        }
        for record in state.delivered.values() {
            state.active.insert(record.key.clone());
        }
        state.reconciled = state.active.is_empty();
        Ok(Push {
            inner: Arc::new(Mutex::new(state)),
            wake: Arc::new(Notify::new()),
        })
    }

    /// The `pushDir` this state persists under — the delivery worker
    /// loads `vapid_private.pem`/`vapid_public.pem` from it. `None`
    /// (in-memory `Push::default`) means an ephemeral VAPID pair.
    #[allow(dead_code)] // consumed by push_delivery::spawn_push_worker
    pub(crate) fn push_dir(&self) -> Option<PathBuf> {
        self.lock().dir.clone()
    }

    /// `Manager.Subscriptions` — a copy of the registry.
    #[allow(dead_code)] // read by tests; the delivery worker port uses it
    pub(crate) fn subscriptions(&self) -> Vec<Subscription> {
        self.lock().subscriptions.clone()
    }

    /// `Manager.Policy` — stored policy or the default, with an
    /// expired timed snooze cleared on the returned copy (the stored
    /// preference survives, matching the oracle).
    fn policy(&self, device_id: &str, locale: &str) -> DevicePolicy {
        let state = self.lock();
        let mut policy = state
            .policies
            .get(device_id)
            .cloned()
            .unwrap_or_else(|| default_device_policy(device_id, locale));
        // `snoozed && !until.IsZero() && !until.After(now)` — a stored
        // zero time means indefinite snooze, not an expired one.
        if policy.snoozed
            && policy
                .snooze_until
                .is_some_and(|until| !until.is_zero() && Timestamp::now() >= until)
        {
            policy.snoozed = false;
            policy.snooze_until = None;
        }
        policy
    }

    /// `PolicyEngine.Set` — normalize, store, persist, roll back on
    /// persist failure.
    fn set_policy(&self, policy: DevicePolicy) -> Result<(), &'static str> {
        let policy = normalize_policy(policy)?;
        let mut state = self.lock();
        let previous = state
            .policies
            .insert(policy.device_id.clone(), policy.clone());
        if let Err(code) = persist_policies(&state) {
            match previous {
                Some(p) => {
                    state.policies.insert(policy.device_id.clone(), p);
                }
                None => {
                    state.policies.remove(&policy.device_id);
                }
            }
            return Err(code);
        }
        Ok(())
    }

    /// `PolicyEngine.SetViewedPane` — `None` clears the marker.
    pub(crate) fn set_viewed_pane(&self, device_id: &str, target: Option<TargetRef>) {
        let mut state = self.lock();
        match target {
            Some(target) => {
                state.viewed_panes.insert(device_id.to_owned(), target);
            }
            None => {
                state.viewed_panes.remove(device_id);
            }
        }
    }

    /// `reservePushTest` — 10s per device; stale entries drop on the
    /// way past so months of devices don't accumulate timestamps.
    fn reserve_test(&self, device_id: &str, now: Instant) -> bool {
        let mut state = self.lock();
        state
            .test_last
            .retain(|_, last| now.duration_since(*last) < PUSH_TEST_INTERVAL);
        if state
            .test_last
            .get(device_id)
            .is_some_and(|last| now.duration_since(*last) < PUSH_TEST_INTERVAL)
        {
            return false;
        }
        state.test_last.insert(device_id.to_owned(), now);
        true
    }

    /// `Manager.Subscribe` — keys required, endpoint allowlist, same-
    /// endpoint device-mismatch rejection, replace-set pruning, and
    /// the queued-delivery migration.
    pub(crate) fn subscribe(
        &self,
        sub: Subscription,
        replace_endpoints: &[String],
    ) -> Result<(), &'static str> {
        if sub.keys.p256dh.is_empty() || sub.keys.auth.is_empty() {
            return Err("push_invalid_subscription");
        }
        if !valid_push_endpoint(&sub.endpoint) {
            return Err("push_subscription_endpoint_not_allowed");
        }
        let mut state = self.lock();
        let replace: HashSet<&str> = replace_endpoints
            .iter()
            .filter(|e| !e.is_empty())
            .map(String::as_str)
            .collect();
        let mut filtered: Vec<Subscription> = state
            .subscriptions
            .iter()
            .filter(|existing| {
                !(replace.contains(existing.endpoint.as_str())
                    && existing.endpoint != sub.endpoint
                    && existing.device_id == sub.device_id)
            })
            .cloned()
            .collect();
        if let Some(index) = filtered.iter().position(|s| s.endpoint == sub.endpoint) {
            if filtered[index].device_id != sub.device_id {
                return Err("push_subscription_device_mismatch");
            }
            filtered[index] = sub.clone();
            persist_subscriptions(&state.dir, &filtered)?;
            // Re-registering refreshes delivered records too — the
            // replacement set includes the new endpoint itself.
            let mut replacements = replace_endpoints.to_vec();
            replacements.push(sub.endpoint.clone());
            if let Err(code) =
                replace_subscriptions_locked(&mut state, &sub.device_id, &replacements, &sub)
            {
                // The oracle re-writes the pre-change registry on a
                // queue persist failure (`_ = m.persist(m.subscriptions)`).
                let _ = persist_subscriptions(&state.dir, &state.subscriptions);
                return Err(code);
            }
            state.subscriptions = filtered;
            return Ok(());
        }
        filtered.push(sub.clone());
        persist_subscriptions(&state.dir, &filtered)?;
        if let Err(code) =
            replace_subscriptions_locked(&mut state, &sub.device_id, replace_endpoints, &sub)
        {
            let _ = persist_subscriptions(&state.dir, &state.subscriptions);
            return Err(code);
        }
        state.subscriptions = filtered;
        Ok(())
    }

    /// `Manager.UnsubscribeDevice` — match by device + (endpoint or
    /// client id), persist, then migrate the queue bookkeeping.
    fn unsubscribe_device(
        &self,
        device_id: &str,
        endpoints: &[String],
        client_id: &str,
    ) -> Result<(), &'static str> {
        if device_id.trim().is_empty() {
            return Err("push_device_required");
        }
        let mut state = self.lock();
        let remove: HashSet<&str> = endpoints
            .iter()
            .filter(|e| !e.is_empty())
            .map(String::as_str)
            .collect();
        let mut filtered = Vec::with_capacity(state.subscriptions.len());
        let mut removed_endpoints = Vec::new();
        for sub in &state.subscriptions {
            let matched = sub.device_id == device_id
                && (remove.contains(sub.endpoint.as_str())
                    || (!client_id.is_empty() && sub.client_id == client_id));
            if matched {
                removed_endpoints.push(sub.endpoint.clone());
            } else {
                filtered.push(sub.clone());
            }
        }
        persist_subscriptions(&state.dir, &filtered)?;
        state.subscriptions = filtered;
        if !removed_endpoints.is_empty() {
            // `m.queue.removeSubscriptions` — the registry is already
            // committed; a queue persist failure propagates like the
            // oracle (no registry restore).
            remove_subscriptions_locked(&mut state, device_id, &removed_endpoints)?;
        }
        Ok(())
    }

    /// `Manager.RemoveDevice` — credential revocation cleanup:
    /// subscriptions, queued + delivered entries, active/retracting
    /// keys, policy, cooldown slots, viewed marker.
    #[allow(dead_code)] // called once credential revocation is ported
    pub(crate) fn remove_device(&self, device_id: &str) -> Result<(), &'static str> {
        if device_id.trim().is_empty() {
            return Err("push_device_required");
        }
        let mut state = self.lock();
        let filtered: Vec<Subscription> = state
            .subscriptions
            .iter()
            .filter(|s| s.device_id != device_id)
            .cloned()
            .collect();
        persist_subscriptions(&state.dir, &filtered)?;
        state.subscriptions = filtered;
        state.active.retain(|k| k.device_id != device_id);
        state.retracting.retain(|k| k.device_id != device_id);
        // `m.queue.removeDevice` — drop the device's pending +
        // delivered rows, persist, restore the rows on failure.
        let previous_entries = state.entries.clone();
        let previous_delivered = state.delivered.clone();
        state
            .entries
            .retain(|_, e| e.subscription.device_id != device_id);
        state
            .delivered
            .retain(|_, r| r.subscription.device_id != device_id);
        if let Err(code) = persist_queue_locked(&mut state) {
            state.entries = previous_entries;
            state.delivered = previous_delivered;
            return Err(code);
        }
        state.viewed_panes.remove(device_id);
        state.policies.remove(device_id);
        let prefix = format!("{device_id}\x00");
        state
            .last_accepted
            .retain(|slot, _| !slot.starts_with(&prefix));
        persist_policies(&state)
    }

    /// `Manager.Publish` — last-subscription-per-device selection,
    /// policy gate, payload build, queue insertion, `active` mark.
    /// Called by `push_test_device` today; the notification producer
    /// port calls it for real events.
    pub(crate) fn publish(&self, request: PublishRequest) -> Result<PublishResult, &'static str> {
        let now = request.created_at.unwrap_or_else(Timestamp::now);
        let expires_at = request
            .expires_at
            .unwrap_or_else(|| now.add_ns(PUBLISH_DEFAULT_TTL_NS));
        let mut state = self.lock();
        let mut selected = Vec::new();
        let mut seen = HashSet::new();
        for subscription in state.subscriptions.iter().rev() {
            if subscription.device_id.is_empty()
                || seen.contains(subscription.device_id.as_str())
                || (!request.key.device_id.is_empty()
                    && request.key.device_id != subscription.device_id)
            {
                continue;
            }
            seen.insert(subscription.device_id.clone());
            selected.push(subscription.clone());
        }
        let mut result = PublishResult::default();
        for subscription in selected {
            let mut key = request.key.clone();
            key.device_id = subscription.device_id.clone();
            let decision = decide_locked(&state, &key, &subscription.locale, now);
            if !decision.deliver {
                result.suppressed += 1;
                continue;
            }
            let payload = build_payload(
                &state.signer,
                &key,
                &subscription.locale,
                request.preview,
                Some(expires_at),
                false,
            )?;
            let event = PushEvent {
                key: key.clone(),
                payload,
                created_at: now,
                expires_at,
                retract: false,
            };
            event.validate(now)?;
            enqueue_locked(&mut state, event, subscription, decision.due_at)?;
            state.active.insert(key);
            result.queued += 1;
        }
        drop(state);
        // `m.signal()` — kick the delivery worker only when something
        // actually queued (the oracle gates the wake on Queued > 0).
        if result.queued > 0 {
            self.wake.notify_one();
        }
        Ok(result)
    }

    /// `Manager.Resolve` — cancel pending entries, retract delivered
    /// ones, drop the key from `active` (open-ref goes stale).
    #[allow(dead_code)] // called by the notification producer port
    pub(crate) fn resolve(&self, key: &PushEventKey) -> Result<(), &'static str> {
        key.validate()?;
        let mut state = self.lock();
        // `cancelKey` — drop pending entries for the key, persist, and
        // put them back when the write fails.
        let mut removed: BTreeMap<String, QueueEntry> = BTreeMap::new();
        state.entries.retain(|id, e| {
            if e.event.key == *key {
                removed.insert(id.clone(), e.clone());
                return false;
            }
            true
        });
        if !removed.is_empty() {
            if let Err(code) = persist_queue_locked(&mut state) {
                state.entries.append(&mut removed);
                return Err(code);
            }
        }
        let records: Vec<DeliveredRecord> = state
            .delivered
            .values()
            .filter(|r| r.key == *key)
            .cloned()
            .collect();
        state.active.remove(key);
        if records.is_empty() {
            // `forgetDelivered` — the persist is unconditional like
            // the oracle's; no rollback there either.
            state.delivered.retain(|_, r| r.key != *key);
            persist_queue_locked(&mut state)?;
            return Ok(());
        }
        state.retracting.insert(key.clone());
        let now = Timestamp::now();
        let expires_at = now.add_ns(PUBLISH_DEFAULT_TTL_NS);
        for record in records {
            let payload = build_payload(
                &state.signer,
                key,
                &record.subscription.locale,
                PREVIEW_HIDDEN,
                Some(expires_at),
                true,
            )?;
            let event = PushEvent {
                key: key.clone(),
                payload,
                created_at: now,
                expires_at,
                retract: true,
            };
            enqueue_locked(&mut state, event, record.subscription.clone(), now)?;
        }
        drop(state);
        self.wake.notify_one(); // `m.signal()` after queueing retractions
        Ok(())
    }

    /// `Manager.ResolvePaneID` — resolve every active key on a pane
    /// except `except_event_id`.
    #[allow(dead_code)]
    pub(crate) fn resolve_pane_id(
        &self,
        pane_id: &str,
        except_event_id: &str,
    ) -> Result<(), &'static str> {
        let keys: Vec<PushEventKey> = self
            .lock()
            .active
            .iter()
            .filter(|k| k.pane_id == pane_id && k.event_id != except_event_id)
            .cloned()
            .collect();
        for key in keys {
            self.resolve(&key)?;
        }
        Ok(())
    }

    /// `Manager.RecoveredKeys` — the queue's `activeKeys` (entries ∪
    /// delivered keys, `notificationTag`-sorted like the oracle); the
    /// notification producer's reconcile call walks it.
    #[allow(dead_code)]
    pub(crate) fn recovered_keys(&self) -> Vec<PushEventKey> {
        let state = self.lock();
        let mut seen = HashSet::new();
        for entry in state.entries.values() {
            seen.insert(entry.event.key.clone());
        }
        for record in state.delivered.values() {
            seen.insert(record.key.clone());
        }
        let mut keys: Vec<PushEventKey> = seen.into_iter().collect();
        keys.sort_by_cached_key(notification_tag);
        keys
    }

    /// `Manager.Reconcile` — recovered keys absent from the first
    /// authoritative inventory are retracted, then `reconciled` opens
    /// the delivery loop and reference verification.
    #[allow(dead_code)]
    pub(crate) fn reconcile(&self, current: &[PushEventKey]) -> Result<(), &'static str> {
        let current_set: HashSet<PushEventKey> = current.iter().cloned().collect();
        for key in &current_set {
            key.validate().map_err(|_| "push_invalid_event_key")?;
        }
        let stale: Vec<PushEventKey> = {
            let mut state = self.lock();
            // The oracle re-closes the gate while retractions queue.
            state.reconciled = false;
            state
                .active
                .iter()
                .filter(|k| {
                    k.category != CATEGORY_UPDATE
                        && k.category != CATEGORY_TEST
                        && !current_set.contains(*k)
                })
                .cloned()
                .collect()
        };
        for key in stale {
            self.resolve(&key)?;
        }
        self.lock().reconciled = true;
        self.wake.notify_one(); // `m.signal()` — delivery opens once reconciled
        Ok(())
    }

    /// `Manager.VerifyEventReference` — signed claims + the key is
    /// still live (`reconciled && active`), else `push_stale_reference`.
    fn verify_event_reference(
        &self,
        token: &str,
        now: Timestamp,
    ) -> Result<ReferenceClaims, RefError> {
        let state = self.lock();
        let claims = state.signer.verify(token, now)?;
        if !(state.reconciled && state.active.contains(&claims.key)) {
            return Err(RefError::Stale);
        }
        Ok(claims)
    }

    /// `signer.Sign` exposed for the notification producer port (and
    /// tests): mint the `event_ref` a payload URL carries.
    #[allow(dead_code)]
    pub(crate) fn sign_event_reference(
        &self,
        key: &PushEventKey,
        expires_at: Timestamp,
    ) -> Result<String, &'static str> {
        self.lock().signer.sign(&ReferenceClaims {
            key: key.clone(),
            expires_at,
        })
    }

    /// `PolicyEngine.MarkAccepted` — cooldown bookkeeping for the
    /// delivery worker (accepted sends start the cooldown window).
    #[allow(dead_code)]
    pub(crate) fn mark_accepted(
        &self,
        key: &PushEventKey,
        accepted_at: Timestamp,
    ) -> Result<(), &'static str> {
        let mut state = self.lock();
        let slot = policy_slot(key);
        let previous = state.last_accepted.insert(slot.clone(), accepted_at);
        if let Err(code) = persist_policies(&state) {
            match previous {
                Some(t) => {
                    state.last_accepted.insert(slot, t);
                }
                None => {
                    state.last_accepted.remove(&slot);
                }
            }
            return Err(code);
        }
        Ok(())
    }

    /// `PolicyEngine.ClaimUpdateVersion` — once-per-version update
    /// notification dedup.
    #[allow(dead_code)]
    pub(crate) fn claim_update_version(
        &self,
        device_id: &str,
        version: &str,
    ) -> Result<bool, &'static str> {
        if device_id.trim().is_empty() || version.trim().is_empty() {
            return Err("push_update_version_required");
        }
        let mut state = self.lock();
        let mut policy = state
            .policies
            .get(device_id)
            .cloned()
            .unwrap_or_else(|| default_device_policy(device_id, "en"));
        if !policy.update_once {
            return Ok(true);
        }
        if policy
            .update_versions
            .get(version)
            .copied()
            .unwrap_or(false)
        {
            return Ok(false);
        }
        policy.update_versions.insert(version.to_owned(), true);
        state.policies.insert(device_id.to_owned(), policy.clone());
        if let Err(code) = persist_policies(&state) {
            policy.update_versions.remove(version);
            state.policies.insert(device_id.to_owned(), policy);
            return Err(code);
        }
        Ok(true)
    }

    /// `PolicyEngine.SetGlobalSnooze` — relay-wide snooze override.
    #[allow(dead_code)]
    pub(crate) fn set_global_snooze(&self, until: Option<Timestamp>) -> Result<(), &'static str> {
        let mut state = self.lock();
        let previous = state.global_snooze_until;
        state.global_snooze_until = until;
        if let Err(code) = persist_policies(&state) {
            state.global_snooze_until = previous;
            return Err(code);
        }
        Ok(())
    }

    /// `PolicyEngine.Decide` — the per-event delivery gate (the
    /// delivery worker port inspects `code`; tests assert on it).
    #[allow(dead_code)]
    fn decide(&self, key: &PushEventKey, locale: &str, now: Timestamp) -> PolicyDecision {
        decide_locked(&self.lock(), key, locale, now)
    }

    // -----------------------------------------------------------------------
    // Delivery-worker seams — the `Manager.RunOnce`/`durableQueue.processDue`
    // surface `push_delivery` drives. Snapshot-then-apply mirrors the oracle:
    // `dueEntries` copies under `mu`, sends run unlocked, and every mutation
    // re-checks the snapshot (`sameQueueEntry`) so a raced `resolve`/
    // `subscribe`/`unsubscribe` never gets clobbered.
    // -----------------------------------------------------------------------

    /// `m.wake` — the single-permit notify `publish`/`resolve`/
    /// `reconcile`/`recover_pruned` raise and the worker sleeps on
    /// beside its 250ms tick.
    pub(crate) fn delivery_wake(&self) -> Arc<Notify> {
        self.wake.clone()
    }

    /// `Manager.reconciled` gate — `RunOnce` is a no-op until the
    /// producer supplies the first authoritative inventory.
    pub(crate) fn is_reconciled(&self) -> bool {
        self.lock().reconciled
    }

    /// `durableQueue.dueEntries` — every entry with `due_at <= now`,
    /// sorted `(due_at, id)` like the oracle's `sort.Slice`.
    pub(crate) fn due_entries(&self, now: Timestamp) -> Vec<QueueEntry> {
        let state = self.lock();
        let mut entries: Vec<QueueEntry> = state
            .entries
            .values()
            .filter(|e| e.due_at <= now)
            .cloned()
            .collect();
        entries.sort_by(|a, b| a.due_at.cmp(&b.due_at).then_with(|| a.id.cmp(&b.id)));
        entries
    }

    /// `RunOnce`'s `current` callback — a retracting key always
    /// delivers; anything else must be `active` and still pass the
    /// live policy gate (`PolicyEngine.Allows`, locale `"en"`).
    pub(crate) fn entry_current(&self, key: &PushEventKey, now: Timestamp) -> bool {
        let state = self.lock();
        if state.retracting.contains(key) {
            return true;
        }
        state.active.contains(key) && allows_locked(&state, key, "en", now)
    }

    /// `durableQueue.finishInMemory` — remove the entry if the queue
    /// still holds this exact snapshot; `Accepted` non-retract events
    /// move into `delivered` (the retraction ledger).
    pub(crate) fn finish_entry(
        &self,
        snapshot: &QueueEntry,
        disposition: Disposition,
        now: Timestamp,
    ) -> bool {
        let mut state = self.lock();
        if state.entries.get(&snapshot.id) != Some(snapshot) {
            return false;
        }
        state.entries.remove(&snapshot.id);
        if disposition == Disposition::Accepted && !snapshot.event.retract {
            state.delivered.insert(
                snapshot.id.clone(),
                DeliveredRecord {
                    key: snapshot.event.key.clone(),
                    subscription: snapshot.subscription.clone(),
                    tag: notification_tag(&snapshot.event.key),
                    accepted_at: now,
                },
            );
        }
        // `dirty = q.finishInMemory(...) || dirty` — the pass's
        // `flush()` persists it.
        state.queue_dirty = true;
        true
    }

    /// `durableQueue.rescheduleInMemory` — bump `attempts`/`due_at` on
    /// an unchanged snapshot; the pass's `flush()` persists it.
    pub(crate) fn reschedule_entry(
        &self,
        snapshot: &QueueEntry,
        attempts: u32,
        next: Timestamp,
    ) -> bool {
        let mut state = self.lock();
        match state.entries.get_mut(&snapshot.id) {
            Some(current) if *current == *snapshot => {
                current.attempts = attempts;
                current.due_at = next;
                state.queue_dirty = true;
                true
            }
            _ => false,
        }
    }

    /// `RunOnce`'s `accepted` callback — `PolicyEngine.MarkAccepted`
    /// starts the cooldown slot unless the key is mid-retraction.
    pub(crate) fn note_accepted(
        &self,
        key: &PushEventKey,
        accepted_at: Timestamp,
    ) -> Result<(), &'static str> {
        if self.lock().retracting.contains(key) {
            return Ok(());
        }
        self.mark_accepted(key, accepted_at)
    }

    /// `queue.hasEntriesFor` — `finish`'s sweep predicate; also the
    /// worker-test seam for "still queued".
    #[allow(dead_code)] // exercised by the push_delivery drain tests
    pub(crate) fn has_entries_for(&self, key: &PushEventKey) -> bool {
        self.lock().entries.values().any(|e| e.event.key == *key)
    }

    /// `Manager.recoverPrunedSubscriptionsWhileProcessing` — for each
    /// device whose deliveries hit terminal endpoints: pick the
    /// fallback subscription (latest registered that isn't pruned),
    /// persist the pruned registry, then move those endpoints' pending
    /// entries + the just-failed events onto the fallback and drop
    /// their delivered records. Persist failure restores the pruned
    /// entries with backoff (`restorePrunedWhileProcessing`) and
    /// reports the error, exactly like `finish` in the oracle.
    pub(crate) fn recover_pruned(
        &self,
        results: &[DeliveryResult],
        now: Timestamp,
    ) -> Result<(), &'static str> {
        // Group pruned results by device (BTreeMap = the oracle's
        // sorted deviceID iteration).
        let mut pruned: BTreeMap<String, (BTreeSet<String>, Vec<&DeliveryResult>)> =
            BTreeMap::new();
        for result in results {
            if result.disposition != Disposition::Pruned {
                continue;
            }
            let device = pruned
                .entry(result.subscription.device_id.clone())
                .or_default();
            device.0.insert(result.subscription.endpoint.clone());
            device.1.push(result);
        }
        if pruned.is_empty() {
            return Ok(());
        }

        let mut state = self.lock();
        // `recoveries` — fallback selection happens before the
        // registry write, against the pre-prune subscription list
        // (the oracle scans `m.subscriptions` back-to-front under mu).
        let mut recoveries = Vec::with_capacity(pruned.len());
        let mut requeued = false;
        for (device_id, (endpoints, device_results)) in &pruned {
            let fallback = state
                .subscriptions
                .iter()
                .rev()
                .find(|s| s.device_id == *device_id && !endpoints.contains(&s.endpoint))
                .cloned();
            requeued = requeued || (fallback.is_some() && !device_results.is_empty());
            recoveries.push((device_id, endpoints, fallback, device_results));
        }

        let filtered: Vec<Subscription> = state
            .subscriptions
            .iter()
            .filter(|s| match pruned.get(&s.device_id) {
                Some((endpoints, _)) => !endpoints.contains(&s.endpoint),
                None => true,
            })
            .cloned()
            .collect();
        if let Err(code) = persist_subscriptions(&state.dir, &filtered) {
            // `restorePrunedWhileProcessing` — put the pruned entries
            // back with backoff so nothing is lost on persist failure.
            restore_pruned_locked(&mut state, results, now);
            return Err(code);
        }

        // `queue.recoverPrunedWhileProcessing` — pending entries and
        // delivered records on pruned endpoints go away; with a
        // fallback the entries migrate to it and the just-failed
        // events requeue (fresh `DueAt=now`, `Attempts=0`).
        for (device_id, endpoints, fallback, device_results) in &recoveries {
            let migrated: Vec<QueueEntry> = state
                .entries
                .values()
                .filter(|e| {
                    e.subscription.device_id == **device_id
                        && endpoints.contains(&e.subscription.endpoint)
                })
                .cloned()
                .collect();
            for mut entry in migrated {
                state.entries.remove(&entry.id);
                // `dirty = true` on the delete like the oracle —
                // `finish`'s `flush()` persists the recovery.
                state.queue_dirty = true;
                if let Some(fallback) = fallback {
                    entry.subscription = fallback.clone();
                    entry.id = delivery_id(&entry.event.key, &fallback.endpoint);
                    state.entries.entry(entry.id.clone()).or_insert(entry);
                }
            }
            let delivered_ids: Vec<String> = state
                .delivered
                .iter()
                .filter(|(_, r)| {
                    r.subscription.device_id == **device_id
                        && endpoints.contains(&r.subscription.endpoint)
                })
                .map(|(id, _)| id.clone())
                .collect();
            for id in delivered_ids {
                state.delivered.remove(&id);
                state.queue_dirty = true;
            }
            let Some(fallback) = fallback else {
                continue;
            };
            for result in device_results.iter() {
                let id = delivery_id(&result.event.key, &fallback.endpoint);
                if state.entries.contains_key(&id) {
                    continue;
                }
                state.entries.insert(
                    id.clone(),
                    QueueEntry {
                        id,
                        event: result.event.clone(),
                        subscription: fallback.clone(),
                        due_at: now,
                        attempts: 0,
                    },
                );
                state.queue_dirty = true;
            }
        }
        state.subscriptions = filtered;
        if requeued {
            self.wake.notify_one();
        }
        Ok(())
    }

    /// `RunOnce`'s epilogue — for every key that saw results this
    /// pass: finished retractions clear `retracting` + delivered
    /// records; terminal test/update keys forget delivered state;
    /// keys with nothing left queued (and no delivered ledger for real
    /// categories) leave `active`. The `forgetDelivered` halves persist
    /// once before the in-memory set mutations — the oracle's per-key
    /// ordering (a failed write leaves `retracting`/`active` intact).
    pub(crate) fn sweep_keys(&self, results: &[DeliveryResult]) -> Result<(), &'static str> {
        let mut state = self.lock();
        let mut seen = HashSet::new();
        for result in results {
            seen.insert(result.key.clone());
        }
        let mut dirty = false;
        for key in &seen {
            let has_entries = state.entries.values().any(|e| e.event.key == *key);
            let forget = (state.retracting.contains(key) && !has_entries)
                || (!has_entries
                    && (key.category == CATEGORY_TEST || key.category == CATEGORY_UPDATE));
            if forget {
                let before = state.delivered.len();
                state.delivered.retain(|_, r| r.key != *key);
                dirty = dirty || state.delivered.len() != before;
            }
        }
        if dirty {
            persist_queue_locked(&mut state)?;
        }
        for key in seen {
            let has_entries = state.entries.values().any(|e| e.event.key == key);
            if state.retracting.contains(&key) && !has_entries {
                state.retracting.remove(&key);
                continue;
            }
            if has_entries {
                continue;
            }
            if key.category == CATEGORY_TEST
                || key.category == CATEGORY_UPDATE
                || !state.delivered.values().any(|r| r.key == key)
            {
                state.active.remove(&key);
            }
        }
        Ok(())
    }

    /// `finish`'s `flush()` — persist `queue.json` iff the pass dirtied
    /// the queue; a clean pass writes nothing.
    pub(crate) fn flush_queue(&self) -> Result<(), &'static str> {
        let mut state = self.lock();
        if !state.queue_dirty {
            return Ok(());
        }
        persist_queue_locked(&mut state)
    }
}

/// `PolicyEngine.Decide` over locked state.
fn decide_locked(
    state: &State,
    key: &PushEventKey,
    locale: &str,
    now: Timestamp,
) -> PolicyDecision {
    let policy = state
        .policies
        .get(&key.device_id)
        .cloned()
        .unwrap_or_else(|| default_device_policy(&key.device_id, locale));
    let enabled = policy
        .categories
        .as_ref()
        .map(|c| c.get(key.category.as_str()).copied().unwrap_or(false))
        .unwrap_or(false);
    if !enabled {
        return PolicyDecision {
            deliver: false,
            due_at: now,
            code: "push_category_disabled",
        };
    }
    // Indefinite: `Snoozed && SnoozeUntil.IsZero()`; timed:
    // `SnoozeUntil.After(now)`; global: `GlobalSnoozeUntil.After(now)`.
    let timed_snooze = policy
        .snooze_until
        .is_some_and(|until| !until.is_zero() && now < until);
    if (policy.snoozed && policy.snooze_until.is_none_or(|until| until.is_zero()))
        || timed_snooze
        || state.global_snooze_until.is_some_and(|until| now < until)
    {
        return PolicyDecision {
            deliver: false,
            due_at: now,
            code: "push_snoozed",
        };
    }
    if state
        .viewed_panes
        .get(&key.device_id)
        .is_some_and(|viewed| *viewed == key.target())
    {
        return PolicyDecision {
            deliver: false,
            due_at: now,
            code: "push_viewed_pane",
        };
    }
    let mut due = now.add_ns(policy.settle);
    if let Some(last) = state.last_accepted.get(&policy_slot(key)) {
        let cooldown_end = last.add_ns(policy.cooldown);
        if cooldown_end > due {
            due = cooldown_end;
        }
    }
    PolicyDecision {
        deliver: true,
        due_at: due,
        code: "",
    }
}

// ---------------------------------------------------------------------------
// Delivery results — `DeliveryDisposition`/`DeliveryResult`, the worker
// pass vocabulary `recover_pruned`/`sweep_keys` consume.
// ---------------------------------------------------------------------------

/// `maxDeliveryAttempts` — a delivery retires after 6 send attempts.
pub(crate) const MAX_DELIVERY_ATTEMPTS: u32 = 6;

/// `push.DeliveryDisposition`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Disposition {
    /// `service_accepted` — 2xx from the push service.
    Accepted,
    /// `queued_for_retry` — retryable failure, rescheduled.
    Retrying,
    /// `dropped` — terminal-per-message failure or attempts/TTL spent.
    Dropped,
    /// `subscription_pruned` — the endpoint is permanently gone (or a
    /// sibling entry already proved it this pass).
    Pruned,
    /// `expired` — `now` reached `expires_at` before delivery.
    Expired,
    /// `stale` — the event is no longer `current` (resolved, or the
    /// live policy now suppresses it).
    Stale,
}

/// `push.DeliveryResult` — one processed queue entry. `event` +
/// `subscription` are the oracle's `json:"-"` fields: only `Pruned`
/// results need them (`recover_pruned` re-queues against a fallback).
#[derive(Debug, Clone)]
pub(crate) struct DeliveryResult {
    pub(crate) key: PushEventKey,
    pub(crate) endpoint: String,
    pub(crate) disposition: Disposition,
    pub(crate) attempts: u32,
    /// `json:"next_attempt"` — kept for parity/reporting; the worker
    /// tests assert the backoff schedule off it.
    #[allow(dead_code)]
    pub(crate) next_attempt: Option<Timestamp>,
    pub(crate) event: PushEvent,
    pub(crate) subscription: Subscription,
}

/// `retryDelay` — `1s << (attempts-1)` capped at one minute, in
/// nanoseconds for `Timestamp::add_ns`.
pub(crate) fn retry_delay_ns(attempts: u32) -> i64 {
    let attempts = attempts.max(1);
    // `time.Second << (attempts-1)`; attempts is bounded by
    // MAX_DELIVERY_ATTEMPTS but saturate anyway — any overshoot lands
    // on the one-minute cap.
    NS_PER_SEC
        .checked_shl(attempts - 1)
        .unwrap_or(i64::MAX)
        .min(60 * NS_PER_SEC)
}

/// `PolicyEngine.Allows` over locked state — the `current` predicate's
/// policy half: category enabled, no snooze, pane not being viewed.
fn allows_locked(state: &State, key: &PushEventKey, locale: &str, now: Timestamp) -> bool {
    let policy = state
        .policies
        .get(&key.device_id)
        .cloned()
        .unwrap_or_else(|| default_device_policy(&key.device_id, locale));
    let enabled = policy
        .categories
        .as_ref()
        .map(|c| c.get(key.category.as_str()).copied().unwrap_or(false))
        .unwrap_or(false);
    if !enabled {
        return false;
    }
    let timed_snooze = policy
        .snooze_until
        .is_some_and(|until| !until.is_zero() && now < until);
    if (policy.snoozed && policy.snooze_until.is_none_or(|until| until.is_zero()))
        || timed_snooze
        || state.global_snooze_until.is_some_and(|until| now < until)
    {
        return false;
    }
    if state
        .viewed_panes
        .get(&key.device_id)
        .is_some_and(|viewed| *viewed == key.target())
    {
        return false;
    }
    true
}

/// `queue.restorePrunedWhileProcessing` — on registry-persist failure,
/// requeue each pruned result's entry with `retryDelay` backoff
/// (capped below `expires_at`) when the delivery id isn't already
/// back in the queue.
fn restore_pruned_locked(state: &mut State, results: &[DeliveryResult], now: Timestamp) {
    for result in results {
        if result.disposition != Disposition::Pruned {
            continue;
        }
        let id = delivery_id(&result.event.key, &result.subscription.endpoint);
        if state.entries.contains_key(&id) {
            continue;
        }
        let mut due_at = now.add_ns(retry_delay_ns(result.attempts));
        if due_at >= result.event.expires_at {
            due_at = now;
        }
        state.entries.insert(
            id.clone(),
            QueueEntry {
                id,
                event: result.event.clone(),
                subscription: result.subscription.clone(),
                due_at,
                attempts: result.attempts,
            },
        );
        state.queue_dirty = true;
    }
}

/// `queue.enqueue` — device binding + replace-by-delivery-id, then the
/// oracle's immediate `persistLocked` with rollback on write failure.
fn enqueue_locked(
    state: &mut State,
    event: PushEvent,
    subscription: Subscription,
    due_at: Timestamp,
) -> Result<String, &'static str> {
    if subscription.device_id.is_empty() || subscription.device_id != event.key.device_id {
        return Err("push_subscription_device_mismatch");
    }
    let id = delivery_id(&event.key, &subscription.endpoint);
    let previous = state.entries.insert(
        id.clone(),
        QueueEntry {
            id: id.clone(),
            event,
            subscription,
            due_at,
            attempts: 0,
        },
    );
    if let Err(code) = persist_queue_locked(state) {
        match previous {
            Some(entry) => {
                state.entries.insert(id.clone(), entry);
            }
            None => {
                state.entries.remove(&id);
            }
        }
        return Err(code);
    }
    Ok(id)
}

/// `queue.replaceSubscriptionsWhileProcessing` — migrate pending
/// entries to the replacement subscription; prune matching delivered
/// records (same-endpoint records refresh instead); persist with the
/// oracle's both-maps rollback on write failure.
fn replace_subscriptions_locked(
    state: &mut State,
    device_id: &str,
    endpoints: &[String],
    replacement: &Subscription,
) -> Result<(), &'static str> {
    let replace: HashSet<&str> = endpoints
        .iter()
        .filter(|e| !e.is_empty())
        .map(String::as_str)
        .collect();
    if replace.is_empty() {
        return Ok(());
    }
    let previous_entries = state.entries.clone();
    let previous_delivered = state.delivered.clone();
    let old_entries: Vec<(String, QueueEntry)> = state
        .entries
        .iter()
        .filter(|(_, e)| {
            e.subscription.device_id == device_id
                && replace.contains(e.subscription.endpoint.as_str())
        })
        .map(|(id, e)| (id.clone(), e.clone()))
        .collect();
    for (old_id, mut entry) in old_entries {
        state.entries.remove(&old_id);
        entry.subscription = replacement.clone();
        entry.id = delivery_id(&entry.event.key, &replacement.endpoint);
        state.entries.entry(entry.id.clone()).or_insert(entry);
    }
    let delivered_ids: Vec<String> = state
        .delivered
        .iter()
        .filter_map(|(id, r)| {
            if r.subscription.device_id == device_id
                && replace.contains(r.subscription.endpoint.as_str())
            {
                if r.subscription.endpoint == replacement.endpoint {
                    return None;
                }
                Some(id.clone())
            } else {
                None
            }
        })
        .collect();
    for id in delivered_ids {
        state.delivered.remove(&id);
    }
    for record in state.delivered.values_mut() {
        if record.subscription.device_id == device_id
            && record.subscription.endpoint == replacement.endpoint
            && replace.contains(replacement.endpoint.as_str())
        {
            record.subscription = replacement.clone();
        }
    }
    if let Err(code) = persist_queue_locked(state) {
        state.entries = previous_entries;
        state.delivered = previous_delivered;
        return Err(code);
    }
    Ok(())
}

/// `queue.removeSubscriptions` — drop pending + delivered state for
/// removed endpoints; persist with the both-maps rollback.
fn remove_subscriptions_locked(
    state: &mut State,
    device_id: &str,
    endpoints: &[String],
) -> Result<(), &'static str> {
    let remove: HashSet<&str> = endpoints
        .iter()
        .filter(|e| !e.is_empty())
        .map(String::as_str)
        .collect();
    if remove.is_empty() {
        return Ok(());
    }
    let previous_entries = state.entries.clone();
    let previous_delivered = state.delivered.clone();
    state.entries.retain(|_, e| {
        !(e.subscription.device_id == device_id
            && remove.contains(e.subscription.endpoint.as_str()))
    });
    state.delivered.retain(|_, r| {
        !(r.subscription.device_id == device_id
            && remove.contains(r.subscription.endpoint.as_str()))
    });
    if let Err(code) = persist_queue_locked(state) {
        state.entries = previous_entries;
        state.delivered = previous_delivered;
        return Err(code);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Persistence — `subscriptions.json` (pythonFile wrapper),
// `policies.json` (policyFile), `queue.json` (`durableQueue` — the
// envelope and load/salvage/quarantine live in `super::push_queue`),
// `action_ref.key`.
// ---------------------------------------------------------------------------

fn load_subscriptions(path: &Path) -> io::Result<Vec<Subscription>> {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    // Python wrapper format first: {"subscriptions":[…]} — `!= nil`
    // semantics: a decoded wrapper with a present-but-empty list wins.
    if let Ok(file) = serde_json::from_slice::<SubscriptionsFile>(&data) {
        if let Some(list) = file.subscriptions {
            let subs: Vec<Subscription> = list
                .into_iter()
                .map(|f| Subscription {
                    endpoint: f.subscription.endpoint,
                    keys: f.subscription.keys,
                    device_id: f.device_id,
                    locale: f.locale,
                    platform: f.platform,
                    user_agent: f.user_agent,
                    notify_finished: f.notify_finished,
                    client_id: f.client_id,
                })
                .filter(|s| valid_push_endpoint(&s.endpoint))
                .collect();
            return Ok(subs);
        }
    }
    // Legacy flat array fallback.
    let flat: Vec<Subscription> =
        serde_json::from_slice(&data).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(flat
        .into_iter()
        .filter(|s| valid_push_endpoint(&s.endpoint))
        .collect())
}

/// `policyFile` — `global_snooze_until` always emits (Go `time.Time`
/// `omitempty` is a no-op), `policies` always emits, `last_accepted`
/// omits when empty.
#[derive(Serialize)]
struct PoliciesFile<'a> {
    #[serde(with = "go_zero_time")]
    global_snooze_until: Option<Timestamp>,
    policies: &'a BTreeMap<String, DevicePolicy>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    last_accepted: &'a BTreeMap<String, Timestamp>,
}

#[derive(Default)]
struct LoadedPolicies {
    global_snooze_until: Option<Timestamp>,
    policies: BTreeMap<String, DevicePolicy>,
    last_accepted: BTreeMap<String, Timestamp>,
}

#[derive(Deserialize)]
struct PoliciesFileRead {
    #[serde(default, with = "go_zero_time")]
    global_snooze_until: Option<Timestamp>,
    #[serde(default, deserialize_with = "de_default")]
    policies: BTreeMap<String, DevicePolicy>,
    #[serde(default, deserialize_with = "de_default")]
    last_accepted: BTreeMap<String, Timestamp>,
}

fn load_policies(path: &Path) -> io::Result<LoadedPolicies> {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(LoadedPolicies::default()),
        Err(e) => return Err(e),
    };
    let mut file: PoliciesFileRead =
        serde_json::from_slice(&data).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    // `update_once` migration: policies persisted before the flag
    // existed get `true` (the oracle checks the raw bytes for the key).
    if let Ok(raw) = serde_json::from_slice::<serde_json::Value>(&data) {
        let raw_policies = raw.get("policies").and_then(|p| p.as_object());
        for (device_id, policy) in file.policies.iter_mut() {
            let has_flag = raw_policies
                .and_then(|p| p.get(device_id))
                .and_then(|p| p.as_object())
                .is_some_and(|o| o.contains_key("update_once"));
            if !has_flag {
                policy.update_once = true;
            }
        }
    }
    Ok(LoadedPolicies {
        global_snooze_until: file.global_snooze_until,
        policies: file.policies,
        last_accepted: file.last_accepted,
    })
}

fn persist_subscriptions(dir: &Option<PathBuf>, subs: &[Subscription]) -> Result<(), &'static str> {
    let Some(dir) = dir else {
        return Ok(());
    };
    #[derive(Serialize)]
    struct FileSub<'a> {
        subscription: FileSubInner<'a>,
        #[serde(skip_serializing_if = "str::is_empty")]
        device_id: &'a str,
        #[serde(skip_serializing_if = "str::is_empty")]
        locale: &'a str,
        #[serde(skip_serializing_if = "str::is_empty")]
        platform: &'a str,
        client_id: &'a str,
        user_agent: &'a str,
        notify_finished: bool,
    }
    #[derive(Serialize)]
    struct FileSubInner<'a> {
        endpoint: &'a str,
        keys: &'a SubscriptionKeys,
    }
    #[derive(Serialize)]
    struct File<'a> {
        subscriptions: Vec<FileSub<'a>>,
    }
    let subscriptions: Vec<FileSub> = subs
        .iter()
        .map(|s| FileSub {
            subscription: FileSubInner {
                endpoint: &s.endpoint,
                keys: &s.keys,
            },
            device_id: &s.device_id,
            locale: &s.locale,
            platform: &s.platform,
            client_id: &s.client_id,
            user_agent: &s.user_agent,
            notify_finished: s.notify_finished,
        })
        .collect();
    let file = File { subscriptions };
    let mut data = serde_json::to_string_pretty(&file).map_err(|_| "push_persist_failed")?;
    data.push('\n');
    atomic_write(&dir.join("subscriptions.json"), data.as_bytes(), 0o600)
        .map_err(|_| "push_persist_failed")
}

fn persist_policies(state: &State) -> Result<(), &'static str> {
    let Some(dir) = &state.dir else {
        return Ok(());
    };
    let file = PoliciesFile {
        global_snooze_until: state.global_snooze_until,
        policies: &state.policies,
        last_accepted: &state.last_accepted,
    };
    let mut data = serde_json::to_string_pretty(&file).map_err(|_| "push_persist_failed")?;
    data.push('\n');
    atomic_write(&dir.join("policies.json"), data.as_bytes(), 0o600)
        .map_err(|_| "push_persist_failed")
}

/// `durableQueue.persistLocked` — write `queue.json` under the push
/// dir (the envelope + `maxQueueEntries`/`maxQueueBytes` caps live in
/// `super::push_queue`); a no-op for in-memory state. A successful
/// write covers every pending mutation, so it clears `queue_dirty`.
fn persist_queue_locked(state: &mut State) -> Result<(), &'static str> {
    let Some(dir) = state.dir.clone() else {
        return Ok(());
    };
    super::push_queue::persist(&dir, &state.entries, &state.delivered)?;
    state.queue_dirty = false;
    Ok(())
}

/// `atomicWrite` — sibling temp file + rename, mode applied.
pub(crate) fn atomic_write(path: &Path, data: &[u8], mode: u32) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    set_mode(&tmp, mode)?;
    std::fs::rename(&tmp, path)?;
    set_mode(path, mode)?;
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_dir_permissions(path: &Path) -> io::Result<()> {
    set_mode(path, 0o700)
}

#[cfg(not(unix))]
fn set_dir_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

pub(crate) fn set_private_permissions(path: &Path) -> io::Result<()> {
    set_mode(path, 0o600)
}

// ---------------------------------------------------------------------------
// Handler helpers — caller identity, target currency, frame builders.
// ---------------------------------------------------------------------------

/// `pushTargetCurrent` — the claimed pane must still be the live
/// agent: primary session, ids present, nonnegative generation, and
/// the pane's current terminal/session/generation.
fn push_target_current(topology: &Topology, target: &TargetRef) -> bool {
    if target.server_session_id != "primary"
        || target.pane_id.is_empty()
        || target.terminal_id.is_empty()
        || target.generation < 0
    {
        return false;
    }
    let Some(agent) = topology
        .agents()
        .into_iter()
        .find(|a| a.pane_id == target.pane_id)
    else {
        return false;
    };
    agent.terminal_id == target.terminal_id
        && agent.agent_session_id == target.agent_session_id
        && agent.generation == target.generation
}

fn raw_json(value: &serde_json::Value) -> Option<MaybeNull<RawJson>> {
    serde_json::value::to_raw_value(value)
        .ok()
        .map(|raw| MaybeNull::Value(RawJson(raw)))
}

fn policy_frame(policy: &serde_json::Value) -> Outbound {
    Outbound::PushPolicy(PushPolicyMessage {
        policy: raw_json(policy),
        r#type: "push_policy".to_owned(),
    })
}

fn policy_result_frame(
    ok: bool,
    code: Option<&'static str>,
    policy: Option<&serde_json::Value>,
) -> Outbound {
    Outbound::PushPolicyResult(PushPolicyResultMessage {
        code: code.map(str::to_owned),
        ok: Some(ok),
        policy: policy.and_then(raw_json),
        r#type: "push_policy_result".to_owned(),
    })
}

fn ok_subscribed(ok: bool) -> Outbound {
    Outbound::PushSubscribed(OkMessage {
        ok: Some(ok),
        r#type: "push_subscribed".to_owned(),
    })
}

fn ok_unsubscribed(ok: bool) -> Outbound {
    Outbound::PushUnsubscribed(OkMessage {
        ok: Some(ok),
        r#type: "push_unsubscribed".to_owned(),
    })
}

fn viewed_pane_result() -> Outbound {
    Outbound::PushViewedPaneResult(OkMessage {
        ok: Some(true),
        r#type: "push_viewed_pane_result".to_owned(),
    })
}

fn test_result_frame(stage: &str) -> Outbound {
    Outbound::PushTestResult(PushTestResultMessage {
        stage: Some(stage.to_owned()),
        r#type: "push_test_result".to_owned(),
    })
}

/// The terminal `action_receipt` — `confirmed`, or
/// `failed_before_dispatch` carrying the failing push code.
fn receipt(request_id: &str, action_id: &str, code: Option<&'static str>) -> Outbound {
    Outbound::ActionReceipt(action_receipt_response(
        request_id,
        ActionReceipt {
            action_id: action_id.to_owned(),
            phase: ActionReceiptPhase::from(match code {
                Some(_) => ActionReceiptPhase::FAILED_BEFORE_DISPATCH,
                None => ActionReceiptPhase::CONFIRMED,
            }),
            error: code.map(|c| ApiError::new(c, BTreeMap::new())),
        },
    ))
}

/// `d.fail`-style `command_result` + the push frame spliced between it
/// and the terminal receipt (oracle order: command_result, push frame).
fn outcome_then(
    outcome: Outcome,
    extra: Outbound,
    request_id: &str,
    action: &str,
    action_id: &str,
) -> Vec<Outbound> {
    let mut frames = outcome.frames(request_id, action, action_id);
    frames.insert(1, extra);
    frames
}

/// The oracle's failed `command_result` for push command actions —
/// `phase:"failed"` + `failed_before_dispatch` receipt carrying the
/// push code rather than generic `invalid_request`.
fn failed_outcome(error: &str, code: &'static str) -> Outcome {
    Outcome {
        ok: false,
        phase: "failed",
        error: error.to_owned(),
        pane_id: String::new(),
        data: None,
        receipt_phase: ActionReceiptPhase::FAILED_BEFORE_DISPATCH,
        receipt_error: Some(ApiError::new(code, BTreeMap::new())),
    }
}

// ---------------------------------------------------------------------------
// The eight actions.
// ---------------------------------------------------------------------------

/// `push_policy_get` — answers `{"type":"push_policy","policy":…}`.
pub(crate) async fn policy_get(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    let device = ctx.device_id.clone();
    let policy = ctx.push.policy(&device, "en");
    vec![
        policy_frame(&policy_response(&policy)),
        receipt(request_id, action_id, None),
    ]
}

/// `push_policy_set` — `command_result` then `push_policy_result`;
/// rejects carry the specific code (`push_invalid_policy` /
/// `push_invalid_duration` / `push_invalid_snooze` /
/// `push_invalid_category`).
pub(crate) async fn policy_set(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    let device = ctx.device_id.clone();
    let current = ctx.push.policy(&device, "en");
    let applied = bound_push_policy(message.policy.as_ref(), &device, "en", current)
        .and_then(|policy| ctx.push.set_policy(policy.clone()).map(|_| policy));
    match applied {
        Err(code) => outcome_then(
            failed_outcome("Notification policy was rejected", code),
            policy_result_frame(false, Some(code), None),
            request_id,
            "push_policy_set",
            action_id,
        ),
        Ok(policy) => {
            let response = policy_response(&policy);
            outcome_then(
                Outcome::completed("", Some(serde_json::json!({ "policy": response.clone() }))),
                policy_result_frame(true, None, Some(&response)),
                request_id,
                "push_policy_set",
                action_id,
            )
        }
    }
}

/// `push_subscribe` — registers the device's subscription; emits
/// `push_subscribed`. Trust-bearing fields are overwritten with the
/// caller's identity like the oracle (`device_id`, `client_id`,
/// `notify_finished`, `locale`, `platform:"other"`, `user_agent:""`).
pub(crate) async fn subscribe(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    let device = ctx.device_id.clone();
    let mut code: Option<&'static str> = Some("push_invalid_subscription");
    let mut ok = false;
    if let Some(raw) = &message.subscription {
        if let Ok(Some(mut sub)) = serde_json::from_str::<Option<Subscription>>(raw.get()) {
            sub.client_id = message.client_id.clone();
            sub.notify_finished = message.notify_finished;
            sub.device_id = device;
            sub.locale = "en".to_owned();
            // The authenticated connection does not carry a trusted
            // browser platform — default to no actions (oracle).
            sub.platform = PLATFORM_OTHER.to_owned();
            sub.user_agent = String::new();
            match ctx.push.subscribe(sub, &message.replace_endpoints) {
                Ok(()) => {
                    ok = true;
                    code = None;
                }
                Err(c) => code = Some(c),
            }
        }
    }
    vec![ok_subscribed(ok), receipt(request_id, action_id, code)]
}

/// `push_unsubscribe` — emits `push_unsubscribed`.
pub(crate) async fn unsubscribe(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    let device = ctx.device_id.clone();
    let code = ctx
        .push
        .unsubscribe_device(&device, &message.endpoints, &message.client_id)
        .err();
    vec![
        ok_unsubscribed(code.is_none()),
        receipt(request_id, action_id, code),
    ]
}

/// `push_test_device` — 10s rate limit per device, then publish a
/// `test` event through the policy+queue path:
/// `queued` / `dropped` / `rate_limited`.
pub(crate) async fn test_device(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    let device = ctx.device_id.clone();
    if !ctx.push.reserve_test(&device, Instant::now()) {
        return vec![
            test_result_frame("rate_limited"),
            receipt(request_id, action_id, None),
        ];
    }
    let now = Timestamp::now();
    let key = PushEventKey {
        device_id: device,
        event_id: "test".to_owned(),
        category: CATEGORY_TEST.to_owned(),
        ..PushEventKey::default()
    };
    let published = ctx.push.publish(PublishRequest {
        key,
        preview: PREVIEW_HIDDEN,
        created_at: Some(now),
        expires_at: Some(now.add_ns(TEST_EVENT_TTL_NS)),
    });
    let stage = match published {
        Ok(result) if result.queued > 0 => "queued",
        _ => "dropped",
    };
    vec![
        test_result_frame(stage),
        receipt(request_id, action_id, None),
    ]
}

/// `push_snooze` — suppress notifications until an RFC3339 instant
/// (or indefinitely); answers `push_policy_result` only.
pub(crate) async fn snooze(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    let device = ctx.device_id.clone();
    let applied = (|| -> Result<DevicePolicy, &'static str> {
        let mut policy = ctx.push.policy(&device, "en");
        policy.snoozed = message.snoozed;
        policy.snooze_until = None;
        if !message.snooze_until.is_empty() {
            policy.snooze_until =
                Some(parse_rfc3339(&message.snooze_until).ok_or("push_invalid_snooze")?);
        }
        // The oracle collapses every SetPolicy failure here to
        // `push_invalid_snooze`.
        ctx.push
            .set_policy(policy.clone())
            .map_err(|_| "push_invalid_snooze")?;
        Ok(policy)
    })();
    match applied {
        Err(code) => vec![
            policy_result_frame(false, Some(code), None),
            receipt(request_id, action_id, Some(code)),
        ],
        Ok(policy) => vec![
            policy_result_frame(true, None, Some(&policy_response(&policy))),
            receipt(request_id, action_id, None),
        ],
    }
}

/// `push_viewed_pane` — records the viewed pane (or clears it);
/// only a visible+unlocked+currently-authoritative target counts.
pub(crate) async fn viewed_pane(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    let device = ctx.device_id.clone();
    let target = match &message.target {
        Some(target)
            if message.visible
                && message.unlocked
                && push_target_current(&ctx.topology, target) =>
        {
            Some(target.clone())
        }
        _ => None,
    };
    ctx.push.set_viewed_pane(&device, target);
    vec![viewed_pane_result(), receipt(request_id, action_id, None)]
}

/// `push_open_ref` — verifies the signed event reference, the device
/// binding, and that the claimed target is still current. Every
/// rejection is the oracle's generic `"Notification target is no
/// longer available"` `command_result`; the receipt keeps the specific
/// code.
pub(crate) async fn open_ref(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    let device = ctx.device_id.clone();
    let checked = ctx
        .push
        .verify_event_reference(&message.event_ref, Timestamp::now())
        .and_then(|claims| {
            if claims.key.device_id != device {
                return Err(RefError::Invalid);
            }
            if !push_target_current(&ctx.topology, &claims.key.target()) {
                return Err(RefError::Stale);
            }
            Ok(claims)
        });
    match checked {
        Err(error) => failed_outcome("Notification target is no longer available", error.code())
            .frames(request_id, "push_open_ref", action_id),
        Ok(claims) => Outcome::completed(
            &claims.key.pane_id,
            Some(serde_json::json!({
                "target": claims.key.target(),
                "event_id": claims.key.event_id,
                "category": claims.key.category,
            })),
        )
        .frames(request_id, "push_open_ref", action_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology::Topology;
    use crate::TopologyActor;
    use lerdr_herdr::AgentInfo;
    use tokio_util::sync::CancellationToken;

    /// Dead-socket `ActionContext` — nothing these handlers touch dials
    /// Herdr (activity.rs's fixture pattern).
    fn test_context_with(topology: Topology, push: Push, client_id: &str) -> ActionContext {
        let client = lerdr_herdr::Client::unix("/nonexistent-lerdr-push-test.sock");
        let cancel = CancellationToken::new();
        cancel.cancel();
        ActionContext {
            client: client.clone(),
            topology: Arc::new(topology),
            handle: TopologyActor::spawn(client.clone(), cancel),
            leases: crate::actions::leases::Leases::new(client),
            profiles: crate::actions::profiles::Resolver::with_config_home(
                tempfile::tempdir().expect("tempdir").keep(),
            ),
            questions: crate::actions::questions::Questions::default(),
            uploads: crate::actions::uploads::Uploads::new(
                tempfile::tempdir().expect("tempdir").keep(),
            ),
            activities: crate::actions::activity::Journal::default(),
            push,
            speech: crate::actions::speech::Speech::default(),
            notices: crate::actions::Notices::default(),
            audit: None,
            device_id: "test-device".to_owned(),
            client_id: client_id.to_owned(),
        }
    }

    fn test_context(push: Push, client_id: &str) -> ActionContext {
        test_context_with(Topology::default(), push, client_id)
    }

    fn inbound(fields: serde_json::Value) -> Inbound {
        let mut fields = fields;
        // `Inbound::decode` requires a non-empty `type`.
        fields["type"] = serde_json::Value::String("push_test".to_owned());
        Inbound::decode(&serde_json::to_vec(&fields).unwrap()).expect("inbound")
    }

    fn valid_sub(endpoint: &str) -> serde_json::Value {
        serde_json::json!({
            "endpoint": endpoint,
            "keys": { "p256dh": "key-material", "auth": "auth-secret" },
        })
    }

    fn agent_topology() -> Topology {
        let mut topology = Topology::default();
        topology.accept(lerdr_herdr::SessionSnapshot {
            agents: vec![AgentInfo {
                pane_id: "pane-1".into(),
                terminal_id: "term-1".into(),
                workspace_id: "ws".into(),
                tab_id: "tab-1".into(),
                agent_status: lerdr_herdr::AgentStatus::Working,
                agent_session: Some(lerdr_herdr::AgentSessionInfo {
                    source: "sess".into(),
                    agent: "devin".into(),
                    kind: lerdr_herdr::AgentSessionRefKind::Id,
                    value: "sess-1".into(),
                }),
                ..AgentInfo::default()
            }],
            ..lerdr_herdr::SessionSnapshot::default()
        });
        topology
    }

    fn current_target() -> TargetRef {
        TargetRef {
            server_session_id: "primary".into(),
            pane_id: "pane-1".into(),
            terminal_id: "term-1".into(),
            generation: 0,
            agent_session_id: "sess-1".into(),
        }
    }

    fn question_key(device: &str) -> PushEventKey {
        PushEventKey {
            device_id: device.into(),
            server_session_id: "primary".into(),
            pane_id: "pane-1".into(),
            terminal_id: "term-1".into(),
            agent_session_id: "sess-1".into(),
            generation: 0,
            event_id: "evt-1".into(),
            interaction_revision: 3,
            category: CATEGORY_QUESTION.into(),
        }
    }

    #[tokio::test]
    async fn policy_get_returns_default_policy_frame() {
        let ctx = test_context(Push::default(), "client-1");
        let frames = policy_get(ctx, "req-1", "act-1", &inbound(serde_json::json!({}))).await;
        let [Outbound::PushPolicy(m), Outbound::ActionReceipt(r)] = frames.as_slice() else {
            panic!("unexpected frames: {frames:?}");
        };
        let policy: serde_json::Value =
            serde_json::from_str(m.policy.as_ref().unwrap().value().unwrap().get()).unwrap();
        assert_eq!(policy["device_id"], "test-device");
        assert_eq!(policy["locale"], "en");
        assert_eq!(policy["settle_ms"], 2000);
        assert_eq!(policy["cooldown_ms"], 30_000);
        assert_eq!(policy["snoozed"], false);
        assert_eq!(policy["update_once"], true);
        assert_eq!(policy["categories"]["attention"], true);
        assert_eq!(policy["categories"]["finished"], false);
        assert!(policy.get("snooze_until").is_none());
        assert_eq!(r.receipt.as_ref().unwrap().phase.as_str(), "confirmed");
    }

    #[tokio::test]
    async fn policy_set_then_get_roundtrips() {
        let push = Push::default();
        let ctx = test_context(push.clone(), "client-1");
        let set = policy_set(
            ctx.clone(),
            "req-1",
            "act-1",
            &inbound(serde_json::json!({
                "policy": {
                    "categories": {"attention": false},
                    "settle_ms": 500,
                    "cooldown_ms": 10_000,
                    "snoozed": false,
                    "update_once": false,
                }
            })),
        )
        .await;
        let [Outbound::CommandResult(cr), Outbound::PushPolicyResult(pr), Outbound::ActionReceipt(_)] =
            set.as_slice()
        else {
            panic!("unexpected frames: {set:?}");
        };
        assert_eq!(cr.ok, Some(true));
        assert_eq!(cr.phase.as_deref(), Some("completed"));
        assert_eq!(pr.ok, Some(true));
        let stored = push.policy("test-device", "en");
        assert_eq!(stored.settle, 500 * NS_PER_MS);
        assert_eq!(stored.cooldown, 10_000 * NS_PER_MS);
        // absent categories keys → the wire map replaces wholesale.
        assert_eq!(
            stored.categories.as_ref().unwrap().get("attention"),
            Some(&false)
        );
        assert_eq!(stored.categories.as_ref().unwrap().get("question"), None);
        assert!(!stored.update_once);
        // policy_get reflects the stored policy.
        let frames = policy_get(ctx, "req-2", "act-2", &inbound(serde_json::json!({}))).await;
        let Some(Outbound::PushPolicy(m)) = frames.first() else {
            panic!("expected push_policy");
        };
        let policy: serde_json::Value =
            serde_json::from_str(m.policy.as_ref().unwrap().value().unwrap().get()).unwrap();
        assert_eq!(policy["settle_ms"], 500);
        assert_eq!(policy["categories"]["attention"], false);
        assert!(policy["categories"].get("question").is_none());
    }

    #[tokio::test]
    async fn policy_set_rejects_missing_and_malformed_policy() {
        let ctx = test_context(Push::default(), "client-1");
        for msg in [
            inbound(serde_json::json!({})),
            inbound(serde_json::json!({"policy": 42})),
            inbound(serde_json::json!({"policy": {"settle_ms": "fast"}})),
        ] {
            let frames = policy_set(ctx.clone(), "r", "a", &msg).await;
            let [Outbound::CommandResult(cr), Outbound::PushPolicyResult(pr), Outbound::ActionReceipt(rcpt)] =
                frames.as_slice()
            else {
                panic!("unexpected frames: {frames:?}");
            };
            assert_eq!(cr.ok, Some(false));
            assert_eq!(cr.phase.as_deref(), Some("failed"));
            assert_eq!(pr.ok, Some(false));
            assert_eq!(pr.code.as_deref(), Some("push_invalid_policy"));
            assert_eq!(
                rcpt.receipt.as_ref().unwrap().phase.as_str(),
                "failed_before_dispatch"
            );
        }
    }

    #[tokio::test]
    async fn policy_set_rejects_invalid_duration() {
        let ctx = test_context(Push::default(), "client-1");
        // `i64::MAX / 1e6 + 1` ms wraps `Duration` negative — Go's
        // `Duration(ms) * Millisecond` overflow path.
        for settle in [-1, i64::MAX / 1_000_000 + 1] {
            let frames = policy_set(
                ctx.clone(),
                "r",
                "a",
                &inbound(serde_json::json!({"policy": {"settle_ms": settle}})),
            )
            .await;
            let Some(Outbound::PushPolicyResult(pr)) = frames.get(1) else {
                panic!("expected push_policy_result");
            };
            assert_eq!(pr.code.as_deref(), Some("push_invalid_duration"));
        }
        // Null policy decodes to the zero wire — a valid patch.
        let frames = policy_set(
            ctx.clone(),
            "r",
            "a",
            &inbound(serde_json::json!({"policy": null})),
        )
        .await;
        let Some(Outbound::PushPolicyResult(pr)) = frames.get(1) else {
            panic!("expected push_policy_result");
        };
        assert_eq!(pr.ok, Some(true));
    }

    #[tokio::test]
    async fn policy_set_rejects_invalid_category_and_snooze() {
        let ctx = test_context(Push::default(), "client-1");
        let frames = policy_set(
            ctx.clone(),
            "r",
            "a",
            &inbound(serde_json::json!({
                "policy": {"categories": {"bogus": true}, "snooze_until": "soon"}
            })),
        )
        .await;
        let Some(Outbound::PushPolicyResult(pr)) = frames.get(1) else {
            panic!("expected push_policy_result");
        };
        // boundPushPolicy parses snooze first — `push_invalid_snooze`.
        assert_eq!(pr.code.as_deref(), Some("push_invalid_snooze"));
        let frames = policy_set(
            ctx.clone(),
            "r",
            "a",
            &inbound(serde_json::json!({"policy": {"categories": {"bogus": true}}})),
        )
        .await;
        let Some(Outbound::PushPolicyResult(pr)) = frames.get(1) else {
            panic!("expected push_policy_result");
        };
        assert_eq!(pr.code.as_deref(), Some("push_invalid_category"));
    }

    #[tokio::test]
    async fn snooze_sets_and_clears() {
        let push = Push::default();
        let ctx = test_context(push.clone(), "client-1");
        let frames = snooze(
            ctx.clone(),
            "r",
            "a",
            &inbound(serde_json::json!({"snoozed": true, "snooze_until": "2999-01-01T00:00:00Z"})),
        )
        .await;
        let [Outbound::PushPolicyResult(pr), Outbound::ActionReceipt(_)] = frames.as_slice() else {
            panic!("unexpected frames: {frames:?}");
        };
        assert_eq!(pr.ok, Some(true));
        let policy: serde_json::Value =
            serde_json::from_str(pr.policy.as_ref().unwrap().value().unwrap().get()).unwrap();
        assert_eq!(policy["snoozed"], true);
        assert_eq!(policy["snooze_until"], "2999-01-01T00:00:00Z");
        assert!(push.policy("test-device", "en").snoozed);
    }

    #[tokio::test]
    async fn snooze_rejects_bad_timestamp() {
        let ctx = test_context(Push::default(), "client-1");
        let frames = snooze(
            ctx.clone(),
            "r",
            "a",
            &inbound(serde_json::json!({"snoozed": true, "snooze_until": "tomorrow"})),
        )
        .await;
        let [Outbound::PushPolicyResult(pr), Outbound::ActionReceipt(rcpt)] = frames.as_slice()
        else {
            panic!("unexpected frames: {frames:?}");
        };
        assert_eq!(pr.ok, Some(false));
        assert_eq!(pr.code.as_deref(), Some("push_invalid_snooze"));
        assert_eq!(
            rcpt.receipt.as_ref().unwrap().error.as_ref().unwrap().code,
            "push_invalid_snooze"
        );
    }

    #[tokio::test]
    async fn subscribe_frames_and_mismatch() {
        let push = Push::default();
        let ctx = test_context(push.clone(), "client-1");
        let frames = subscribe(
            ctx.clone(),
            "r",
            "a",
            &inbound(serde_json::json!({
                "subscription": valid_sub("https://fcm.googleapis.com/send/one"),
                "client_id": "uuid-1:relay",
                "notify_finished": true,
            })),
        )
        .await;
        let [Outbound::PushSubscribed(m), Outbound::ActionReceipt(_)] = frames.as_slice() else {
            panic!("unexpected frames: {frames:?}");
        };
        assert_eq!(m.ok, Some(true));
        let subs = push.subscriptions();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].device_id, "test-device");
        assert_eq!(subs[0].client_id, "uuid-1:relay");
        assert!(subs[0].notify_finished);
        assert_eq!(subs[0].platform, "other");
        // A different authenticated device on the same endpoint
        // mismatches — the wire `client_id` claim is not the device key.
        let mut other = ctx.clone();
        other.device_id = "other-device".to_owned();
        let frames = subscribe(
            other,
            "r",
            "a",
            &inbound(serde_json::json!({
                "subscription": valid_sub("https://fcm.googleapis.com/send/one"),
                "client_id": "uuid-2:relay",
            })),
        )
        .await;
        let Some(Outbound::PushSubscribed(m)) = frames.first() else {
            panic!("expected push_subscribed");
        };
        assert_eq!(m.ok, Some(false));
    }

    #[tokio::test]
    async fn subscribe_rejects_disallowed_and_malformed() {
        let ctx = test_context(Push::default(), "client-1");
        for sub in [
            serde_json::json!("not-an-object"),
            serde_json::json!({"endpoint": "https://internal.example.test/push"}),
            serde_json::json!({"endpoint": "https://fcm.googleapis.com/x"}), // no keys
        ] {
            let frames = subscribe(
                ctx.clone(),
                "r",
                "a",
                &inbound(serde_json::json!({"subscription": sub})),
            )
            .await;
            let Some(Outbound::PushSubscribed(m)) = frames.first() else {
                panic!("expected push_subscribed");
            };
            assert_eq!(m.ok, Some(false));
        }
    }

    #[tokio::test]
    async fn unsubscribe_removes_subscription() {
        let push = Push::default();
        let ctx = test_context(push.clone(), "client-1");
        let _ = subscribe(
            ctx.clone(),
            "r",
            "a",
            &inbound(serde_json::json!({
                "subscription": valid_sub("https://fcm.googleapis.com/send/one"),
                "client_id": "uuid-1",
            })),
        )
        .await;
        assert_eq!(push.subscriptions().len(), 1);
        let frames = unsubscribe(
            ctx.clone(),
            "r",
            "a",
            &inbound(serde_json::json!({"client_id": "uuid-1"})),
        )
        .await;
        let [Outbound::PushUnsubscribed(m), Outbound::ActionReceipt(_)] = frames.as_slice() else {
            panic!("unexpected frames: {frames:?}");
        };
        assert_eq!(m.ok, Some(true));
        // Client id match removed the subscription.
        assert!(push.subscriptions().is_empty());
    }

    #[tokio::test]
    async fn test_device_queued_then_rate_limited() {
        let push = Push::default();
        let ctx = test_context(push.clone(), "client-1");
        let _ = subscribe(
            ctx.clone(),
            "r",
            "a",
            &inbound(serde_json::json!({
                "subscription": valid_sub("https://fcm.googleapis.com/send/one"),
                "client_id": "uuid-1",
            })),
        )
        .await;
        // Binding learned from subscribe — the rate ledger keys the device.
        let frames = test_device(ctx.clone(), "r", "a", &inbound(serde_json::json!({}))).await;
        let [Outbound::PushTestResult(m), Outbound::ActionReceipt(_)] = frames.as_slice() else {
            panic!("unexpected frames: {frames:?}");
        };
        assert_eq!(m.stage.as_deref(), Some("queued"));
        let frames = test_device(ctx.clone(), "r", "a", &inbound(serde_json::json!({}))).await;
        let Some(Outbound::PushTestResult(m)) = frames.first() else {
            panic!("expected push_test_result");
        };
        assert_eq!(m.stage.as_deref(), Some("rate_limited"));
    }

    #[tokio::test]
    async fn test_device_drops_without_subscription() {
        let ctx = test_context(Push::default(), "client-1");
        let frames = test_device(ctx.clone(), "r", "a", &inbound(serde_json::json!({}))).await;
        let Some(Outbound::PushTestResult(m)) = frames.first() else {
            panic!("expected push_test_result");
        };
        assert_eq!(m.stage.as_deref(), Some("dropped"));
    }

    #[tokio::test]
    async fn viewed_pane_suppresses_publish_for_that_target() {
        let push = Push::default();
        let ctx = test_context_with(agent_topology(), push.clone(), "client-1");
        let _ = subscribe(
            ctx.clone(),
            "r",
            "a",
            &inbound(serde_json::json!({
                "subscription": valid_sub("https://fcm.googleapis.com/send/one"),
                "client_id": "uuid-1",
            })),
        )
        .await;
        let frames = viewed_pane(
            ctx.clone(),
            "r",
            "a",
            &inbound(serde_json::json!({
                "visible": true,
                "unlocked": true,
                "target": current_target(),
            })),
        )
        .await;
        let [Outbound::PushViewedPaneResult(m), Outbound::ActionReceipt(_)] = frames.as_slice()
        else {
            panic!("unexpected frames: {frames:?}");
        };
        assert_eq!(m.ok, Some(true));
        // Same-target publish is suppressed by the viewed-pane ledger.
        let result = push
            .publish(PublishRequest {
                key: question_key("test-device"),
                preview: PREVIEW_QUESTION,
                created_at: Some(Timestamp::now()),
                expires_at: Some(Timestamp::now().add_ns(60 * NS_PER_SEC)),
            })
            .unwrap();
        assert_eq!(result.queued, 0);
        assert_eq!(result.suppressed, 1);
        assert_eq!(
            push.decide(&question_key("test-device"), "en", Timestamp::now())
                .code,
            "push_viewed_pane"
        );
        // A not-current target clears the marker instead.
        let frames = viewed_pane(
            ctx.clone(),
            "r",
            "a",
            &inbound(serde_json::json!({
                "visible": true,
                "unlocked": true,
                "target": {"server_session_id":"primary","pane_id":"gone","terminal_id":"t","generation":0,"agent_session_id":"s"},
            })),
        )
        .await;
        assert!(matches!(
            frames.first(),
            Some(Outbound::PushViewedPaneResult(_))
        ));
        let result = push
            .publish(PublishRequest {
                key: question_key("test-device"),
                preview: PREVIEW_QUESTION,
                created_at: Some(Timestamp::now()),
                expires_at: Some(Timestamp::now().add_ns(60 * NS_PER_SEC)),
            })
            .unwrap();
        assert_eq!(result.queued, 1);
    }

    #[tokio::test]
    async fn open_ref_roundtrip_and_failures() {
        let push = Push::default();
        let ctx = test_context_with(agent_topology(), push.clone(), "client-1");
        let _ = subscribe(
            ctx.clone(),
            "r",
            "a",
            &inbound(serde_json::json!({
                "subscription": valid_sub("https://fcm.googleapis.com/send/one"),
                "client_id": "uuid-1",
            })),
        )
        .await;
        // Queue the notification: the key becomes active.
        let key = question_key("test-device");
        let result = push
            .publish(PublishRequest {
                key: key.clone(),
                preview: PREVIEW_QUESTION,
                created_at: Some(Timestamp::now()),
                expires_at: Some(Timestamp::now().add_ns(60 * NS_PER_SEC)),
            })
            .unwrap();
        assert_eq!(result.queued, 1);
        let token = push
            .sign_event_reference(&key, Timestamp::now().add_ns(60 * NS_PER_SEC))
            .unwrap();
        let frames = open_ref(
            ctx.clone(),
            "req-9",
            "act-9",
            &inbound(serde_json::json!({"event_ref": token})),
        )
        .await;
        let [Outbound::CommandResult(cr), Outbound::ActionReceipt(_)] = frames.as_slice() else {
            panic!("unexpected frames: {frames:?}");
        };
        assert_eq!(cr.ok, Some(true));
        assert_eq!(cr.phase.as_deref(), Some("completed"));
        assert_eq!(cr.pane_id.as_deref(), Some("pane-1"));
        let data: serde_json::Value =
            serde_json::from_str(cr.data.as_ref().unwrap().value().unwrap().get()).unwrap();
        assert_eq!(data["event_id"], "evt-1");
        assert_eq!(data["category"], "question");
        assert_eq!(data["target"]["pane_id"], "pane-1");
        // Garbage token → generic failure.
        let frames = open_ref(
            ctx.clone(),
            "r",
            "a",
            &inbound(serde_json::json!({"event_ref": "bogus.token"})),
        )
        .await;
        let Some(Outbound::CommandResult(cr)) = frames.first() else {
            panic!("expected command_result");
        };
        assert_eq!(cr.ok, Some(false));
        assert_eq!(cr.phase.as_deref(), Some("failed"));
        assert_eq!(
            cr.error.as_deref(),
            Some("Notification target is no longer available")
        );
    }

    #[tokio::test]
    async fn open_ref_rejects_stale_expired_and_foreign_refs() {
        let push = Push::default();
        let ctx = test_context_with(agent_topology(), push.clone(), "client-1");
        let key = question_key("test-device");
        // Not published → not active → stale.
        let stale = push
            .sign_event_reference(&key, Timestamp::now().add_ns(60 * NS_PER_SEC))
            .unwrap();
        let frames = open_ref(
            ctx.clone(),
            "r",
            "a",
            &inbound(serde_json::json!({"event_ref": stale})),
        )
        .await;
        let Some(Outbound::CommandResult(cr)) = frames.first() else {
            panic!("expected command_result");
        };
        assert_eq!(cr.ok, Some(false));
        // Expired.
        let expired = push
            .sign_event_reference(&key, Timestamp::now().add_ns(-NS_PER_SEC))
            .unwrap();
        let frames = open_ref(
            ctx.clone(),
            "r",
            "a",
            &inbound(serde_json::json!({"event_ref": expired})),
        )
        .await;
        let Some(Outbound::CommandResult(cr)) = frames.first() else {
            panic!("expected command_result");
        };
        assert_eq!(cr.ok, Some(false));
        // Foreign device key.
        let foreign = push
            .sign_event_reference(
                &question_key("someone-else"),
                Timestamp::now().add_ns(60 * NS_PER_SEC),
            )
            .unwrap();
        let frames = open_ref(
            ctx.clone(),
            "r",
            "a",
            &inbound(serde_json::json!({"event_ref": foreign})),
        )
        .await;
        let Some(Outbound::CommandResult(cr)) = frames.first() else {
            panic!("expected command_result");
        };
        assert_eq!(cr.ok, Some(false));
    }

    #[tokio::test]
    async fn authenticated_device_keys_push_state() {
        let push = Push::default();
        let ctx = test_context(push.clone(), "client-9");
        let _ = subscribe(
            ctx.clone(),
            "r",
            "a",
            &inbound(serde_json::json!({
                "subscription": valid_sub("https://fcm.googleapis.com/send/one"),
                "client_id": "uuid-1:relay",
            })),
        )
        .await;
        // The wire `client_id` is a subscription claim — device state
        // keys on the authenticated device id, not the claimed uuid.
        let sub = &push.subscriptions()[0];
        assert_eq!(sub.device_id, "test-device");
        assert_eq!(sub.client_id, "uuid-1:relay");
        let frames = policy_get(ctx.clone(), "r", "a", &inbound(serde_json::json!({}))).await;
        let Some(Outbound::PushPolicy(m)) = frames.first() else {
            panic!("expected push_policy");
        };
        let policy: serde_json::Value =
            serde_json::from_str(m.policy.as_ref().unwrap().value().unwrap().get()).unwrap();
        assert_eq!(policy["device_id"], "test-device");
    }

    #[test]
    fn reference_signer_roundtrip_and_tamper() {
        let signer = ReferenceSigner::ephemeral();
        let claims = ReferenceClaims {
            key: question_key("dev"),
            expires_at: Timestamp::now().add_ns(60 * NS_PER_SEC),
        };
        let token = signer.sign(&claims).unwrap();
        let verified = signer.verify(&token, Timestamp::now()).unwrap();
        assert_eq!(verified.key, claims.key);
        assert!(signer.verify("no-dots", Timestamp::now()).is_err());
        let mut forged = token.clone();
        forged.replace_range(0..1, "A");
        assert!(signer.verify(&forged, Timestamp::now()).is_err());
    }

    #[test]
    fn hmac_matches_rfc4231_case1() {
        let mac = hmac_sha256(&[0x0b; 20], b"Hi There");
        assert_eq!(
            hex::encode(mac),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn rfc3339_roundtrip() {
        let t = parse_rfc3339("2026-09-20T16:05:00Z").unwrap();
        assert_eq!(t.to_rfc3339(), "2026-09-20T16:05:00Z");
        let t = parse_rfc3339("2026-09-20T16:05:00.120000000+02:30").unwrap();
        assert_eq!(t.nanos, 120_000_000);
        assert_eq!(t.to_rfc3339(), "2026-09-20T13:35:00Z");
        assert_eq!(t.to_rfc3339_nano(), "2026-09-20T13:35:00.12Z");
        assert!(parse_rfc3339("2026-02-30T00:00:00Z").is_none());
        assert!(parse_rfc3339("2026-09-20 16:05:00Z").is_none());
        assert!(parse_rfc3339("2026-09-20T16:05Z").is_none());
        // Go's zero time round-trips as the file format's sentinel.
        assert_eq!(
            Timestamp::default().to_rfc3339_nano(),
            "0001-01-01T00:00:00Z"
        );
    }

    #[test]
    fn persistence_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let push = Push::new(dir.path()).unwrap();
        let sub = Subscription {
            endpoint: "https://fcm.googleapis.com/send/x".into(),
            keys: SubscriptionKeys {
                p256dh: "k".into(),
                auth: "a".into(),
            },
            device_id: "dev-1".into(),
            locale: "en".into(),
            platform: "other".into(),
            user_agent: String::new(),
            notify_finished: true,
            client_id: "uuid-1".into(),
        };
        push.subscribe(sub, &[]).unwrap();
        let mut policy = default_device_policy("dev-1", "en");
        policy.snoozed = true;
        policy.snooze_until = parse_rfc3339("2999-01-01T00:00:00Z");
        policy
            .categories
            .as_mut()
            .unwrap()
            .insert("brief".into(), false);
        push.set_policy(policy).unwrap();
        let reloaded = Push::new(dir.path()).unwrap();
        assert_eq!(reloaded.subscriptions().len(), 1);
        let policy = reloaded.policy("dev-1", "en");
        assert!(policy.snoozed);
        assert_eq!(
            policy.snooze_until.unwrap().to_rfc3339(),
            "2999-01-01T00:00:00Z"
        );
        assert_eq!(
            policy.categories.as_ref().unwrap().get("brief"),
            Some(&false)
        );
        // The signer key persisted: tokens still verify…
        // (they'd fail `active` after restart, like recovered-but-
        // unreconciled oracle state — sign/verify itself round-trips).
        let signer_claims = ReferenceClaims {
            key: question_key("dev-1"),
            expires_at: Timestamp::now().add_ns(60 * NS_PER_SEC),
        };
        let token = reloaded.lock().signer.sign(&signer_claims).unwrap();
        assert!(reloaded
            .lock()
            .signer
            .verify(&token, Timestamp::now())
            .is_ok());
    }
}
