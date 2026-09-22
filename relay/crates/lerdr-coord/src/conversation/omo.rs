//! OMO (Oh My OpenAgent) reader — port of `omo.go`: filename-pattern session
//! location, identity verification against the `type:"session"` record, the
//! `senpi.todo-state` custom rows, and the size/mtime/identity cache.

use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use serde_json::Value;

use super::claude::{capture_file_source, file_identity};
use super::reader::{
    contained_regular_file, load_tail_file, Error, Reader, DEFAULT_PAGE_SIZE,
    MAX_CONVERSATION_BYTES, MAX_OMO_CACHE_ENTRIES, MAX_PAGE_SIZE,
};
use super::records::{parse_transcript, MAX_ENTRY_BYTES};
use super::roots;
use super::types::{Entry, Location, OmoTodoPhase, OmoTodoState, OmoTodoTask, Page};

const MAX_OMO_TODO_BYTES: usize = 512 * 1024;
const MAX_OMO_IDENTITY_BYTES: u64 = 1024 * 1024;
const MAX_OMO_PHASES: usize = 128;
const MAX_OMO_TASKS: usize = 1000;
const MAX_OMO_TODO_STRING: usize = 4096;

/// `omoFilename` — `YYYY-MM-DDTHH-MM-SS-mmmZ_<id>.jsonl`.
fn omo_filename_session(name: &str) -> Option<&str> {
    let stem = name.strip_suffix(".jsonl")?;
    let (stamp, id) = stem.split_once('_')?;
    // stamp: 4-2-2 'T' 2-2-2 '-' 3 'Z'
    let b = stamp.as_bytes();
    if b.len() != 24
        || !b[0..4].iter().all(|c| c.is_ascii_digit())
        || b[4] != b'-'
        || !b[5..7].iter().all(|c| c.is_ascii_digit())
        || b[7] != b'-'
        || !b[8..10].iter().all(|c| c.is_ascii_digit())
        || b[10] != b'T'
        || !b[11..13].iter().all(|c| c.is_ascii_digit())
        || b[13] != b'-'
        || !b[14..16].iter().all(|c| c.is_ascii_digit())
        || b[16] != b'-'
        || !b[17..19].iter().all(|c| c.is_ascii_digit())
        || b[19] != b'-'
        || !b[20..23].iter().all(|c| c.is_ascii_digit())
        || b[23] != b'Z'
    {
        return None;
    }
    if id.is_empty()
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        return None;
    }
    Some(id)
}

/// `omoIdentity`.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct OmoIdentity {
    pub id: String,
    pub cwd: String,
}

/// `omoCacheEntry`.
pub(crate) struct OmoCacheEntry {
    pub size: i64,
    pub mod_time: i64,
    pub identity: String,
    pub entries: Vec<Entry>,
    pub plan: OmoTodoState,
    pub clipped: bool,
}

/// `safeOMOIdentity` — `[A-Za-z0-9_-]`, ≤128.
fn safe_omo_identity(value: &str) -> bool {
    if value.is_empty() || value.len() > 128 {
        return false;
    }
    value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// `encodeOMOCWD` — `/work` → `--work--`; runs of `/`, `\`, `:` → `-`.
fn encode_omo_cwd(cwd: &str) -> String {
    let trimmed = cwd.trim().trim_start_matches(['/', '\\']);
    let mut encoded = String::with_capacity(trimmed.len());
    let mut in_run = false;
    for ch in trimmed.chars() {
        if ch == '/' || ch == '\\' || ch == ':' {
            if !in_run {
                encoded.push('-');
                in_run = true;
            }
        } else {
            encoded.push(ch);
            in_run = false;
        }
    }
    format!("--{encoded}--")
}

/// `omoProjectDirectories` — the encoded cwd dir, or every `--…--` dir when
/// the cwd is empty.
fn omo_project_directories(root: &str, cwd: &str) -> Vec<std::path::PathBuf> {
    let root_path = Path::new(root);
    if !cwd.trim().is_empty() {
        let candidate = root_path.join(encode_omo_cwd(cwd));
        if candidate.is_dir() {
            return vec![candidate];
        }
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(root_path) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.starts_with("--") && name.ends_with("--") && entry.path().is_dir()
        })
        .map(|entry| entry.path())
        .collect()
}

/// `omoIdentityFromPath`.
fn omo_identity_from_path(path: &Path) -> Option<OmoIdentity> {
    let name = path.file_name()?.to_string_lossy();
    let id = omo_filename_session(&name)?;
    if !safe_omo_identity(id) {
        return None;
    }
    Some(OmoIdentity {
        id: id.to_string(),
        cwd: String::new(),
    })
}

/// `omoPathMatchesCWD`.
fn omo_path_matches_cwd(path: &Path, cwd: &str) -> bool {
    if cwd.trim().is_empty() {
        return true;
    }
    let parent = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    parent == encode_omo_cwd(cwd)
}

/// `verifyOMOIdentityFile` — scan ≤1 MiB for a `type:"session"` record; it
/// must carry the expected id and (when a cwd is requested) the exact cwd.
fn verify_omo_identity_file(
    file: &std::fs::File,
    expected: &OmoIdentity,
    requested_cwd: &str,
) -> bool {
    let mut reader = BufReader::new(file.take(MAX_OMO_IDENTITY_BYTES));
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return false,
            Ok(_) => {}
        }
        // bufio.Scanner drops the trailing newline; cap the logical line at
        // maxEntryBytes the same way (overlong lines error the scan and stop).
        let text = if line.last() == Some(&b'\n') {
            &line[..line.len() - 1]
        } else {
            &line[..]
        };
        if text.len() > MAX_ENTRY_BYTES {
            return false;
        }
        #[derive(serde::Deserialize)]
        struct SessionRecord {
            #[serde(default, rename = "type")]
            record_type: String,
            #[serde(default)]
            id: String,
            #[serde(default)]
            cwd: String,
        }
        let Ok(record) = serde_json::from_slice::<SessionRecord>(text) else {
            continue;
        };
        if record.record_type != "session" {
            continue;
        }
        if record.id != expected.id {
            return false;
        }
        if !requested_cwd.trim().is_empty() && record.cwd != requested_cwd {
            return false;
        }
        return true;
    }
}

/// `latestValidOMOTodo` — last *valid* `senpi.todo-state` row wins; an invalid
/// later row cannot erase an earlier valid state. Returns
/// `(state, found, invalid_only)`.
fn latest_valid_omo_todo(text: &[u8], session_id: &str) -> (OmoTodoState, bool, bool) {
    let mut latest = OmoTodoState::default();
    let mut found = false;
    let mut saw_todo = false;
    for line in text.split(|b| *b == b'\n') {
        if line.len() > MAX_OMO_TODO_BYTES {
            continue;
        }
        let Ok(record) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let Some(record) = record.as_object() else {
            continue;
        };
        if record.get("type").and_then(Value::as_str) != Some("custom")
            || record.get("customType").and_then(Value::as_str) != Some("senpi.todo-state")
        {
            continue;
        }
        saw_todo = true;
        let timestamp = record
            .get("timestamp")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if let Some(data) = record.get("data") {
            if let Some(state) = decode_omo_todo(data, session_id, &timestamp) {
                latest = state;
                found = true;
            }
        }
    }
    (latest, found, saw_todo && !found)
}

/// `decodeOMOTodo` — strict v2 schema; any malformed phase/task invalidates
/// the whole row.
fn decode_omo_todo(data: &Value, session_id: &str, timestamp: &str) -> Option<OmoTodoState> {
    let object = data.as_object()?;
    if object.get("schema").and_then(Value::as_str) != Some("v2") {
        return None;
    }
    let phases = object.get("phases")?.as_array()?;
    let mut state = OmoTodoState {
        session_id: session_id.to_string(),
        version: 2,
        updated_at: timestamp.to_string(),
        phases: Vec::new(),
        ..OmoTodoState::default()
    };
    let mut task_count = 0usize;
    for (phase_index, phase) in phases.iter().enumerate() {
        let phase = phase.as_object()?;
        let name = phase
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let tasks = phase.get("tasks").and_then(Value::as_array);
        if name.is_empty() || name.len() > MAX_OMO_TODO_STRING || tasks.is_none() {
            return None;
        }
        if phase_index >= MAX_OMO_PHASES {
            state.truncated = true;
            continue;
        }
        let mut output_phase = OmoTodoPhase {
            name,
            tasks: Vec::new(),
        };
        for task in tasks.unwrap() {
            let task = task.as_object()?;
            let content = task
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let status = task.get("status").and_then(Value::as_str).unwrap_or("");
            let id = task.get("id").and_then(Value::as_str).unwrap_or("");
            if content.is_empty()
                || content.len() > MAX_OMO_TODO_STRING
                || !valid_omo_todo_status(status)
                || id.len() > 128
            {
                return None;
            }
            if task_count >= MAX_OMO_TASKS {
                state.truncated = true;
                continue;
            }
            output_phase.tasks.push(OmoTodoTask {
                id: id.to_string(),
                content,
                status: status.to_string(),
            });
            task_count += 1;
        }
        state.phases.push(output_phase);
    }
    Some(state)
}

/// `validOMOTodoStatus`.
fn valid_omo_todo_status(status: &str) -> bool {
    matches!(
        status,
        "pending" | "in_progress" | "completed" | "abandoned"
    )
}

impl Reader {
    /// `locateOMO` — absolute `.jsonl` ids are contained-checked and must keep
    /// the encoded-cwd parent; bare ids scan each root's project directories
    /// for the `…_<id>.jsonl` filename. Two different resolved paths are an
    /// `invalid_session` ambiguity.
    pub(crate) fn omo_locate(
        &self,
        cwd: &str,
        session_id: &str,
    ) -> (Location, OmoIdentity, &'static str) {
        if session_id.is_empty() {
            return (
                Location::default(),
                OmoIdentity::default(),
                "invalid_session",
            );
        }
        let home = self.home.to_string_lossy().into_owned();
        let env = self.env();
        let roots = roots::omo_roots(&home, env);
        if Path::new(session_id).is_absolute() {
            for root in &roots {
                let Some(path) = contained_regular_file(Path::new(session_id), Path::new(root))
                else {
                    continue;
                };
                let Some(identity) = omo_identity_from_path(&path) else {
                    return (
                        Location::default(),
                        OmoIdentity::default(),
                        "invalid_session",
                    );
                };
                if !omo_path_matches_cwd(&path, cwd) {
                    return (
                        Location::default(),
                        OmoIdentity::default(),
                        "invalid_session",
                    );
                }
                return (
                    Location {
                        path: path.to_string_lossy().into_owned(),
                        root: root.clone(),
                        title: String::new(),
                    },
                    identity,
                    "",
                );
            }
            return (
                Location::default(),
                OmoIdentity::default(),
                "path_uncontained",
            );
        }
        if !safe_omo_identity(session_id) {
            return (
                Location::default(),
                OmoIdentity::default(),
                "invalid_session",
            );
        }
        let mut found = Location::default();
        for root in &roots {
            for directory in omo_project_directories(root, cwd) {
                let Ok(entries) = std::fs::read_dir(&directory) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    match omo_filename_session(&name) {
                        Some(id) if id == session_id => {}
                        _ => continue,
                    }
                    let Some(path) = contained_regular_file(&entry.path(), Path::new(root)) else {
                        continue;
                    };
                    let path_str = path.to_string_lossy().into_owned();
                    if !found.path.is_empty() && found.path != path_str {
                        return (
                            Location::default(),
                            OmoIdentity::default(),
                            "invalid_session",
                        );
                    }
                    found = Location {
                        path: path_str,
                        root: root.clone(),
                        title: String::new(),
                    };
                }
            }
        }
        if found.path.is_empty() {
            return (
                Location::default(),
                OmoIdentity::default(),
                "invalid_session",
            );
        }
        (
            found,
            OmoIdentity {
                id: session_id.to_string(),
                cwd: cwd.to_string(),
            },
            "",
        )
    }

    /// `loadOMO` — identity check, cache lookup, tail parse + todo state.
    fn omo_load(
        &self,
        location: &Location,
        identity: &OmoIdentity,
        cwd: &str,
    ) -> (Vec<Entry>, OmoTodoState, bool, &'static str) {
        let Ok(source) = capture_file_source(location) else {
            return (
                Vec::new(),
                OmoTodoState::default(),
                false,
                "source_unavailable",
            );
        };
        if !verify_omo_identity_file(&source.file, identity, cwd) {
            return (
                Vec::new(),
                OmoTodoState::default(),
                false,
                "invalid_session",
            );
        }
        let key = format!("{}\x00{}\x00{}", location.path, identity.id, cwd);
        let source_identity = file_identity(&source.info);
        {
            let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(cached) = inner.omo_cache.get(&key) {
                let mod_time = source
                    .info
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos() as i64)
                    .unwrap_or(0);
                if cached.size == source.end
                    && cached.mod_time == mod_time
                    && cached.identity == source_identity
                {
                    return (
                        cached.entries.clone(),
                        cached.plan.clone(),
                        cached.clipped,
                        "",
                    );
                }
            }
        }
        let Ok((text, clipped)) = load_tail_file(&source.file, MAX_CONVERSATION_BYTES) else {
            return (
                Vec::new(),
                OmoTodoState::default(),
                false,
                "source_unavailable",
            );
        };
        let entries = parse_transcript("pi", &text);
        let mut plan = OmoTodoState {
            available: true,
            session_id: identity.id.clone(),
            phases: Vec::new(),
            truncated: clipped,
            ..OmoTodoState::default()
        };
        let (state, found, invalid_only) = latest_valid_omo_todo(&text, &identity.id);
        if found {
            let mut state = state;
            state.available = true;
            state.truncated = state.truncated || clipped;
            plan = state;
        } else if invalid_only {
            plan.reason_code = "source_corrupt".to_string();
        }
        // Cache only when the path still resolves to the captured file
        // (`os.SameFile` + same size + same mtime).
        if let Ok(current) = std::fs::metadata(&location.path) {
            let same = current.dev() == source.info.dev()
                && current.ino() == source.info.ino()
                && current.len() == source.info.len()
                && current.modified().ok() == source.info.modified().ok();
            if same {
                let mod_time = source
                    .info
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos() as i64)
                    .unwrap_or(0);
                let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
                if inner.omo_cache.len() >= MAX_OMO_CACHE_ENTRIES {
                    inner.omo_cache.clear();
                }
                inner.omo_cache.insert(
                    key,
                    OmoCacheEntry {
                        size: source.end,
                        mod_time,
                        identity: source_identity,
                        entries: entries.clone(),
                        plan: plan.clone(),
                        clipped,
                    },
                );
            }
        }
        (entries, plan, clipped, "")
    }

    /// `readOMO` — locate + load + page; an unknown `before` is
    /// `invalid_cursor` (unlike the flat providers).
    pub(crate) fn omo_read(
        &self,
        cwd: &str,
        session_id: &str,
        before: &str,
        limit: usize,
    ) -> Result<Page, Error> {
        let session_id = session_id.trim();
        let (location, identity, code) = self.omo_locate(cwd, session_id);
        if !code.is_empty() {
            return Ok(Page::unavailable(
                code,
                "OMO conversation history is unavailable.",
            ));
        }
        let (entries, plan, clipped, code) = self.omo_load(&location, &identity, cwd);
        if !code.is_empty() {
            let reason = if code == "invalid_session" {
                "OMO session identity does not match the requested session."
            } else {
                "OMO conversation history is unavailable."
            };
            return Ok(Page::unavailable(code, reason));
        }
        let limit = if limit < 1 {
            DEFAULT_PAGE_SIZE
        } else {
            limit.min(MAX_PAGE_SIZE)
        };
        let mut end = entries.len();
        if !before.is_empty() {
            let mut found = false;
            for (index, entry) in entries.iter().enumerate() {
                if entry.id == before {
                    end = index;
                    found = true;
                    break;
                }
            }
            if !found {
                return Ok(Page::unavailable(
                    "invalid_cursor",
                    "The conversation cursor is invalid.",
                ));
            }
        }
        let start = end.saturating_sub(limit);
        Ok(Page {
            available: true,
            entries: entries[start..end].to_vec(),
            has_more: start > 0,
            total: entries.len() as i64,
            file_truncated: clipped,
            source_corrupt: plan.reason_code == "source_corrupt",
            omo_plan: Some(plan),
            source_path: location.path.clone(),
            ..Page::default()
        })
    }
}
