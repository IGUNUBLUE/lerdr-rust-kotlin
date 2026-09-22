//! Golden fixture conformance for `fixtures/conversation/conversation.page.*.json`.
//!
//! Mirrors the oracle exporter `internal/conversation/page_export_test.go`:
//! each vector's `files` (and sqlite `statements`) materialize under a fresh
//! temporary HOME, each `steps[]` entry runs through [`Reader`], and the
//! resulting [`Page`] is compared field-for-field against the declared
//! expectations. Claude continuation entry ids strip their inode-derived
//! namespace prefix exactly like the exporter's `exportCursor`.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use lerdr_fixture::{vector_name, Suite};
use serde_json::{Map, Value};
use tempfile::TempDir;

use super::reader::Reader;
use super::types::{Entry, Page, ToolActivity};

// Environment the locator consults is cleared by construction: the injected
// env map starts empty (matching the exporter's `clearConversationEnv`,
// which blanks every `*_CONFIG_DIR`, `CODEX_HOME`, `PI_CODING_AGENT_DIR`,
// `SENPI_CODING_AGENT_DIR`, `OMO_CODING_AGENT_DIR`, `HERMES_HOME`, every
// `HERDR_*_DIRS`/`LERDR_*_DIRS` list) and only `PATH`, `XDG_DATA_HOME`, and
// the sqlite data-dir variables are added back.

/// `namespacedEntryID` — `^[0-9a-f]{12}-[0-9a-f]{24}(-[0-9]+)?$`.
fn is_namespaced_id(id: &str) -> bool {
    let Some((prefix, rest)) = id.split_once('-') else {
        return false;
    };
    if prefix.len() != 12 || !prefix.bytes().all(|b| b.is_ascii_hexdigit()) {
        return false;
    }
    let suffix = match rest.rsplit_once('-') {
        Some((suffix, n)) if !suffix.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => suffix,
        _ => rest,
    };
    suffix.len() == 24 && suffix.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `exportCursor` — strip the inode-derived 12-hex prefix off namespaced ids.
fn export_cursor(id: &str) -> (String, bool) {
    if is_namespaced_id(id) {
        (id[13..].to_string(), true)
    } else {
        (id.to_string(), false)
    }
}

/// `exportEntries` — `convEntry` with the same `omitempty` behaviour.
fn export_entry(entry: &Entry) -> Value {
    let mut map = Map::new();
    let (id, namespaced) = export_cursor(&entry.id);
    map.insert("id".to_string(), Value::String(id));
    if namespaced {
        map.insert("namespaced".to_string(), Value::Bool(true));
    }
    if !entry.timestamp.is_empty() {
        map.insert(
            "timestamp".to_string(),
            Value::String(entry.timestamp.clone()),
        );
    }
    map.insert("role".to_string(), Value::String(entry.role.clone()));
    if !entry.text.is_empty() {
        map.insert("text".to_string(), Value::String(entry.text.clone()));
    }
    if !entry.tools.is_empty() {
        map.insert(
            "tools".to_string(),
            Value::Array(entry.tools.iter().map(export_tool).collect()),
        );
    }
    if entry.truncated {
        map.insert("truncated".to_string(), Value::Bool(true));
    }
    Value::Object(map)
}

fn export_tool(tool: &ToolActivity) -> Value {
    let mut map = Map::new();
    if !tool.id.is_empty() {
        map.insert("id".to_string(), Value::String(tool.id.clone()));
    }
    map.insert("name".to_string(), Value::String(tool.name.clone()));
    if !tool.input.is_empty() {
        map.insert("input".to_string(), Value::String(tool.input.clone()));
    }
    if !tool.output.is_empty() {
        map.insert("output".to_string(), Value::String(tool.output.clone()));
    }
    if tool.error {
        map.insert("error".to_string(), Value::Bool(true));
    }
    if tool.truncated {
        map.insert("truncated".to_string(), Value::Bool(true));
    }
    Value::Object(map)
}

/// `filepath.Join(home, logical)` — Go joins absolute operands literally
/// (`Join("/tmp/x", "/work")` → `/tmp/x/work`), unlike `Path::join`.
fn join_logical(home: &Path, logical: &str) -> PathBuf {
    home.join(logical.trim_start_matches('/'))
}

fn run_sqlite(database: &Path, statements: &[String]) -> Result<(), String> {
    let mut child = Command::new("sqlite3")
        .arg(database)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("spawn sqlite3: {err}"))?;
    child
        .stdin
        .as_mut()
        .expect("stdin piped")
        .write_all(statements.join("\n").as_bytes())
        .map_err(|err| format!("sqlite3 stdin: {err}"))?;
    let output = child
        .wait_with_output()
        .map_err(|err| format!("sqlite3 wait: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "sqlite3 {}: {}",
            database.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

/// Live resources for one materialized vector.
struct Materialized {
    reader: Reader,
    cwd: String,
    primary: Option<PathBuf>,
    database: Option<PathBuf>,
    _home: TempDir,
    _data_dir: Option<TempDir>,
    _xdg: TempDir,
}

/// `exportFixtureHome` — files under a temp HOME, sqlite database under a
/// temp data dir wired through `HERDR_*_DATA_DIRS`, env injection via
/// [`Reader::new_with_env`].
fn materialize(vector: &Value) -> Result<Materialized, String> {
    let home = TempDir::new().map_err(|err| format!("home tempdir: {err}"))?;
    let xdg = TempDir::new().map_err(|err| format!("xdg tempdir: {err}"))?;
    let home_path = home.path().to_path_buf();

    let mut primary = None;
    if let Some(files) = vector.get("files").and_then(Value::as_array) {
        for (index, file) in files.iter().enumerate() {
            let rel = file
                .get("path")
                .and_then(Value::as_str)
                .ok_or("file.path missing")?;
            let path = home_path.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|err| format!("mkdir {}: {err}", parent.display()))?;
            }
            let lines: Vec<&str> = file
                .get("jsonl_lines")
                .and_then(Value::as_array)
                .map(|lines| lines.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let mut content = lines.join("\n");
            if !lines.is_empty() {
                content.push('\n');
            }
            std::fs::write(&path, content)
                .map_err(|err| format!("write {}: {err}", path.display()))?;
            if index == 0 {
                primary = Some(path);
            }
        }
    }

    // `clearConversationEnv`: the injected env returns None for every name
    // the locator consults (see CLEARED_ENV) except the ones set here.
    let mut env: HashMap<String, String> = HashMap::new();
    if let Ok(path) = std::env::var("PATH") {
        env.insert("PATH".to_string(), path);
    }
    env.insert(
        "XDG_DATA_HOME".to_string(),
        xdg.path().to_string_lossy().into_owned(),
    );

    let mut data_dir = None;
    let mut database = None;
    if let Some(sqlite) = vector.get("sqlite") {
        let db_name = sqlite
            .get("database")
            .and_then(Value::as_str)
            .ok_or("sqlite.database missing")?;
        let dir = TempDir::new().map_err(|err| format!("data tempdir: {err}"))?;
        let db = dir.path().join(db_name);
        // sqlite vectors carry logical workspace paths; materialize the real
        // dir under HOME and substitute the bound path (`exportFixtureHome`).
        let logical_workspace = vector
            .get("workspace")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| vector.get("cwd").and_then(Value::as_str))
            .filter(|s| !s.is_empty());
        let statements: Vec<String> = sqlite
            .get("statements")
            .and_then(Value::as_array)
            .map(|s| {
                s.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let statements = if let Some(logical) = logical_workspace {
            let workspace = join_logical(&home_path, logical);
            std::fs::create_dir_all(&workspace)
                .map_err(|err| format!("mkdir {}: {err}", workspace.display()))?;
            let real = workspace.to_string_lossy().into_owned();
            statements
                .iter()
                .map(|statement| statement.replace(&format!("'{logical}'"), &format!("'{real}'")))
                .collect()
        } else {
            statements
        };
        run_sqlite(&db, &statements)?;
        match db_name {
            "opencode.db" => {
                env.insert(
                    "HERDR_OPENCODE_DATA_DIRS".to_string(),
                    dir.path().to_string_lossy().into_owned(),
                );
            }
            "state.db" => {
                env.insert(
                    "HERDR_HERMES_DATA_DIRS".to_string(),
                    dir.path().to_string_lossy().into_owned(),
                );
            }
            other => return Err(format!("unknown sqlite database {other:?}")),
        }
        database = Some(db);
        data_dir = Some(dir);
    }

    // The cwd is logical for JSONL readers (only used for project-dir
    // encoding); sqlite readers compare EvalSymlinks'd directories, so it is
    // materialized under HOME like the exporter does.
    let logical_cwd = vector
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let cwd = if sqlite_is_present(vector) && !logical_cwd.is_empty() {
        let real = join_logical(&home_path, &logical_cwd);
        std::fs::create_dir_all(&real).map_err(|err| format!("mkdir {}: {err}", real.display()))?;
        real.to_string_lossy().into_owned()
    } else {
        logical_cwd
    };

    let reader = Reader::new_with_env(home_path, Box::new(move |key: &str| env.get(key).cloned()));
    Ok(Materialized {
        reader,
        cwd,
        primary,
        database,
        _home: home,
        _data_dir: data_dir,
        _xdg: xdg,
    })
}

fn sqlite_is_present(vector: &Value) -> bool {
    vector.get("sqlite").is_some()
}

/// One vector — `runConvVector`: appends, cursor threading, field asserts.
fn run_vector(vector: &Value) -> Result<(), String> {
    let name = vector_name(vector).to_string();
    let agent = vector
        .get("agent")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let session_id = vector
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let fixture = materialize(vector).map_err(|err| format!("{name}: {err}"))?;
    let steps = vector
        .get("steps")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{name}: steps missing"))?;
    let mut wire_before: Vec<String> = Vec::new();
    for step in steps.iter() {
        let label = step
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let fail = |err: String| format!("{name} step {label}: {err}");

        if let Some(lines) = step.get("append_jsonl_lines").and_then(Value::as_array) {
            let primary = fixture
                .primary
                .as_ref()
                .ok_or_else(|| fail("append_jsonl_lines without files".to_string()))?;
            let mut handle = std::fs::OpenOptions::new()
                .append(true)
                .open(primary)
                .map_err(|err| fail(format!("append open: {err}")))?;
            for line in lines.iter().filter_map(Value::as_str) {
                writeln!(handle, "{line}").map_err(|err| fail(format!("append: {err}")))?;
            }
        }
        if let Some(statements) = step.get("append_sql_statements").and_then(Value::as_array) {
            let database = fixture
                .database
                .as_ref()
                .ok_or_else(|| fail("append_sql_statements without sqlite".to_string()))?;
            let statements: Vec<String> = statements
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect();
            run_sqlite(database, &statements).map_err(&fail)?;
        }

        let before = match step.get("before_step").and_then(Value::as_u64) {
            Some(back) => wire_before
                .get(back as usize)
                .cloned()
                .filter(|b| !b.is_empty())
                .ok_or_else(|| fail(format!("before_step {back} produced no cursor")))?,
            None => step
                .get("before")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        };
        let limit = step.get("limit").and_then(Value::as_u64).unwrap_or(0) as usize;
        let page: Page = if fixture.cwd.is_empty() {
            fixture
                .reader
                .read(&agent, &session_id, Some(&before), limit)
        } else {
            fixture
                .reader
                .read_for(&agent, &fixture.cwd, &session_id, Some(&before), limit)
        }
        .map_err(|err| fail(format!("read: {err}")))?;
        wire_before.push(
            page.entries
                .first()
                .map(|entry| entry.id.clone())
                .unwrap_or_default(),
        );

        assert_step(&page, step).map_err(fail)?;
    }
    Ok(())
}

/// Compare an observed [`Page`] against the step's declared fields.
fn assert_step(page: &Page, step: &Value) -> Result<(), String> {
    let expect_bool = |key: &str| step.get(key).and_then(Value::as_bool).unwrap_or_default();
    let expect_str = |key: &str| step.get(key).and_then(Value::as_str).unwrap_or_default();

    if page.available != expect_bool("available") {
        return Err(format!(
            "available: expected {}, got {} (reason_code {:?})",
            expect_bool("available"),
            page.available,
            page.reason_code
        ));
    }
    if page.reason_code != expect_str("reason_code") {
        return Err(format!(
            "reason_code: expected {:?}, got {:?}",
            expect_str("reason_code"),
            page.reason_code
        ));
    }
    let actual_entries: Vec<Value> = page.entries.iter().map(export_entry).collect();
    let expected_entries = step
        .get("expected_entries")
        .cloned()
        .unwrap_or(Value::Array(Vec::new()));
    if Value::Array(actual_entries.clone()) != expected_entries {
        return Err(format!(
            "entries mismatch:\n  expected {}\n  actual   {}",
            serde_json::to_string_pretty(&expected_entries).unwrap_or_default(),
            serde_json::to_string_pretty(&actual_entries).unwrap_or_default()
        ));
    }
    if page.has_more != expect_bool("has_more") {
        return Err(format!(
            "has_more: expected {}, got {}",
            expect_bool("has_more"),
            page.has_more
        ));
    }
    if page.total != step.get("total").and_then(Value::as_i64).unwrap_or(0) {
        return Err(format!(
            "total: expected {}, got {}",
            step.get("total").and_then(Value::as_i64).unwrap_or(0),
            page.total
        ));
    }
    for (key, actual) in [
        ("file_truncated", page.file_truncated),
        ("source_corrupt", page.source_corrupt),
        ("continuation_incomplete", page.continuation_incomplete),
    ] {
        if actual != expect_bool(key) {
            return Err(format!(
                "{key}: expected {}, got {actual}",
                expect_bool(key)
            ));
        }
    }
    if page.continuation_reason != expect_str("continuation_reason") {
        return Err(format!(
            "continuation_reason: expected {:?}, got {:?}",
            expect_str("continuation_reason"),
            page.continuation_reason
        ));
    }
    match (step.get("omo_plan"), &page.omo_plan) {
        (None, None) => {}
        (Some(expected), Some(actual)) => {
            let actual = serde_json::to_value(actual).unwrap_or_default();
            if &actual != expected {
                return Err(format!(
                    "omo_plan mismatch:\n  expected {}\n  actual   {}",
                    serde_json::to_string_pretty(expected).unwrap_or_default(),
                    serde_json::to_string_pretty(&actual).unwrap_or_default()
                ));
            }
        }
        (expected, actual) => {
            return Err(format!(
                "omo_plan presence: expected {}, got {}",
                expected.is_some(),
                actual.is_some()
            ))
        }
    }
    // `next_cursor` — the first entry id (namespaced ids compare after the
    // inode prefix is stripped, per the suite notes).
    if let Some(expected) = step.get("next_cursor").and_then(Value::as_str) {
        let actual = page
            .entries
            .first()
            .map(|entry| entry.id.clone())
            .unwrap_or_default();
        if step
            .get("next_cursor_namespaced")
            .and_then(Value::as_bool)
            .unwrap_or_default()
        {
            let (stripped, namespaced) = export_cursor(&actual);
            if !namespaced || stripped != expected {
                return Err(format!(
                    "next_cursor: expected namespaced *-{expected}, got {actual:?}"
                ));
            }
        } else if actual != expected {
            return Err(format!(
                "next_cursor: expected {expected:?}, got {actual:?}"
            ));
        }
    }
    Ok(())
}

/// Run every vector in one suite, reporting the first failure.
fn run_suite(suite: &str) {
    let loaded =
        Suite::load("conversation", suite).unwrap_or_else(|err| panic!("load {suite}: {err}"));
    for vector in &loaded.vectors {
        if let Err(err) = run_vector(vector) {
            panic!("{suite}: {err}");
        }
    }
}

#[test]
fn conversation_page_claude() {
    run_suite("conversation.page.claude");
}

#[test]
fn conversation_page_codex() {
    run_suite("conversation.page.codex");
}

#[test]
fn conversation_page_qoder() {
    run_suite("conversation.page.qoder");
}

#[test]
fn conversation_page_pi() {
    run_suite("conversation.page.pi");
}

#[test]
fn conversation_page_omp() {
    run_suite("conversation.page.omp");
}

#[test]
fn conversation_page_omo() {
    run_suite("conversation.page.omo");
}

#[test]
fn conversation_page_opencode() {
    run_suite("conversation.page.opencode");
}

#[test]
fn conversation_page_hermes() {
    run_suite("conversation.page.hermes");
}

#[test]
fn conversation_page_errors() {
    run_suite("conversation.page.errors");
}
