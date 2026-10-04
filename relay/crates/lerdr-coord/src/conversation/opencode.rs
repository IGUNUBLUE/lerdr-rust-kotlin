//! OpenCode reader — port of `opencode.go`. Queries `opencode.db` through the
//! `sqlite3` CLI (`-readonly -batch -json`) with a file-stamp-keyed cache.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use super::reader::{contained_regular_file, Error, Reader, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE};
use super::records::{new_tool_activity, normalize_entries_for_response, MAX_ENTRY_BYTES};
use super::roots;
use super::sqlite::{look_path, run_json_query, SqliteError};
use super::types::{Entry, Page};
use super::util::{clamp_text, sanitize_text};

const OPENCODE_QUERY_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_OPENCODE_OUTPUT: usize = 8 * 1024 * 1024;
const MAX_OPENCODE_CACHE: usize = 128;

#[derive(Debug, Default, Deserialize)]
struct OpenCodeRow {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    directory: String,
    #[serde(default)]
    #[allow(dead_code)]
    title: String,
    #[serde(default)]
    #[allow(dead_code)]
    time_updated: i64,
    #[serde(default)]
    #[allow(dead_code)]
    agent: String,
    #[serde(default)]
    message_id: String,
    #[serde(default)]
    time_created: i64,
    #[serde(default)]
    message_data: String,
    #[serde(default)]
    message_type: String,
    #[serde(default)]
    part_id: String,
    #[serde(default)]
    part_data: String,
    #[serde(default)]
    message_total: i64,
    #[serde(default)]
    cursor_found: i64,
    /// Internal: which database produced these rows (Go `Database`, `json:"-"`).
    #[serde(skip)]
    database: String,
}

/// `openCodeFileStamp` — database + WAL size/mtime pair.
#[derive(Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    database_size: i64,
    database_time: i64,
    wal_size: i64,
    wal_time: i64,
}

fn file_stamp(database: &str) -> Option<FileStamp> {
    let info = std::fs::metadata(database).ok()?;
    let mut stamp = FileStamp {
        database_size: info.len() as i64,
        database_time: info
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0),
        wal_size: 0,
        wal_time: 0,
    };
    if let Ok(wal) = std::fs::metadata(format!("{database}-wal")) {
        stamp.wal_size = wal.len() as i64;
        stamp.wal_time = wal
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
    }
    Some(stamp)
}

struct CacheEntry {
    stamp: FileStamp,
    rows: Vec<OpenCodeRow>,
    has_more: bool,
}

#[derive(Default)]
struct OpenCodeCache {
    pages: HashMap<String, CacheEntry>,
    schemas: HashMap<String, (FileStamp, bool)>,
}

/// `openCodeReader`.
pub(crate) struct OpenCodeReader {
    cache: Mutex<OpenCodeCache>,
}

impl OpenCodeReader {
    pub(crate) fn new() -> Self {
        OpenCodeReader {
            cache: Mutex::new(OpenCodeCache::default()),
        }
    }

    /// `databases` — `sqlite3` must be on PATH; each `OpenCodeDBs` candidate is
    /// contained-checked against the paired `OpenCodeData` root.
    fn databases(&self, reader: &Reader) -> Result<Vec<String>, &'static str> {
        if !look_path("sqlite3", reader.env()) {
            return Err("source_unavailable");
        }
        let home = reader.home.to_string_lossy().into_owned();
        let env = reader.env();
        let roots = roots::opencode_data_roots(&home, env);
        let candidates = roots::opencode_dbs(&home, env);
        let mut databases = Vec::new();
        for (index, candidate) in candidates.iter().enumerate() {
            let Some(root) = roots.get(index) else {
                break;
            };
            if let Some(path) = contained_regular_file(Path::new(candidate), Path::new(root)) {
                databases.push(path.to_string_lossy().into_owned());
            }
        }
        if databases.is_empty() {
            return Err("source_unavailable");
        }
        Ok(databases)
    }

    /// `openCodeReader.read` — try each database until one answers for the
    /// session; first query failure wins when none matches.
    fn read(
        &self,
        reader: &Reader,
        session_id: &str,
        before: &str,
        limit: usize,
    ) -> (Vec<Entry>, bool, bool, OpenCodeRow, &'static str) {
        if !valid_session_id(session_id) {
            return (
                Vec::new(),
                false,
                false,
                OpenCodeRow::default(),
                "invalid_session",
            );
        }
        if !before.is_empty() && (before.len() > 256 || before.contains(['\x00', '\r', '\n'])) {
            return (
                Vec::new(),
                false,
                false,
                OpenCodeRow::default(),
                "invalid_cursor",
            );
        }
        let databases = match self.databases(reader) {
            Ok(databases) => databases,
            Err(code) => return (Vec::new(), false, false, OpenCodeRow::default(), code),
        };
        let mut first_failure = "";
        for database in databases {
            let (rows, has_more, query_code) =
                self.query(reader, &database, session_id, before, limit);
            if !query_code.is_empty() {
                if first_failure.is_empty() {
                    first_failure = query_code;
                }
                continue;
            }
            if rows.is_empty() || rows[0].session_id != session_id {
                continue;
            }
            if !before.is_empty() && rows[0].cursor_found == 0 {
                return (
                    Vec::new(),
                    false,
                    false,
                    OpenCodeRow::default(),
                    "invalid_cursor",
                );
            }
            let metadata = OpenCodeRow {
                session_id: rows[0].session_id.clone(),
                directory: rows[0].directory.clone(),
                title: rows[0].title.clone(),
                message_id: rows[0].message_id.clone(),
                message_total: rows[0].message_total,
                cursor_found: rows[0].cursor_found,
                database,
                ..OpenCodeRow::default()
            };
            let (entries, corrupt) = parse_rows(&rows);
            return (entries, has_more, corrupt, metadata, "");
        }
        if !first_failure.is_empty() {
            return (
                Vec::new(),
                false,
                false,
                OpenCodeRow::default(),
                first_failure,
            );
        }
        (
            Vec::new(),
            false,
            false,
            OpenCodeRow::default(),
            "invalid_session",
        )
    }

    fn schema_v2(&self, database: &str, stamp: Option<FileStamp>) -> Result<bool, &'static str> {
        if let Some(stamp) = stamp {
            let cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
            if let Some((cached, v2)) = cache.schemas.get(database) {
                if *cached == stamp {
                    return Ok(*v2);
                }
            }
        }
        let output = run_json_query(
            "sqlite3",
            database,
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='session_v2') AS v2;",
            MAX_OPENCODE_OUTPUT,
            OPENCODE_QUERY_TIMEOUT,
        )
        .map_err(|err| match err {
            SqliteError::OutputLimit => "output_limit",
            SqliteError::QueryFailed => "query_failed",
        })?;
        let rows: Vec<Value> = serde_json::from_slice(&output).map_err(|_| "source_corrupt")?;
        let v2 = rows
            .first()
            .and_then(|row| row["v2"].as_i64())
            .ok_or("source_corrupt")?
            != 0;
        if let Some(stamp) = stamp.filter(|stamp| file_stamp(database) == Some(*stamp)) {
            let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(entry) = cache.schemas.get_mut(database) {
                *entry = (stamp, v2);
            } else {
                if cache.schemas.len() >= MAX_OPENCODE_CACHE {
                    cache.schemas.clear();
                }
                cache.schemas.insert(database.to_owned(), (stamp, v2));
            }
        }
        Ok(v2)
    }

    /// Read the detected native schema; v1 orders by `(time_created, id)`,
    /// v2 by its unique per-session sequence. Cursors remain message ids.
    fn query(
        &self,
        _reader: &Reader,
        database: &str,
        session_id: &str,
        before: &str,
        limit: usize,
    ) -> (Vec<OpenCodeRow>, bool, &'static str) {
        let stamp = file_stamp(database);
        let cache_key = format!("{database}\x00{session_id}\x00{before}\x00{limit}");
        if let Some(stamp) = stamp {
            let cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(entry) = cache.pages.get(&cache_key) {
                if entry.stamp == stamp {
                    // Rows are not Clone-cheap; reparse cost is acceptable but
                    // the cache stores raw JSON so clone via serde is trivial.
                    let rows: Vec<OpenCodeRow> = entry.rows.iter().map(clone_row).collect();
                    return (rows, entry.has_more, "");
                }
            }
        }
        let v2 = match self.schema_v2(database, stamp) {
            Ok(v2) => v2,
            Err(code) => return (Vec::new(), false, code),
        };
        let query = if v2 {
            v2_query(session_id, before, limit)
        } else {
            let session_hex = hex::encode(session_id.as_bytes());
            let mut cursor_cte =
                "cursor AS (SELECT NULL AS time_created,NULL AS id WHERE 0)".to_string();
            let mut cursor_filter = String::new();
            let mut cursor_found = "1".to_string();
            if !before.is_empty() {
                let cursor_hex = hex::encode(before.as_bytes());
                cursor_cte = format!(
                    "cursor AS (SELECT time_created,id FROM message WHERE session_id=CAST(X'{session_hex}' AS TEXT) AND id=CAST(X'{cursor_hex}' AS TEXT))"
                );
                cursor_filter = " AND EXISTS(SELECT 1 FROM cursor) AND (m.time_created < (SELECT time_created FROM cursor) OR (m.time_created = (SELECT time_created FROM cursor) AND m.id < (SELECT id FROM cursor)))".to_string();
                cursor_found = "EXISTS(SELECT 1 FROM cursor)".to_string();
            }
            format!(
                "WITH {cursor_cte},selected AS (\
                SELECT m.id,m.time_created,m.data FROM message AS m WHERE m.session_id=CAST(X'{session_hex}' AS TEXT){cursor_filter} \
                ORDER BY m.time_created DESC,m.id DESC LIMIT {limit_plus}\
            ) SELECT s.id AS session_id,s.directory,s.title,s.time_updated,COALESCE(s.agent,'') AS agent,\
            COALESCE(sm.id,'') AS message_id,COALESCE(sm.time_created,0) AS time_created,COALESCE(sm.data,'null') AS message_data,\
            COALESCE(p.id,'') AS part_id,COALESCE(p.data,'null') AS part_data,\
            (SELECT COUNT(*) FROM message WHERE session_id=s.id) AS message_total,{cursor_found} AS cursor_found \
            FROM session AS s LEFT JOIN selected AS sm ON 1=1 LEFT JOIN part AS p ON p.message_id=sm.id \
            WHERE s.id=CAST(X'{session_hex}' AS TEXT) ORDER BY sm.time_created DESC,sm.id DESC,p.id DESC;",
                limit_plus = limit + 1,
            )
        };
        let output = match run_json_query(
            "sqlite3",
            database,
            &query,
            MAX_OPENCODE_OUTPUT,
            OPENCODE_QUERY_TIMEOUT,
        ) {
            Ok(output) => output,
            Err(SqliteError::OutputLimit) => return (Vec::new(), false, "output_limit"),
            Err(SqliteError::QueryFailed) => return (Vec::new(), false, "query_failed"),
        };
        let mut rows: Vec<OpenCodeRow> = match serde_json::from_slice(&output) {
            Ok(rows) => rows,
            Err(_) => return (Vec::new(), false, "source_corrupt"),
        };
        rows.reverse();
        // Distinct message ids in chronological order; the extra (oldest)
        // message's rows are dropped when the page is full.
        let mut message_ids: Vec<String> = Vec::with_capacity(limit + 1);
        let mut seen: HashSet<String> = HashSet::with_capacity(limit + 1);
        for row in &rows {
            if row.message_id.is_empty() || seen.contains(&row.message_id) {
                continue;
            }
            seen.insert(row.message_id.clone());
            message_ids.push(row.message_id.clone());
        }
        let has_more = message_ids.len() > limit;
        if has_more {
            let extra_id = message_ids[0].clone();
            rows.retain(|row| row.message_id != extra_id);
        }
        if let Some(stamp) = stamp {
            if file_stamp(database) == Some(stamp) {
                let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
                if cache.pages.len() >= MAX_OPENCODE_CACHE {
                    cache.pages.clear();
                }
                cache.pages.insert(
                    cache_key,
                    CacheEntry {
                        stamp,
                        rows: rows.iter().map(clone_row).collect(),
                        has_more,
                    },
                );
            }
        }
        (rows, has_more, "")
    }
}

fn v2_query(session_id: &str, before: &str, limit: usize) -> String {
    let session = hex::encode(session_id.as_bytes());
    let cursor = if before.is_empty() {
        "SELECT NULL AS seq WHERE 0".to_owned()
    } else {
        format!(
            "SELECT seq FROM session_message WHERE session_id=CAST(X'{session}' AS TEXT) \
             AND id=CAST(X'{}' AS TEXT) AND type IN ('user','assistant')",
            hex::encode(before.as_bytes())
        )
    };
    let older = if before.is_empty() {
        ""
    } else {
        " AND m.seq < (SELECT seq FROM cursor)"
    };
    let found = if before.is_empty() {
        "1"
    } else {
        "EXISTS(SELECT 1 FROM cursor)"
    };
    format!(
        "WITH cursor AS ({cursor}),selected AS (\
         SELECT m.id,m.type,m.seq,m.time_created,m.data FROM session_message m \
         WHERE m.session_id=CAST(X'{session}' AS TEXT) AND m.type IN ('user','assistant'){older} \
         ORDER BY m.seq DESC LIMIT {}\
         ) SELECT s.id AS session_id,s.directory,COALESCE(s.title,'') AS title,s.time_updated,COALESCE(s.agent,'') AS agent,\
         COALESCE(sm.id,'') AS message_id,COALESCE(sm.time_created,0) AS time_created,\
         COALESCE(sm.data,'null') AS message_data,COALESCE(sm.type,'') AS message_type,\
         (SELECT COUNT(*) FROM session_message WHERE session_id=s.id AND type IN ('user','assistant')) AS message_total,\
         {found} AS cursor_found FROM session_v2 s LEFT JOIN selected sm ON 1=1 \
         WHERE s.id=CAST(X'{session}' AS TEXT) ORDER BY sm.seq DESC;",
        limit + 1
    )
}

fn clone_row(row: &OpenCodeRow) -> OpenCodeRow {
    OpenCodeRow {
        session_id: row.session_id.clone(),
        directory: row.directory.clone(),
        title: row.title.clone(),
        time_updated: row.time_updated,
        agent: row.agent.clone(),
        message_id: row.message_id.clone(),
        time_created: row.time_created,
        message_data: row.message_data.clone(),
        message_type: row.message_type.clone(),
        part_id: row.part_id.clone(),
        part_data: row.part_data.clone(),
        message_total: row.message_total,
        cursor_found: row.cursor_found,
        database: row.database.clone(),
    }
}

/// `sameOpenCodeDirectory` — `EvalSymlinks` on both paths, then equality.
pub(crate) fn same_opencode_directory(cwd: &str, directory: &str) -> bool {
    let real_cwd = std::fs::canonicalize(super::roots::clean_path(cwd));
    let real_directory = std::fs::canonicalize(super::roots::clean_path(directory));
    match (real_cwd, real_directory) {
        (Ok(cwd), Ok(directory)) => cwd == directory,
        _ => false,
    }
}

/// `validOpenCodeSessionID` — `ses_` + 4..124 alnum, total ≤128.
fn valid_session_id(value: &str) -> bool {
    if !value.starts_with("ses_") || value.len() < 8 || value.len() > 128 {
        return false;
    }
    value[4..].bytes().all(|b| b.is_ascii_alphanumeric())
}

/// `parseOpenCodeRows` — group part rows by message; text parts join with
/// '\n'; tool parts become `ToolActivity`; anything unparseable or with a
/// non-visible role marks `corrupt`.
fn parse_rows(rows: &[OpenCodeRow]) -> (Vec<Entry>, bool) {
    let mut entries: Vec<Entry> = if rows.first().is_some_and(|row| !row.message_type.is_empty()) {
        Vec::with_capacity(rows.len())
    } else {
        Vec::new()
    };
    let mut by_message: HashMap<String, usize> = HashMap::new();
    let mut corrupt = false;
    for row in rows {
        if row.message_id.is_empty() {
            continue;
        }
        if row.message_id.len() > 256 || row.message_id.contains(['\x00', '\r', '\n']) {
            corrupt = true;
            continue;
        }
        if !row.message_type.is_empty() {
            match parse_v2_row(row) {
                Ok(entry) => entries.push(entry),
                Err(_) => corrupt = true,
            }
            continue;
        }
        let index = match by_message.get(&row.message_id) {
            Some(index) => *index,
            None => {
                let parsed: Result<Value, _> = serde_json::from_str(&row.message_data);
                let role = parsed
                    .as_ref()
                    .ok()
                    .and_then(|v| v.get("role"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if parsed.is_err() || (role != "user" && role != "assistant") {
                    corrupt = true;
                    continue;
                }
                let mut entry = Entry {
                    id: row.message_id.clone(),
                    role,
                    ..Entry::default()
                };
                if row.time_created > 0 {
                    entry.timestamp = super::util::format_unix_millis(row.time_created);
                }
                let index = entries.len();
                by_message.insert(row.message_id.clone(), index);
                entries.push(entry);
                index
            }
        };
        if row.part_id.is_empty() {
            continue;
        }
        let part: Value = match serde_json::from_str(&row.part_data) {
            Ok(part) => part,
            Err(_) => {
                corrupt = true;
                continue;
            }
        };
        let entry = &mut entries[index];
        match part.get("type").and_then(Value::as_str).unwrap_or("") {
            "text" => {
                let text = sanitize_text(part.get("text").and_then(Value::as_str).unwrap_or(""));
                if text.is_empty() {
                    continue;
                }
                if !entry.text.is_empty() {
                    entry.text.push('\n');
                }
                entry.text.push_str(&text);
            }
            "tool" => {
                // `part.State.Input` is json.RawMessage in the retired implementation — the
                // raw member text is passed through, not re-marshalled (which
                // would sort object keys).
                let input = super::util::json_member_raw(&row.part_data, "state")
                    .and_then(|state| super::util::json_member_raw(state, "input"))
                    .filter(|raw| !raw.is_empty() && *raw != "null")
                    .unwrap_or("")
                    .to_string();
                let mut activity = new_tool_activity(
                    part.get("callID").and_then(Value::as_str).unwrap_or(""),
                    part.get("tool").and_then(Value::as_str).unwrap_or(""),
                    Some(&Value::String(input)),
                );
                let output = sanitize_text(
                    part.pointer("/state/output")
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                );
                let (output, truncated) = clamp_text(&output, MAX_ENTRY_BYTES);
                activity.output = output;
                activity.truncated = truncated;
                let status = part
                    .pointer("/state/status")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let error_text = part
                    .pointer("/state/error")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                activity.error = status == "error" || !error_text.trim().is_empty();
                entry.tools.push(activity);
            }
            _ => {}
        }
    }
    let mut kept = Vec::with_capacity(entries.len());
    for mut entry in entries {
        let (text, truncated) = clamp_text(&entry.text, MAX_ENTRY_BYTES);
        entry.text = text;
        entry.truncated = truncated;
        if !entry.text.is_empty() || !entry.tools.is_empty() {
            kept.push(entry);
        }
    }
    (kept, corrupt)
}

fn append_text(target: &mut String, text: &str) {
    let text = sanitize_text(text);
    if text.is_empty() {
        return;
    }
    if target.is_empty() {
        *target = text;
    } else {
        target.push('\n');
        target.push_str(&text);
    }
}

fn parse_v2_row(row: &OpenCodeRow) -> Result<Entry, serde_json::Error> {
    let data: Value = serde_json::from_str(&row.message_data)?;
    let mut entry = Entry {
        id: row.message_id.clone(),
        role: row.message_type.clone(),
        timestamp: if row.time_created > 0 {
            super::util::format_unix_millis(row.time_created)
        } else {
            String::new()
        },
        ..Entry::default()
    };
    if row.message_type == "user" {
        append_text(&mut entry.text, data["text"].as_str().unwrap_or(""));
    } else if let Some(content) = data["content"].as_array() {
        for item in content {
            match item["type"].as_str() {
                Some("text") => append_text(&mut entry.text, item["text"].as_str().unwrap_or("")),
                Some("tool") => {
                    let state = &item["state"];
                    let mut tool = new_tool_activity(
                        item["id"].as_str().unwrap_or(""),
                        item["name"].as_str().unwrap_or(""),
                        state.get("input"),
                    );
                    if let Some(content) = state["content"].as_array() {
                        for part in content {
                            if part["type"] == "text" {
                                append_text(&mut tool.output, part["text"].as_str().unwrap_or(""));
                            }
                        }
                    }
                    let (output, truncated) = clamp_text(&tool.output, MAX_ENTRY_BYTES);
                    tool.output = output;
                    tool.truncated |= truncated;
                    tool.error = state["status"] == "error"
                        || state
                            .pointer("/error/message")
                            .and_then(Value::as_str)
                            .is_some_and(|s| !s.trim().is_empty());
                    entry.tools.push(tool);
                }
                _ => {}
            }
        }
    }
    if entry.text.is_empty() && entry.tools.is_empty() {
        append_text(
            &mut entry.text,
            data.pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or(""),
        );
    }
    Ok(entry)
}

impl Reader {
    /// `readOpenCodeFor` — trim + clamp, read, workspace check, then page.
    pub(crate) fn opencode_read_for(
        &self,
        cwd: &str,
        session_id: &str,
        before: &str,
        limit: usize,
    ) -> Result<Page, Error> {
        let session_id = session_id.trim();
        let limit = if limit < 1 {
            DEFAULT_PAGE_SIZE
        } else {
            limit.min(MAX_PAGE_SIZE)
        };
        let (mut entries, has_more, corrupt, metadata, code) =
            self.opencode.read(self, session_id, before, limit);
        if !code.is_empty() {
            return Ok(Page::unavailable(
                code,
                "OpenCode conversation history is unavailable.",
            ));
        }
        if !cwd.is_empty() && !same_opencode_directory(cwd, &metadata.directory) {
            return Ok(Page::unavailable(
                "invalid_session",
                "This conversation belongs to a different workspace.",
            ));
        }
        normalize_entries_for_response(&mut entries);
        Ok(Page {
            available: true,
            entries,
            has_more,
            total: metadata.message_total,
            source_corrupt: corrupt,
            cursor_before: metadata.message_id,
            source_path: metadata.database,
            ..Page::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::process::Command;
    use tempfile::TempDir;

    fn sql(database: &Path, sql: &str) {
        let output = Command::new("sqlite3")
            .arg(database)
            .arg(sql)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn v2_sequence_pages_exclude_idle_and_fence_workspace_and_cursor() {
        let home = TempDir::new().unwrap();
        let cwd = home.path().join("work");
        let root = home.path().join("data");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        let database = root.join("opencode.db");
        sql(&database, &format!(
            "CREATE TABLE session_v2(id TEXT PRIMARY KEY,directory TEXT,title TEXT,time_updated INTEGER,agent TEXT);
             CREATE TABLE session_message(id TEXT PRIMARY KEY,session_id TEXT,type TEXT,seq INTEGER,time_created INTEGER,data TEXT,UNIQUE(session_id,seq));
             INSERT INTO session_v2 VALUES('ses_history123',CAST(X'{}' AS TEXT),NULL,0,NULL);",
            hex::encode(cwd.to_str().unwrap())
        ));
        for (id, kind, seq, time, data) in [
            ("msg_user", "user", 4, 900, json!({"text":"question"})),
            (
                "msg_middle",
                "assistant",
                5,
                100,
                json!({"content":[{"type":"reasoning","text":"not visible"},{"type":"text","text":"first"},{"type":"text","text":"second"}]}),
            ),
            ("msg_idle", "idle", 12, 1000, json!({"outcome":"success"})),
            (
                "msg_latest",
                "assistant",
                18,
                50,
                json!({"content":[{"type":"text","text":"answer"}]}),
            ),
        ] {
            sql(
                &database,
                &format!(
                    "INSERT INTO session_message VALUES('{id}','ses_history123','{kind}',{seq},{time},CAST(X'{}' AS TEXT));",
                    hex::encode(data.to_string())
                ),
            );
        }
        let data_root = root.to_string_lossy().into_owned();
        let reader = Reader::new_with_env(
            home.path().to_owned(),
            Box::new(move |key| (key == "LERDR_OPENCODE_DATA_DIRS").then(|| data_root.clone())),
        );
        let cwd = cwd.to_str().unwrap();
        let page = reader
            .opencode_read_for(cwd, "ses_history123", "", 2)
            .unwrap();
        assert!(page.available && page.has_more && !page.source_corrupt);
        assert_eq!(page.total, 3);
        assert_eq!(
            page.entries
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            ["msg_middle", "msg_latest"]
        );
        assert_eq!(page.entries[0].text, "first\nsecond");
        assert_eq!(page.entries[1].text, "answer");
        let older = reader
            .opencode_read_for(cwd, "ses_history123", &page.cursor_before, 2)
            .unwrap();
        assert!(!older.has_more);
        assert_eq!(older.entries[0].id, "msg_user");
        assert_eq!(older.entries[0].role, "user");
        assert_eq!(older.entries[0].text, "question");
        assert_eq!(
            reader
                .opencode_read_for(cwd, "ses_history123", "msg_absent", 2)
                .unwrap()
                .reason_code,
            "invalid_cursor"
        );
        assert_eq!(
            reader
                .opencode_read_for(home.path().to_str().unwrap(), "ses_history123", "", 2)
                .unwrap()
                .reason_code,
            "invalid_session"
        );
    }

    #[test]
    fn v2_tool_states_keep_partial_inputs_and_native_text_results() {
        for (status, input, expected_input, expected_output, failed) in [
            ("streaming", json!("{\"path\":"), "{\"path\":", "", false),
            (
                "running",
                json!({"path":"file"}),
                "{\"path\":\"file\"}",
                "",
                false,
            ),
            (
                "completed",
                json!({"path":"file"}),
                "{\"path\":\"file\"}",
                "saved\nok",
                false,
            ),
            (
                "error",
                json!({"path":"file"}),
                "{\"path\":\"file\"}",
                "",
                true,
            ),
        ] {
            let state = json!({
                "status":status,"input":input,
                "content":if status == "completed" { json!([{"type":"text","text":"saved"},{"type":"file","uri":"file:///result","mime":"text/plain"},{"type":"text","text":"ok"}]) } else { json!([]) },
                "error":if failed { json!({"type":"tool.failed","message":"denied"}) } else { Value::Null }
            });
            let row = OpenCodeRow {
                message_id: "msg_tool".into(),
                message_type: "assistant".into(),
                message_data:
                    json!({"content":[{"type":"tool","id":"call1","name":"write","state":state}]})
                        .to_string(),
                ..OpenCodeRow::default()
            };
            let (entries, corrupt) = parse_rows(&[row]);
            assert!(!corrupt);
            let tool = &entries[0].tools[0];
            assert_eq!((&*tool.id, &*tool.name), ("call1", "write"));
            assert_eq!(tool.input, expected_input);
            assert_eq!(tool.output, expected_output);
            assert_eq!(tool.error, failed);
        }
    }

    #[test]
    fn v2_corrupt_payload_does_not_hide_native_assistant_error() {
        let rows = [
            OpenCodeRow { message_id: "msg_bad".into(), message_type: "user".into(), message_data: "{".into(), ..OpenCodeRow::default() },
            OpenCodeRow { message_id: "msg_error".into(), message_type: "assistant".into(), message_data: json!({"content":[],"error":{"type":"provider.invalid-output","message":"Tool call is missing id or name"}}).to_string(), ..OpenCodeRow::default() },
        ];
        let (entries, corrupt) = parse_rows(&rows);
        assert!(corrupt);
        assert_eq!(entries[0].id, "msg_error");
        assert_eq!(entries[0].text, "Tool call is missing id or name");
    }
}
