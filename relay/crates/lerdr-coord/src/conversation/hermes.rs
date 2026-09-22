//! Hermes reader — port of `hermes.go`. Same `sqlite3` CLI strategy as
//! OpenCode: `-readonly -batch -json` with hex-literal text parameters.

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use super::opencode::same_opencode_directory;
use super::reader::{
    contained_regular_file, safe_session_id, Error, Reader, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE,
};
use super::records::{
    new_tool_activity, normalize_entries_for_response, tool_association_id, MAX_ENTRY_BYTES,
};
use super::roots;
use super::sqlite::{look_path, run_json_query, SqliteError};
use super::types::{Entry, Location, Page, ToolActivity};
use super::util::{
    clamp_text, first_string, first_value, format_unix_seconds_float, sanitize_text,
};

const HERMES_QUERY_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_HERMES_OUTPUT: usize = 8 * 1024 * 1024;

#[derive(Debug, Default, Deserialize)]
pub(crate) struct HermesRow {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    message_id: i64,
    #[serde(default)]
    role: String,
    #[serde(default)]
    content_hex: String,
    // `tool_call_id`, `tool_name` and `display_kind` are selected by the CTE
    // (dedup partition + `<> 'hidden'` filter) but not consumed per row.
    #[serde(default)]
    tool_calls: String,
    #[serde(default)]
    timestamp: f64,
    #[serde(default)]
    message_total: i64,
    #[serde(default)]
    cursor_found: i64,
}

#[derive(Debug, Deserialize)]
struct HermesToolRow {
    #[serde(default)]
    tool_call_id: String,
    #[serde(default)]
    content_hex: String,
}

/// `hermesReader` — stateless (the oracle carries no query cache for Hermes).
pub(crate) struct HermesReader;

impl HermesReader {
    pub(crate) fn new() -> Self {
        HermesReader
    }

    /// `databases` — `sqlite3` on PATH plus each `HermesDBs` candidate
    /// contained under its paired `HermesData` root.
    fn databases(&self, reader: &Reader) -> Result<Vec<String>, &'static str> {
        if !look_path("sqlite3", reader.env()) {
            return Err("source_unavailable");
        }
        let home = reader.home.to_string_lossy().into_owned();
        let env = reader.env();
        let roots = roots::hermes_data_roots(&home, env);
        let candidates = roots::hermes_dbs(&home, env);
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

    /// `locateContext` — probe each database for the session (limit 1); a
    /// session row whose cwd mismatches is `invalid_session`.
    pub(crate) fn locate(
        &self,
        reader: &Reader,
        cwd: &str,
        session_id: &str,
    ) -> (Location, &'static str) {
        if !safe_session_id(session_id) {
            return (Location::default(), "invalid_session");
        }
        let databases = match self.databases(reader) {
            Ok(databases) => databases,
            Err(code) => return (Location::default(), code),
        };
        let mut first_failure = "";
        for database in databases {
            let (rows, _, query_code) = self.query(&database, session_id, "", 1);
            if !query_code.is_empty() {
                if first_failure.is_empty() {
                    first_failure = query_code;
                }
                continue;
            }
            if rows.is_empty() || rows[0].session_id != session_id {
                continue;
            }
            if !cwd.is_empty()
                && !rows[0].cwd.is_empty()
                && !same_opencode_directory(cwd, &rows[0].cwd)
            {
                return (Location::default(), "invalid_session");
            }
            let root = Path::new(&database)
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            return (
                Location {
                    path: database,
                    root,
                    title: rows[0].title.clone(),
                },
                "",
            );
        }
        if !first_failure.is_empty() {
            return (Location::default(), first_failure);
        }
        (Location::default(), "invalid_session")
    }

    /// `readFrom` — cursor check, query, parse, attach tool outputs.
    fn read_from(
        &self,
        database: &str,
        session_id: &str,
        before: &str,
        limit: usize,
    ) -> (Vec<Entry>, bool, bool, HermesRow, &'static str) {
        if !safe_session_id(session_id) {
            return (
                Vec::new(),
                false,
                false,
                HermesRow::default(),
                "invalid_session",
            );
        }
        if !before.is_empty() {
            match before.parse::<i64>() {
                Ok(cursor) if cursor > 0 => {}
                _ => {
                    return (
                        Vec::new(),
                        false,
                        false,
                        HermesRow::default(),
                        "invalid_cursor",
                    )
                }
            }
        }
        let (rows, has_more, query_code) = self.query(database, session_id, before, limit);
        if !query_code.is_empty() {
            return (Vec::new(), false, false, HermesRow::default(), query_code);
        }
        if rows.is_empty() || rows[0].session_id != session_id {
            return (
                Vec::new(),
                false,
                false,
                HermesRow::default(),
                "invalid_session",
            );
        }
        if !before.is_empty() && rows[0].cursor_found == 0 {
            return (
                Vec::new(),
                false,
                false,
                HermesRow::default(),
                "invalid_cursor",
            );
        }
        let (mut entries, mut corrupt) = parse_rows(&rows);
        if self.attach_tool_results(database, session_id, &mut entries) {
            corrupt = true;
        }
        let metadata = HermesRow {
            session_id: rows[0].session_id.clone(),
            cwd: rows[0].cwd.clone(),
            title: rows[0].title.clone(),
            message_id: rows[0].message_id,
            message_total: rows[0].message_total,
            cursor_found: rows[0].cursor_found,
            ..HermesRow::default()
        };
        (entries, has_more, corrupt, metadata, "")
    }

    /// `query` — the deduping `visible`/`displayed` CTE is verbatim from the
    /// oracle: `logical_id` collapses compaction copies, `generation=1` picks
    /// the active/newest row per group, and the cursor resolves a raw `id` to
    /// its group's `logical_id`.
    fn query(
        &self,
        database: &str,
        session_id: &str,
        before: &str,
        limit: usize,
    ) -> (Vec<HermesRow>, bool, &'static str) {
        let limit = if limit < 1 {
            DEFAULT_PAGE_SIZE
        } else {
            limit.min(MAX_PAGE_SIZE)
        };
        let session_hex = hex::encode(session_id.as_bytes());
        let mut cursor_cte = "cursor AS (SELECT NULL AS logical_id WHERE 0)".to_string();
        let mut cursor_filter = String::new();
        let mut cursor_found = "1".to_string();
        if !before.is_empty() {
            let cursor: i64 = match before.parse() {
                Ok(cursor) if cursor > 0 => cursor,
                _ => return (Vec::new(), false, "invalid_cursor"),
            };
            cursor_cte = format!(
                "cursor AS (SELECT logical_id FROM visible WHERE id={cursor} AND role IN ('user','assistant') LIMIT 1)"
            );
            cursor_filter = " AND EXISTS(SELECT 1 FROM cursor) AND \
                d.logical_id < (SELECT logical_id FROM cursor)"
                .to_string();
            cursor_found = "EXISTS(SELECT 1 FROM cursor)".to_string();
        }
        let query = format!(
            "WITH raw AS (\
                SELECT m.id,m.role,m.active,m.compacted,COALESCE(m.content,'') AS content,\
                COALESCE(m.tool_call_id,'') AS tool_call_id,COALESCE(m.tool_calls,'') AS tool_calls,\
                COALESCE(m.tool_name,'') AS tool_name,COALESCE(m.display_kind,'') AS display_kind,\
                COALESCE(m.timestamp,0) AS logical_timestamp \
                FROM messages AS m \
                WHERE m.session_id=CAST(X'{session_hex}' AS TEXT) AND (m.active=1 OR m.compacted=1) \
                AND COALESCE(m.display_kind,'') <> 'hidden'\
            ), visible AS (\
                SELECT raw.*,\
                MIN(id) OVER (\
                PARTITION BY role,content,tool_call_id,tool_calls,tool_name,logical_timestamp\
                ) AS logical_id,\
                ROW_NUMBER() OVER (\
                PARTITION BY role,content,tool_call_id,tool_calls,tool_name,logical_timestamp \
                ORDER BY active DESC,id DESC\
                ) AS generation \
                FROM raw\
            ), displayed AS (\
                SELECT id,logical_id,role,content,tool_call_id,tool_calls,tool_name,display_kind,logical_timestamp \
                FROM visible WHERE generation=1 AND role IN ('user','assistant')\
            ), {cursor_cte}, selected AS (\
                SELECT id,logical_id,role,content,tool_call_id,tool_calls,tool_name,display_kind,logical_timestamp \
                FROM displayed AS d WHERE 1=1{cursor_filter} \
                ORDER BY logical_id DESC LIMIT {limit_plus}\
            ) \
            SELECT s.id AS session_id,COALESCE(s.cwd,'') AS cwd,COALESCE(s.title,'') AS title,\
            COALESCE(sm.logical_id,0) AS message_id,COALESCE(sm.role,'') AS role,\
            hex(COALESCE(sm.content,'')) AS content_hex,COALESCE(sm.tool_call_id,'') AS tool_call_id,\
            COALESCE(sm.tool_calls,'') AS tool_calls,COALESCE(sm.tool_name,'') AS tool_name,\
            COALESCE(sm.display_kind,'') AS display_kind,COALESCE(sm.logical_timestamp,0) AS timestamp,\
            (SELECT COUNT(*) FROM displayed) AS message_total,{cursor_found} AS cursor_found \
            FROM sessions AS s LEFT JOIN selected AS sm ON 1=1 \
            WHERE s.id=CAST(X'{session_hex}' AS TEXT) ORDER BY sm.logical_id DESC;",
            limit_plus = limit + 1,
        );
        let output = match run_json_query(
            "sqlite3",
            database,
            &query,
            MAX_HERMES_OUTPUT,
            HERMES_QUERY_TIMEOUT,
        ) {
            Ok(output) => output,
            Err(SqliteError::OutputLimit) => return (Vec::new(), false, "output_limit"),
            Err(SqliteError::QueryFailed) => return (Vec::new(), false, "query_failed"),
        };
        let mut rows: Vec<HermesRow> = match serde_json::from_slice(&output) {
            Ok(rows) => rows,
            Err(_) => return (Vec::new(), false, "source_corrupt"),
        };
        let anchor_count = rows.iter().filter(|row| row.message_id > 0).count();
        let has_more = anchor_count > limit;
        if has_more && !rows.is_empty() {
            rows.truncate(rows.len() - 1);
        }
        rows.reverse();
        (rows, has_more, "")
    }

    /// `attachToolResults` — fetch `role='tool'` rows for pending call ids and
    /// merge their content into the matching tools; a failed second query
    /// counts as corrupt.
    fn attach_tool_results(&self, database: &str, session_id: &str, entries: &mut [Entry]) -> bool {
        let mut call_ids: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for entry in entries.iter() {
            for tool in &entry.tools {
                let id = tool_association_id(tool);
                if !id.is_empty() && seen.insert(id.clone()) {
                    call_ids.push(id);
                }
            }
        }
        if call_ids.is_empty() {
            return false;
        }
        let (rows, code) = self.query_tool_rows(database, session_id, &call_ids);
        if !code.is_empty() {
            return true;
        }
        let mut corrupt = false;
        for row in rows {
            let call_id = row.tool_call_id.trim().to_string();
            let (output_text, output_corrupt) = hermes_text_hex_value(&row.content_hex);
            if output_corrupt {
                corrupt = true;
                continue;
            }
            let (output, truncated) = clamp_text(&sanitize_text(&output_text), MAX_ENTRY_BYTES);
            if output.is_empty() {
                continue;
            }
            for entry in entries.iter_mut() {
                for tool in entry.tools.iter_mut() {
                    if tool_association_id(tool) != call_id {
                        continue;
                    }
                    if !tool.output.is_empty() && tool.output != output {
                        tool.output.push('\n');
                        tool.output.push_str(&output);
                        let (clipped, was_truncated) = clamp_text(&tool.output, MAX_ENTRY_BYTES);
                        tool.output = clipped;
                        tool.truncated = was_truncated;
                    } else if tool.output.is_empty() {
                        tool.output = output.clone();
                        tool.truncated = truncated;
                    }
                }
            }
        }
        corrupt
    }

    /// `queryToolRows`.
    fn query_tool_rows(
        &self,
        database: &str,
        session_id: &str,
        call_ids: &[String],
    ) -> (Vec<HermesToolRow>, &'static str) {
        let session_hex = hex::encode(session_id.as_bytes());
        let encoded_ids: Vec<String> = call_ids
            .iter()
            .map(|id| format!("CAST(X'{}' AS TEXT)", hex::encode(id.as_bytes())))
            .collect();
        let query = format!(
            "SELECT COALESCE(m.tool_call_id,'') AS tool_call_id,COALESCE(m.tool_name,'') AS tool_name,\
            hex(COALESCE(m.content,'')) AS content_hex FROM messages AS m \
            WHERE m.session_id=CAST(X'{session_hex}' AS TEXT) AND m.role='tool' \
            AND (m.active=1 OR m.compacted=1) AND COALESCE(m.display_kind,'') <> 'hidden' \
            AND m.tool_call_id IN ({}) ORDER BY m.id;",
            encoded_ids.join(",")
        );
        let output = match run_json_query(
            "sqlite3",
            database,
            &query,
            MAX_HERMES_OUTPUT,
            HERMES_QUERY_TIMEOUT,
        ) {
            Ok(output) => output,
            Err(SqliteError::OutputLimit) => return (Vec::new(), "output_limit"),
            Err(SqliteError::QueryFailed) => return (Vec::new(), "query_failed"),
        };
        match serde_json::from_slice(&output) {
            Ok(rows) => (rows, ""),
            Err(_) => (Vec::new(), "source_corrupt"),
        }
    }
}

/// `parseHermesRows` — visible rows only; empty text with no tools is dropped;
/// entry id is the decimal `logical_id`.
fn parse_rows(rows: &[HermesRow]) -> (Vec<Entry>, bool) {
    let mut entries = Vec::with_capacity(rows.len());
    let mut corrupt = false;
    for row in rows {
        if row.message_id <= 0 || (row.role != "user" && row.role != "assistant") {
            continue;
        }
        let (tools, tools_corrupt) = parse_tool_calls(&row.tool_calls);
        let (text_value, text_corrupt) = hermes_text_hex_value(&row.content_hex);
        corrupt = corrupt || tools_corrupt || text_corrupt;
        let text = sanitize_text(&text_value);
        if text.is_empty() && tools.is_empty() {
            continue;
        }
        let (text, truncated) = clamp_text(&text, MAX_ENTRY_BYTES);
        entries.push(Entry {
            id: row.message_id.to_string(),
            timestamp: format_unix_seconds_float(row.timestamp),
            role: row.role.clone(),
            text,
            tools,
            truncated,
        });
    }
    (entries, corrupt)
}

/// `parseHermesToolCalls` — `tool_calls` may be an array, an object with a
/// `tool_calls` array, or a single call object; anything else is corrupt.
fn parse_tool_calls(raw: &str) -> (Vec<ToolActivity>, bool) {
    let raw = raw.trim();
    if raw.is_empty() || raw == "null" {
        return (Vec::new(), false);
    }
    let decoded: Value = match serde_json::from_str(raw) {
        Ok(decoded) => decoded,
        Err(_) => return (Vec::new(), true),
    };
    let calls: Vec<Value> = match &decoded {
        Value::Array(calls) => calls.clone(),
        Value::Object(map) => match map.get("tool_calls").and_then(Value::as_array) {
            Some(nested) => nested.clone(),
            None => vec![decoded.clone()],
        },
        _ => return (Vec::new(), true),
    };
    let mut activities = Vec::with_capacity(calls.len());
    let mut corrupt = false;
    for raw_call in calls {
        let Some(call) = raw_call.as_object() else {
            corrupt = true;
            continue;
        };
        let function = call.get("function").and_then(Value::as_object);
        let mut id = first_string(call, &["id", "call_id", "tool_call_id"]);
        let mut name = first_string(call, &["name", "tool_name"]);
        let mut input = first_value(call, &["arguments", "input"]).cloned();
        if let Some(function) = function {
            if id.is_empty() {
                id = first_string(function, &["id", "call_id", "tool_call_id"]);
            }
            if name.is_empty() {
                name = first_string(function, &["name", "tool_name"]);
            }
            if input.is_none() {
                input = first_value(function, &["arguments", "input"]).cloned();
            }
        }
        activities.push(new_tool_activity(&id, &name, input.as_ref()));
    }
    (activities, corrupt)
}

/// `hermesText` — a `\x00json:` prefix wraps a JSON value; anything else is
/// literal text.
fn hermes_text(raw: &str) -> String {
    if let Some(rest) = raw.strip_prefix("\x00json:") {
        if let Ok(decoded) = serde_json::from_str::<Value>(rest) {
            return super::util::text_value(&decoded);
        }
    }
    super::util::text_value(&Value::String(raw.to_string()))
}

/// `hermesTextHexValue` — hex-decode then `hermesText`; a bad hex payload is
/// corrupt.
fn hermes_text_hex_value(encoded: &str) -> (String, bool) {
    if encoded.is_empty() {
        return (String::new(), false);
    }
    let Ok(decoded) = hex::decode(encoded) else {
        return (String::new(), true);
    };
    (hermes_text(&String::from_utf8_lossy(&decoded)), false)
}

impl Reader {
    /// `r.hermes.locate` through the location cache (`agentKey = "hermes"`).
    pub(crate) fn hermes_locate(&self, cwd: &str, session_id: &str) -> Location {
        self.hermes.locate(self, cwd, session_id).0
    }

    /// `readHermesFor` — validates the ids, locates through the cached tuple
    /// path, then queries the located database.
    pub(crate) fn hermes_read_for(
        &self,
        agent: &str,
        cwd: &str,
        session_id: &str,
        before: &str,
        limit: usize,
    ) -> Result<Page, Error> {
        let session_id = session_id.trim();
        if !safe_session_id(session_id) {
            return Ok(Page::unavailable(
                "invalid_session",
                "This agent has not reported a conversation session yet.",
            ));
        }
        if !before.is_empty() {
            match before.parse::<i64>() {
                Ok(cursor) if cursor > 0 => {}
                _ => {
                    return Ok(Page::unavailable(
                        "invalid_cursor",
                        "This conversation page cursor is invalid.",
                    ))
                }
            }
        }
        let limit = if limit < 1 {
            DEFAULT_PAGE_SIZE
        } else {
            limit.min(MAX_PAGE_SIZE)
        };
        let location = self.locate(agent, cwd, session_id);
        if location.path.is_empty() {
            if self.hermes.databases(self).is_err() {
                return Ok(Page::unavailable(
                    "source_unavailable",
                    "Hermes conversation history is unavailable.",
                ));
            }
            return Ok(Page::unavailable(
                "invalid_session",
                "No conversation log is available for this session.",
            ));
        }
        let (mut entries, has_more, corrupt, metadata, code) =
            self.hermes
                .read_from(&location.path, session_id, before, limit);
        if !code.is_empty() {
            return Ok(Page::unavailable(
                code,
                "Hermes conversation history is unavailable.",
            ));
        }
        if !cwd.is_empty()
            && !metadata.cwd.is_empty()
            && !same_opencode_directory(cwd, &metadata.cwd)
        {
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
            cursor_before: metadata.message_id.to_string(),
            source_path: location.path.clone(),
            ..Page::default()
        })
    }
}
