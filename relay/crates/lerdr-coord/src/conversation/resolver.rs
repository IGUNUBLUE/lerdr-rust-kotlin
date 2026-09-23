//! `Resolver` — the agent-session title resolver, a port of the oracle's
//! `internal/session/resolver.go`. The topology commit feeds every agent
//! row through [`Resolver::session_name_with_project`]
//! (`resolveAgentSessionName`, `internal/app/server.go`), which shares the
//! [`Reader`]'s directory-aware transcript location and then applies the
//! provider-specific title grammar:
//!
//! - OMP: first `type:"title"` header wins; else the latest
//!   `title_change`; else the last `session.title`
//!   (`extractOMPSessionTitle`).
//! - Pi: the latest `session_info.name` (`extractPiSessionTitle`).
//! - Hermes: the `Location.title` the SQLite reader already populated
//!   (`isHermesSessionAgent` arm).
//! - Claude/Qoder: the last non-empty field per recognized record type,
//!   precedence `custom-title` > `ai-title` > `summary` (`extractTitle`).
//! - Codex: `session_index.jsonl` beside the sessions root, first row
//!   whose `id` matches (`codexIndexThreadName`).
//! - Anything else: `""`.
//!
//! Like the oracle, the resolver caches titles for 60 s keyed on the
//! normalized `(agent, cwd, foreground_cwd, session_id)` tuple and
//! validates the cached entry against the freshly resolved location, so a
//! moved transcript re-resolves inside the TTL. Divergence: the oracle's
//! cache is unbounded; this port caps it at [`MAX_TITLE_CACHE_ENTRIES`]
//! with the same sweep-then-clear eviction as the location cache, matching
//! the relay's bounded-state rule.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::reader::normalize_project_context;
use super::reader::{is_hermes_agent, Reader};
use super::types::{Location, ProjectContext};

const TITLE_CACHE_TTL: Duration = Duration::from_secs(60);
const MAX_TITLE_CACHE_ENTRIES: usize = 2048;
/// `scanner.Buffer(256KB, 1MB)` — the OMP/Pi/Claude scanners refuse a
/// single record over 1 MiB (`bufio.ErrTooLong` ends the scan).
const MAX_RECORD_BYTES: usize = 1024 * 1024;
/// `codexIndexThreadName` uses the default `bufio.Scanner`, whose token
/// cap is `bufio.MaxScanTokenSize` (64 KiB).
const MAX_INDEX_RECORD_BYTES: usize = 64 * 1024;

#[derive(Hash, PartialEq, Eq)]
struct TitleKey {
    agent: String,
    cwd: String,
    foreground_cwd: String,
    session_id: String,
}

struct TitleEntry {
    name: String,
    location: Location,
    expires: Instant,
}

/// `session.Resolver` — `Send + Sync`, the title cache behind a mutex;
/// resolution is synchronous file I/O identical to the oracle's.
pub struct Resolver {
    reader: Arc<Reader>,
    titles: Mutex<HashMap<TitleKey, TitleEntry>>,
}

// `Topology` derives `Debug`; the reader's env closure can't.
impl std::fmt::Debug for Resolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Resolver").finish_non_exhaustive()
    }
}

impl Resolver {
    /// `NewResolver` — own reader (no shared location cache). The relay
    /// always shares the projector's reader; tests construct standalone
    /// resolvers through this.
    #[cfg(test)]
    pub fn new(home: PathBuf) -> Self {
        Self::with_reader(Arc::new(Reader::new(home)))
    }

    /// `NewResolverWithReader` — share transcript-location decisions with
    /// conversation-history consumers.
    pub fn with_reader(reader: Arc<Reader>) -> Self {
        Resolver {
            reader,
            titles: Mutex::new(HashMap::new()),
        }
    }

    /// `SessionNameWithProject` — normalize the project context, trim the
    /// session id, locate the transcript through the shared reader, then
    /// apply the provider title grammar. `""` means "no title".
    pub fn session_name_with_project(
        &self,
        agent: &str,
        project: &ProjectContext,
        session_id: &str,
    ) -> String {
        let project = normalize_project_context(agent, project.clone());
        let session_id = session_id.trim();
        if session_id.is_empty() {
            return String::new();
        }
        let agent_lower = agent.trim().to_lowercase();
        let key = TitleKey {
            agent: agent_lower.clone(),
            cwd: project.cwd.clone(),
            foreground_cwd: project.foreground_cwd.clone(),
            session_id: session_id.to_string(),
        };
        let location = self.reader.locate_with_project(agent, &project, session_id);
        if location.path.is_empty() {
            return String::new();
        }
        let now = Instant::now();
        {
            let titles = self.titles.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(entry) = titles.get(&key) {
                if entry.location == location && now < entry.expires {
                    return entry.name.clone();
                }
            }
        }
        let name = session_title(&agent_lower, &location, session_id);
        let mut titles = self.titles.lock().unwrap_or_else(|p| p.into_inner());
        if titles.len() >= MAX_TITLE_CACHE_ENTRIES {
            titles.retain(|_, entry| now < entry.expires);
            if titles.len() >= MAX_TITLE_CACHE_ENTRIES {
                titles.clear();
            }
        }
        titles.insert(
            key,
            TitleEntry {
                name: name.clone(),
                location,
                expires: now + TITLE_CACHE_TTL,
            },
        );
        name
    }
}

/// The `switch` in `SessionNameWithProject` — the oracle matches OMP/Pi on
/// the literal lower-cased name (no separator stripping) and Hermes on the
/// separator-stripped name; `qoder`/`claude`/`codex` are substring tests.
fn session_title(agent_lower: &str, location: &Location, session_id: &str) -> String {
    if is_omp_session_agent(agent_lower) {
        extract_omp_title(Path::new(&location.path))
    } else if is_pi_session_agent(agent_lower) {
        extract_pi_title(Path::new(&location.path))
    } else if is_hermes_agent(agent_lower) {
        location.title.clone()
    } else if agent_lower.contains("qoder") || agent_lower.contains("claude") {
        extract_title(Path::new(&location.path))
    } else if agent_lower.contains("codex") {
        let index = Path::new(&location.root)
            .parent()
            .map(|dir| dir.join("session_index.jsonl"))
            .unwrap_or_else(|| PathBuf::from("session_index.jsonl"));
        codex_index_thread_name(&index, session_id)
    } else {
        String::new()
    }
}

/// `isOMPSessionAgent` — the caller already lower-cased and trimmed.
fn is_omp_session_agent(agent_lower: &str) -> bool {
    matches!(agent_lower, "omp" | "oh-my-pi" | "oh my pi" | "ohmypi")
}

/// `isPiSessionAgent` — the caller already lower-cased and trimmed.
fn is_pi_session_agent(agent_lower: &str) -> bool {
    matches!(agent_lower, "pi" | "pi-coding-agent")
}

/// Scan a JSONL file like `bufio.Scanner` bounded at `max_record_bytes`:
/// each line becomes a [`Value`] and unparseable lines are skipped; a
/// single over-long line aborts the scan (`ErrTooLong`).
fn scan_records(path: &Path, max_record_bytes: usize, mut each: impl FnMut(&Value)) {
    let Ok(file) = std::fs::File::open(path) else {
        return;
    };
    let mut reader = BufReader::new(file);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let Ok(n) = reader.read_until(b'\n', &mut buf) else {
            break;
        };
        if n == 0 {
            break;
        }
        // Scanner counts the token without the newline; a trailing `\r` is
        // part of the token, which `serde_json` treats as whitespace anyway.
        let token_len = if buf.last() == Some(&b'\n') { n - 1 } else { n };
        if token_len > max_record_bytes {
            break;
        }
        if let Ok(record) = serde_json::from_slice::<Value>(&buf) {
            each(&record);
        }
    }
}

fn trimmed_field(record: &Value, field: &str) -> String {
    record
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned()
}

/// `extractOMPSessionTitle` — the first `type:"title"` record wins
/// verbatim (even empty after trimming); otherwise the last
/// `title_change`; otherwise the last `session.title`.
fn extract_omp_title(path: &Path) -> String {
    let mut session_title = String::new();
    let mut header_title: Option<String> = None;
    let mut latest_title: Option<String> = None;
    scan_records(path, MAX_RECORD_BYTES, |record| {
        match record.get("type").and_then(Value::as_str) {
            Some("title") => {
                if header_title.is_none() {
                    header_title = Some(trimmed_field(record, "title"));
                }
            }
            Some("session") => session_title = trimmed_field(record, "title"),
            Some("title_change") => latest_title = Some(trimmed_field(record, "title")),
            _ => {}
        }
    });
    if let Some(title) = header_title {
        return title;
    }
    if let Some(title) = latest_title {
        return title;
    }
    session_title
}

/// `extractPiSessionTitle` — the last `session_info.name` wins.
fn extract_pi_title(path: &Path) -> String {
    let mut name = String::new();
    scan_records(path, MAX_RECORD_BYTES, |record| {
        if record.get("type").and_then(Value::as_str) == Some("session_info") {
            name = trimmed_field(record, "name");
        }
    });
    name
}

/// `codexIndexThreadName` — the first index row whose `id` matches the
/// session returns its `thread_name` verbatim (no trim).
fn codex_index_thread_name(index: &Path, session_id: &str) -> String {
    let mut found = String::new();
    let mut matched = false;
    scan_records(index, MAX_INDEX_RECORD_BYTES, |record| {
        if matched {
            return;
        }
        if record.get("id").and_then(Value::as_str) == Some(session_id) {
            matched = true;
            found = record
                .get("thread_name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
        }
    });
    found
}

/// `extractTitle` — Claude/Qoder transcripts: per recognized record type
/// the first non-empty field in `titleFields` order wins; the last record
/// of each type overwrites; final precedence is `custom-title` >
/// `ai-title` > `summary`.
fn extract_title(path: &Path) -> String {
    const TITLE_FIELDS: [&str; 7] = [
        "customTitle",
        "aiTitle",
        "title",
        "summary",
        "text",
        "name",
        "value",
    ];
    const TITLE_TYPES: [&str; 3] = ["custom-title", "ai-title", "summary"];
    let mut found: HashMap<String, String> = HashMap::new();
    scan_records(path, MAX_RECORD_BYTES, |record| {
        let Some(record_type) = record.get("type").and_then(Value::as_str) else {
            return;
        };
        if !TITLE_TYPES.contains(&record_type) {
            return;
        }
        for field in TITLE_FIELDS {
            let value = trimmed_field(record, field);
            if !value.is_empty() {
                found.insert(record_type.to_string(), value);
                break;
            }
        }
    });
    for record_type in TITLE_TYPES {
        if let Some(value) = found.get(record_type) {
            if !value.is_empty() {
                return value.clone();
            }
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as StdMap;
    use tempfile::TempDir;

    /// `exportFixtureHome` — a temp HOME plus an env lookup that only sees
    /// the variables the fixture pins (`XDG_DATA_HOME` keeps sqlite readers
    /// out of the operator's data dir). The caller keeps both TempDirs.
    fn fixture() -> (TempDir, TempDir, Arc<Reader>) {
        let home = TempDir::new().unwrap();
        let xdg = TempDir::new().unwrap();
        let mut env: StdMap<String, String> = StdMap::new();
        env.insert(
            "XDG_DATA_HOME".to_string(),
            xdg.path().to_string_lossy().into_owned(),
        );
        let reader = Arc::new(Reader::new_with_env(
            home.path().to_path_buf(),
            Box::new(move |key: &str| env.get(key).cloned()),
        ));
        (home, xdg, reader)
    }

    fn write(home: &TempDir, rel: &str, lines: &[&str]) {
        let path = home.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut content = lines.join("\n");
        if !lines.is_empty() {
            content.push('\n');
        }
        std::fs::write(&path, content).unwrap();
    }

    #[test]
    fn omp_title_precedence() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        // Header `title` beats a later `title_change` and `session.title`.
        std::fs::write(
            &path,
            concat!(
                "{\"type\":\"session\",\"title\":\"session name\"}\n",
                "{\"type\":\"title_change\",\"title\":\"renamed\"}\n",
                "{\"type\":\"title\",\"title\":\"  Header Title  \"}\n",
            ),
        )
        .unwrap();
        assert_eq!(extract_omp_title(&path), "Header Title");

        // Without a header, the last `title_change` wins.
        std::fs::write(
            &path,
            concat!(
                "{\"type\":\"session\",\"title\":\"session name\"}\n",
                "{\"type\":\"title_change\",\"title\":\"first\"}\n",
                "{\"type\":\"title_change\",\"title\":\"latest\"}\n",
            ),
        )
        .unwrap();
        assert_eq!(extract_omp_title(&path), "latest");

        // Otherwise the last `session.title`.
        std::fs::write(
            &path,
            concat!(
                "{\"type\":\"session\",\"title\":\"old\"}\n",
                "{\"type\":\"message\",\"title\":\"ignored\"}\n",
                "{\"type\":\"session\",\"title\":\"session name\"}\n",
                "not json\n",
            ),
        )
        .unwrap();
        assert_eq!(extract_omp_title(&path), "session name");
    }

    #[test]
    fn pi_title_latest_session_info() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"type\":\"session_info\",\"name\":\"  First  \"}\n",
                "{\"type\":\"message\"}\n",
                "{\"type\":\"session_info\",\"name\":\"Renamed\"}\n",
            ),
        )
        .unwrap();
        assert_eq!(extract_pi_title(&path), "Renamed");
    }

    #[test]
    fn generic_title_type_and_field_precedence() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        // `custom-title` beats `ai-title` and `summary`; the first
        // non-empty field in titleFields order wins per record.
        std::fs::write(
            &path,
            concat!(
                "{\"type\":\"summary\",\"summary\":\"summarized\"}\n",
                "{\"type\":\"ai-title\",\"title\":\"ai name\"}\n",
                "{\"type\":\"custom-title\",\"customTitle\":\"custom name\",\"summary\":\"wrong\"}\n",
                "{\"type\":\"user\",\"customTitle\":\"ignored type\"}\n",
            ),
        )
        .unwrap();
        assert_eq!(extract_title(&path), "custom name");

        // Without custom-title, ai-title wins; last record of the type wins.
        std::fs::write(
            &path,
            concat!(
                "{\"type\":\"summary\",\"summary\":\"summarized\"}\n",
                "{\"type\":\"ai-title\",\"title\":\"old ai\"}\n",
                "{\"type\":\"ai-title\",\"title\":\"new ai\"}\n",
            ),
        )
        .unwrap();
        assert_eq!(extract_title(&path), "new ai");

        // Fields are tried in titleFields order: `customTitle` before `title`.
        std::fs::write(
            &path,
            "{\"type\":\"summary\",\"title\":\"t\",\"customTitle\":\"  ct  \"}\n",
        )
        .unwrap();
        assert_eq!(extract_title(&path), "ct");
    }

    #[test]
    fn codex_index_thread_name_match() {
        let dir = TempDir::new().unwrap();
        let index = dir.path().join("session_index.jsonl");
        std::fs::write(
            &index,
            concat!(
                "{\"id\":\"other\",\"thread_name\":\"Other\"}\n",
                "{\"id\":\"abc-123\",\"thread_name\":\"  Raw Name  \"}\n",
            ),
        )
        .unwrap();
        // `thread_name` is returned verbatim (no trim).
        assert_eq!(codex_index_thread_name(&index, "abc-123"), "  Raw Name  ");
        assert_eq!(codex_index_thread_name(&index, "missing"), "");
        assert_eq!(
            codex_index_thread_name(&dir.path().join("absent.jsonl"), "x"),
            ""
        );
    }

    fn claude_fixture(home: &TempDir, cwd: &str, session_id: &str, lines: &[&str]) {
        // `claudeProjectName` maps every non-alphanumeric to `-`.
        let dir_name: String = cwd
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        write(
            home,
            &format!(".claude/projects/{dir_name}/{session_id}.jsonl"),
            lines,
        );
    }

    #[test]
    fn session_name_with_project_end_to_end() {
        let (home, _xdg, reader) = fixture();
        let resolver = Resolver::with_reader(reader);
        claude_fixture(
            &home,
            "/work/repo",
            "sess-1",
            &[
                "{\"type\":\"summary\",\"summary\":\"First\"}",
                "{\"type\":\"ai-title\",\"title\":\"AI name\"}",
            ],
        );
        let project = ProjectContext {
            cwd: "/work/repo".to_string(),
            foreground_cwd: String::new(),
        };
        assert_eq!(
            resolver.session_name_with_project("claude", &project, "sess-1"),
            "AI name"
        );
        // Whitespace session ids resolve to nothing; the trim is part of
        // the lookup tuple so " sess-1 " finds the same transcript.
        assert_eq!(
            resolver.session_name_with_project("claude", &project, " sess-1 "),
            "AI name"
        );
        assert_eq!(
            resolver.session_name_with_project("claude", &project, "   "),
            ""
        );
        // Unsupported agents resolve the tuple but have no title grammar.
        assert_eq!(
            resolver.session_name_with_project("unknown-agent", &project, "sess-1"),
            ""
        );
        // A missing transcript is an empty title, not an error.
        assert_eq!(
            resolver.session_name_with_project("claude", &project, "missing"),
            ""
        );
    }

    #[test]
    fn session_name_foreground_cwd_claude_only() {
        let (home, _xdg, reader) = fixture();
        let resolver = Resolver::with_reader(reader);
        // The transcript lives under the foreground project dir.
        claude_fixture(
            &home,
            "/other/worktree",
            "sess-fg",
            &["{\"type\":\"summary\",\"summary\":\"FG Title\"}"],
        );
        // Claude accepts an absolute, non-redundant foreground hint.
        let project = ProjectContext {
            cwd: "/work/repo".to_string(),
            foreground_cwd: "/other/worktree".to_string(),
        };
        assert_eq!(
            resolver.session_name_with_project("claude", &project, "sess-fg"),
            "FG Title"
        );
        // A relative foreground hint is dropped by normalization.
        let relative = ProjectContext {
            cwd: "/work/repo".to_string(),
            foreground_cwd: "rel/dir".to_string(),
        };
        assert_eq!(
            resolver.session_name_with_project("claude", &relative, "sess-fg"),
            ""
        );
        // Qoder shares the transcript grammar but not the hint: the same
        // file under a qoder layout is unreachable via foreground.
        let qoder = ProjectContext {
            cwd: "/work/repo".to_string(),
            foreground_cwd: "/other/worktree".to_string(),
        };
        assert_eq!(
            resolver.session_name_with_project("qoder", &qoder, "sess-fg"),
            ""
        );
    }

    #[test]
    fn hermes_title_comes_from_location() {
        let location = Location {
            path: "/db/state.db".to_string(),
            root: "/db".to_string(),
            title: "DB Title".to_string(),
        };
        assert_eq!(session_title("hermes", &location, "s"), "DB Title");
        assert_eq!(session_title("hermes-agent", &location, "s"), "DB Title");
    }

    #[test]
    fn dispatch_matches_oracle_agent_sets() {
        let location = Location {
            path: String::new(),
            root: String::new(),
            title: String::new(),
        };
        // Literal-name sets: "oh_my_pi" does not normalize into the OMP arm.
        assert!(!is_omp_session_agent("oh_my_pi"));
        assert!(is_omp_session_agent("oh-my-pi"));
        // Substring arms.
        let dir = TempDir::new().unwrap();
        let transcript = dir.path().join("t.jsonl");
        std::fs::write(&transcript, "{\"type\":\"summary\",\"summary\":\"S\"}\n").unwrap();
        let located = Location {
            path: transcript.to_string_lossy().into_owned(),
            ..location.clone()
        };
        assert_eq!(session_title("my-claude-wrapper", &located, "s"), "S");
        assert_eq!(session_title("my-qoder", &located, "s"), "S");
        // A bare substring hit like "codex" with a real file location but no
        // index yields "".
        assert_eq!(session_title("codex", &located, "s"), "");
    }
}
