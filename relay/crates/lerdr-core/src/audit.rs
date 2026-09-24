//! Write-audit log — the `internal/audit` port.
//!
//! Ports `internal/audit/logger.go` (`Logger` over
//! `<cacheDir>/audit/remote-writes.jsonl`) plus the record-building helpers
//! from `internal/app/server.go`: `auditAction` ([`action_of`]),
//! `auditWriteDetails` ([`write_details`]), `boundedAuditString`
//! ([`bounded_audit_string`]), and `auditInteger` ([`audit_integer`]).
//!
//! Security contract preserved from the oracle:
//!
//! - The audit directory is `0700`, the log file `0600`; both are repaired
//!   on open and on every append.
//! - The log path must be a regular file — never a symlink — checked by
//!   `Lstat` at open, at rotation, and again via `fstat`+`SameFile` on every
//!   append (with the create/open race retried up to 3 times).
//! - `send_secret` records get `{"text_bytes": N}` only — no `payload_sha256`
//!   and no `keys`: the digest of a low-entropy secret is crackable offline
//!   and the key list can spell the secret out.
//! - Records rotate `remote-writes.jsonl` → `.1` → `.2` → `.3` past 5 MiB,
//!   then fsync the directory; each record is written + fsynced + closed
//!   individually.
//!
//! `write_details`/`action_of` take the *decoded raw map*
//! (`&serde_json::Map<String, Value>`) — the same `message map[string]any`
//! the oracle hashes and inspects — not [`lerdr_core::protocol::Inbound`]:
//! `payload_sha256` must cover every wire field, including the ones the
//! typed view drops. `Inbound::raw_fields` is private, so callers pass the
//! map they decoded (the session layer's `raw_map`).

// Nothing in the crate calls this API yet — the orchestrator wires it into
// the session dispatch/`command_result` path (`recordWriteAudit`) in the
// follow-up commit.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, ErrorKind, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::protocol::classify_action;

/// `maxAuditBytes` — rotate once `size + next` would exceed 5 MiB.
const MAX_AUDIT_BYTES: u64 = 5 * 1024 * 1024;
/// `maxRotations` — keep `.1` through `.3`.
const MAX_ROTATIONS: u32 = 3;
/// Per-record line cap (`128*1024` in `Append`).
const MAX_RECORD_BYTES: usize = 128 * 1024;
/// `filepath.Join(dir, "remote-writes.jsonl")`.
const LOG_NAME: &str = "remote-writes.jsonl";

/// `audit.Record` — one JSONL line. Field order mirrors the Go struct so the
/// serialized record reads identically; `skip_serializing_if` mirrors
/// `omitempty` (Go drops empty strings, nil pointers, and empty maps).
///
/// `timestamp` is stamped by [`AuditLog::append`] (RFC3339Nano UTC) — any
/// caller-supplied value is overwritten, exactly like `record.Timestamp =
/// time.Now().UTC().Format(time.RFC3339Nano)` in `Append`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Record {
    pub timestamp: String,
    pub stage: String,
    pub action: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub request_id: String,
    /// `client_id` has no `omitempty` in Go — always emitted.
    pub client_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub connection_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub pane_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub agent: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub project: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub session: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub host: String,
    /// `*bool` in Go — `Some` is always emitted, `None` omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub phase: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub error: String,
    /// `map[string]any` in Go — `omitempty` drops nil *and* empty maps, so an
    /// empty `details` (e.g. result records, where Go sets `Details = nil`)
    /// omits the key.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, Value>,
}

/// `audit.Logger` — appends one fsync'd JSON line per remote write.
///
/// `Send`/`Sync` come free: appends serialize on the inner mutex like Go's
/// `l.mu`, so the orchestrator can share one `AuditLog` across sessions via
/// `Arc` (mutex poisoning is recovered rather than fatal — Go mutexes never
/// poison).
#[derive(Debug)]
pub struct AuditLog {
    /// `None` is the nil-`Logger` analogue: `Append` on nil returns nil.
    inner: Option<Inner>,
}

#[derive(Debug)]
struct Inner {
    /// `l.dir` — fsync'd after rotation.
    dir: PathBuf,
    /// `l.path` — `<dir>/remote-writes.jsonl`.
    path: PathBuf,
    /// `l.mu` — serializes rotate + append.
    mu: Mutex<()>,
}

impl AuditLog {
    /// `audit.Open` — create/repair `<cache_dir>/audit` (0700) and validate
    /// the log file (existing path must be a regular file, chmod 0600).
    pub fn open(cache_dir: &Path) -> io::Result<AuditLog> {
        let dir = cache_dir.join("audit");
        // `os.MkdirAll(dir, 0o700)` — mode applies to every created element.
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .map_err(|e| wrap(e, "create audit directory"))?;
        // `os.Chmod(dir, 0o700)` repairs permissions on a pre-existing dir.
        fs::set_permissions(&dir, Permissions::from_mode(0o700))
            .map_err(|e| wrap(e, "protect audit directory"))?;
        let path = dir.join(LOG_NAME);
        match fs::symlink_metadata(&path) {
            Ok(info) => {
                if !info.file_type().is_file() || info.file_type().is_symlink() {
                    return Err(plain("audit path is not a regular file"));
                }
                fs::set_permissions(&path, Permissions::from_mode(0o600))
                    .map_err(|e| wrap(e, "protect audit file"))?;
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(wrap(e, "inspect audit file")),
        }
        Ok(AuditLog {
            inner: Some(Inner {
                dir,
                path,
                mu: Mutex::new(()),
            }),
        })
    }

    /// The nil-`Logger` analogue — every [`append`](Self::append) is a no-op
    /// returning `Ok(())`, like `Logger.Append` on a nil receiver.
    pub fn noop() -> AuditLog {
        AuditLog { inner: None }
    }

    /// Whether this is the [`noop`](Self::noop) variant (wiring may want to
    /// skip record construction entirely).
    pub fn is_noop(&self) -> bool {
        self.inner.is_none()
    }

    /// `Logger.Append` — stamp, clamp, marshal, rotate if needed, then a
    /// symlink-safe append + fsync + close.
    pub fn append(&self, mut record: Record) -> io::Result<()> {
        let Some(inner) = &self.inner else {
            return Ok(());
        };
        record.timestamp = rfc3339_nano(SystemTime::now());
        record.stage = clamp(&record.stage, 32);
        record.action = clamp(&record.action, 80);
        record.request_id = clamp(&record.request_id, 160);
        record.client_id = clamp(&record.client_id, 160);
        record.connection_id = clamp(&record.connection_id, 160);
        record.pane_id = clamp(&record.pane_id, 160);
        record.agent = clamp(&record.agent, 160);
        record.project = clamp(&record.project, 512);
        record.session = clamp(&record.session, 512);
        record.host = clamp(&record.host, 255);
        record.phase = clamp(&record.phase, 80);
        record.error = clamp(&record.error, 1000);
        // `json.Marshal(record)` — the Go-compatible formatter so the line
        // (and the size it is measured against) matches the oracle's bytes.
        let mut line = crate::json::to_vec(&record).map_err(|e| wrap(e, "encode audit record"))?;
        line.push(b'\n');
        if line.len() > MAX_RECORD_BYTES {
            return Err(plain("audit record exceeds size limit"));
        }

        let _guard = inner.mu.lock().unwrap_or_else(PoisonError::into_inner);
        inner.rotate_if_needed(line.len() as u64)?;
        let mut file = inner.open_append_file()?;
        // Like `Append`: a short write is an error (write_all loops), and the
        // fsync runs even when the write failed — errors join on the first.
        let write_result = file.write_all(&line);
        let sync_result = file.sync_all();
        // `file.Close()` is implicit on drop; a drop can't surface close(2)
        // errors the way Go's join does — the fsync above is the durable part.
        write_result
            .and(sync_result)
            .map_err(|e| wrap(e, "append audit record"))
    }
}

impl Inner {
    /// `Logger.openAppendFile` — symlink-safe append open, retried 3 times
    /// when the file appears or vanishes mid-open.
    fn open_append_file(&self) -> io::Result<File> {
        for _ in 0..3 {
            let info = match fs::symlink_metadata(&self.path) {
                Err(e) if e.kind() == ErrorKind::NotFound => {
                    // Missing: `O_CREATE|O_EXCL|O_APPEND|O_WRONLY, 0o600`.
                    match OpenOptions::new()
                        .create_new(true)
                        .append(true)
                        .mode(0o600)
                        .open(&self.path)
                    {
                        Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
                        Err(e) => return Err(wrap(e, "create audit file")),
                        Ok(file) => {
                            let opened = file
                                .metadata()
                                .map_err(|e| wrap(e, "validate new audit file"))?;
                            if !opened.file_type().is_file() {
                                return Err(plain("new audit path is not a regular file"));
                            }
                            file.set_permissions(Permissions::from_mode(0o600))
                                .map_err(|e| wrap(e, "protect audit file"))?;
                            return Ok(file);
                        }
                    }
                }
                Err(e) => return Err(wrap(e, "inspect audit file")),
                Ok(info) => info,
            };
            if !info.file_type().is_file() || info.file_type().is_symlink() {
                return Err(plain("audit path is not a regular file"));
            }
            let file = match OpenOptions::new().append(true).open(&self.path) {
                Err(e) if e.kind() == ErrorKind::NotFound => continue,
                Err(e) => return Err(wrap(e, "open audit file")),
                Ok(file) => file,
            };
            let opened = file
                .metadata()
                .map_err(|e| wrap(e, "validate audit file"))?;
            // `os.SameFile(info, opened)`.
            if !opened.file_type().is_file() || !same_file(&info, &opened) {
                return Err(plain("audit file changed while it was opening"));
            }
            file.set_permissions(Permissions::from_mode(0o600))
                .map_err(|e| wrap(e, "protect audit file"))?;
            return Ok(file);
        }
        Err(plain("audit file changed repeatedly while it was opening"))
    }

    /// `Logger.rotateIfNeeded` — when `size + next` exceeds 5 MiB, rename the
    /// chain `remote-writes.jsonl` → `.1` → `.2` → `.3` (deleting `.3`
    /// first), then fsync the directory so the renames are durable.
    fn rotate_if_needed(&self, next_bytes: u64) -> io::Result<()> {
        let info = match fs::symlink_metadata(&self.path) {
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(wrap(e, "inspect audit file")),
            Ok(info) => info,
        };
        if !info.file_type().is_file() || info.file_type().is_symlink() {
            return Err(plain("audit path is not a regular file"));
        }
        if info.len() + next_bytes <= MAX_AUDIT_BYTES {
            return Ok(());
        }
        for index in (1..=MAX_ROTATIONS).rev() {
            let old = if index == 1 {
                self.path.clone()
            } else {
                rotated(&self.path, index - 1)
            };
            let new = rotated(&self.path, index);
            if index == MAX_ROTATIONS {
                // `os.Remove(newPath)` — removes a file, symlink, or empty
                // dir; only NotFound is tolerated.
                match fs::symlink_metadata(&new) {
                    Ok(meta) if meta.is_dir() => {
                        fs::remove_dir(&new).map_err(|e| wrap(e, "remove oldest audit rotation"))?
                    }
                    Ok(_) => fs::remove_file(&new)
                        .map_err(|e| wrap(e, "remove oldest audit rotation"))?,
                    Err(e) if e.kind() == ErrorKind::NotFound => {}
                    Err(e) => return Err(wrap(e, "remove oldest audit rotation")),
                }
            }
            match fs::rename(&old, &new) {
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                result => result.map_err(|e| wrap(e, "rotate audit file"))?,
            }
        }
        // `directory.Sync()` — fsync the directory so the rename chain is
        // durable.
        let directory = File::open(&self.dir).map_err(|e| wrap(e, "open audit directory"))?;
        directory
            .sync_all()
            .map_err(|e| wrap(e, "sync audit rotation"))
    }
}

/// `fmt.Sprintf("%s.%d", l.path, index)`.
fn rotated(path: &Path, index: u32) -> PathBuf {
    PathBuf::from(format!("{}.{index}", path.display()))
}

/// `os.SameFile` — same device + inode.
fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}

/// `isAuditedWrite` — `ClassifyAction(action)` must be known and `Audited`.
/// The catalog (and its `audited` flags) is ported once in
/// [`classify_action`]; this stays a thin predicate over it.
pub fn is_audited(action: &str) -> bool {
    classify_action(action).is_some_and(|m| m.audited)
}

/// `auditAction` — the `type` field, else `action` when `type` is empty or
/// `"command"`. Operates on the decoded map: a `"command"` message with no
/// `action` yields `""` here even though `Inbound::decode_map` would keep
/// `type = "command"`.
pub fn action_of(message: &serde_json::Map<String, Value>) -> String {
    let action = message.get("type").and_then(Value::as_str).unwrap_or("");
    if action.is_empty() || action == "command" {
        return message
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
    }
    action.to_owned()
}

/// `auditWriteDetails` — the secret-safety core of the audit record.
///
/// `send_secret` gets `{"text_bytes": N}` only — no digest, no keys. Every
/// other action gets `payload_sha256` + `payload_bytes` (SHA-256 and byte
/// length of the Go-compatible re-marshal of the decoded map — key order is
/// normalized exactly like `json.Marshal`) plus bounded copies of the
/// audited fields.
pub fn write_details(message: &serde_json::Map<String, Value>) -> BTreeMap<String, Value> {
    let mut details = BTreeMap::new();
    // A secret answering a noecho prompt gets no payload digest and no keys:
    // the digest of a low-entropy secret is crackable offline and the keys
    // spell the secret out. Only its shape is auditable.
    if action_of(message) == "send_secret" {
        if let Some(text) = message.get("text").and_then(Value::as_str) {
            details.insert("text_bytes".to_owned(), Value::from(text.len()));
        }
        return details;
    }

    // `json.Marshal(message)` — Go marshals maps with sorted keys and renders
    // integral float64s without a fraction, so the map is number-normalized
    // before the Go-compatible formatter runs (serde_json::Map is already
    // BTreeMap-backed → sorted).
    let mut normalized = message.clone();
    normalized.values_mut().for_each(normalize_numbers);
    if let Ok(encoded) = crate::json::to_vec(&normalized) {
        let digest = Sha256::digest(&encoded);
        details.insert(
            "payload_sha256".to_owned(),
            Value::from(hex::encode(digest)),
        );
        details.insert("payload_bytes".to_owned(), Value::from(encoded.len()));
    }

    // `stringLimits` — bounded copies of audited string fields (present and
    // non-empty only).
    const STRING_LIMITS: &[(&str, usize)] = &[
        ("name", 256),
        ("label", 256),
        ("profile_id", 160),
        ("workspace_id", 160),
        ("before_workspace_id", 160),
        ("cwd", 1024),
        ("path", 1024),
        ("branch", 512),
        ("base", 512),
        ("filename", 512),
        ("mime", 128),
        ("activity_label", 256),
    ];
    for (key, limit) in STRING_LIMITS {
        if let Some(value) = message.get(*key).and_then(Value::as_str) {
            if !value.is_empty() {
                details.insert(
                    (*key).to_owned(),
                    Value::from(bounded_audit_string(value, *limit)),
                );
            }
        }
    }
    // Byte lengths only for payload-bearing strings.
    for key in ["text", "prompt", "choice", "data"] {
        if let Some(value) = message.get(key).and_then(Value::as_str) {
            if !value.is_empty() {
                details.insert(format!("{key}_bytes"), Value::from(value.len()));
            }
        }
    }
    // Non-negative integral numerics.
    for key in ["index", "insert_index", "total", "_server_sequence"] {
        if let Some(value) = message.get(key).and_then(audit_integer) {
            details.insert(key.to_owned(), Value::from(value));
        }
    }
    // Presence-gated bools — `false` is recorded when the field is present.
    if let Some(force) = message.get("force").and_then(Value::as_bool) {
        details.insert("force".to_owned(), Value::from(force));
    }
    if let Some(close_group) = message.get("close_group").and_then(Value::as_bool) {
        details.insert("close_group".to_owned(), Value::from(close_group));
    }
    // `workspace_ids`/`expected_workspace_ids`: non-empty strings only,
    // ≤32 entries, each ≤160 runes. `keys`: ≤32 strings, ≤64 runes each —
    // the oracle's `keys` arm does *not* drop empty strings.
    let workspace_ids = bounded_str_list(message.get("workspace_ids"), 160, 32, true);
    if !workspace_ids.is_empty() {
        details.insert("workspace_ids".to_owned(), Value::from(workspace_ids));
    }
    let expected = bounded_str_list(message.get("expected_workspace_ids"), 160, 32, true);
    if !expected.is_empty() {
        details.insert("expected_workspace_ids".to_owned(), Value::from(expected));
    }
    let keys = bounded_str_list(message.get("keys"), 64, 32, false);
    if !keys.is_empty() {
        details.insert("keys".to_owned(), Value::from(keys));
    }
    // `selected_indices` — `auditInteger` each (negatives and non-integral
    // floats drop out), ≤128 entries.
    let indices: Vec<i64> = message
        .get("selected_indices")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(audit_integer).take(128).collect())
        .unwrap_or_default();
    if !indices.is_empty() {
        details.insert("selected_indices".to_owned(), Value::from(indices));
    }
    details
}

/// The `d.state.Agent(paneID)` read `recordWriteAudit` fills on every
/// record — empty fields omit themselves on the wire.
#[derive(Debug, Clone, Default)]
pub struct Attribution {
    pub agent: String,
    pub project: String,
    pub session: String,
    pub host: String,
}

/// The message-context fields `recordWriteAudit` pulls out of the decoded
/// wire map. Built once at admission (attempt record) and reused for the
/// completion record — the async handler path keeps the context without
/// retaining the raw payload.
#[derive(Debug, Clone)]
pub struct RequestContext {
    /// `auditAction(message)` — `type`, else `action` when `type` is
    /// `"command"`/empty.
    pub action: String,
    pub request_id: String,
    /// `message["client_id"]`, else `"connection:" + client.ID()`.
    pub client_id: String,
    /// The transport connection id (`client.ID()`).
    pub connection_id: String,
    /// `pane_id`, else `target.pane_id`.
    pub pane_id: String,
}

impl RequestContext {
    /// The extraction half of `recordWriteAudit` — straight off the
    /// decoded map so fields `Inbound` drops still count.
    pub fn from_message(
        message: &serde_json::Map<String, Value>,
        connection_id: &str,
    ) -> RequestContext {
        let str_field = |key: &str| {
            message
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        let mut pane_id = str_field("pane_id");
        if pane_id.is_empty() {
            if let Some(target) = message.get("target").and_then(Value::as_object) {
                pane_id = target
                    .get("pane_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
            }
        }
        let mut client_id = str_field("client_id");
        if client_id.is_empty() {
            client_id = format!("connection:{connection_id}");
        }
        RequestContext {
            action: action_of(message),
            request_id: str_field("request_id"),
            client_id,
            connection_id: connection_id.to_owned(),
            pane_id,
        }
    }

    /// Same context for callers that only retained the decoded
    /// [`Inbound`](crate::protocol::Inbound) — `type` is the operation for
    /// every message that passed `RequestScope` admission.
    pub fn from_inbound(inbound: &crate::protocol::Inbound, connection_id: &str) -> RequestContext {
        let mut pane_id = inbound.pane_id.clone();
        if pane_id.is_empty() {
            if let Some(target) = &inbound.target {
                pane_id = target.pane_id.clone();
            }
        }
        let mut client_id = inbound.client_id.clone();
        if client_id.is_empty() {
            client_id = format!("connection:{connection_id}");
        }
        RequestContext {
            action: inbound.r#type.clone(),
            request_id: inbound.request_id.clone(),
            client_id,
            connection_id: connection_id.to_owned(),
            pane_id,
        }
    }

    fn base(&self, stage: &str, attribution: Attribution) -> Record {
        Record {
            timestamp: String::new(),
            stage: stage.to_owned(),
            action: self.action.clone(),
            request_id: self.request_id.clone(),
            client_id: self.client_id.clone(),
            connection_id: self.connection_id.clone(),
            pane_id: self.pane_id.clone(),
            agent: attribution.agent,
            project: attribution.project,
            session: attribution.session,
            host: attribution.host,
            ok: None,
            phase: String::new(),
            error: String::new(),
            details: BTreeMap::new(),
        }
    }
}

/// `recordWriteAudit(client, msg, nil)` — the admission record, emitted
/// after authorization and before the action switch.
pub fn attempt_record(
    context: &RequestContext,
    message: &serde_json::Map<String, Value>,
    attribution: Attribution,
) -> Record {
    let mut record = context.base("attempt", attribution);
    record.details = write_details(message);
    record
}

/// `recordWriteAudit(client, msg, result)` — the completion record. The
/// oracle derives attribution from the *message's* pane and only then
/// applies `result.PaneID` as the record's pane override; details stay
/// empty (`Details = nil`).
pub fn result_record(
    context: &RequestContext,
    result: &crate::protocol::CommandResultMessage,
    attribution: Attribution,
) -> Record {
    let mut record = context.base("result", attribution);
    record.ok = result.ok;
    record.phase = result.phase.clone().unwrap_or_default();
    record.error = result.error.clone().unwrap_or_default();
    if let Some(pane_id) = &result.pane_id {
        if !pane_id.is_empty() {
            record.pane_id = pane_id.clone();
        }
    }
    record
}

/// The `[]any`/`[]string` arms of the list loops: collect string items
/// (optionally skipping empties), each bounded to `runes`, stopping once
/// `cap` entries were *appended* — the `len(...) == cap` break in Go counts
/// appended entries, matching `Iterator::take`.
fn bounded_str_list(
    value: Option<&Value>,
    runes: usize,
    cap: usize,
    skip_empty: bool,
) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .filter(|s| !(skip_empty && s.is_empty()))
                .map(|s| bounded_audit_string(s, runes))
                .take(cap)
                .collect()
        })
        .unwrap_or_default()
}

/// `boundedAuditString`/`clamp` — `strings.TrimSpace` then truncate to
/// `limit` *runes* (never bytes — a multi-byte char can't be split).
fn bounded_audit_string(value: &str, limit: usize) -> String {
    clamp(value, limit)
}

/// `clamp` in logger.go — same trim+rune-truncate the record fields get.
fn clamp(value: &str, limit: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= limit {
        return trimmed.to_owned();
    }
    trimmed.chars().take(limit).collect()
}

/// `auditInteger` — on the audit path every number arrives as a decoded
/// `float64` (`json.Unmarshal` into `map[string]any` never produces ints, so
/// the `int`/`uint64` arms are unreachable), making the `float64` arm the
/// whole behavior. Routing through `as_f64` also preserves the precision
/// quirk: `9007199254740993` decodes as `2^53` and *is* accepted, exactly
/// like the oracle.
fn audit_integer(value: &Value) -> Option<i64> {
    match value.as_f64() {
        // `number >= 0 && number <= 1<<53 && number == float64(int64(number))`.
        Some(f) if f >= 0.0 && f <= (1u64 << 53) as f64 && f == f.trunc() => Some(f as i64),
        _ => None,
    }
}

/// Twin of `normalize_numbers` in `lerdr-core/src/protocol/inbound.rs`:
/// `json.Marshal` renders integral float64s without a fraction (`500.0` →
/// `500`), so floats that are exactly representable as `i64` are rewritten
/// before hashing — this is what keeps `payload_sha256` byte-identical to
/// the oracle's digest of `json.Marshal(message)`. Wider than the `2^53-1`
/// bound used for the typed decode: exactness of the cast is the only
/// requirement here.
fn normalize_numbers(value: &mut Value) {
    match value {
        Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                // f64 → i64 is exact for integral |f| strictly below 2^63;
                // `i64::MAX as f64` rounds *up* to 2^63, hence the strict `<`.
                const MAX_EXACT: f64 = 9_223_372_036_854_775_808.0; // 2^63
                if f.fract() == 0.0 && f >= i64::MIN as f64 && f < MAX_EXACT {
                    *value = Value::from(f as i64);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(normalize_numbers),
        Value::Object(map) => map.values_mut().for_each(normalize_numbers),
        _ => {}
    }
}

/// `time.Now().UTC().Format(time.RFC3339Nano)` — fraction printed with
/// trailing zeros stripped, omitted entirely when zero, `Z` for UTC.
fn rfc3339_nano(now: SystemTime) -> String {
    let (secs, nanos) = match now.duration_since(UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i64, i64::from(d.subsec_nanos())),
        // Pre-epoch clock: now = epoch - d.
        Err(e) => {
            let d = e.duration();
            if d.subsec_nanos() == 0 {
                (-(d.as_secs() as i64), 0)
            } else {
                (
                    -(d.as_secs() as i64) - 1,
                    1_000_000_000 - i64::from(d.subsec_nanos()),
                )
            }
        }
    };
    let days = secs.div_euclid(86_400);
    let seconds_of_day = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let (hh, mm, ss) = (
        seconds_of_day / 3_600,
        seconds_of_day % 3_600 / 60,
        seconds_of_day % 60,
    );
    let fraction = if nanos == 0 {
        String::new()
    } else {
        let mut digits = format!("{nanos:09}");
        while digits.ends_with('0') {
            digits.pop();
        }
        format!(".{digits}")
    };
    format!("{year:04}-{month:02}-{day:02}T{hh:02}:{mm:02}:{ss:02}{fraction}Z")
}

/// Howard Hinnant's `civil_from_days` — proleptic-Gregorian date for a count
/// of days since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (
        if month <= 2 { year + 1 } else { year },
        month as u32,
        day as u32,
    )
}

/// A bare-message error for the oracle's `errors.New` paths.
fn plain(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

/// `fmt.Errorf("context: %w", err)` — keeps the underlying `ErrorKind`
/// (`serde_json::Error` maps through its `io::Error` conversion).
fn wrap(error: impl Into<io::Error>, context: &'static str) -> io::Error {
    let error = error.into();
    io::Error::new(error.kind(), format!("{context}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    use serde_json::{json, Map};
    use tempfile::TempDir;

    fn open_log(dir: &TempDir) -> (AuditLog, PathBuf) {
        let log = AuditLog::open(dir.path()).expect("open");
        let path = dir.path().join("audit").join(LOG_NAME);
        (log, path)
    }

    fn record(stage: &str, action: &str) -> Record {
        Record {
            stage: stage.to_owned(),
            action: action.to_owned(),
            client_id: "client-1".to_owned(),
            connection_id: "conn-1".to_owned(),
            ..Record::default()
        }
    }

    /// One appended line, parsed back as a JSON object.
    fn read_lines(path: &Path) -> Vec<Map<String, Value>> {
        fs::read_to_string(path)
            .expect("read log")
            .lines()
            .map(|line| serde_json::from_str(line).expect("valid JSONL"))
            .collect()
    }

    fn msg(pairs: &[(&str, Value)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    // ── open ──────────────────────────────────────────────────────────

    #[test]
    fn open_creates_private_directory() {
        let dir = TempDir::new().unwrap();
        let (log, _) = open_log(&dir);
        let meta = fs::metadata(dir.path().join("audit")).unwrap();
        assert_eq!(meta.mode() & 0o777, 0o700);
        assert!(!log.is_noop());
    }

    #[test]
    fn open_repairs_dir_and_file_permissions() {
        let dir = TempDir::new().unwrap();
        let audit_dir = dir.path().join("audit");
        fs::create_dir(&audit_dir).unwrap();
        fs::set_permissions(&audit_dir, Permissions::from_mode(0o755)).unwrap();
        let path = audit_dir.join(LOG_NAME);
        fs::write(&path, b"").unwrap();
        fs::set_permissions(&path, Permissions::from_mode(0o644)).unwrap();

        open_log(&dir);

        assert_eq!(fs::metadata(&audit_dir).unwrap().mode() & 0o777, 0o700);
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    }

    #[test]
    fn open_rejects_symlink() {
        let dir = TempDir::new().unwrap();
        let audit_dir = dir.path().join("audit");
        fs::create_dir(&audit_dir).unwrap();
        let target = dir.path().join("elsewhere");
        fs::write(&target, b"").unwrap();
        symlink(&target, audit_dir.join(LOG_NAME)).unwrap();
        let err = AuditLog::open(dir.path()).unwrap_err();
        assert_eq!(err.to_string(), "audit path is not a regular file");
    }

    #[test]
    fn open_rejects_non_regular_path() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("audit").join(LOG_NAME)).unwrap();
        assert!(AuditLog::open(dir.path()).is_err());
    }

    // ── append ────────────────────────────────────────────────────────

    #[test]
    fn noop_append_succeeds_without_io() {
        let log = AuditLog::noop();
        assert!(log.is_noop());
        log.append(record("attempt", "send_text")).expect("noop ok");
    }

    #[test]
    fn append_writes_stamped_jsonl() {
        let dir = TempDir::new().unwrap();
        let (log, path) = open_log(&dir);
        let mut r = record("attempt", "send_text");
        r.request_id = "req-9".to_owned();
        r.pane_id = "pane-3".to_owned();
        r.details.insert("text_bytes".to_owned(), json!(11));
        log.append(r).unwrap();

        let lines = read_lines(&path);
        assert_eq!(lines.len(), 1);
        let line = &lines[0];
        let stamp = line["timestamp"].as_str().unwrap();
        assert!(
            stamp.ends_with('Z') && stamp.contains('T'),
            "RFC3339: {stamp}"
        );
        assert_eq!(line["stage"], "attempt");
        assert_eq!(line["action"], "send_text");
        assert_eq!(line["request_id"], "req-9");
        assert_eq!(line["client_id"], "client-1");
        assert_eq!(line["connection_id"], "conn-1");
        assert_eq!(line["pane_id"], "pane-3");
        assert_eq!(line["details"]["text_bytes"], 11);
        // omitempty: unset fields absent entirely.
        for absent in [
            "agent", "project", "session", "host", "ok", "phase", "error",
        ] {
            assert!(!line.contains_key(absent), "{absent} should be omitted");
        }
        // File is append-private.
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    }

    #[test]
    fn append_emits_ok_false_not_omitted() {
        let dir = TempDir::new().unwrap();
        let (log, path) = open_log(&dir);
        let mut r = record("result", "workspace_close");
        r.ok = Some(false);
        r.phase = "confirmed".to_owned();
        r.error = "refused".to_owned();
        log.append(r).unwrap();
        let line = &read_lines(&path)[0];
        assert_eq!(line["ok"], false);
        assert_eq!(line["phase"], "confirmed");
        assert_eq!(line["error"], "refused");
        assert!(!line.contains_key("details"));
    }

    #[test]
    fn append_clamps_runes_not_bytes() {
        let dir = TempDir::new().unwrap();
        let (log, path) = open_log(&dir);
        let mut r = record(&"é".repeat(40), &"x".repeat(100));
        // Each 'é' is 2 bytes: 32 runes = 64 bytes — a byte-truncate would
        // split or miscount.
        assert_eq!(r.stage.chars().count(), 40);
        r.error = "e".repeat(1200);
        r.host = "h".repeat(300);
        r.project = "p".repeat(600);
        r.pane_id = "  padded  ".to_owned();
        log.append(r).unwrap();

        let line = &read_lines(&path)[0];
        assert_eq!(line["stage"].as_str().unwrap().chars().count(), 32);
        assert_eq!(line["action"].as_str().unwrap().chars().count(), 80);
        assert_eq!(line["error"].as_str().unwrap().chars().count(), 1000);
        assert_eq!(line["host"].as_str().unwrap().chars().count(), 255);
        assert_eq!(line["project"].as_str().unwrap().chars().count(), 512);
        assert_eq!(line["pane_id"], "padded", "TrimSpace before truncate");
    }

    #[test]
    fn append_rejects_oversized_record() {
        let dir = TempDir::new().unwrap();
        let (log, _) = open_log(&dir);
        let mut r = record("attempt", "upload_chunk");
        // Base fields are clamped, but `details` is not — a fat detail value
        // pushes the marshaled line past 128 KiB.
        r.details
            .insert("data".to_owned(), json!("x".repeat(200 * 1024)));
        let err = log.append(r).unwrap_err();
        assert_eq!(err.to_string(), "audit record exceeds size limit");
    }

    #[test]
    fn append_rejects_symlink_path() {
        let dir = TempDir::new().unwrap();
        let (log, path) = open_log(&dir);
        let target = dir.path().join("elsewhere");
        fs::write(&target, b"").unwrap();
        symlink(&target, &path).unwrap();
        let err = log.append(record("attempt", "send_text")).unwrap_err();
        assert_eq!(err.to_string(), "audit path is not a regular file");
    }

    #[test]
    fn append_recreates_deleted_file() {
        let dir = TempDir::new().unwrap();
        let (log, path) = open_log(&dir);
        log.append(record("attempt", "send_text")).unwrap();
        fs::remove_file(&path).unwrap();
        log.append(record("result", "send_text")).unwrap();
        let lines = read_lines(&path);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["stage"], "result");
    }

    #[test]
    fn append_survives_concurrent_swap() {
        // The create/open retry path (lstat→open races) can't be injected
        // deterministically; this drives real races instead — a thread
        // repeatedly deletes the file while appends run. Every append must
        // return (the retry may also exhaust → error, never panic) and the
        // surviving file must stay valid JSONL.
        let dir = TempDir::new().unwrap();
        let (log, path) = open_log(&dir);
        let swap_path = path.clone();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop2 = stop.clone();
        let swapper = std::thread::spawn(move || {
            while !stop2.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = fs::remove_file(&swap_path);
                std::thread::yield_now();
            }
        });
        for i in 0..60 {
            let mut r = record("attempt", "send_text");
            r.request_id = format!("race-{i}");
            let _ = log.append(r); // retries may exhaust under the race
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        swapper.join().unwrap();
        // After the race settles, a clean append must succeed.
        log.append(record("attempt", "send_text")).unwrap();
        for line in read_lines(&path) {
            assert_eq!(line["stage"], "attempt");
        }
    }

    // ── rotation ──────────────────────────────────────────────────────

    #[test]
    fn rotates_chain_and_drops_oldest() {
        let dir = TempDir::new().unwrap();
        let (log, path) = open_log(&dir);
        // Seed the chain with distinct payloads; current sits at the 5 MiB
        // cap so the next append rotates.
        let marker = |b: u8| vec![b; MAX_AUDIT_BYTES as usize];
        fs::write(&path, marker(b'0')).unwrap();
        fs::write(rotated(&path, 1), marker(b'1')).unwrap();
        fs::write(rotated(&path, 2), marker(b'2')).unwrap();
        fs::write(rotated(&path, 3), marker(b'3')).unwrap();

        log.append(record("attempt", "send_text")).unwrap();

        assert_eq!(fs::read(rotated(&path, 3)).unwrap(), marker(b'2'));
        assert_eq!(fs::read(rotated(&path, 2)).unwrap(), marker(b'1'));
        assert_eq!(fs::read(rotated(&path, 1)).unwrap(), marker(b'0'));
        // `.3` (b'3') was deleted by the rename onto it; current holds the
        // fresh record.
        let lines = read_lines(&path);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["action"], "send_text");
    }

    #[test]
    fn rotation_boundary_is_exclusive() {
        // Oracle: `info.Size() + nextBytes <= maxAuditBytes` → no rotate.
        // Drive `rotate_if_needed` directly so the boundary doesn't depend on
        // the stamped line's length.
        let dir = TempDir::new().unwrap();
        let (log, path) = open_log(&dir);
        let inner = log.inner.as_ref().unwrap();
        // size + next == 5 MiB exactly → no rotation.
        fs::write(&path, vec![b'x'; MAX_AUDIT_BYTES as usize - 1]).unwrap();
        inner.rotate_if_needed(1).unwrap();
        assert!(
            rotated(&path, 1).metadata().is_err(),
            "no rotation at boundary"
        );
        // size + next == 5 MiB + 1 → rotate.
        inner.rotate_if_needed(2).unwrap();
        assert!(rotated(&path, 1).metadata().is_ok(), "rotation past 5 MiB");
        assert_eq!(
            fs::metadata(rotated(&path, 1)).unwrap().len(),
            MAX_AUDIT_BYTES - 1
        );
    }

    #[test]
    fn append_rejects_symlink_during_rotation_check() {
        let dir = TempDir::new().unwrap();
        let (log, path) = open_log(&dir);
        let target = dir.path().join("elsewhere");
        fs::write(&target, vec![b'x'; 6 * 1024 * 1024]).unwrap();
        symlink(&target, &path).unwrap();
        // rotateIfNeeded's Lstat rejects before openAppendFile is reached.
        let err = log.append(record("attempt", "send_text")).unwrap_err();
        assert_eq!(err.to_string(), "audit path is not a regular file");
    }

    // ── details / action ──────────────────────────────────────────────

    #[test]
    fn send_secret_details_are_shape_only() {
        let message = msg(&[
            ("type", json!("send_secret")),
            ("text", json!("hunter2")),
            ("keys", json!(["h", "u", "n"])),
            ("name", json!("should-not-appear")),
            ("index", json!(4)),
        ]);
        let details = write_details(&message);
        assert_eq!(details.len(), 1);
        assert_eq!(details["text_bytes"], 7);
        assert!(!details.contains_key("payload_sha256"));
        assert!(!details.contains_key("payload_bytes"));
        assert!(!details.contains_key("keys"));
        assert!(!details.contains_key("name"));
    }

    #[test]
    fn send_secret_via_command_envelope() {
        // action_of reads `action` when `type` is "command" — the rewrite the
        // oracle performs before classify.
        let message = msg(&[
            ("type", json!("command")),
            ("action", json!("send_secret")),
            ("text", json!("s")),
        ]);
        let details = write_details(&message);
        assert_eq!(details.len(), 1);
        assert_eq!(details["text_bytes"], 1);
    }

    #[test]
    fn payload_digest_matches_go_marshal() {
        // Go hashes `json.Marshal(message)`: sorted keys, integral floats
        // without fraction. The expected digest below is over this literal —
        // key order in the wire message is deliberately different.
        let message = msg(&[
            ("type", json!("workspace_create")),
            ("name", json!("api")),
            ("cwd", json!("/srv/api")),
            ("workspace_id", json!("w1")),
            ("protocol", json!(3.0)), // float on the wire → "3" after marshal
        ]);
        let details = write_details(&message);
        let canonical = "{\"cwd\":\"/srv/api\",\"name\":\"api\",\"protocol\":3,\
                         \"type\":\"workspace_create\",\"workspace_id\":\"w1\"}";
        let expect = hex::encode(Sha256::digest(canonical.as_bytes()));
        assert_eq!(details["payload_sha256"], json!(expect));
        assert_eq!(details["payload_bytes"], json!(canonical.len()));
        assert_eq!(details["name"], "api");
        assert_eq!(details["cwd"], "/srv/api");
        assert_eq!(details["workspace_id"], "w1");
    }

    #[test]
    fn workspace_create_details_spot_check() {
        let message = msg(&[
            ("type", json!("workspace_create")),
            ("name", json!("  spaced name  ")),
            ("label", json!(&"l".repeat(300))),
            ("cwd", json!("/w")),
            ("force", json!(false)), // presence-gated: false is recorded
            ("expected_workspace_ids", json!(["a", "", "b"])),
            ("text", json!("body")),
        ]);
        let details = write_details(&message);
        assert_eq!(details["name"], "spaced name");
        assert_eq!(details["label"].as_str().unwrap().chars().count(), 256);
        assert_eq!(details["force"], false);
        assert_eq!(details["expected_workspace_ids"], json!(["a", "b"]));
        assert_eq!(details["text_bytes"], 4);
        assert!(!details.contains_key("text"), "only the length is recorded");
        assert!(details.contains_key("payload_sha256"));
    }

    #[test]
    fn send_keys_details_spot_check() {
        let message = msg(&[
            ("type", json!("send_keys")),
            (
                "keys",
                json!([
                    "ctrl",
                    "",
                    &"k".repeat(100), // empty kept, long truncated
                ]),
            ),
            ("index", json!(2.0)),        // integral float → 2
            ("total", json!(-1)),         // negative dropped
            ("insert_index", json!("x")), // non-numeric dropped
            ("selected_indices", json!([0, 3.0, -2, "4", 9])),
        ]);
        let details = write_details(&message);
        let keys = details["keys"].as_array().unwrap();
        assert_eq!(keys[0], "ctrl");
        assert_eq!(keys[1], "", "oracle keeps empty key strings");
        assert_eq!(keys[2].as_str().unwrap().chars().count(), 64);
        assert_eq!(details["index"], 2);
        assert!(!details.contains_key("total"));
        assert!(!details.contains_key("insert_index"));
        assert_eq!(details["selected_indices"], json!([0, 3, 9]));
    }

    #[test]
    fn list_caps_are_enforced() {
        let long_list: Vec<String> = (0..40).map(|i| format!("w{i}")).collect();
        let message = msg(&[
            ("type", json!("workspace_reorder")),
            ("workspace_ids", json!(long_list)),
            ("keys", json!(long_list)),
            ("selected_indices", json!((0..200).collect::<Vec<i64>>())),
        ]);
        let details = write_details(&message);
        assert_eq!(details["workspace_ids"].as_array().unwrap().len(), 32);
        assert_eq!(details["keys"].as_array().unwrap().len(), 32);
        assert_eq!(details["selected_indices"].as_array().unwrap().len(), 128);
    }

    #[test]
    fn action_of_semantics() {
        assert_eq!(
            action_of(&msg(&[("type", json!("send_text"))])),
            "send_text"
        );
        assert_eq!(
            action_of(&msg(&[
                ("type", json!("command")),
                ("action", json!("submit_prompt"))
            ])),
            "submit_prompt"
        );
        assert_eq!(action_of(&msg(&[("action", json!("respond"))])), "respond");
        // "command" with no action → "" (differs from Inbound::decode_map,
        // which keeps type="command").
        assert_eq!(action_of(&msg(&[("type", json!("command"))])), "");
        assert_eq!(action_of(&msg(&[])), "");
        // Non-string fields read as absent.
        assert_eq!(action_of(&msg(&[("type", json!(7))])), "");
    }

    #[test]
    fn is_audited_matches_catalog() {
        const AUDITED: &[&str] = &[
            "agent_clear",
            "agent_rename",
            "agent_restart",
            "agent_start",
            "agent_stop",
            "answer_question",
            "clarify_question",
            "create_device_invitation",
            "layout_apply",
            "navigate_question",
            "push_test_device",
            "rename_device",
            "respond",
            "reset_devices",
            "revoke_device",
            "send_input",
            "send_keys",
            "send_secret",
            "send_text",
            "speech_voice_install",
            "speech_voice_remove",
            "submit_prompt",
            "tab_reorder",
            "upload_begin",
            "upload_cancel",
            "upload_chunk",
            "upload_finish",
            "workspace_close",
            "workspace_create",
            "workspace_rename",
            "workspace_reorder",
            "worktree_create",
            "worktree_open",
            "worktree_remove",
        ];
        assert_eq!(AUDITED.len(), 34);
        for action in AUDITED {
            assert!(is_audited(action), "{action} should be audited");
        }
        for action in [
            "acknowledge_pane",
            "watch_pane",
            "read_pane",
            "device_list",
            "install_update",
            "push_policy_set",
            "speak_text",
            "worktree_list",
            "pane_search",
            "pane_selection_read",
            "pane_link_resolve",
            "pane_link_activate",
            "layout_export",
            "no_such_action",
            "",
        ] {
            assert!(!is_audited(action), "{action} should not be audited");
        }
    }

    // ── helpers ───────────────────────────────────────────────────────

    #[test]
    fn bounded_audit_string_trims_then_truncates_runes() {
        assert_eq!(bounded_audit_string("  hi  ", 10), "hi");
        let value = "é".repeat(300);
        assert_eq!(bounded_audit_string(&value, 160).chars().count(), 160);
        assert_eq!(bounded_audit_string("abc", 160), "abc");
    }

    #[test]
    fn audit_integer_accepts_oracle_cases() {
        assert_eq!(audit_integer(&json!(5)), Some(5));
        assert_eq!(audit_integer(&json!(5.0)), Some(5));
        assert_eq!(audit_integer(&json!(9007199254740992u64)), Some(1 << 53));
        // Go decodes every number as float64: 2^53+1 rounds to 2^53 and is
        // accepted — the oracle returns 9007199254740992 here.
        assert_eq!(audit_integer(&json!(9007199254740993u64)), Some(1 << 53));
        assert_eq!(audit_integer(&json!(-1)), None);
        assert_eq!(audit_integer(&json!(5.5)), None);
        assert_eq!(audit_integer(&json!(u64::MAX)), None);
        assert_eq!(audit_integer(&json!("5")), None);
        assert_eq!(audit_integer(&json!(null)), None);
    }

    #[test]
    fn rfc3339_nano_formats_like_go() {
        // epoch 1704163445 = 2024-01-02T02:44:05Z
        let epoch = UNIX_EPOCH + std::time::Duration::new(1_704_163_445, 0);
        assert_eq!(rfc3339_nano(epoch), "2024-01-02T02:44:05Z");
        let frac = UNIX_EPOCH + std::time::Duration::new(1_704_163_445, 123_400_000);
        assert_eq!(rfc3339_nano(frac), "2024-01-02T02:44:05.1234Z");
        let nano = UNIX_EPOCH + std::time::Duration::new(1_704_163_445, 7);
        assert_eq!(rfc3339_nano(nano), "2024-01-02T02:44:05.000000007Z");
    }
}
