//! Attachment uploads — the `internal/upload` + `app/uploads.go` port.
//!
//! `upload_begin` stages a session, `upload_chunk` streams base64 payloads
//! into it, `upload_finish` verifies digests and publishes the attachment
//! index, `upload_cancel` aborts. Answers are `upload_*_result` frames
//! (`{type, request_id, result}` / `{type, request_id, error}`), not
//! `command_result` — the oracle's shape. `send_text`/`submit_prompt`
//! expand `Attachment: <ref>` lines through the finished index before
//! dispatch (see [`expand_attachment_references`]).
//!
//! The manager stages under `<dir>/sessions/<upload-id>/NNNN.part`,
//! publishes finished files under `<dir>/objects/<ref><ext>`, and persists
//! the finished-attachment index to `<dir>/attachments.json`. All limits,
//! digest checks, content sniffing, and cleanup rules come from
//! `internal/upload/upload.go`; every failure code is funneled through
//! [`public_upload_error_code`] at the reply boundary exactly like the
//! oracle's `sendUploadError`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use lerdr_core::json::{de_default, MaybeNull, RawJson};
use lerdr_core::protocol::{ApiError, Inbound, Outbound, TargetRef, UploadResultMessage};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::ActionContext;
use crate::topology::Topology;

// `Default*` bounds from internal/upload/upload.go. The protocol docs and
// e2e fixtures illustrate larger example values; the oracle constants are
// authoritative.
/// `DefaultChunkBytes`.
const CHUNK_BYTES: usize = 256 * 1024;
/// `DefaultMaxFiles`.
const MAX_FILES: usize = 8;
/// `DefaultMaxFileBytes` — 20 MiB.
const MAX_FILE_BYTES: i64 = 20 * 1024 * 1024;
/// `DefaultMaxBatchBytes` — 50 MiB.
const MAX_BATCH_BYTES: i64 = 50 * 1024 * 1024;
/// `DefaultMaxSessions`.
const MAX_SESSIONS: usize = 64;
/// `DefaultMaxSessionsPerOwner`.
const MAX_SESSIONS_PER_OWNER: usize = 4;
/// `DefaultSessionTTL`.
const SESSION_TTL: Duration = Duration::from_secs(30 * 60);
/// `DefaultAttachmentTTL`.
const ATTACHMENT_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// `attachmentIndexFilename` / `attachmentIndexVersion`.
const INDEX_FILENAME: &str = "attachments.json";
const INDEX_VERSION: i64 = 1;
/// Bytes kept from the head/tail of each staged file for sniffing
/// (`sessionFilePrefixBytes` / `sessionFileSuffixBytes`).
const PREFIX_BYTES: usize = 512;
const SUFFIX_BYTES: usize = 1024;
/// `io.LimitReader(file, 16<<20)` on index load.
const MAX_INDEX_BYTES: u64 = 16 << 20;
/// `attachmentMaxContainerEntries` / `attachmentMaxExpandedBytes` for the
/// document-container check.
const MAX_CONTAINER_ENTRIES: usize = 4096;
const MAX_EXPANDED_BYTES: u64 = 200 * 1024 * 1024;
/// `len(spec.Name) > 128` cap.
const MAX_NAME_BYTES: usize = 128;
const SESSIONS_DIR: &str = "sessions";
const OBJECTS_DIR: &str = "objects";

/// The public failure the expansion seam reports — `server.go`'s
/// `sendAttachmentDispatchFailure` text.
#[allow(dead_code)] // read once input.rs wires expansion
const ATTACHMENTS_UNAVAILABLE: &str =
    "One or more attachments are no longer available for this agent";

// ── errors ────────────────────────────────────────────────────────────

/// `upload.Error` — an internal code plus bounded args, mapped to the
/// public `attachment_*` codes at the reply boundary.
#[derive(Debug)]
struct UploadError {
    code: &'static str,
    args: BTreeMap<String, serde_json::Value>,
}

impl UploadError {
    fn new(code: &'static str) -> Self {
        Self {
            code,
            args: BTreeMap::new(),
        }
    }

    /// `&upload.Error{Code: ..., Args: map[string]any{...}}` — every
    /// arg the oracle emits is an integer.
    fn with_args(code: &'static str, args: &[(&'static str, i64)]) -> Self {
        Self {
            code,
            args: args
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).into()))
                .collect(),
        }
    }
}

impl std::fmt::Display for UploadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.code)
    }
}

/// `publicUploadErrorCode` — internal `upload.Error` code → public
/// `attachment_*` code; anything unmapped becomes
/// `attachment_upload_failed`.
fn public_upload_error_code(code: &str) -> &'static str {
    match code {
        "upload_batch_count_invalid" => "attachment_batch_limit",
        "upload_batch_too_large" => "attachment_batch_too_large",
        "upload_file_size_invalid" | "upload_file_too_large" => "attachment_file_too_large",
        "upload_name_invalid" => "attachment_invalid_name",
        "upload_type_unsupported"
        | "upload_extension_mismatch"
        | "upload_content_type_mismatch" => "attachment_unknown_mime",
        "upload_session_expired" => "attachment_upload_expired",
        "upload_session_limit" => "attachment_upload_busy",
        "upload_chunk_out_of_order"
        | "upload_chunk_digest_mismatch"
        | "upload_final_digest_mismatch"
        | "upload_incomplete"
        | "upload_scope_mismatch"
        | "upload_session_not_found" => "attachment_upload_state_unknown",
        _ => "attachment_upload_failed",
    }
}

// ── media types ───────────────────────────────────────────────────────

/// `allowedMediaTypes` — the media types the oracle sniffs.
fn allowed_extensions(media_type: &str) -> Option<&'static [&'static str]> {
    match media_type {
        "image/png" => Some(&[".png"]),
        "image/jpeg" => Some(&[".jpg", ".jpeg"]),
        "image/gif" => Some(&[".gif"]),
        "image/webp" => Some(&[".webp"]),
        "image/heic" => Some(&[".heic"]),
        "image/heif" => Some(&[".heif"]),
        "application/pdf" => Some(&[".pdf"]),
        "application/json" => Some(&[".json"]),
        "text/plain" => Some(&[".txt", ".text", ".log"]),
        "text/markdown" => Some(&[".md", ".markdown"]),
        "text/csv" => Some(&[".csv"]),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
            Some(&[".docx"])
        }
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => Some(&[".xlsx"]),
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => {
            Some(&[".pptx"])
        }
        "application/vnd.oasis.opendocument.text" => Some(&[".odt"]),
        "application/vnd.oasis.opendocument.spreadsheet" => Some(&[".ods"]),
        "application/vnd.oasis.opendocument.presentation" => Some(&[".odp"]),
        _ => None,
    }
}

/// `canonicalExtension` — the storage extension for published objects.
fn canonical_extension(media_type: &str) -> &'static str {
    match media_type {
        "image/png" => ".png",
        "image/jpeg" => ".jpg",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        "image/heic" => ".heic",
        "image/heif" => ".heif",
        "application/pdf" => ".pdf",
        "application/json" => ".json",
        "text/plain" => ".txt",
        "text/markdown" => ".md",
        "text/csv" => ".csv",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => ".docx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => ".xlsx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => ".pptx",
        "application/vnd.oasis.opendocument.text" => ".odt",
        "application/vnd.oasis.opendocument.spreadsheet" => ".ods",
        "application/vnd.oasis.opendocument.presentation" => ".odp",
        _ => ".data",
    }
}

// ── timestamps ────────────────────────────────────────────────────────

/// Go `time.Time` on the wire — RFC3339Nano. The oracle formats in local
/// time; the instant is what matters, so we always emit UTC (`Z`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Timestamp(SystemTime);

impl Default for Timestamp {
    fn default() -> Self {
        Self(UNIX_EPOCH)
    }
}

impl Serialize for Timestamp {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format_timestamp(self.0))
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        parse_timestamp(&value)
            .map(Self)
            .ok_or_else(|| serde::de::Error::custom("invalid RFC3339 timestamp"))
    }
}

/// Nanoseconds since the Unix epoch, signed.
fn epoch_nanos(time: SystemTime) -> i128 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => {
            i128::from(duration.as_secs()) * 1_000_000_000 + i128::from(duration.subsec_nanos())
        }
        Err(error) => {
            -(i128::from(error.duration().as_secs()) * 1_000_000_000
                + i128::from(error.duration().subsec_nanos()))
        }
    }
}

fn time_from_nanos(nanos: i128) -> SystemTime {
    if nanos >= 0 {
        UNIX_EPOCH + Duration::from_nanos(nanos as u64)
    } else {
        UNIX_EPOCH - Duration::from_nanos((-nanos) as u64)
    }
}

/// `time.Format(RFC3339Nano)` — UTC, fraction trimmed of trailing zeros.
fn format_timestamp(time: SystemTime) -> String {
    let nanos = epoch_nanos(time);
    let seconds = nanos.div_euclid(1_000_000_000) as i64;
    let sub = nanos.rem_euclid(1_000_000_000) as u32;
    let days = seconds.div_euclid(86_400);
    let day_seconds = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (
        day_seconds / 3600,
        day_seconds % 3600 / 60,
        day_seconds % 60,
    );
    let mut out = format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}");
    if sub != 0 {
        let mut fraction = format!("{sub:09}");
        while fraction.ends_with('0') {
            fraction.pop();
        }
        out.push('.');
        out.push_str(&fraction);
    }
    out.push('Z');
    out
}

/// `time.Parse(RFC3339)` — strict shape, `Z` or `±HH:MM` offsets.
fn parse_timestamp(value: &str) -> Option<SystemTime> {
    let bytes = value.as_bytes();
    if bytes.len() < 20 {
        return None;
    }
    let digits = |offset: usize, count: usize| -> Option<i64> {
        let slice = bytes.get(offset..offset + count)?;
        if !slice.iter().all(u8::is_ascii_digit) {
            return None;
        }
        Some(
            slice
                .iter()
                .fold(0i64, |acc, b| acc * 10 + i64::from(b - b'0')),
        )
    };
    if bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let year = digits(0, 4)?;
    let month = digits(5, 2)?;
    let day = digits(8, 2)?;
    if !(1..=12).contains(&month) || day < 1 || day > i64::from(days_in_month(month, year)) {
        return None;
    }
    if bytes[10] != b'T' && bytes[10] != b't' {
        return None;
    }
    if bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }
    let hour = digits(11, 2)?;
    let minute = digits(14, 2)?;
    let second = digits(17, 2)?;
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let mut index = 19;
    let mut sub = 0u32;
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        let start = index;
        let mut seen = 0u32;
        while bytes.get(index).is_some_and(|b| b.is_ascii_digit()) {
            if seen < 9 {
                sub = sub * 10 + u32::from(bytes[index] - b'0');
                seen += 1;
            }
            index += 1;
        }
        if index == start {
            return None;
        }
        sub *= 10u32.pow(9 - seen);
    }
    let offset_seconds: i64 = match bytes.get(index) {
        Some(b'Z') | Some(b'z') => {
            index += 1;
            0
        }
        Some(b'+') | Some(b'-') => {
            let sign = if bytes[index] == b'-' { -1i64 } else { 1 };
            let offset_hour = digits(index + 1, 2)?;
            if bytes.get(index + 3) != Some(&b':') {
                return None;
            }
            let offset_minute = digits(index + 4, 2)?;
            if offset_hour > 23 || offset_minute > 59 {
                return None;
            }
            index += 6;
            sign * (offset_hour * 3600 + offset_minute)
        }
        _ => return None,
    };
    if index != bytes.len() {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset_seconds;
    Some(time_from_nanos(
        i128::from(seconds) * 1_000_000_000 + i128::from(sub),
    ))
}

/// Howard Hinnant's civil-from-days — proleptic Gregorian.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    if month <= 2 {
        year += 1;
    }
    (year, month, day)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_prime = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn days_in_month(month: i64, year: i64) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Go's `time.Time{}.IsZero()` — 0001-01-01T00:00:00Z.
fn is_zero_time(time: SystemTime) -> bool {
    time == UNIX_EPOCH - Duration::from_secs(62_135_596_800)
}

// ── request decoding ──────────────────────────────────────────────────

/// `upload.BeginRequest` minus `Owner` (`json:"-"` — set by the handler,
/// never from the wire).
struct BeginRequest {
    target: TargetRef,
    files: Vec<FileSpec>,
}

/// `upload.ChunkRequest`.
struct ChunkRequest {
    target: TargetRef,
    upload_id: String,
    file_index: i64,
    sequence: i64,
    data: Vec<u8>,
    sha256: String,
}

/// `upload.FinishRequest`.
struct FinishRequest {
    target: TargetRef,
    upload_id: String,
    files: Vec<FileDigest>,
}

/// `upload.FileSpec` — inbound name/media-type/declared size.
#[derive(Debug, Clone, Default, Deserialize)]
struct FileSpec {
    #[serde(default, deserialize_with = "de_default")]
    name: String,
    #[serde(default, deserialize_with = "de_default")]
    media_type: String,
    #[serde(default, deserialize_with = "de_default")]
    bytes: i64,
}

/// `upload.FileDigest` — the finish-time claimed digest list.
#[derive(Debug, Default, Deserialize)]
struct FileDigest {
    #[serde(default, deserialize_with = "de_default")]
    file_index: i64,
    #[serde(default, deserialize_with = "de_default")]
    sha256: String,
}

/// `decodeUploadRequest` — pull upload fields out of the inbound map.
/// Field type mismatches fail the whole decode like Go's `json.Unmarshal`
/// round-trip; absent/`null` fields take the Go zero value.
fn decode_begin(message: &Inbound) -> Result<BeginRequest, ()> {
    Ok(BeginRequest {
        target: message.target.clone().unwrap_or_default(),
        files: decode_vec(message, "files")?,
    })
}

fn decode_chunk(message: &Inbound) -> Result<ChunkRequest, ()> {
    Ok(ChunkRequest {
        target: message.target.clone().unwrap_or_default(),
        upload_id: decode_string(message, "upload_id")?,
        file_index: decode_int(message, "file_index")?,
        sequence: decode_int(message, "sequence")?,
        // `data` arrives base64-encoded (Go `[]byte`); its decoder
        // ignores CR/LF inside the payload.
        data: decode_base64(&message.data)?,
        sha256: decode_string(message, "sha256")?,
    })
}

fn decode_finish(message: &Inbound) -> Result<FinishRequest, ()> {
    Ok(FinishRequest {
        target: message.target.clone().unwrap_or_default(),
        upload_id: decode_string(message, "upload_id")?,
        files: decode_vec(message, "files")?,
    })
}

fn decode_cancel(message: &Inbound) -> Result<(TargetRef, String), ()> {
    Ok((
        message.target.clone().unwrap_or_default(),
        decode_string(message, "upload_id")?,
    ))
}

fn decode_string(message: &Inbound, key: &str) -> Result<String, ()> {
    match message.raw(key) {
        None | Some(serde_json::Value::Null) => Ok(String::new()),
        Some(serde_json::Value::String(value)) => Ok(value.clone()),
        Some(_) => Err(()),
    }
}

fn decode_int(message: &Inbound, key: &str) -> Result<i64, ()> {
    match message.raw(key) {
        None | Some(serde_json::Value::Null) => Ok(0),
        Some(value) => {
            // The marshal round-trip prints integral float64s (`3.0`) as
            // `3`, which the int decoder then accepts.
            let mut value = value.clone();
            normalize_numbers(&mut value);
            value.as_i64().ok_or(())
        }
    }
}

fn decode_vec<T>(message: &Inbound, key: &str) -> Result<Vec<T>, ()>
where
    T: serde::de::DeserializeOwned + Default,
{
    match message.raw(key) {
        None | Some(serde_json::Value::Null) => Ok(Vec::new()),
        // Element-wise: Go's `json.Unmarshal` maps a `null` array entry to
        // the element's zero value rather than failing the decode.
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .map(|item| {
                if item.is_null() {
                    Ok(T::default())
                } else {
                    // The oracle's marshal/Unmarshal round-trip normalizes
                    // integral floats to ints (`8.0` -> `8`).
                    let mut item = item.clone();
                    normalize_numbers(&mut item);
                    serde_json::from_value(item).map_err(|_| ())
                }
            })
            .collect(),
        Some(_) => Err(()),
    }
}

fn normalize_numbers(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Number(number) => {
            if let Some(float) = number.as_f64() {
                if number.as_i64().is_none()
                    && float.fract() == 0.0
                    && float.abs() < 9_007_199_254_740_992.0
                {
                    *value = serde_json::Value::from(float as i64);
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(normalize_numbers),
        serde_json::Value::Object(map) => map.values_mut().for_each(normalize_numbers),
        _ => {}
    }
}

/// `encoding/base64` via Go's `[]byte` unmarshal — CR/LF are skipped
/// inside the encoded payload.
fn decode_base64(value: &str) -> Result<Vec<u8>, ()> {
    let filtered: Vec<u8> = value
        .bytes()
        .filter(|byte| *byte != b'\r' && *byte != b'\n')
        .collect();
    base64::engine::general_purpose::STANDARD
        .decode(filtered)
        .map_err(|_| ())
}

// ── wire payloads ─────────────────────────────────────────────────────

/// `upload.BeginResult`.
#[derive(Debug, Serialize)]
struct BeginResultPayload {
    upload_id: String,
    chunk_bytes: i64,
    expires_at: Timestamp,
    limits: LimitsPayload,
}

#[derive(Debug, Serialize)]
struct LimitsPayload {
    max_files: i64,
    max_file_bytes: i64,
    max_batch_bytes: i64,
}

/// `upload.ChunkResult`.
#[derive(Debug, Serialize)]
struct ChunkResultPayload {
    file_index: i64,
    next_sequence: i64,
    received_bytes: i64,
}

/// `upload.FinishResult`.
#[derive(Debug, Serialize)]
struct FinishResultPayload {
    attachments: Vec<AttachmentPayload>,
}

/// `upload.Attachment` — `path` is `json:"-"` and never leaves the relay.
#[derive(Debug, Serialize)]
struct AttachmentPayload {
    #[serde(rename = "ref")]
    reference: String,
    name: String,
    media_type: String,
    bytes: i64,
    sha256: String,
    expires_at: Timestamp,
}

#[derive(Debug, Serialize)]
struct EmptyPayload {}

#[derive(Clone, Copy)]
enum ResultKind {
    Begin,
    Chunk,
    Finish,
    Cancel,
}

impl ResultKind {
    fn type_str(self) -> &'static str {
        match self {
            Self::Begin => "upload_begin_result",
            Self::Chunk => "upload_chunk_result",
            Self::Finish => "upload_finish_result",
            Self::Cancel => "upload_cancel_result",
        }
    }

    fn wrap(self, message: UploadResultMessage) -> Outbound {
        match self {
            Self::Begin => Outbound::UploadBeginResult(message),
            Self::Chunk => Outbound::UploadChunkResult(message),
            Self::Finish => Outbound::UploadFinishResult(message),
            Self::Cancel => Outbound::UploadCancelResult(message),
        }
    }
}

/// `sendUploadResult` — `{type, request_id, result}`.
fn upload_result(request_id: &str, kind: ResultKind, payload: &impl Serialize) -> Outbound {
    let body = lerdr_core::json::to_string(payload)
        .and_then(serde_json::value::RawValue::from_string)
        .expect("result payloads serialize");
    kind.wrap(UploadResultMessage {
        error: None,
        request_id: Some(request_id.to_owned()),
        result: Some(MaybeNull::Value(RawJson(body))),
        r#type: kind.type_str().to_owned(),
    })
}

/// `sendUploadError` — `{type, request_id, error:{code,args?}}`.
fn upload_error(
    request_id: &str,
    kind: ResultKind,
    code: &str,
    args: BTreeMap<String, serde_json::Value>,
) -> Outbound {
    kind.wrap(UploadResultMessage {
        error: Some(ApiError::new(code, args)),
        request_id: Some(request_id.to_owned()),
        result: None,
        r#type: kind.type_str().to_owned(),
    })
}

// ── shared state ──────────────────────────────────────────────────────

/// The oracle's injectable `now`/`random` seams — used verbatim by the
/// production path through `systemNow`/`crypto/rand` equivalents.
type NowFn = Arc<dyn Fn() -> SystemTime + Send + Sync>;
type RandomFn = Arc<dyn Fn(&mut [u8]) -> Result<(), ()> + Send + Sync>;

/// Shared upload state — one per relay (the oracle's `upload.Manager`).
/// Owns staged sessions, disk persistence, and the finished-attachment
/// index `Resolve` consults. All lifecycle errors are [`UploadError`]s
/// that the handlers translate through [`public_upload_error_code`].
#[derive(Clone)]
pub(crate) struct Uploads {
    inner: Arc<UploadsInner>,
}

struct UploadsInner {
    /// `<runtime-dir>/uploads` — staging root for in-flight sessions and
    /// the published attachment records.
    dir: PathBuf,
    /// `NewManager` failure (`upload_root_*` / quarantine) — the oracle
    /// leaves the manager nil and answers `attachment_upload_unavailable`.
    available: AtomicBool,
    now: NowFn,
    random: RandomFn,
    /// Unique temp-name counter for index writes.
    temp_seq: AtomicU64,
    /// `m.mu` — sessions, tombstones, and the in-memory attachment index.
    shared: Mutex<Shared>,
}

#[derive(Default)]
struct Shared {
    /// `m.sessions` — upload_id → staged session.
    sessions: HashMap<String, Session>,
    /// `m.tombstones` — cancelled/discarded sessions kept for their TTL.
    tombstones: HashMap<String, Tombstone>,
    /// `m.attachments` — ref → published record.
    attachments: HashMap<String, AttachmentRecord>,
}

/// `upload.Session` — per-file `*os.File`s stay open while chunks stream.
struct Session {
    target: TargetRef,
    owner: String,
    expires_at: SystemTime,
    files: Vec<SessionFile>,
    /// Index of the file currently being filled (`current`).
    current: usize,
    /// Global chunk counter across all files (`sequence`).
    sequence: i64,
}

/// `upload.SessionFile` — staged `.part` plus the running digest and the
/// head/tail buffers the sniffer reads.
struct SessionFile {
    spec: FileSpec,
    rel_path: String,
    file: Option<File>,
    received: i64,
    hash: Sha256,
    prefix: Vec<u8>,
    suffix: Vec<u8>,
    /// Incomplete UTF-8 tail carried between chunks.
    utf8_tail: Vec<u8>,
    invalid_utf: bool,
    has_nul: bool,
}

impl SessionFile {
    fn new(spec: FileSpec) -> Self {
        Self {
            spec,
            rel_path: String::new(),
            file: None,
            received: 0,
            hash: Sha256::new(),
            prefix: Vec::new(),
            suffix: Vec::new(),
            utf8_tail: Vec::new(),
            invalid_utf: false,
            has_nul: false,
        }
    }
}

impl Session {
    fn close_files(&mut self) {
        for item in &mut self.files {
            drop(item.file.take());
        }
    }
}

/// `upload.Tombstone` — keeps a discarded session's target for its TTL so
/// a late `upload_cancel` still answers scope-mismatch instead of
/// pretending success.
struct Tombstone {
    target: TargetRef,
    expires_at: SystemTime,
}

/// `upload.Attachment` — the resolved record; `path` stays relay-side.
#[derive(Debug, Clone)]
pub(crate) struct Attachment {
    pub reference: String,
    pub name: String,
    pub media_type: String,
    pub bytes: i64,
    pub sha256: String,
    pub expires_at: SystemTime,
    #[allow(dead_code)] // read once input.rs wires expansion
    pub path: PathBuf,
}

/// `upload.attachmentRecord` — attachment plus scope and the
/// "restored from disk" bit that loosens scope matching.
#[derive(Clone)]
struct AttachmentRecord {
    attachment: Attachment,
    target: TargetRef,
    rel_path: String,
    #[allow(dead_code)] // read once input.rs wires expansion
    persisted_scope: bool,
}

/// `attachmentIndexPublishedError` — persist failures that happen *after*
/// the index rename leave the in-memory records published.
struct PersistFailure {
    error: UploadError,
    published: bool,
}

impl Uploads {
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self::build(
            dir,
            Arc::new(SystemTime::now),
            Arc::new(|buffer: &mut [u8]| {
                use rand::RngCore as _;
                rand::rng().fill_bytes(buffer);
                Ok(())
            }),
        )
    }

    #[cfg(test)]
    fn with_clock(dir: PathBuf, now: NowFn, random: RandomFn) -> Self {
        Self::build(dir, now, random)
    }

    fn build(dir: PathBuf, now: NowFn, random: RandomFn) -> Self {
        let uploads = Self {
            inner: Arc::new(UploadsInner {
                dir,
                available: AtomicBool::new(true),
                now,
                random,
                temp_seq: AtomicU64::new(0),
                shared: Mutex::new(Shared::default()),
            }),
        };
        if let Err(error) = uploads.initialize() {
            tracing::warn!(
                error = %error,
                root = %uploads.inner.dir.display(),
                "attachment uploads unavailable"
            );
            uploads.inner.available.store(false, Ordering::Relaxed);
        }
        uploads
    }

    /// `s.uploadM != nil` — `NewManager` succeeded.
    fn is_available(&self) -> bool {
        self.inner.available.load(Ordering::Relaxed)
    }

    fn now(&self) -> SystemTime {
        (self.inner.now)()
    }

    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.inner.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `<dir>/<rel>` — every `rel` is manager-generated (`sessions/<id>`,
    /// `objects/<ref><ext>`, index files), never client input.
    fn root(&self, rel: &str) -> PathBuf {
        debug_assert!(
            Path::new(rel)
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
            "unsafe manager-relative path {rel}"
        );
        self.inner.dir.join(rel)
    }

    /// `NewManager`'s startup sequence.
    fn initialize(&self) -> Result<(), UploadError> {
        fs::create_dir_all(&self.inner.dir)
            .map_err(|_| UploadError::new("upload_root_unavailable"))?;
        let info = fs::symlink_metadata(&self.inner.dir)
            .map_err(|_| UploadError::new("upload_root_unavailable"))?;
        if info.file_type().is_symlink() || !info.is_dir() {
            return Err(UploadError::new("upload_root_unsafe"));
        }
        fs::set_permissions(&self.inner.dir, Permissions::from_mode(0o700))
            .map_err(|_| UploadError::new("upload_root_unavailable"))?;
        self.ensure_private_directory(SESSIONS_DIR)?;
        self.ensure_private_directory(OBJECTS_DIR)?;
        self.clear_session_disk()?;
        match self.load_attachments() {
            Err(load_error) => {
                self.lock().attachments.clear();
                // `quarantineIndex` failure aborts NewManager itself.
                self.quarantine_index()?;
                tracing::warn!(
                    error = %load_error,
                    root = %self.inner.dir.display(),
                    "discarding invalid attachment index"
                );
            }
            Ok(0) => {}
            Ok(dropped) => {
                tracing::warn!(
                    dropped,
                    root = %self.inner.dir.display(),
                    "dropped unusable attachment records"
                );
            }
        }
        self.cleanup();
        self.persist().map_err(|failure| failure.error)?;
        Ok(())
    }

    /// `ensurePrivateDirectory` — create (or validate) a private child.
    fn ensure_private_directory(&self, name: &str) -> Result<(), UploadError> {
        let path = self.inner.dir.join(name);
        match fs::create_dir(&path) {
            Err(error) if error.kind() != ErrorKind::AlreadyExists => {
                return Err(UploadError::new("upload_root_unavailable"));
            }
            _ => {}
        }
        let info =
            fs::symlink_metadata(&path).map_err(|_| UploadError::new("upload_root_unavailable"))?;
        if info.file_type().is_symlink() || !info.is_dir() {
            return Err(UploadError::new("upload_root_unsafe"));
        }
        fs::set_permissions(&path, Permissions::from_mode(0o700))
            .map_err(|_| UploadError::new("upload_root_unavailable"))
    }

    /// `clearSessionDiskLocked` — wipe leftover staged sessions at startup.
    fn clear_session_disk(&self) -> Result<(), UploadError> {
        let entries = fs::read_dir(self.inner.dir.join(SESSIONS_DIR))
            .map_err(|_| UploadError::new("upload_root_unavailable"))?;
        for entry in entries {
            let entry = entry.map_err(|_| UploadError::new("upload_root_unavailable"))?;
            let file_type = entry
                .file_type()
                .map_err(|_| UploadError::new("upload_root_unavailable"))?;
            // DirEntry types don't follow links — a symlinked "session"
            // dir removes as a file, matching Go's `entry.IsDir()` path.
            if file_type.is_dir() {
                fs::remove_dir_all(entry.path())
                    .map_err(|_| UploadError::new("upload_root_unavailable"))?;
            } else {
                fs::remove_file(entry.path())
                    .map_err(|_| UploadError::new("upload_root_unavailable"))?;
            }
        }
        Ok(())
    }

    /// `quarantineIndex` — rename a corrupt index aside for forensics.
    fn quarantine_index(&self) -> Result<(), UploadError> {
        let invalid = || UploadError::new("upload_metadata_invalid");
        let stamp = epoch_nanos(self.now());
        for attempt in 0..1000u32 {
            let name = if attempt == 0 {
                format!("attachments.invalid-{stamp}.json")
            } else {
                format!("attachments.invalid-{stamp}-{attempt}.json")
            };
            let path = self.inner.dir.join(&name);
            match fs::symlink_metadata(&path) {
                Ok(_) => continue,
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    fs::rename(self.inner.dir.join(INDEX_FILENAME), &path)
                        .map_err(|_| invalid())?;
                    return Ok(());
                }
                Err(_) => return Err(invalid()),
            }
        }
        Err(invalid())
    }

    /// `m.opaqueID` — 192 bits of entropy, base64url-no-pad (32 chars).
    fn opaque_id(&self) -> Result<String, UploadError> {
        let mut buffer = [0u8; 24];
        (self.inner.random)(&mut buffer).map_err(|_| UploadError::new("upload_id_unavailable"))?;
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buffer))
    }

    /// `m.Begin`.
    fn begin(&self, owner: &str, request: BeginRequest) -> Result<BeginResultPayload, UploadError> {
        self.cleanup();
        let owner = {
            let trimmed = owner.trim();
            if trimmed.is_empty() {
                "unattributed".to_owned()
            } else {
                trimmed.to_owned()
            }
        };
        if !valid_target(&request.target) {
            return Err(UploadError::new("upload_target_invalid"));
        }
        if request.files.is_empty() || request.files.len() > MAX_FILES {
            return Err(UploadError::with_args(
                "upload_batch_count_invalid",
                &[("max_files", MAX_FILES as i64)],
            ));
        }
        let mut total = 0i64;
        let mut files = Vec::with_capacity(request.files.len());
        for spec in &request.files {
            let normalized = normalize_spec(spec)?;
            if total > MAX_BATCH_BYTES - normalized.bytes {
                return Err(UploadError::with_args(
                    "upload_batch_too_large",
                    &[("max_bytes", MAX_BATCH_BYTES)],
                ));
            }
            total += normalized.bytes;
            files.push(SessionFile::new(normalized));
        }
        let upload_id = self.opaque_id()?;
        let expires_at = self.now() + SESSION_TTL;
        let mut session = Session {
            target: request.target,
            owner,
            expires_at,
            files,
            current: 0,
            sequence: 0,
        };
        let mut shared = self.lock();
        if shared.sessions.len() >= MAX_SESSIONS {
            return Err(UploadError::with_args(
                "upload_session_limit",
                &[("max_sessions", MAX_SESSIONS as i64)],
            ));
        }
        let owner_sessions = shared
            .sessions
            .values()
            .filter(|existing| existing.owner == session.owner)
            .count();
        if owner_sessions >= MAX_SESSIONS_PER_OWNER {
            return Err(UploadError::with_args(
                "upload_session_limit",
                &[("max_sessions", MAX_SESSIONS_PER_OWNER as i64)],
            ));
        }
        if shared.sessions.contains_key(&upload_id) {
            return Err(UploadError::new("upload_staging_failed"));
        }
        if let Err(error) = self.stage_session(&upload_id, &mut session) {
            self.discard_staging(&upload_id, &mut session);
            return Err(error);
        }
        shared.sessions.insert(upload_id.clone(), session);
        Ok(BeginResultPayload {
            upload_id,
            chunk_bytes: CHUNK_BYTES as i64,
            expires_at: Timestamp(expires_at),
            limits: LimitsPayload {
                max_files: MAX_FILES as i64,
                max_file_bytes: MAX_FILE_BYTES,
                max_batch_bytes: MAX_BATCH_BYTES,
            },
        })
    }

    /// `root.Mkdir("sessions/<id>")` + `NNNN.part` files at 0600.
    fn stage_session(&self, upload_id: &str, session: &mut Session) -> Result<(), UploadError> {
        let staging = || UploadError::new("upload_staging_failed");
        let dir_rel = format!("{SESSIONS_DIR}/{upload_id}");
        fs::create_dir(self.root(&dir_rel)).map_err(|_| staging())?;
        fs::set_permissions(self.root(&dir_rel), Permissions::from_mode(0o700))
            .map_err(|_| staging())?;
        for (index, item) in session.files.iter_mut().enumerate() {
            item.rel_path = format!("{dir_rel}/{index:04}.part");
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(self.root(&item.rel_path))
                .map_err(|_| staging())?;
            item.file = Some(file);
        }
        Ok(())
    }

    /// `discardSession(session, false)` for a session that never made it
    /// into the map — close handles and remove the staged tree.
    fn discard_staging(&self, upload_id: &str, session: &mut Session) {
        session.close_files();
        let _ = fs::remove_dir_all(self.root(&format!("{SESSIONS_DIR}/{upload_id}")));
    }

    /// `m.Chunk`.
    fn chunk(&self, request: &ChunkRequest) -> Result<ChunkResultPayload, UploadError> {
        let mut shared = self.lock();
        let now = self.now();
        let session = lock_session(
            &mut shared,
            &self.inner.dir,
            now,
            &request.upload_id,
            &request.target,
        )?;
        let result = chunk_locked(session, request);
        // Any post-lock failure discards the session (with a tombstone).
        if result.is_err() {
            discard_locked(&mut shared, &self.inner.dir, &request.upload_id, true);
        }
        result
    }

    /// `m.Finish`.
    fn finish(&self, request: &FinishRequest) -> Result<FinishResultPayload, UploadError> {
        let mut shared = self.lock();
        let now = self.now();
        let session = lock_session(
            &mut shared,
            &self.inner.dir,
            now,
            &request.upload_id,
            &request.target,
        )?;
        if let Err(error) = self.verify_session(session, request) {
            discard_locked(&mut shared, &self.inner.dir, &request.upload_id, true);
            return Err(error);
        }
        // Like the oracle's `fail`, every post-lock error discards the
        // session (with a tombstone) — success does the same.
        let result = self.publish(&mut shared, &request.upload_id);
        discard_locked(&mut shared, &self.inner.dir, &request.upload_id, true);
        result.map(|attachments| FinishResultPayload { attachments })
    }

    /// `m.Cancel` — idempotent for unknown and already-cancelled ids; a
    /// live session or live tombstone under a different target answers
    /// `upload_scope_mismatch`.
    fn cancel(&self, target: &TargetRef, upload_id: &str) -> Result<(), UploadError> {
        let mut shared = self.lock();
        let now = self.now();
        if !shared.sessions.contains_key(upload_id) {
            if shared
                .tombstones
                .get(upload_id)
                .is_some_and(|previous| now >= previous.expires_at)
            {
                shared.tombstones.remove(upload_id);
            }
            match shared.tombstones.get(upload_id) {
                Some(previous) if previous.target != *target => {
                    return Err(UploadError::new("upload_scope_mismatch"))
                }
                _ => return Ok(()),
            }
        }
        if shared.sessions[upload_id].target != *target {
            return Err(UploadError::new("upload_scope_mismatch"));
        }
        discard_locked(&mut shared, &self.inner.dir, upload_id, true);
        Ok(())
    }

    /// `m.Resolve` — consult the published index; on a stale/changed
    /// object the record is evicted and the index rewritten.
    #[allow(dead_code)] // read once input.rs wires expansion
    fn resolve(&self, target: &TargetRef, reference: &str) -> Result<Attachment, UploadError> {
        self.cleanup();
        let mut shared = self.lock();
        let Some(record) = shared.attachments.get(reference).cloned() else {
            return Err(UploadError::new("attachment_not_found"));
        };
        if !attachment_target_matches(&record, target) {
            return Err(UploadError::new("attachment_scope_mismatch"));
        }
        let outcome =
            open_attachment(&self.inner.dir, &record.rel_path).and_then(|(file, info)| {
                drop(file);
                if info.len() == record.attachment.bytes as u64 {
                    Ok(())
                } else {
                    Err(UploadError::new("attachment_changed"))
                }
            });
        match outcome {
            Ok(()) => Ok(record.attachment),
            Err(error) => {
                shared.attachments.remove(reference);
                let _ = fs::remove_file(self.root(&record.rel_path));
                if let Err(failure) = self.persist_locked(&shared) {
                    return Err(failure.error);
                }
                Err(error)
            }
        }
    }

    /// `m.Cleanup` — expired sessions/tombstones/attachments plus a disk
    /// sweep for orphans, symlinks, and legacy root files.
    fn cleanup(&self) -> usize {
        let now = self.now();
        let mut shared = self.lock();
        let mut removed = 0usize;
        let expired_sessions: Vec<String> = shared
            .sessions
            .iter()
            .filter(|(_, session)| now >= session.expires_at)
            .map(|(id, _)| id.clone())
            .collect();
        for upload_id in expired_sessions {
            if let Some(mut session) = shared.sessions.remove(&upload_id) {
                session.close_files();
                let _ = fs::remove_dir_all(self.root(&format!("{SESSIONS_DIR}/{upload_id}")));
                removed += 1;
            }
        }
        shared
            .tombstones
            .retain(|_, tombstone| now < tombstone.expires_at);
        let expired_attachments: Vec<String> = shared
            .attachments
            .iter()
            .filter(|(_, record)| now >= record.attachment.expires_at)
            .map(|(reference, _)| reference.clone())
            .collect();
        for reference in &expired_attachments {
            if let Some(record) = shared.attachments.remove(reference) {
                let _ = fs::remove_file(self.root(&record.rel_path));
                removed += 1;
            }
        }
        removed += self.cleanup_disk(&shared, now);
        if !expired_attachments.is_empty() {
            let _ = self.persist_locked(&shared);
        }
        removed
    }

    /// `cleanupDiskLocked` — sweeps sessions/, objects/, and legacy root
    /// upload files (`timestamp-hash.ext` from the pre-session uploader).
    fn cleanup_disk(&self, shared: &Shared, now: SystemTime) -> usize {
        let indexed: HashSet<&str> = shared
            .attachments
            .values()
            .map(|record| record.rel_path.as_str())
            .collect();
        let mut removed = 0usize;
        // `{"sessions", sessionTTL}, {"objects", attachTTL}` — sessions/
        // drops only dirs, objects/ drops only regular files.
        let sweep = |dir_name: &str, ttl: Duration, removed: &mut usize| {
            let Ok(entries) = fs::read_dir(self.root(dir_name)) else {
                return;
            };
            for entry in entries.flatten() {
                let rel_path = format!("{dir_name}/{}", entry.file_name().to_string_lossy());
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                if meta.file_type().is_symlink() {
                    if fs::remove_file(entry.path()).is_ok() {
                        *removed += 1;
                    }
                    continue;
                }
                if dir_name == OBJECTS_DIR && indexed.contains(rel_path.as_str()) {
                    continue;
                }
                let Ok(age) = now.duration_since(meta.modified().unwrap_or(UNIX_EPOCH)) else {
                    continue;
                };
                if age < ttl {
                    continue;
                }
                if dir_name == SESSIONS_DIR && meta.is_dir() {
                    if fs::remove_dir_all(entry.path()).is_ok() {
                        *removed += 1;
                    }
                } else if dir_name == OBJECTS_DIR
                    && meta.is_file()
                    && fs::remove_file(entry.path()).is_ok()
                {
                    *removed += 1;
                }
            }
        };
        sweep(SESSIONS_DIR, SESSION_TTL, &mut removed);
        sweep(OBJECTS_DIR, ATTACHMENT_TTL, &mut removed);
        if let Ok(entries) = fs::read_dir(&self.inner.dir) {
            for entry in entries.flatten() {
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                if !meta.is_file() {
                    continue;
                }
                let file_name = entry.file_name();
                let Some(name) = file_name.to_str() else {
                    continue;
                };
                if !legacy_upload_filename(name) {
                    continue;
                }
                let Ok(age) = now.duration_since(meta.modified().unwrap_or(UNIX_EPOCH)) else {
                    continue;
                };
                if age < ATTACHMENT_TTL {
                    continue;
                }
                if fs::remove_file(entry.path()).is_ok() {
                    removed += 1;
                }
            }
        }
        removed
    }

    /// `m.persistAttachmentsLocked` — write `attachments.json` through a
    /// temp file + rename + dir sync. Post-rename failures mark
    /// [`PersistFailure::published`].
    fn persist(&self) -> Result<(), PersistFailure> {
        let shared = self.lock();
        self.persist_locked(&shared)
    }

    fn persist_locked(&self, shared: &Shared) -> Result<(), PersistFailure> {
        let unavailable = || UploadError::new("upload_metadata_unavailable");
        let fail = |error: UploadError| PersistFailure {
            error,
            published: false,
        };
        let published = |error: UploadError| PersistFailure {
            error,
            published: true,
        };
        let mut records: Vec<DiskAttachmentRecord> =
            shared.attachments.values().map(disk_record).collect();
        records.sort_by(|a, b| a.attachment.reference.cmp(&b.attachment.reference));
        let mut data = lerdr_core::json::to_vec(&DiskIndex {
            schema_version: INDEX_VERSION,
            attachments: records,
        })
        .map_err(|_| fail(unavailable()))?;
        data.push(b'\n');
        let stamp = epoch_nanos(self.now());
        let seq = self.inner.temp_seq.fetch_add(1, Ordering::Relaxed);
        let temp_path = self
            .inner
            .dir
            .join(format!(".attachments-{stamp:x}-{seq:x}.tmp"));
        let mut temp = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp_path)
        {
            Ok(file) => file,
            Err(_) => return Err(fail(unavailable())),
        };
        let result = (|| -> Result<(), PersistFailure> {
            temp.write_all(&data).map_err(|_| fail(unavailable()))?;
            temp.sync_all().map_err(|_| fail(unavailable()))?;
            drop(temp);
            fs::rename(&temp_path, self.inner.dir.join(INDEX_FILENAME))
                .map_err(|_| fail(unavailable()))?;
            // Sync the directory so the rename is durable — failures here
            // leave the index already published.
            let dir = File::open(&self.inner.dir).map_err(|_| published(unavailable()))?;
            dir.sync_all().map_err(|_| published(unavailable()))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        result
    }

    /// `m.loadAttachments` — rehydrate `attachments.json`; any structural
    /// failure invalidates the whole file (caller quarantines), while a
    /// bad/expired/missing record is dropped individually.
    fn load_attachments(&self) -> Result<usize, UploadError> {
        let invalid = || UploadError::new("upload_metadata_invalid");
        let index_path = self.inner.dir.join(INDEX_FILENAME);
        let info = match fs::symlink_metadata(&index_path) {
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(0),
            Err(_) => return Err(invalid()),
            Ok(info) => info,
        };
        if info.file_type().is_symlink() || !info.is_file() {
            return Err(invalid());
        }
        let mut data = Vec::new();
        File::open(&index_path)
            .map_err(|_| invalid())?
            .take(MAX_INDEX_BYTES + 1)
            .read_to_end(&mut data)
            .map_err(|_| invalid())?;
        if data.len() as u64 > MAX_INDEX_BYTES {
            return Err(invalid());
        }
        let index: DiskIndex = serde_json::from_slice(&data).map_err(|_| invalid())?;
        if index.schema_version != INDEX_VERSION {
            return Err(invalid());
        }
        let now = self.now();
        let mut dropped = 0usize;
        let mut shared = self.lock();
        for stored in index.attachments {
            let attachment = stored.attachment;
            let spec_ok = normalize_spec(&FileSpec {
                name: attachment.name.clone(),
                media_type: attachment.media_type.clone(),
                bytes: attachment.bytes,
            })
            .is_ok();
            let id_ok = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(attachment.reference.as_bytes())
                .map(|decoded| decoded.len() == 24)
                .unwrap_or(false);
            let expected_path = format!(
                "{OBJECTS_DIR}/{}{}",
                attachment.reference,
                canonical_extension(&attachment.media_type)
            );
            if !id_ok
                || !spec_ok
                || !valid_digest(&attachment.sha256)
                || !valid_disk_target(&stored.target)
                || stored.rel_path != expected_path
                || is_zero_time(attachment.expires_at.0)
            {
                dropped += 1;
                continue;
            }
            if now >= attachment.expires_at.0 {
                let _ = fs::remove_file(self.root(&stored.rel_path));
                continue;
            }
            if shared.attachments.contains_key(&attachment.reference) {
                dropped += 1;
                continue;
            }
            let record = AttachmentRecord {
                attachment: Attachment {
                    reference: attachment.reference,
                    name: attachment.name,
                    media_type: attachment.media_type,
                    bytes: attachment.bytes,
                    sha256: attachment.sha256,
                    expires_at: attachment.expires_at.0,
                    path: self.root(&stored.rel_path),
                },
                target: stored.target.to_target_ref(),
                rel_path: stored.rel_path,
                persisted_scope: true,
            };
            match open_attachment(&self.inner.dir, &record.rel_path) {
                Err(_) => dropped += 1,
                Ok((file, meta)) => {
                    if meta.len() != record.attachment.bytes as u64 {
                        dropped += 1;
                    } else {
                        shared
                            .attachments
                            .insert(record.attachment.reference.clone(), record);
                    }
                    drop(file);
                }
            }
        }
        Ok(dropped)
    }

    /// `m.syncObjectsDirectory`.
    fn sync_objects_directory(&self) -> Result<(), UploadError> {
        let dir = File::open(self.root(OBJECTS_DIR))
            .map_err(|_| UploadError::new("upload_metadata_unavailable"))?;
        dir.sync_all()
            .map_err(|_| UploadError::new("upload_metadata_unavailable"))
    }

    /// `m.Finish` phase 1 — completeness, final digests, content, sync,
    /// and the same-inode staging check.
    fn verify_session(
        &self,
        session: &mut Session,
        request: &FinishRequest,
    ) -> Result<(), UploadError> {
        if session.current != session.files.len() || request.files.len() != session.files.len() {
            return Err(UploadError::new("upload_incomplete"));
        }
        for (index, item) in session.files.iter_mut().enumerate() {
            let claimed = &request.files[index];
            let actual = hex::encode(item.hash.clone().finalize());
            if claimed.file_index != index as i64
                || !valid_digest(&claimed.sha256)
                || !ct_eq(actual.as_bytes(), claimed.sha256.to_lowercase().as_bytes())
            {
                return Err(UploadError::with_args(
                    "upload_final_digest_mismatch",
                    &[("file_index", index as i64)],
                ));
            }
            if item.received != item.spec.bytes {
                return Err(UploadError::with_args(
                    "upload_incomplete",
                    &[("file_index", index as i64)],
                ));
            }
            validate_content(item)?;
            let file = item
                .file
                .as_mut()
                .ok_or_else(|| UploadError::new("upload_staging_failed"))?;
            file.sync_all()
                .map_err(|_| UploadError::new("upload_staging_failed"))?;
            let changed = || {
                UploadError::with_args("upload_staging_changed", &[("file_index", index as i64)])
            };
            let path_info =
                fs::symlink_metadata(self.root(&item.rel_path)).map_err(|_| changed())?;
            let opened_info = file.metadata().map_err(|_| changed())?;
            if path_info.file_type().is_symlink()
                || !path_info.is_file()
                || !same_file(&path_info, &opened_info)
                || opened_info.len() != item.spec.bytes as u64
            {
                return Err(changed());
            }
        }
        Ok(())
    }

    /// `m.Finish` phase 2 — move `.part` files to `objects/<ref><ext>`,
    /// register records, persist the index. Rolls back created files and
    /// records on failure (except post-rename dir-sync failures).
    fn publish(
        &self,
        shared: &mut Shared,
        upload_id: &str,
    ) -> Result<Vec<AttachmentPayload>, UploadError> {
        let expires_at = self.now() + ATTACHMENT_TTL;
        let mut created: Vec<String> = Vec::new();
        let mut pending: Vec<AttachmentRecord> = Vec::new();
        let mut attachments: Vec<AttachmentPayload> = Vec::new();
        let mut stage = || -> Option<UploadError> {
            let session = shared.sessions.get_mut(upload_id)?;
            for item in &mut session.files {
                let reference = match self.opaque_id() {
                    Ok(reference) => reference,
                    Err(error) => return Some(error),
                };
                let rel_path = format!(
                    "{OBJECTS_DIR}/{reference}{}",
                    canonical_extension(&item.spec.media_type)
                );
                // `item.file.Close()` — std drops silently; `sync_all`
                // above already covered durability.
                drop(item.file.take());
                if fs::rename(self.root(&item.rel_path), self.root(&rel_path)).is_err() {
                    return Some(UploadError::new("upload_staging_failed"));
                }
                created.push(rel_path.clone());
                let attachment = Attachment {
                    reference,
                    name: item.spec.name.clone(),
                    media_type: item.spec.media_type.clone(),
                    bytes: item.spec.bytes,
                    sha256: hex::encode(item.hash.clone().finalize()),
                    expires_at,
                    path: self.root(&rel_path),
                };
                pending.push(AttachmentRecord {
                    attachment: attachment.clone(),
                    target: session.target.clone(),
                    rel_path,
                    persisted_scope: false,
                });
                attachments.push(AttachmentPayload {
                    reference: attachment.reference,
                    name: attachment.name,
                    media_type: attachment.media_type,
                    bytes: attachment.bytes,
                    sha256: attachment.sha256,
                    expires_at: Timestamp(attachment.expires_at),
                });
            }
            None
        };
        // Rollback: staged files already moved under objects/ are
        // removed; index records are only removed once they were inserted
        // (the persist-failure path below).
        let remove_created = |created: &[String]| {
            for rel_path in created {
                let _ = fs::remove_file(self.root(rel_path));
            }
        };
        if let Some(error) = stage() {
            remove_created(&created);
            return Err(error);
        }
        if let Err(error) = self.sync_objects_directory() {
            remove_created(&created);
            return Err(error);
        }
        if pending.iter().any(|record| {
            shared
                .attachments
                .contains_key(&record.attachment.reference)
        }) {
            remove_created(&created);
            return Err(UploadError::new("upload_staging_failed"));
        }
        for record in &pending {
            shared
                .attachments
                .insert(record.attachment.reference.clone(), record.clone());
        }
        match self.persist_locked(shared) {
            Ok(()) => Ok(attachments),
            Err(failure) if failure.published => Err(failure.error),
            Err(failure) => {
                for record in &pending {
                    shared.attachments.remove(&record.attachment.reference);
                }
                remove_created(&created);
                Err(failure.error)
            }
        }
    }
}

/// `lockSession` — fetch a live session, expiring it on the spot.
fn lock_session<'a>(
    shared: &'a mut Shared,
    root: &Path,
    now: SystemTime,
    upload_id: &str,
    target: &TargetRef,
) -> Result<&'a mut Session, UploadError> {
    let Some(session) = shared.sessions.get(upload_id) else {
        return Err(UploadError::new("upload_session_not_found"));
    };
    if now >= session.expires_at {
        let mut session = shared.sessions.remove(upload_id).expect("checked above");
        session.close_files();
        let _ = fs::remove_dir_all(root.join(SESSIONS_DIR).join(upload_id));
        return Err(UploadError::new("upload_session_expired"));
    }
    let session = shared.sessions.get_mut(upload_id).expect("checked above");
    if session.target != *target {
        return Err(UploadError::new("upload_scope_mismatch"));
    }
    Ok(session)
}

/// `discardSession` — close handles, remove the staged tree, optionally
/// record a tombstone.
fn discard_locked(shared: &mut Shared, root: &Path, upload_id: &str, tombstone: bool) {
    let Some(mut session) = shared.sessions.remove(upload_id) else {
        return;
    };
    session.close_files();
    let _ = fs::remove_dir_all(root.join(SESSIONS_DIR).join(upload_id));
    if tombstone {
        shared.tombstones.insert(
            upload_id.to_owned(),
            Tombstone {
                target: session.target,
                expires_at: session.expires_at,
            },
        );
    }
}

/// `m.Chunk`'s in-session checks — ordering, size, digest, capacity,
/// then the append itself.
fn chunk_locked(
    session: &mut Session,
    request: &ChunkRequest,
) -> Result<ChunkResultPayload, UploadError> {
    if session.current >= session.files.len()
        || request.file_index != session.current as i64
        || request.sequence != session.sequence
    {
        return Err(UploadError::with_args(
            "upload_chunk_out_of_order",
            &[
                ("expected_file_index", session.current as i64),
                ("expected_sequence", session.sequence),
            ],
        ));
    }
    if request.data.is_empty() || request.data.len() > CHUNK_BYTES {
        return Err(UploadError::with_args(
            "upload_chunk_size_invalid",
            &[("max_bytes", CHUNK_BYTES as i64)],
        ));
    }
    if !valid_digest(&request.sha256) || !digest_matches(&request.data, &request.sha256) {
        return Err(UploadError::new("upload_chunk_digest_mismatch"));
    }
    let item = &mut session.files[session.current];
    if item.received > item.spec.bytes - request.data.len() as i64 {
        return Err(UploadError::with_args(
            "upload_file_too_large",
            &[("expected_bytes", item.spec.bytes)],
        ));
    }
    let file = item
        .file
        .as_mut()
        .ok_or_else(|| UploadError::new("upload_staging_failed"))?;
    file.write_all(&request.data)
        .map_err(|_| UploadError::new("upload_staging_failed"))?;
    item.hash.update(&request.data);
    item.received += request.data.len() as i64;
    capture(item, &request.data);
    session.sequence += 1;
    let result = ChunkResultPayload {
        file_index: request.file_index,
        next_sequence: session.sequence,
        received_bytes: item.received,
    };
    if item.received == item.spec.bytes {
        session.current += 1;
    }
    Ok(result)
}

// ── validation helpers ────────────────────────────────────────────────

/// `validTarget` — the manager-side target sanity check. The oracle
/// only requires non-empty fields here; the `server_session_id ==
/// "primary"` pin lives in the handler-level `validateUploadTarget`.
fn valid_target(target: &TargetRef) -> bool {
    !target.server_session_id.trim().is_empty()
        && !target.pane_id.trim().is_empty()
        && !target.terminal_id.trim().is_empty()
        && target.generation >= 0
}

/// `validDiskTarget` — persisted records carry no generation, so only
/// the non-empty fields are checked.
fn valid_disk_target(target: &DiskAttachmentTarget) -> bool {
    !target.server_session_id.trim().is_empty()
        && !target.pane_id.trim().is_empty()
        && !target.terminal_id.trim().is_empty()
}

/// `attachmentTargetMatches` — full equality for same-run records;
/// persisted records ignore `generation` (the oracle's restart seam).
#[allow(dead_code)] // read once input.rs wires expansion
fn attachment_target_matches(record: &AttachmentRecord, target: &TargetRef) -> bool {
    if !record.persisted_scope {
        return record.target == *target;
    }
    record.target.server_session_id == target.server_session_id
        && record.target.pane_id == target.pane_id
        && record.target.terminal_id == target.terminal_id
        && record.target.agent_session_id == target.agent_session_id
}

/// `normalizeSpec` — media-type casing, size bound, name hygiene,
/// type/extension agreement.
fn normalize_spec(spec: &FileSpec) -> Result<FileSpec, UploadError> {
    let mut spec = spec.clone();
    // `strings.ToLower(strings.TrimSpace(strings.Split(m, ";")[0]))`
    spec.media_type = spec
        .media_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    // The oracle validates the *raw* name: `TrimSpace` only decides
    // emptiness, `len` bounds the untrimmed string, and the name is
    // carried through unmodified (spaces included). `utf8.ValidString`
    // and `filepath.Base(name) != name` are no-ops here — Rust strings
    // are always UTF-8 and `/` is already rejected.
    if spec.name.trim().is_empty()
        || spec.name.len() > MAX_NAME_BYTES
        || spec.name == "."
        || spec.name == ".."
        || spec.name.contains(['/', '\\', '\u{0}', '\r', '\n'])
    {
        return Err(UploadError::new("upload_name_invalid"));
    }
    if spec.bytes < 1 || spec.bytes > MAX_FILE_BYTES {
        return Err(UploadError::with_args(
            "upload_file_size_invalid",
            &[("max_bytes", MAX_FILE_BYTES)],
        ));
    }
    let Some(allowed) = allowed_extensions(&spec.media_type) else {
        return Err(UploadError::new("upload_type_unsupported"));
    };
    let extension = path_ext(&spec.name);
    if !allowed.contains(&extension.as_str()) {
        return Err(UploadError::new("upload_extension_mismatch"));
    }
    Ok(spec)
}

/// `filepath.Ext` + `strings.ToLower` — suffix from the final dot.
fn path_ext(name: &str) -> String {
    match name.rfind('.') {
        Some(index) => name[index..].to_lowercase(),
        None => String::new(),
    }
}

/// `validDigest` — 64 lowercase-or-uppercase hex chars.
fn valid_digest(value: &str) -> bool {
    value.len() == 64 && hex::decode(value).is_ok()
}

/// `digestMatches` — constant-time compare against the claimed hex.
fn digest_matches(data: &[u8], claimed: &str) -> bool {
    let actual = hex::encode(Sha256::digest(data));
    ct_eq(actual.as_bytes(), claimed.to_lowercase().as_bytes())
}

/// `subtle.ConstantTimeCompare`.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// `validAttachmentReference` — exactly 32 chars of `[A-Za-z0-9_-]`.
#[allow(dead_code)] // read once input.rs wires expansion
fn valid_attachment_reference(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// `legacyUploadFilename` — `^[0-9]{8}-[0-9]{6}-[0-9a-fA-F]{8}-.+\.(ext)`
/// for the pre-session uploader's root files.
fn legacy_upload_filename(name: &str) -> bool {
    const EXTS: [&str; 8] = ["png", "jpg", "jpeg", "webp", "gif", "heic", "heif", "img"];
    let bytes = name.as_bytes();
    if bytes.len() < 26 {
        return false;
    }
    let digit = |b: &u8| b.is_ascii_digit();
    let hex_digit = |b: &u8| b.is_ascii_hexdigit();
    if !bytes[..8].iter().all(digit)
        || bytes[8] != b'-'
        || !bytes[9..15].iter().all(digit)
        || bytes[15] != b'-'
        || !bytes[16..24].iter().all(hex_digit)
        || bytes[24] != b'-'
    {
        return false;
    }
    let rest = &name[25..];
    EXTS.iter()
        .any(|ext| rest.len() > ext.len() + 1 && rest.ends_with(&format!(".{ext}")))
}

/// `openAttachment` — lstat/open/fstat with the same-inode guarantee.
fn open_attachment(root: &Path, rel_path: &str) -> Result<(File, fs::Metadata), UploadError> {
    let path = root.join(rel_path);
    let info = fs::symlink_metadata(&path).map_err(|_| UploadError::new("attachment_changed"))?;
    if info.file_type().is_symlink() || !info.is_file() {
        return Err(UploadError::new("attachment_changed"));
    }
    let file = File::open(&path).map_err(|_| UploadError::new("attachment_changed"))?;
    let opened = file
        .metadata()
        .map_err(|_| UploadError::new("attachment_changed"))?;
    if !opened.is_file() || !same_file(&info, &opened) {
        return Err(UploadError::new("attachment_changed"));
    }
    Ok((file, opened))
}

fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}

// ── content validation ────────────────────────────────────────────────

const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";
const JPEG_MAGIC: &[u8] = b"\xff\xd8\xff";
const GIF87_MAGIC: &[u8] = b"GIF87a";
const GIF89_MAGIC: &[u8] = b"GIF89a";

/// `validateContent` — magic bytes, text/JSON checks, and the
/// ZIP-based document-container check.
fn validate_content(item: &mut SessionFile) -> Result<(), UploadError> {
    // The oracle stamps `file_index: -1` here — the helper doesn't know
    // the position; the args still ride along inside the public error.
    let mismatch = || UploadError::with_args("upload_content_type_mismatch", &[("file_index", -1)]);
    match item.spec.media_type.as_str() {
        "image/png" if !item.prefix.starts_with(PNG_MAGIC) => return Err(mismatch()),
        "image/jpeg" if !item.prefix.starts_with(JPEG_MAGIC) => return Err(mismatch()),
        "image/gif"
            if !item.prefix.starts_with(GIF87_MAGIC) && !item.prefix.starts_with(GIF89_MAGIC) =>
        {
            return Err(mismatch())
        }
        "image/webp"
            if item.prefix.len() < 12
                || &item.prefix[0..4] != b"RIFF"
                || &item.prefix[8..12] != b"WEBP" =>
        {
            return Err(mismatch())
        }
        "image/heic" | "image/heif" if !valid_iso_image(&item.prefix) => return Err(mismatch()),
        "application/pdf"
            if !item.prefix.starts_with(b"%PDF-")
                || !item
                    .suffix
                    .windows(b"%%EOF".len())
                    .any(|window| window == b"%%EOF") =>
        {
            return Err(mismatch())
        }
        // `validTextContent(item) && validJSONContent(item.file)` — a
        // nil file reads as invalid JSON in the oracle.
        "application/json"
            if !valid_text_content(item) || !item.file.as_mut().is_some_and(valid_json_content) =>
        {
            return Err(mismatch())
        }
        "text/plain" | "text/markdown" | "text/csv" if !valid_text_content(item) => {
            return Err(mismatch())
        }
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
            valid_document_container(item, &["[Content_Types].xml", "word/document.xml"], None)?
        }
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => {
            valid_document_container(item, &["[Content_Types].xml", "xl/workbook.xml"], None)?
        }
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => {
            valid_document_container(item, &["[Content_Types].xml", "ppt/presentation.xml"], None)?
        }
        "application/vnd.oasis.opendocument.text" => valid_document_container(
            item,
            &["mimetype", "content.xml"],
            Some("application/vnd.oasis.opendocument.text"),
        )?,
        "application/vnd.oasis.opendocument.spreadsheet" => valid_document_container(
            item,
            &["mimetype", "content.xml"],
            Some("application/vnd.oasis.opendocument.spreadsheet"),
        )?,
        "application/vnd.oasis.opendocument.presentation" => valid_document_container(
            item,
            &["mimetype", "content.xml"],
            Some("application/vnd.oasis.opendocument.presentation"),
        )?,
        // normalizeSpec only admits the types above — the `_` arm covers
        // media types with no sniff rule (none today).
        _ => {}
    }
    Ok(())
}

/// `capture` — maintain the sniff buffers as chunks stream.
fn capture(item: &mut SessionFile, data: &[u8]) {
    if item.prefix.len() < PREFIX_BYTES {
        let take = (PREFIX_BYTES - item.prefix.len()).min(data.len());
        item.prefix.extend_from_slice(&data[..take]);
    }
    if data.len() >= SUFFIX_BYTES {
        item.suffix.clear();
        item.suffix
            .extend_from_slice(&data[data.len() - SUFFIX_BYTES..]);
    } else {
        item.suffix.extend_from_slice(data);
        if item.suffix.len() > SUFFIX_BYTES {
            let excess = item.suffix.len() - SUFFIX_BYTES;
            item.suffix.drain(..excess);
        }
    }
    if item.spec.media_type.starts_with("text/") || item.spec.media_type == "application/json" {
        if data.contains(&0) {
            item.has_nul = true;
        }
        capture_utf8(item, data);
    }
}

/// `captureUTF8` — flag malformed UTF-8; carry an incomplete trailing
/// sequence into the next chunk.
fn capture_utf8(item: &mut SessionFile, data: &[u8]) {
    let mut combined = std::mem::take(&mut item.utf8_tail);
    combined.extend_from_slice(data);
    match std::str::from_utf8(&combined) {
        Ok(_) => {}
        Err(error) => {
            let rest = &combined[error.valid_up_to()..];
            if error.error_len().is_some() {
                item.invalid_utf = true;
            } else {
                item.utf8_tail = rest.to_vec();
            }
        }
    }
}

/// `validTextContent`.
fn valid_text_content(item: &SessionFile) -> bool {
    !item.invalid_utf && item.utf8_tail.is_empty() && !item.has_nul
}

/// `validJSONContent` — exactly one JSON value, then EOF.
fn valid_json_content(file: &mut File) -> bool {
    if file.seek(SeekFrom::Start(0)).is_err() {
        return false;
    }
    serde_json::from_reader::<_, serde_json::Value>(file).is_ok()
}

/// `validISOImage` — `ftyp` box carrying a HEIF-brand.
fn valid_iso_image(data: &[u8]) -> bool {
    const BRANDS: [[u8; 4]; 8] = [
        *b"heic", *b"heix", *b"hevc", *b"hevx", *b"heim", *b"heis", *b"mif1", *b"msf1",
    ];
    if data.len() < 16 || &data[4..8] != b"ftyp" {
        return false;
    }
    let box_size = u32::from_be_bytes(data[0..4].try_into().expect("slice len")) as usize;
    if box_size < 16 || box_size > data.len() {
        return false;
    }
    if BRANDS.iter().any(|brand| data[8..12] == *brand) {
        return true;
    }
    let mut offset = 16;
    while offset + 4 <= box_size {
        if BRANDS
            .iter()
            .any(|brand| data[offset..offset + 4] == *brand)
        {
            return true;
        }
        offset += 4;
    }
    false
}

// ── document containers (ZIP) ─────────────────────────────────────────

/// One central-directory record — what `validDocumentContainer` needs
/// from `archive/zip`'s `File` struct.
struct ZipEntry {
    name: Vec<u8>,
    method: u16,
    compressed: u64,
    uncompressed: u64,
    local_offset: u64,
    symlink: bool,
}

/// `validDocumentContainer` — real ZIP, clean names, required members,
/// optional stored `mimetype` identity, and the expansion cap.
fn valid_document_container(
    item: &mut SessionFile,
    required: &[&str],
    mime: Option<&str>,
) -> Result<(), UploadError> {
    let mismatch = || UploadError::new("upload_content_type_mismatch");
    let size = item.spec.bytes as u64;
    let file = item
        .file
        .as_mut()
        .ok_or_else(|| UploadError::new("upload_staging_failed"))?;
    let entries = read_central_directory(file, size).ok_or_else(mismatch)?;
    if entries.is_empty() || entries.len() > MAX_CONTAINER_ENTRIES {
        return Err(mismatch());
    }
    let mut present: HashMap<Vec<u8>, &ZipEntry> = HashMap::with_capacity(entries.len());
    for entry in &entries {
        if entry.name.contains(&0) {
            return Err(mismatch());
        }
        if entry.symlink {
            return Err(mismatch());
        }
        let clean = clean_path(&entry.name);
        if clean != entry.name
            || clean.first() == Some(&b'/')
            || clean.starts_with(b"../")
            || clean == b".."
        {
            return Err(mismatch());
        }
        if entry.uncompressed > MAX_EXPANDED_BYTES {
            return Err(mismatch());
        }
        present.insert(entry.name.clone(), entry);
    }
    for name in required {
        if !present.contains_key(name.as_bytes()) {
            return Err(mismatch());
        }
    }
    if let Some(mime) = mime {
        let entry = present.get(b"mimetype".as_ref()).ok_or_else(mismatch)?;
        if entry.method != 0 || entry.uncompressed > 128 {
            return Err(mismatch());
        }
        let content = read_entry_data(file, entry, 129).ok_or_else(mismatch)?;
        if content != mime.as_bytes() {
            return Err(mismatch());
        }
    }
    let mut expanded = 0u64;
    for entry in &entries {
        expanded += entry.uncompressed;
        if expanded > MAX_EXPANDED_BYTES {
            return Err(mismatch());
        }
    }
    Ok(())
}

/// `zip.NewReader` — locate the EOCD (plus ZIP64), then parse every
/// central record. Any structural failure → `None`.
fn read_central_directory(file: &mut File, size: u64) -> Option<Vec<ZipEntry>> {
    const EOCD_SIG: u32 = 0x0605_4b50;
    const EOCD64_SIG: u32 = 0x0606_4b50;
    const EOCD64_LOCATOR_SIG: u32 = 0x0706_4b50;
    const CD_SIG: u32 = 0x0201_4b50;
    if size < 22 {
        return None;
    }
    let tail_len = size.min(22 + u64::from(u16::MAX));
    file.seek(SeekFrom::Start(size - tail_len)).ok()?;
    let mut tail = vec![0u8; tail_len as usize];
    file.read_exact(&mut tail).ok()?;
    let u32le = |buf: &[u8], at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(buf.get(at..at + 4)?.try_into().ok()?))
    };
    let u16le = |buf: &[u8], at: usize| -> Option<u16> {
        Some(u16::from_le_bytes(buf.get(at..at + 2)?.try_into().ok()?))
    };
    let u64le = |buf: &[u8], at: usize| -> Option<u64> {
        Some(u64::from_le_bytes(buf.get(at..at + 8)?.try_into().ok()?))
    };
    let mut eocd = None;
    for index in (0..=tail.len() - 22).rev() {
        if u32le(&tail, index)? != EOCD_SIG {
            continue;
        }
        let comment = u16le(&tail, index + 20)? as usize;
        if index + 22 + comment == tail.len() {
            eocd = Some(index);
            break;
        }
    }
    let eocd = eocd?;
    let mut count = u64::from(u16le(&tail, eocd + 10)?);
    let mut cd_size = u64::from(u32le(&tail, eocd + 12)?);
    let mut cd_offset = u64::from(u32le(&tail, eocd + 16)?);
    if count == u64::from(u16::MAX)
        || cd_size == u64::from(u32::MAX)
        || cd_offset == u64::from(u32::MAX)
    {
        // ZIP64 locator sits immediately before the EOCD.
        if eocd < 20 || u32le(&tail, eocd - 20)? != EOCD64_LOCATOR_SIG {
            return None;
        }
        let eocd64_offset = u64le(&tail, eocd - 12)?;
        file.seek(SeekFrom::Start(eocd64_offset)).ok()?;
        let mut record = [0u8; 56];
        file.read_exact(&mut record).ok()?;
        if u32le(&record, 0)? != EOCD64_SIG {
            return None;
        }
        count = u64le(&record, 32)?;
        cd_size = u64le(&record, 40)?;
        cd_offset = u64le(&record, 48)?;
    }
    if cd_offset.checked_add(cd_size)? > size {
        return None;
    }
    file.seek(SeekFrom::Start(cd_offset)).ok()?;
    let mut directory = vec![0u8; cd_size as usize];
    file.read_exact(&mut directory).ok()?;
    let mut entries = Vec::with_capacity(count.min(4096) as usize);
    let mut at = 0usize;
    for _ in 0..count {
        if u32le(&directory, at)? != CD_SIG {
            return None;
        }
        let version_made = u16le(&directory, at + 4)?;
        let method = u16le(&directory, at + 10)?;
        let mut compressed = u64::from(u32le(&directory, at + 20)?);
        let mut uncompressed = u64::from(u32le(&directory, at + 24)?);
        let name_len = u16le(&directory, at + 28)? as usize;
        let extra_len = u16le(&directory, at + 30)? as usize;
        let comment_len = u16le(&directory, at + 32)? as usize;
        let external = u32le(&directory, at + 38)?;
        let mut local_offset = u64::from(u32le(&directory, at + 42)?);
        let name = directory.get(at + 46..at + 46 + name_len)?.to_vec();
        let extra = directory.get(at + 46 + name_len..at + 46 + name_len + extra_len)?;
        if uncompressed == u64::from(u32::MAX)
            || compressed == u64::from(u32::MAX)
            || local_offset == u64::from(u32::MAX)
        {
            // ZIP64 extra field — values appear in field order for each
            // member that was saturated.
            let mut cursor = 0usize;
            let mut resolved = false;
            while cursor + 4 <= extra.len() {
                let tag = u16le(extra, cursor)? as usize;
                let size = u16le(extra, cursor + 2)? as usize;
                let body = extra.get(cursor + 4..cursor + 4 + size)?;
                if tag == 0x0001 {
                    // Values appear in field order, one per saturated slot.
                    let need_uncompressed = uncompressed == u64::from(u32::MAX);
                    let need_compressed = compressed == u64::from(u32::MAX);
                    let need_offset = local_offset == u64::from(u32::MAX);
                    let mut field = 0usize;
                    if need_uncompressed {
                        uncompressed = u64le(body, field)?;
                        field += 8;
                    }
                    if need_compressed {
                        compressed = u64le(body, field)?;
                        field += 8;
                    }
                    if need_offset {
                        local_offset = u64le(body, field)?;
                    }
                    resolved = true;
                    break;
                }
                cursor += 4 + size;
            }
            if !resolved {
                return None;
            }
        }
        // `file.uncompressedSize64 > maxExpanded` etc. rely on accurate
        // values; mode bits come from the Unix creator fields.
        let symlink =
            matches!(version_made >> 8, 3 | 19) && (external >> 16) & 0o170000 == 0o120000;
        entries.push(ZipEntry {
            name,
            method,
            compressed,
            uncompressed,
            local_offset,
            symlink,
        });
        at += 46 + name_len + extra_len + comment_len;
    }
    Some(entries)
}

/// `readEntryData` — local header, then stored bytes or a bounded
/// inflate for method 8.
fn read_entry_data(file: &mut File, entry: &ZipEntry, limit: usize) -> Option<Vec<u8>> {
    const LOCAL_SIG: u32 = 0x0403_4b50;
    file.seek(SeekFrom::Start(entry.local_offset)).ok()?;
    let mut header = [0u8; 30];
    file.read_exact(&mut header).ok()?;
    if u32::from_le_bytes(header[0..4].try_into().ok()?) != LOCAL_SIG {
        return None;
    }
    let name_len = u16::from_le_bytes(header[26..28].try_into().ok()?) as u64;
    let extra_len = u16::from_le_bytes(header[28..30].try_into().ok()?) as u64;
    let data_start = entry.local_offset.checked_add(30 + name_len + extra_len)?;
    // The compressed blob must fit inside the uploaded file — this also
    // caps the allocation at the 20 MiB upload bound.
    let file_size = file.metadata().ok()?.len();
    if data_start.checked_add(entry.compressed)? > file_size {
        return None;
    }
    file.seek(SeekFrom::Start(data_start)).ok()?;
    let mut compressed = vec![0u8; entry.compressed as usize];
    file.read_exact(&mut compressed).ok()?;
    match entry.method {
        0 => Some(compressed),
        8 => inflate(&compressed, limit),
        _ => None,
    }
}

/// `filepath.Clean` for `/`-separated names (ZIP entry names are always
/// forward-slash, so a byte-level port is exact).
fn clean_path(path: &[u8]) -> Vec<u8> {
    if path.is_empty() {
        return b".".to_vec();
    }
    let rooted = path[0] == b'/';
    let mut stack: Vec<&[u8]> = Vec::new();
    for segment in path.split(|byte| *byte == b'/') {
        match segment {
            b"" | b"." => {}
            b".." => {
                if matches!(stack.last(), Some(last) if *last != b"..") {
                    stack.pop();
                } else if !rooted {
                    stack.push(b"..");
                }
            }
            _ => stack.push(segment),
        }
    }
    let mut out = Vec::new();
    if rooted {
        out.push(b'/');
    }
    for (index, segment) in stack.iter().enumerate() {
        if index > 0 {
            out.push(b'/');
        }
        out.extend_from_slice(segment);
    }
    if out.is_empty() {
        out.push(b'.');
    }
    out
}

// ── deflate (RFC 1951) ────────────────────────────────────────────────

/// Minimal bounded inflate — the document check only needs the ODF
/// `mimetype` entry (≤ 129 output bytes). Returns `None` on any
/// malformed stream or once `limit` is exceeded (the caller only reads
/// that far anyway).
fn inflate(data: &[u8], limit: usize) -> Option<Vec<u8>> {
    struct Bits<'a> {
        data: &'a [u8],
        byte: usize,
        bit: u32,
    }
    impl Bits<'_> {
        fn next(&mut self) -> Option<u32> {
            if self.byte >= self.data.len() {
                return None;
            }
            let value = (self.data[self.byte] >> self.bit) & 1;
            self.bit += 1;
            if self.bit == 8 {
                self.bit = 0;
                self.byte += 1;
            }
            Some(u32::from(value))
        }
        fn take(&mut self, count: u32) -> Option<u32> {
            let mut value = 0u32;
            for shift in 0..count {
                value |= self.next()? << shift;
            }
            Some(value)
        }
        fn align(&mut self) {
            if self.bit != 0 {
                self.bit = 0;
                self.byte += 1;
            }
        }
    }

    struct Table {
        counts: [u16; 16],
        symbols: Vec<u16>,
    }
    impl Table {
        /// Canonical Huffman table from code lengths (puff.c's `construct`).
        fn new(lengths: &[u8]) -> Self {
            let mut counts = [0u16; 16];
            for &len in lengths {
                if len > 0 {
                    counts[len as usize] += 1;
                }
            }
            let mut offsets = [0u16; 16];
            for len in 1..16 {
                offsets[len] = offsets[len - 1] + counts[len - 1];
            }
            let mut symbols = vec![0u16; lengths.iter().filter(|&&l| l > 0).count()];
            for (symbol, &len) in lengths.iter().enumerate() {
                if len > 0 {
                    symbols[offsets[len as usize] as usize] = symbol as u16;
                    offsets[len as usize] += 1;
                }
            }
            Self { counts, symbols }
        }
        /// Canonical decode walk (puff.c's `decode`).
        fn decode(&self, bits: &mut Bits<'_>) -> Option<u16> {
            let mut code = 0i32;
            let mut first = 0i32;
            let mut index = 0i32;
            for len in 1..16 {
                code |= bits.next()? as i32;
                let count = i32::from(self.counts[len]);
                if code - first < count {
                    return self.symbols.get((index + (code - first)) as usize).copied();
                }
                index += count;
                first = (first + count) << 1;
                code <<= 1;
            }
            None
        }
    }

    const LEN_BASE: [u16; 29] = [
        3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115,
        131, 163, 195, 227, 258,
    ];
    const LEN_EXTRA: [u32; 29] = [
        0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
    ];
    const DIST_BASE: [u16; 30] = [
        1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
        2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
    ];
    const DIST_EXTRA: [u32; 30] = [
        0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
        13, 13,
    ];

    let fixed_lit = || {
        let mut lengths = [0u8; 288];
        for (index, len) in lengths.iter_mut().enumerate() {
            *len = match index {
                0..=143 => 8,
                144..=255 => 9,
                256..=279 => 7,
                _ => 8,
            };
        }
        Table::new(&lengths)
    };
    let fixed_dist = || Table::new(&[5u8; 30]);

    let dynamic = |bits: &mut Bits<'_>| -> Option<(Table, Table)> {
        let hlit = bits.take(5)? as usize + 257;
        let hdist = bits.take(5)? as usize + 1;
        let hclen = bits.take(4)? as usize + 4;
        if hlit > 286 || hdist > 30 {
            return None;
        }
        const ORDER: [usize; 19] = [
            16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
        ];
        let mut cl_lengths = [0u8; 19];
        for &slot in ORDER.iter().take(hclen) {
            cl_lengths[slot] = bits.take(3)? as u8;
        }
        let cl = Table::new(&cl_lengths);
        let total = hlit + hdist;
        let mut lengths = vec![0u8; total];
        let mut filled = 0usize;
        while filled < total {
            match cl.decode(bits)? {
                len @ 0..=15 => {
                    lengths[filled] = len as u8;
                    filled += 1;
                }
                16 => {
                    if filled == 0 {
                        return None;
                    }
                    let prev = lengths[filled - 1];
                    let repeat = 3 + bits.take(2)? as usize;
                    if filled + repeat > total {
                        return None;
                    }
                    for _ in 0..repeat {
                        lengths[filled] = prev;
                        filled += 1;
                    }
                }
                17 => {
                    let repeat = 3 + bits.take(3)? as usize;
                    if filled + repeat > total {
                        return None;
                    }
                    filled += repeat;
                }
                18 => {
                    let repeat = 11 + bits.take(7)? as usize;
                    if filled + repeat > total {
                        return None;
                    }
                    filled += repeat;
                }
                _ => return None,
            }
        }
        Some((Table::new(&lengths[..hlit]), Table::new(&lengths[hlit..])))
    };

    let mut bits = Bits {
        data,
        byte: 0,
        bit: 0,
    };
    let mut out = Vec::new();
    loop {
        let last = bits.next()? == 1;
        match bits.take(2)? {
            0 => {
                bits.align();
                let header = bits.data.get(bits.byte..bits.byte + 4)?;
                let len = u16::from_le_bytes(header[0..2].try_into().ok()?) as usize;
                let nlen = u16::from_le_bytes(header[2..4].try_into().ok()?) as usize;
                if len != !nlen & 0xFFFF {
                    return None;
                }
                bits.byte += 4;
                let block = bits.data.get(bits.byte..bits.byte + len)?;
                out.extend_from_slice(block);
                bits.byte += len;
            }
            block_type @ (1 | 2) => {
                let (lit, dist) = if block_type == 1 {
                    (fixed_lit(), fixed_dist())
                } else {
                    dynamic(&mut bits)?
                };
                loop {
                    let symbol = lit.decode(&mut bits)? as usize;
                    match symbol {
                        0..=255 => out.push(symbol as u8),
                        256 => break,
                        257..=285 => {
                            let idx = symbol - 257;
                            let length =
                                LEN_BASE[idx] as usize + bits.take(LEN_EXTRA[idx])? as usize;
                            let dist_symbol = dist.decode(&mut bits)? as usize;
                            if dist_symbol >= 30 {
                                return None;
                            }
                            let distance = DIST_BASE[dist_symbol] as usize
                                + bits.take(DIST_EXTRA[dist_symbol])? as usize;
                            if distance > out.len() {
                                return None;
                            }
                            for _ in 0..length {
                                let byte = out[out.len() - distance];
                                out.push(byte);
                            }
                        }
                        _ => return None,
                    }
                    if out.len() > limit {
                        return None;
                    }
                }
            }
            _ => return None,
        }
        if out.len() > limit {
            return None;
        }
        if last {
            return Some(out);
        }
    }
}

// ── disk index ────────────────────────────────────────────────────────

/// `attachmentIndex` — `attachments.json` on disk.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiskIndex {
    #[serde(default, deserialize_with = "de_default")]
    schema_version: i64,
    #[serde(default, deserialize_with = "de_default")]
    attachments: Vec<DiskAttachmentRecord>,
}

/// `diskAttachmentRecord`.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiskAttachmentRecord {
    #[serde(default, deserialize_with = "de_default")]
    attachment: DiskAttachment,
    #[serde(default, deserialize_with = "de_default")]
    target: DiskAttachmentTarget,
    #[serde(default, deserialize_with = "de_default")]
    rel_path: String,
}

/// `diskAttachment`.
#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct DiskAttachment {
    #[serde(default, deserialize_with = "de_default", rename = "ref")]
    reference: String,
    #[serde(default, deserialize_with = "de_default")]
    name: String,
    #[serde(default, deserialize_with = "de_default")]
    media_type: String,
    #[serde(default, deserialize_with = "de_default")]
    bytes: i64,
    #[serde(default, deserialize_with = "de_default")]
    sha256: String,
    #[serde(default, deserialize_with = "de_default")]
    expires_at: Timestamp,
}

/// `diskAttachmentTarget` — persisted scope drops `generation`
/// (`json:"-"`).
#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct DiskAttachmentTarget {
    #[serde(default, deserialize_with = "de_default")]
    server_session_id: String,
    #[serde(default, deserialize_with = "de_default")]
    pane_id: String,
    #[serde(default, deserialize_with = "de_default")]
    terminal_id: String,
    #[serde(default, deserialize_with = "de_default")]
    agent_session_id: String,
}

impl DiskAttachmentTarget {
    fn to_target_ref(&self) -> TargetRef {
        TargetRef {
            server_session_id: self.server_session_id.clone(),
            pane_id: self.pane_id.clone(),
            terminal_id: self.terminal_id.clone(),
            generation: 0,
            agent_session_id: self.agent_session_id.clone(),
            ..TargetRef::default()
        }
    }
}

fn disk_record(record: &AttachmentRecord) -> DiskAttachmentRecord {
    DiskAttachmentRecord {
        attachment: DiskAttachment {
            reference: record.attachment.reference.clone(),
            name: record.attachment.name.clone(),
            media_type: record.attachment.media_type.clone(),
            bytes: record.attachment.bytes,
            sha256: record.attachment.sha256.clone(),
            expires_at: Timestamp(record.attachment.expires_at),
        },
        target: DiskAttachmentTarget {
            server_session_id: record.target.server_session_id.clone(),
            pane_id: record.target.pane_id.clone(),
            terminal_id: record.target.terminal_id.clone(),
            agent_session_id: record.target.agent_session_id.clone(),
        },
        rel_path: record.rel_path.clone(),
    }
}

// ── handlers ──────────────────────────────────────────────────────────

/// `validateUploadTarget` — the claimed target must point at a live pane.
/// The Rust topology doesn't project `generation` (the `agents` broadcast
/// carries none — the documented projection gap), so only `0` can match.
fn validate_upload_target(topology: &Topology, target: &TargetRef) -> Result<(), UploadError> {
    let mismatch = || UploadError::new("upload_scope_mismatch");
    if target.server_session_id != "primary" {
        return Err(mismatch());
    }
    let Some(agent) = topology.pane_of(&target.pane_id) else {
        return Err(mismatch());
    };
    if target.terminal_id != agent.terminal_id {
        return Err(mismatch());
    }
    if target.generation != 0 {
        return Err(mismatch());
    }
    let agent_session_id = agent
        .agent_session
        .as_ref()
        .map(|session| session.value.as_str())
        .unwrap_or("");
    if target.agent_session_id != agent_session_id {
        return Err(mismatch());
    }
    Ok(())
}

/// `handleUploadBegin`.
pub(crate) async fn upload_begin(
    ctx: ActionContext,
    request_id: &str,
    _action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    let request = match decode_begin(message) {
        Ok(request) => request,
        Err(()) => {
            return vec![upload_error(
                request_id,
                ResultKind::Begin,
                "attachment_upload_failed",
                BTreeMap::new(),
            )]
        }
    };
    if let Err(error) = validate_upload_target(&ctx.topology, &request.target) {
        return vec![upload_error(
            request_id,
            ResultKind::Begin,
            public_upload_error_code(error.code),
            error.args,
        )];
    }
    if !ctx.uploads.is_available() {
        return vec![upload_error(
            request_id,
            ResultKind::Begin,
            "attachment_upload_unavailable",
            BTreeMap::new(),
        )];
    }
    // `client.Identity()` isn't threaded into ActionContext; the oracle's
    // unauthenticated fallback — `connection:<id>` — is what the relay can
    // attest, so per-owner session caps key off the connection.
    let owner = format!("connection:{}", ctx.client_id);
    match ctx.uploads.begin(&owner, request) {
        Ok(result) => vec![upload_result(request_id, ResultKind::Begin, &result)],
        Err(error) => vec![upload_error(
            request_id,
            ResultKind::Begin,
            public_upload_error_code(error.code),
            error.args,
        )],
    }
}

/// `handleUploadChunk`.
pub(crate) async fn upload_chunk(
    ctx: ActionContext,
    request_id: &str,
    _action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    let request = match decode_chunk(message) {
        Ok(request) => request,
        Err(()) => {
            return vec![upload_error(
                request_id,
                ResultKind::Chunk,
                "attachment_upload_failed",
                BTreeMap::new(),
            )]
        }
    };
    if let Err(error) = validate_upload_target(&ctx.topology, &request.target) {
        // `s.uploadM.Cancel(...)` on scope failure — discards the staged
        // session under the mismatched target.
        if ctx.uploads.is_available() {
            let _ = ctx.uploads.cancel(&request.target, &request.upload_id);
        }
        return vec![upload_error(
            request_id,
            ResultKind::Chunk,
            public_upload_error_code(error.code),
            error.args,
        )];
    }
    if !ctx.uploads.is_available() {
        return vec![upload_error(
            request_id,
            ResultKind::Chunk,
            "attachment_upload_unavailable",
            BTreeMap::new(),
        )];
    }
    match ctx.uploads.chunk(&request) {
        Ok(result) => vec![upload_result(request_id, ResultKind::Chunk, &result)],
        Err(error) => vec![upload_error(
            request_id,
            ResultKind::Chunk,
            public_upload_error_code(error.code),
            error.args,
        )],
    }
}

/// `handleUploadFinish`.
pub(crate) async fn upload_finish(
    ctx: ActionContext,
    request_id: &str,
    _action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    let request = match decode_finish(message) {
        Ok(request) => request,
        Err(()) => {
            return vec![upload_error(
                request_id,
                ResultKind::Finish,
                "attachment_upload_failed",
                BTreeMap::new(),
            )]
        }
    };
    if let Err(error) = validate_upload_target(&ctx.topology, &request.target) {
        if ctx.uploads.is_available() {
            let _ = ctx.uploads.cancel(&request.target, &request.upload_id);
        }
        return vec![upload_error(
            request_id,
            ResultKind::Finish,
            public_upload_error_code(error.code),
            error.args,
        )];
    }
    if !ctx.uploads.is_available() {
        return vec![upload_error(
            request_id,
            ResultKind::Finish,
            "attachment_upload_unavailable",
            BTreeMap::new(),
        )];
    }
    match ctx.uploads.finish(&request) {
        Ok(result) => {
            // `RecordActivity("upload","completed",…)` — "Attached N files",
            // or the single file's name.
            let summary = if result.attachments.len() == 1 {
                format!("Attached {}", result.attachments[0].name)
            } else {
                format!("Attached {} files", result.attachments.len())
            };
            super::record_activity(
                &ctx,
                "upload",
                "completed",
                summary,
                &request.target.pane_id,
                request_id,
            );
            vec![upload_result(request_id, ResultKind::Finish, &result)]
        }
        Err(error) => vec![upload_error(
            request_id,
            ResultKind::Finish,
            public_upload_error_code(error.code),
            error.args,
        )],
    }
}

/// `handleUploadCancel`.
pub(crate) async fn upload_cancel(
    ctx: ActionContext,
    request_id: &str,
    _action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    let (target, upload_id) = match decode_cancel(message) {
        Ok(decoded) => decoded,
        Err(()) => {
            return vec![upload_error(
                request_id,
                ResultKind::Cancel,
                "attachment_upload_failed",
                BTreeMap::new(),
            )]
        }
    };
    if !ctx.uploads.is_available() {
        return vec![upload_error(
            request_id,
            ResultKind::Cancel,
            "attachment_upload_unavailable",
            BTreeMap::new(),
        )];
    }
    match ctx.uploads.cancel(&target, &upload_id) {
        Ok(()) => vec![upload_result(
            request_id,
            ResultKind::Cancel,
            &EmptyPayload {},
        )],
        Err(error) => vec![upload_error(
            request_id,
            ResultKind::Cancel,
            public_upload_error_code(error.code),
            error.args,
        )],
    }
}

// ── attachment reference expansion ────────────────────────────────────

/// `expandPromptAttachmentReferences` — rewrite `Attachment: <ref>` lines
/// to the resolved on-disk path before `send_text`/`submit_prompt`
/// dispatch. Called once per text field by the orchestration seam
/// (`actions/input.rs`); `target` is the message's `TargetRef` claim.
///
/// Returns the exact dispatch-failure text the oracle uses when a
/// reference cannot be resolved (caller answers with a `command_result`
/// phase `failed` and this error string).
#[allow(dead_code)] // read once input.rs wires expansion
pub(crate) async fn expand_attachment_references(
    ctx: &ActionContext,
    target: Option<&TargetRef>,
    value: &mut String,
) -> Result<(), String> {
    expand_references(&ctx.uploads, target, value)
}

#[allow(dead_code)] // read once input.rs wires expansion
fn expand_references(
    uploads: &Uploads,
    target: Option<&TargetRef>,
    value: &mut String,
) -> Result<(), String> {
    if !value.contains("Attachment: ") {
        return Ok(());
    }
    let mut lines: Vec<String> = value.split('\n').map(str::to_owned).collect();
    for line in &mut lines {
        let Some(candidate) = line.trim().strip_prefix("Attachment: ") else {
            continue;
        };
        if !valid_attachment_reference(candidate) {
            continue;
        }
        let Some(target) = target else {
            return Err(ATTACHMENTS_UNAVAILABLE.to_owned());
        };
        if !uploads.is_available() {
            return Err(ATTACHMENTS_UNAVAILABLE.to_owned());
        }
        match uploads.resolve(target, candidate) {
            Ok(attachment) => {
                *line = format!("Attachment: {}", attachment.path.display());
            }
            Err(_) => return Err(ATTACHMENTS_UNAVAILABLE.to_owned()),
        }
    }
    *value = lines.join("\n");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::TopologyActor;
    use crate::topology::Topology;
    use lerdr_herdr::{AgentInfo, AgentSessionInfo, AgentSessionRefKind, SessionSnapshot};
    use std::sync::atomic::AtomicI64;
    use tokio_util::sync::CancellationToken;

    fn sha(data: &[u8]) -> String {
        hex::encode(Sha256::digest(data))
    }

    fn b64(data: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(data)
    }

    fn spec(name: &str, media_type: &str, bytes: i64) -> FileSpec {
        FileSpec {
            name: name.to_owned(),
            media_type: media_type.to_owned(),
            bytes,
        }
    }

    fn target() -> TargetRef {
        TargetRef {
            server_session_id: "primary".to_owned(),
            pane_id: "pane-a".to_owned(),
            terminal_id: "term-1".to_owned(),
            generation: 0,
            agent_session_id: "sess-1".to_owned(),
            ..TargetRef::default()
        }
    }

    fn begin_request(files: Vec<FileSpec>) -> BeginRequest {
        BeginRequest {
            target: target(),
            files,
        }
    }

    fn png_body() -> Vec<u8> {
        let mut data = PNG_MAGIC.to_vec();
        data.extend_from_slice(&[0xAB; 64]);
        data
    }

    fn pdf_body() -> Vec<u8> {
        let mut data = b"%PDF-1.7\n".to_vec();
        data.extend_from_slice(&[b'x'; 32]);
        data.extend_from_slice(b"trailer\n%%EOF\n");
        data
    }

    fn text_body() -> Vec<u8> {
        b"hello attachment\n".to_vec()
    }

    /// Drive a file through begin → one chunk → finish; returns the
    /// published [`AttachmentPayload`] material.
    fn upload_one(
        uploads: &Uploads,
        name: &str,
        media_type: &str,
        body: &[u8],
    ) -> Result<(String, FinishResultPayload), UploadError> {
        let begin = uploads.begin(
            "owner",
            begin_request(vec![spec(name, media_type, body.len() as i64)]),
        )?;
        let mut sequence = 0i64;
        let mut offset = 0usize;
        while offset < body.len() {
            let end = (offset + CHUNK_BYTES).min(body.len());
            let slice = &body[offset..end];
            uploads.chunk(&ChunkRequest {
                target: target(),
                upload_id: begin.upload_id.clone(),
                file_index: 0,
                sequence,
                data: slice.to_vec(),
                sha256: sha(slice),
            })?;
            sequence += 1;
            offset = end;
        }
        let finish = uploads.finish(&FinishRequest {
            target: target(),
            upload_id: begin.upload_id.clone(),
            files: vec![FileDigest {
                file_index: 0,
                sha256: sha(body),
            }],
        })?;
        Ok((begin.upload_id, finish))
    }

    #[test]
    fn begin_stage_chunk_finish_publishes() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let uploads = Uploads::new(dir.clone());
        let (upload_id, finish) =
            upload_one(&uploads, "shot.png", "image/png", &png_body()).expect("cycle");
        assert_eq!(finish.attachments.len(), 1);
        let attachment = &finish.attachments[0];
        assert!(valid_attachment_reference(&attachment.reference));
        assert_eq!(attachment.name, "shot.png");
        assert_eq!(attachment.media_type, "image/png");
        assert_eq!(attachment.bytes, png_body().len() as i64);
        assert_eq!(attachment.sha256, sha(&png_body()));
        // staging removed; object + index exist
        assert!(!dir.join("sessions").join(&upload_id).exists());
        let object = dir
            .join("objects")
            .join(format!("{}.png", attachment.reference));
        assert_eq!(fs::read(&object).expect("object"), png_body());
        assert!(dir.join(INDEX_FILENAME).is_file());
        // resolve hits the published path
        let resolved = uploads
            .resolve(&target(), &attachment.reference)
            .expect("resolve");
        assert_eq!(resolved.path, object);
        assert_eq!(resolved.name, "shot.png");
    }

    #[test]
    fn begin_enforces_bounds_in_oracle_order() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let uploads = Uploads::new(dir);
        // empty + overflow file lists -> batch_count_invalid{max_files:8}
        let err = uploads.begin("o", begin_request(vec![])).unwrap_err();
        assert_eq!(err.code, "upload_batch_count_invalid");
        assert_eq!(err.args["max_files"].as_i64(), Some(8));
        let many = vec![spec("a.png", "image/png", 8); MAX_FILES + 1];
        let err = uploads.begin("o", begin_request(many)).unwrap_err();
        assert_eq!(err.code, "upload_batch_count_invalid");
        // declared size bounds
        for bytes in [0, -1, MAX_FILE_BYTES + 1] {
            let err = uploads
                .begin("o", begin_request(vec![spec("a.png", "image/png", bytes)]))
                .unwrap_err();
            assert_eq!(err.code, "upload_file_size_invalid");
            assert_eq!(err.args["max_bytes"].as_i64(), Some(MAX_FILE_BYTES));
        }
        // batch aggregate bound (no data written — the declared sizes sum)
        let huge = vec![spec("a.png", "image/png", MAX_FILE_BYTES); 3];
        let err = uploads.begin("o", begin_request(huge)).unwrap_err();
        assert_eq!(err.code, "upload_batch_too_large");
        assert_eq!(err.args["max_bytes"].as_i64(), Some(MAX_BATCH_BYTES));
        // name hygiene
        for name in ["", "  ", ".", "..", "a/b.png", "a\\b.png", "a\nb.png"] {
            let err = uploads
                .begin("o", begin_request(vec![spec(name, "image/png", 8)]))
                .unwrap_err();
            assert_eq!(err.code, "upload_name_invalid", "name {name:?}");
        }
        let long = "x".repeat(MAX_NAME_BYTES + 1) + ".png";
        let err = uploads
            .begin("o", begin_request(vec![spec(&long, "image/png", 8)]))
            .unwrap_err();
        assert_eq!(err.code, "upload_name_invalid");
        // unsupported type + extension mismatch
        let err = uploads
            .begin(
                "o",
                begin_request(vec![spec("a.bin", "application/zip", 8)]),
            )
            .unwrap_err();
        assert_eq!(err.code, "upload_type_unsupported");
        let err = uploads
            .begin("o", begin_request(vec![spec("a.jpg", "image/png", 8)]))
            .unwrap_err();
        assert_eq!(err.code, "upload_extension_mismatch");
        // zero target -> target invalid
        let err = uploads
            .begin(
                "o",
                BeginRequest {
                    target: TargetRef::default(),
                    files: vec![spec("a.png", "image/png", 8)],
                },
            )
            .unwrap_err();
        assert_eq!(err.code, "upload_target_invalid");
    }

    #[test]
    fn session_limits_are_enforced() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let uploads = Uploads::new(dir);
        let files = || vec![spec("a.png", "image/png", 8)];
        for _ in 0..MAX_SESSIONS_PER_OWNER {
            uploads
                .begin("owner-a", begin_request(files()))
                .expect("begin");
        }
        let err = uploads
            .begin("owner-a", begin_request(files()))
            .unwrap_err();
        assert_eq!(err.code, "upload_session_limit");
        assert_eq!(
            err.args["max_sessions"].as_i64(),
            Some(MAX_SESSIONS_PER_OWNER as i64)
        );
        // a different owner still begins — fill it too for the global math
        for _ in 0..MAX_SESSIONS_PER_OWNER {
            uploads
                .begin("owner-b", begin_request(files()))
                .expect("other owner");
        }
        // global cap: 8 live now — fill to exactly 64 across fresh owners
        for owner in 1..15 {
            for _ in 0..MAX_SESSIONS_PER_OWNER {
                uploads
                    .begin(&format!("owner-{owner}"), begin_request(files()))
                    .expect("fill");
            }
        }
        // 8 + 14*4 = 64 sessions live — the next begin hits the global cap.
        let err = uploads
            .begin("owner-z", begin_request(files()))
            .unwrap_err();
        assert_eq!(err.code, "upload_session_limit");
        assert_eq!(err.args["max_sessions"].as_i64(), Some(MAX_SESSIONS as i64));
    }

    #[test]
    fn chunk_digest_mismatch_discards_session() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let uploads = Uploads::new(dir);
        let begin = uploads
            .begin("o", begin_request(vec![spec("a.png", "image/png", 4)]))
            .expect("begin");
        let err = uploads
            .chunk(&ChunkRequest {
                target: target(),
                upload_id: begin.upload_id.clone(),
                file_index: 0,
                sequence: 0,
                data: b"nope".to_vec(),
                sha256: sha(b"other"),
            })
            .unwrap_err();
        assert_eq!(err.code, "upload_chunk_digest_mismatch");
        // session is gone now — not merely "out of order"
        let err = uploads
            .chunk(&ChunkRequest {
                target: target(),
                upload_id: begin.upload_id,
                file_index: 0,
                sequence: 1,
                data: b"1234".to_vec(),
                sha256: sha(b"1234"),
            })
            .unwrap_err();
        assert_eq!(err.code, "upload_session_not_found");
    }

    #[test]
    fn chunk_out_of_order_reports_expected_position() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let uploads = Uploads::new(dir);
        let begin = uploads
            .begin("o", begin_request(vec![spec("a.png", "image/png", 4)]))
            .expect("begin");
        let err = uploads
            .chunk(&ChunkRequest {
                target: target(),
                upload_id: begin.upload_id.clone(),
                file_index: 0,
                sequence: 7,
                data: b"1234".to_vec(),
                sha256: sha(b"1234"),
            })
            .unwrap_err();
        assert_eq!(err.code, "upload_chunk_out_of_order");
        assert_eq!(err.args["expected_sequence"].as_i64(), Some(0));
        assert_eq!(err.args["expected_file_index"].as_i64(), Some(0));
    }

    #[test]
    fn chunk_size_and_capacity_bounds() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let uploads = Uploads::new(dir);
        // Every post-lock chunk failure discards the session — each case
        // needs a fresh begin.
        let begin = |bytes: i64| {
            uploads
                .begin("o", begin_request(vec![spec("a.png", "image/png", bytes)]))
                .expect("begin")
                .upload_id
        };
        let req = |upload_id: String, data: Vec<u8>| ChunkRequest {
            target: target(),
            upload_id,
            file_index: 0,
            sequence: 0,
            sha256: sha(&data),
            data,
        };
        let err = uploads.chunk(&req(begin(4), vec![])).unwrap_err();
        assert_eq!(err.code, "upload_chunk_size_invalid");
        let err = uploads
            .chunk(&req(begin(4), vec![7u8; CHUNK_BYTES + 1]))
            .unwrap_err();
        assert_eq!(err.code, "upload_chunk_size_invalid");
        // over-declared cumulative bytes
        let err = uploads
            .chunk(&req(begin(4), b"12345".to_vec()))
            .unwrap_err();
        assert_eq!(err.code, "upload_file_too_large");
        assert_eq!(err.args["expected_bytes"].as_i64(), Some(4));
    }

    #[test]
    fn finish_final_digest_mismatch_cleans_staging() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let uploads = Uploads::new(dir.clone());
        let begin = uploads
            .begin("o", begin_request(vec![spec("a.png", "image/png", 4)]))
            .expect("begin");
        uploads
            .chunk(&ChunkRequest {
                target: target(),
                upload_id: begin.upload_id.clone(),
                file_index: 0,
                sequence: 0,
                data: b"1234".to_vec(),
                sha256: sha(b"1234"),
            })
            .expect("chunk");
        let err = uploads
            .finish(&FinishRequest {
                target: target(),
                upload_id: begin.upload_id.clone(),
                files: vec![FileDigest {
                    file_index: 0,
                    sha256: sha(b"other"),
                }],
            })
            .unwrap_err();
        assert_eq!(err.code, "upload_final_digest_mismatch");
        assert_eq!(err.args["file_index"].as_i64(), Some(0));
        assert!(!dir.join("sessions").join(&begin.upload_id).exists());
    }

    #[test]
    fn finish_rejects_incomplete_sessions() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let uploads = Uploads::new(dir);
        let begin = uploads
            .begin("o", begin_request(vec![spec("a.png", "image/png", 4)]))
            .expect("begin");
        let err = uploads
            .finish(&FinishRequest {
                target: target(),
                upload_id: begin.upload_id,
                files: vec![],
            })
            .unwrap_err();
        assert_eq!(err.code, "upload_incomplete");
    }

    #[test]
    fn cancel_semantics_match_the_oracle() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let uploads = Uploads::new(dir.clone());
        // unknown id: idempotent success
        uploads.cancel(&target(), "missing").expect("unknown");
        let other = TargetRef {
            pane_id: "pane-b".to_owned(),
            ..target()
        };
        let begin = uploads
            .begin("o", begin_request(vec![spec("a.png", "image/png", 4)]))
            .expect("begin");
        // wrong target: scope mismatch, session survives
        let err = uploads.cancel(&other, &begin.upload_id).unwrap_err();
        assert_eq!(err.code, "upload_scope_mismatch");
        uploads
            .chunk(&ChunkRequest {
                target: target(),
                upload_id: begin.upload_id.clone(),
                file_index: 0,
                sequence: 0,
                data: b"1234".to_vec(),
                sha256: sha(b"1234"),
            })
            .expect("session still live");
        // right target: success + staging removed
        uploads.cancel(&target(), &begin.upload_id).expect("cancel");
        assert!(!dir.join("sessions").join(&begin.upload_id).exists());
        // repeat: still success
        uploads.cancel(&target(), &begin.upload_id).expect("repeat");
        // tombstone keeps the original target — wrong target still mismatches
        let err = uploads.cancel(&other, &begin.upload_id).unwrap_err();
        assert_eq!(err.code, "upload_scope_mismatch");
    }

    #[test]
    fn content_sniffing_rejects_mismatched_bodies() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let uploads = Uploads::new(dir);
        // text bytes under an image/png spec
        let err = upload_one(&uploads, "a.png", "image/png", b"not an image").unwrap_err();
        assert_eq!(err.code, "upload_content_type_mismatch");
        // NUL byte in a text file
        let err = upload_one(&uploads, "a.txt", "text/plain", b"a\0b").unwrap_err();
        assert_eq!(err.code, "upload_content_type_mismatch");
        // invalid UTF-8 in markdown
        let err = upload_one(&uploads, "a.md", "text/markdown", &[0xFF, 0xFE, 0xFD]).unwrap_err();
        assert_eq!(err.code, "upload_content_type_mismatch");
        // JSON that doesn't parse
        let err = upload_one(&uploads, "a.json", "application/json", b"{oops").unwrap_err();
        assert_eq!(err.code, "upload_content_type_mismatch");
        // PDF without the EOF marker
        let err =
            upload_one(&uploads, "a.pdf", "application/pdf", b"%PDF-1.4\nno eof").unwrap_err();
        assert_eq!(err.code, "upload_content_type_mismatch");
        // valid variants pass
        upload_one(&uploads, "b.png", "image/png", &png_body()).expect("png");
        upload_one(&uploads, "b.txt", "text/plain", &text_body()).expect("txt");
        upload_one(&uploads, "b.json", "application/json", b"  {\"a\":1}\n").expect("json");
        upload_one(&uploads, "b.pdf", "application/pdf", &pdf_body()).expect("pdf");
    }

    /// Build a stored-method ZIP archive — enough structure for the
    /// container validator (local headers + central directory + EOCD).
    fn make_zip(entries: &[(&str, &[u8])], symlink_indices: &[usize]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut directory = Vec::new();
        for (index, (name, data)) in entries.iter().enumerate() {
            let offset = out.len() as u32;
            let crc = crc32(data);
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            out.extend_from_slice(&20u16.to_le_bytes()); // version needed
            out.extend_from_slice(&0u16.to_le_bytes()); // flags
            out.extend_from_slice(&0u16.to_le_bytes()); // method: store
            out.extend_from_slice(&0u16.to_le_bytes()); // time
            out.extend_from_slice(&0u16.to_le_bytes()); // date
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // extra len
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);

            directory.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            let made_by = if symlink_indices.contains(&index) {
                (3u16 << 8) | 20
            } else {
                20
            };
            directory.extend_from_slice(&made_by.to_le_bytes());
            directory.extend_from_slice(&20u16.to_le_bytes());
            directory.extend_from_slice(&0u16.to_le_bytes());
            directory.extend_from_slice(&0u16.to_le_bytes());
            directory.extend_from_slice(&0u16.to_le_bytes());
            directory.extend_from_slice(&0u16.to_le_bytes());
            directory.extend_from_slice(&crc.to_le_bytes());
            directory.extend_from_slice(&(data.len() as u32).to_le_bytes());
            directory.extend_from_slice(&(data.len() as u32).to_le_bytes());
            directory.extend_from_slice(&(name.len() as u16).to_le_bytes());
            directory.extend_from_slice(&0u16.to_le_bytes()); // extra
            directory.extend_from_slice(&0u16.to_le_bytes()); // comment
            directory.extend_from_slice(&0u16.to_le_bytes()); // disk
            directory.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
            let external = if symlink_indices.contains(&index) {
                (0o120000u32) << 16
            } else {
                0
            };
            directory.extend_from_slice(&external.to_le_bytes());
            directory.extend_from_slice(&offset.to_le_bytes());
            directory.extend_from_slice(name.as_bytes());
        }
        let cd_offset = out.len() as u32;
        out.extend_from_slice(&directory);
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(directory.len() as u32).to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    fn crc32(data: &[u8]) -> u32 {
        // The validator never checks CRCs (the oracle's LimitReader stops
        // before the checksum) — a real table keeps the test honest anyway.
        let mut table = [0u32; 256];
        for (index, slot) in table.iter_mut().enumerate() {
            let mut c = index as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *slot = c;
        }
        data.iter().fold(!0u32, |crc, b| {
            table[((crc ^ u32::from(*b)) & 0xFF) as usize] ^ (crc >> 8)
        }) ^ !0
    }

    #[test]
    fn document_containers_validate_members_and_paths() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let uploads = Uploads::new(dir);
        let docx = make_zip(
            &[
                ("[Content_Types].xml", b"<Types/>".as_ref()),
                ("word/document.xml", b"<w:doc/>".as_ref()),
            ],
            &[],
        );
        upload_one(
            &uploads,
            "a.docx",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            &docx,
        )
        .expect("docx");
        let missing = make_zip(&[("word/document.xml", b"<w:doc/>".as_ref())], &[]);
        let err = upload_one(
            &uploads,
            "b.docx",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            &missing,
        )
        .unwrap_err();
        assert_eq!(err.code, "upload_content_type_mismatch");
        let traversal = make_zip(
            &[
                ("[Content_Types].xml", b"<Types/>".as_ref()),
                ("word/document.xml", b"<w:doc/>".as_ref()),
                ("../evil", b"x".as_ref()),
            ],
            &[],
        );
        let err = upload_one(
            &uploads,
            "c.docx",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            &traversal,
        )
        .unwrap_err();
        assert_eq!(err.code, "upload_content_type_mismatch");
        let linked = make_zip(
            &[
                ("[Content_Types].xml", b"<Types/>".as_ref()),
                ("word/document.xml", b"<w:doc/>".as_ref()),
                ("link", b"target".as_ref()),
            ],
            &[2],
        );
        let err = upload_one(
            &uploads,
            "d.docx",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            &linked,
        )
        .unwrap_err();
        assert_eq!(err.code, "upload_content_type_mismatch");
        // ODF requires the stored mimetype identity
        let odt = make_zip(
            &[
                (
                    "mimetype",
                    b"application/vnd.oasis.opendocument.text".as_ref(),
                ),
                ("content.xml", b"<office/>".as_ref()),
            ],
            &[],
        );
        upload_one(
            &uploads,
            "a.odt",
            "application/vnd.oasis.opendocument.text",
            &odt,
        )
        .expect("odt");
        let wrong_mime = make_zip(
            &[
                ("mimetype", b"text/plain".as_ref()),
                ("content.xml", b"<office/>".as_ref()),
            ],
            &[],
        );
        let err = upload_one(
            &uploads,
            "b.odt",
            "application/vnd.oasis.opendocument.text",
            &wrong_mime,
        )
        .unwrap_err();
        assert_eq!(err.code, "upload_content_type_mismatch");
        // not a zip at all
        let err = upload_one(
            &uploads,
            "e.docx",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            b"plain bytes",
        )
        .unwrap_err();
        assert_eq!(err.code, "upload_content_type_mismatch");
    }

    #[test]
    fn staging_replacement_is_detected_at_finish() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let uploads = Uploads::new(dir.clone());
        let body = png_body();
        let begin = uploads
            .begin(
                "o",
                begin_request(vec![spec("a.png", "image/png", body.len() as i64)]),
            )
            .expect("begin");
        uploads
            .chunk(&ChunkRequest {
                target: target(),
                upload_id: begin.upload_id.clone(),
                file_index: 0,
                sequence: 0,
                data: body.clone(),
                sha256: sha(&body),
            })
            .expect("chunk");
        let staged = dir
            .join("sessions")
            .join(&begin.upload_id)
            .join("0000.part");
        fs::remove_file(&staged).expect("remove staged");
        std::os::unix::fs::symlink(dir.join("objects"), &staged).expect("symlink");
        let err = uploads
            .finish(&FinishRequest {
                target: target(),
                upload_id: begin.upload_id,
                files: vec![FileDigest {
                    file_index: 0,
                    sha256: sha(&body),
                }],
            })
            .unwrap_err();
        assert_eq!(err.code, "upload_staging_changed");
    }

    #[test]
    fn clock_driven_expiry_and_load_drops_expired() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let clock = Arc::new(AtomicI64::new(1_700_000_000));
        let clock_ref = clock.clone();
        let now: NowFn = Arc::new(move || {
            UNIX_EPOCH + Duration::from_secs(clock_ref.load(Ordering::Relaxed) as u64)
        });
        // expiry: begin a session, advance past the TTL, chunk -> expired
        let uploads = Uploads::with_clock(
            dir.clone(),
            now.clone(),
            Arc::new(|b| {
                b.fill(7);
                Ok(())
            }),
        );
        let begin = uploads
            .begin("o", begin_request(vec![spec("a.png", "image/png", 4)]))
            .expect("begin");
        clock.fetch_add(SESSION_TTL.as_secs() as i64 + 1, Ordering::Relaxed);
        let err = uploads
            .chunk(&ChunkRequest {
                target: target(),
                upload_id: begin.upload_id,
                file_index: 0,
                sequence: 0,
                data: b"1234".to_vec(),
                sha256: sha(b"1234"),
            })
            .unwrap_err();
        assert_eq!(err.code, "upload_session_expired");
    }

    #[test]
    fn finished_attachments_survive_restart_with_looser_scope() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let uploads = Uploads::new(dir.clone());
        let (_, finish) = upload_one(&uploads, "a.png", "image/png", &png_body()).expect("cycle");
        let reference = finish.attachments[0].reference.clone();
        drop(uploads);
        let restarted = Uploads::new(dir);
        // persisted records match without generation (oracle restart seam)
        let mut other = target();
        other.generation = 99;
        let resolved = restarted.resolve(&other, &reference).expect("resolve");
        assert_eq!(resolved.name, "a.png");
        // pane/terminal/session still bind
        other.pane_id = "pane-b".to_owned();
        let err = restarted.resolve(&other, &reference).unwrap_err();
        assert_eq!(err.code, "attachment_scope_mismatch");
        let err = restarted.resolve(&target(), "nope").unwrap_err();
        assert_eq!(err.code, "attachment_not_found");
    }

    #[test]
    fn corrupt_index_quarantines_and_stays_available() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        fs::create_dir_all(&dir).expect("dir");
        fs::write(dir.join(INDEX_FILENAME), b"not json").expect("write");
        let uploads = Uploads::new(dir.clone());
        assert!(uploads.is_available());
        assert!(dir.read_dir().expect("read_dir").flatten().any(|e| e
            .file_name()
            .to_string_lossy()
            .starts_with("attachments.invalid-")));
        // index rewritten fresh
        let index: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.join(INDEX_FILENAME)).expect("index"))
                .expect("parse");
        assert_eq!(index["schema_version"], 1);
    }

    #[test]
    fn unavailable_root_marks_manager_unavailable() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let file_path = dir.join("not-a-dir");
        fs::write(&file_path, b"x").expect("file");
        let uploads = Uploads::new(file_path);
        assert!(!uploads.is_available());
    }

    #[test]
    fn resolve_rejects_changed_objects() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let uploads = Uploads::new(dir.clone());
        let (_, finish) = upload_one(&uploads, "a.png", "image/png", &png_body()).expect("cycle");
        let reference = finish.attachments[0].reference.clone();
        // rewrite the object with different bytes — lstat still says
        // "regular file" but the size no longer matches.
        fs::write(
            dir.join("objects").join(format!("{reference}.png")),
            b"smaller",
        )
        .expect("rewrite");
        let err = uploads.resolve(&target(), &reference).unwrap_err();
        assert_eq!(err.code, "attachment_changed");
        // record evicted — the miss is now not_found
        let err = uploads.resolve(&target(), &reference).unwrap_err();
        assert_eq!(err.code, "attachment_not_found");
    }

    #[test]
    fn expand_rewrites_known_references() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let uploads = Uploads::new(dir);
        let (_, finish) = upload_one(&uploads, "a.png", "image/png", &png_body()).expect("cycle");
        let reference = finish.attachments[0].reference.clone();
        let mut text = format!("Look at this:\n  Attachment: {reference}\nthanks");
        expand_references(&uploads, Some(&target()), &mut text).expect("expand");
        let resolved = uploads.resolve(&target(), &reference).expect("resolve");
        let expected = format!("Attachment: {}", resolved.path.display());
        assert!(text.contains(&expected), "rewrote to path: {text}");
        // unchanged: no marker, invalid refs, non-attachment lines
        let mut plain = "no attachments here".to_owned();
        expand_references(&uploads, Some(&target()), &mut plain).expect("plain");
        assert_eq!(plain, "no attachments here");
        let mut invalid = "Attachment: not-a-real-ref".to_owned();
        expand_references(&uploads, Some(&target()), &mut invalid).expect("invalid");
        assert_eq!(invalid, "Attachment: not-a-real-ref");
    }

    #[test]
    fn expand_fails_on_unknown_or_out_of_scope_references() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let uploads = Uploads::new(dir);
        let (_, finish) = upload_one(&uploads, "a.png", "image/png", &png_body()).expect("cycle");
        let reference = finish.attachments[0].reference.clone();
        // unknown ref
        let mut text = format!("Attachment: {}", "z".repeat(32));
        let err = expand_references(&uploads, Some(&target()), &mut text).unwrap_err();
        assert_eq!(err, ATTACHMENTS_UNAVAILABLE);
        // out-of-scope target
        let other = TargetRef {
            pane_id: "pane-b".to_owned(),
            ..target()
        };
        let mut text = format!("Attachment: {reference}");
        let err = expand_references(&uploads, Some(&other), &mut text).unwrap_err();
        assert_eq!(err, ATTACHMENTS_UNAVAILABLE);
        // absent target
        let mut text = format!("Attachment: {reference}");
        let err = expand_references(&uploads, None, &mut text).unwrap_err();
        assert_eq!(err, ATTACHMENTS_UNAVAILABLE);
    }

    // ── handler-level coverage ────────────────────────────────────────

    fn agent() -> AgentInfo {
        AgentInfo {
            pane_id: "pane-a".to_owned(),
            terminal_id: "term-1".to_owned(),
            agent_session: Some(AgentSessionInfo {
                kind: AgentSessionRefKind::Id,
                value: "sess-1".to_owned(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn test_ctx(agents: Vec<AgentInfo>, dir: PathBuf) -> ActionContext {
        let client = lerdr_herdr::Client::unix(dir.join("missing.sock"));
        let mut topology = Topology::default();
        topology.accept(SessionSnapshot {
            agents,
            ..Default::default()
        });
        ActionContext {
            client: client.clone(),
            handle: TopologyActor::spawn(client.clone(), CancellationToken::new()),
            topology: Arc::new(topology),
            leases: crate::actions::leases::Leases::new(client.clone()),
            profiles: crate::actions::profiles::Resolver::with_config_home(dir.join("profiles")),
            questions: crate::actions::questions::Questions::default(),
            uploads: Uploads::new(dir.join("uploads")),
            activities: crate::actions::activity::Journal::default(),
            push: crate::actions::push::Push::default(),
            speech: crate::actions::speech::Speech::default(),
            notices: crate::actions::Notices::default(),
            audit: None,
            device_id: "test-device".to_owned(),
            client_id: "client-1".to_owned(),
        }
    }

    fn message(value: serde_json::Value) -> Inbound {
        let mut map = value.as_object().expect("object").clone();
        map.entry("type".to_owned())
            .or_insert_with(|| serde_json::json!("upload"));
        Inbound::decode_map(&map).expect("decode")
    }

    fn target_json() -> serde_json::Value {
        serde_json::json!({
            "server_session_id": "primary",
            "pane_id": "pane-a",
            "terminal_id": "term-1",
            "generation": 0,
            "agent_session_id": "sess-1",
        })
    }

    /// Extract `{type, request_id, error?, result?}` from a reply frame.
    fn frame_json(outbound: &Outbound) -> serde_json::Value {
        serde_json::from_slice(&outbound.encode()).expect("json")
    }

    #[tokio::test]
    async fn upload_begin_handler_validates_then_stages() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let ctx = test_ctx(vec![agent()], dir);
        // malformed files field -> attachment_upload_failed
        let msg = message(serde_json::json!({
            "target": target_json(),
            "files": "nope",
        }));
        let frames = upload_begin(ctx.clone(), "r1", "a1", &msg).await;
        let frame = frame_json(&frames[0]);
        assert_eq!(frame["type"], "upload_begin_result");
        assert_eq!(frame["request_id"], "r1");
        assert_eq!(frame["error"]["code"], "attachment_upload_failed");
        // unknown pane -> scope error maps to attachment_upload_state_unknown
        let msg = message(serde_json::json!({
            "target": {"server_session_id":"primary","pane_id":"pane-x","terminal_id":"t","generation":0,"agent_session_id":""},
            "files": [{"name":"a.png","media_type":"image/png","bytes":4}],
        }));
        let frames = upload_begin(ctx.clone(), "r2", "a2", &msg).await;
        let frame = frame_json(&frames[0]);
        assert_eq!(frame["error"]["code"], "attachment_upload_state_unknown");
        // happy path — result payload carries limits
        let msg = message(serde_json::json!({
            "target": target_json(),
            "files": [{"name":"a.png","media_type":"image/png","bytes":4}],
        }));
        let frames = upload_begin(ctx.clone(), "r3", "a3", &msg).await;
        let frame = frame_json(&frames[0]);
        assert_eq!(frame["type"], "upload_begin_result");
        assert!(frame["error"].is_null() || frame.get("error").is_none());
        assert_eq!(
            frame["result"]["chunk_bytes"].as_i64(),
            Some(CHUNK_BYTES as i64)
        );
        assert_eq!(
            frame["result"]["limits"]["max_file_bytes"].as_i64(),
            Some(MAX_FILE_BYTES)
        );
        assert_eq!(
            frame["result"]["limits"]["max_batch_bytes"].as_i64(),
            Some(MAX_BATCH_BYTES)
        );
        assert!(valid_attachment_reference(
            frame["result"]["upload_id"].as_str().expect("id")
        ));
    }

    #[tokio::test]
    async fn chunk_and_finish_handlers_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let ctx = test_ctx(vec![agent()], dir);
        let body = png_body();
        let begin = message(serde_json::json!({
            "target": target_json(),
            "files": [{"name":"a.png","media_type":"image/png","bytes":body.len() as i64}],
        }));
        let frames = upload_begin(ctx.clone(), "r1", "a1", &begin).await;
        let upload_id = frame_json(&frames[0])["result"]["upload_id"]
            .as_str()
            .expect("id")
            .to_owned();
        // wrong-order chunk -> state_unknown, session discarded
        let chunk = message(serde_json::json!({
            "target": target_json(),
            "upload_id": upload_id,
            "file_index": 0,
            "sequence": 3,
            "data": b64(&body),
            "sha256": sha(&body),
        }));
        let frames = upload_chunk(ctx.clone(), "r2", "a2", &chunk).await;
        let frame = frame_json(&frames[0]);
        assert_eq!(frame["type"], "upload_chunk_result");
        assert_eq!(frame["error"]["code"], "attachment_upload_state_unknown");
        assert_eq!(
            frame["error"]["args"]["expected_sequence"].as_i64(),
            Some(0)
        );
        // retry the same chunk after discard -> session_not_found still maps
        // to attachment_upload_state_unknown
        let chunk = message(serde_json::json!({
            "target": target_json(),
            "upload_id": upload_id,
            "file_index": 0,
            "sequence": 0,
            "data": b64(&body),
            "sha256": sha(&body),
        }));
        let frames = upload_chunk(ctx.clone(), "r3", "a3", &chunk).await;
        assert_eq!(
            frame_json(&frames[0])["error"]["code"],
            "attachment_upload_state_unknown"
        );
    }

    #[tokio::test]
    async fn full_wire_cycle_and_cancel_result_shape() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let ctx = test_ctx(vec![agent()], dir);
        let body = png_body();
        let begin = message(serde_json::json!({
            "target": target_json(),
            "files": [{"name":"a.png","media_type":"image/png","bytes":body.len() as i64}],
        }));
        let frames = upload_begin(ctx.clone(), "r1", "a1", &begin).await;
        let upload_id = frame_json(&frames[0])["result"]["upload_id"]
            .as_str()
            .expect("id")
            .to_owned();
        let chunk = message(serde_json::json!({
            "target": target_json(),
            "upload_id": upload_id,
            "file_index": 0,
            "sequence": 0,
            "data": b64(&body),
            "sha256": sha(&body),
        }));
        let frames = upload_chunk(ctx.clone(), "r2", "a2", &chunk).await;
        let frame = frame_json(&frames[0]);
        assert_eq!(frame["result"]["file_index"].as_i64(), Some(0));
        assert_eq!(frame["result"]["next_sequence"].as_i64(), Some(1));
        assert_eq!(frame["result"]["received_bytes"], body.len() as i64);
        let finish = message(serde_json::json!({
            "target": target_json(),
            "upload_id": upload_id,
            "files": [{"file_index": 0, "sha256": sha(&body)}],
        }));
        let frames = upload_finish(ctx.clone(), "r3", "a3", &finish).await;
        let frame = frame_json(&frames[0]);
        assert_eq!(frame["type"], "upload_finish_result");
        let reference = frame["result"]["attachments"][0]["ref"]
            .as_str()
            .expect("ref")
            .to_owned();
        assert!(valid_attachment_reference(&reference));
        // cancel on a finished (discarded) id is idempotent
        let cancel = message(serde_json::json!({
            "target": target_json(),
            "upload_id": upload_id,
        }));
        let frames = upload_cancel(ctx.clone(), "r4", "a4", &cancel).await;
        let frame = frame_json(&frames[0]);
        assert_eq!(frame["type"], "upload_cancel_result");
        assert_eq!(frame["request_id"], "r4");
        assert_eq!(frame["result"], serde_json::json!({}));
    }
}
