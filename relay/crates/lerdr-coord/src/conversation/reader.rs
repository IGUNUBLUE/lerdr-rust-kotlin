//! `Reader` — location resolution, bounded caching, and the flat-transcript
//! `ReadWithProject` path (ports of `reader.go`'s locator helpers plus
//! `loadTail`). Provider-specific readers live in `claude.rs`, `omo.rs`,
//! `opencode.rs`, and `hermes.rs`.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::omo::OmoCacheEntry;
use super::roots::{self, EnvLookup};
use super::types::{Location, Page, ProjectContext};
use super::util::normalized_agent;

pub(crate) const MAX_CONVERSATION_BYTES: i64 = 16 * 1024 * 1024;
pub(crate) const DEFAULT_PAGE_SIZE: usize = 80;
pub(crate) const MAX_PAGE_SIZE: usize = 200;

const LOCATION_CACHE_TTL: Duration = Duration::from_secs(60);
const LOCATION_MISS_TTL: Duration = Duration::from_secs(5);
const MAX_LOCATION_CACHE_ENTRIES: usize = 2048;
pub(crate) const MAX_OMO_CACHE_ENTRIES: usize = 8;

/// `canonicalSessionID` — 8-4-4-4-12 hex UUID.
fn canonical_session_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (index, &b) in bytes.iter().enumerate() {
        match index {
            8 | 13 | 18 | 23 => {
                if b != b'-' {
                    return false;
                }
            }
            _ => {
                if !b.is_ascii_hexdigit() {
                    return false;
                }
            }
        }
    }
    true
}

/// `safeSessionID` — `[A-Za-z0-9._-]`, ≤128 bytes, never `.`/`..`.
pub(crate) fn safe_session_id(value: &str) -> bool {
    if value.is_empty() || value.len() > 128 || value == "." || value == ".." {
        return false;
    }
    value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

/// `isClaudeProvider` — claude/claudecode only; qoder shares the transcript
/// grammar but not the foreground hint or the continuation chain.
pub(crate) fn is_claude_provider(agent: &str) -> bool {
    matches!(normalized_agent(agent).as_str(), "claude" | "claudecode")
}

/// `isHermesAgent`.
pub(crate) fn is_hermes_agent(agent: &str) -> bool {
    matches!(normalized_agent(agent).as_str(), "hermes" | "hermesagent")
}

/// `Supported` — public provider check.
pub fn supported(agent: &str) -> bool {
    matches!(
        normalized_agent(agent).as_str(),
        "claude"
            | "claudecode"
            | "qoder"
            | "qodercli"
            | "codex"
            | "openaicodex"
            | "pi"
            | "picodingagent"
            | "omp"
            | "ohmypi"
            | "opencode"
            | "omo"
            | "ohmyopencode"
            | "hermes"
            | "hermesagent"
    )
}

/// `NormalizeProjectContext` — the pane cwd is kept byte-for-byte; the
/// foreground hint is trimmed and kept only for Claude when absolute and
/// non-redundant.
pub(crate) fn normalize_project_context(
    agent: &str,
    mut project: ProjectContext,
) -> ProjectContext {
    project.foreground_cwd = project.foreground_cwd.trim().to_string();
    if !is_claude_provider(agent)
        || project.foreground_cwd.is_empty()
        || !Path::new(&project.foreground_cwd).is_absolute()
        || project.foreground_cwd == project.cwd
    {
        project.foreground_cwd = String::new();
    }
    project
}

#[derive(Hash, PartialEq, Eq)]
pub(crate) struct LocationKey {
    pub agent: String,
    pub cwd: String,
    pub foreground_cwd: String,
    pub session_id: String,
}

pub(crate) struct LocationCacheEntry {
    location: Location,
    expires: Instant,
}

pub(crate) struct ReaderInner {
    pub locations: HashMap<LocationKey, LocationCacheEntry>,
    pub omo_cache: HashMap<String, OmoCacheEntry>,
}

/// `conversation.Reader` — tuple→location cache plus the provider readers.
/// `Send + Sync`: the cache lives behind a mutex; reads are synchronous file
/// and subprocess I/O.
pub struct Reader {
    pub(crate) home: PathBuf,
    pub(crate) env: Option<Box<EnvLookup<'static>>>,
    pub(crate) inner: Mutex<ReaderInner>,
    pub(crate) opencode: super::opencode::OpenCodeReader,
    pub(crate) hermes: super::hermes::HermesReader,
}

impl Reader {
    /// `NewReader` — `home` is the caller's home directory (fixture tests pass
    /// a temporary HOME).
    pub fn new(home: PathBuf) -> Self {
        Reader {
            home,
            env: None,
            inner: Mutex::new(ReaderInner {
                locations: HashMap::new(),
                omo_cache: HashMap::new(),
            }),
            opencode: super::opencode::OpenCodeReader::new(),
            hermes: super::hermes::HermesReader::new(),
        }
    }

    /// Test seam: resolve roots against a supplied environment instead of the
    /// process environment, so fixtures cannot see operator variables like
    /// `PI_CODING_AGENT_DIR`.
    #[cfg(test)]
    pub(crate) fn new_with_env(home: PathBuf, env: Box<EnvLookup<'static>>) -> Self {
        let mut reader = Reader::new(home);
        reader.env = Some(env);
        reader
    }

    pub(crate) fn env(&self) -> &EnvLookup<'_> {
        match &self.env {
            Some(env) => env.as_ref(),
            None => &|key| std::env::var_os(key).map(|v| v.to_string_lossy().into_owned()),
        }
    }

    fn home_str(&self) -> String {
        self.home.to_string_lossy().into_owned()
    }

    /// `Read` — no project context (historical lookups).
    pub fn read(
        &self,
        agent: &str,
        session_id: &str,
        before: Option<&str>,
        limit: usize,
    ) -> Result<Page, Error> {
        self.read_with_project(agent, ProjectContext::default(), session_id, before, limit)
    }

    /// `ReadFor` — pane-cwd lookup without a foreground hint.
    pub fn read_for(
        &self,
        agent: &str,
        cwd: &str,
        session_id: &str,
        before: Option<&str>,
        limit: usize,
    ) -> Result<Page, Error> {
        self.read_with_project(
            agent,
            ProjectContext {
                cwd: cwd.to_string(),
                foreground_cwd: String::new(),
            },
            session_id,
            before,
            limit,
        )
    }

    /// `ReadWithProject` — provider dispatch + the flat JSONL path.
    pub fn read_with_project(
        &self,
        agent: &str,
        project: ProjectContext,
        session_id: &str,
        before: Option<&str>,
        limit: usize,
    ) -> Result<Page, Error> {
        let project = normalize_project_context(agent, project);
        let before = before.unwrap_or("");
        if is_hermes_agent(agent) {
            return self.hermes_read_for(agent, &project.cwd, session_id, before, limit);
        }
        if normalized_agent(agent) == "opencode" {
            return self.opencode_read_for(&project.cwd, session_id, before, limit);
        }
        if matches!(normalized_agent(agent).as_str(), "omo" | "ohmyopencode") {
            return self.omo_read(&project.cwd, session_id, before, limit);
        }
        if !supported(agent) {
            return Ok(Page::unavailable(
                "invalid_provider",
                "Conversation history is not available for this agent.",
            ));
        }
        let session_id = session_id.trim();
        if session_id.is_empty() {
            return Ok(Page::unavailable(
                "invalid_session",
                "This agent has not reported a conversation session yet.",
            ));
        }
        let location = self.locate_with_project(agent, &project, session_id);
        if location.path.is_empty() {
            return Ok(Page::unavailable(
                "invalid_session",
                "No conversation log is available for this session.",
            ));
        }
        if is_claude_provider(agent) {
            return self.claude_read_chain(session_id, location, before, limit);
        }
        let (text, clipped) = load_tail(Path::new(&location.path), MAX_CONVERSATION_BYTES)
            .map_err(|err| Error::new(format!("read conversation log: {err}")))?;
        let entries = super::records::parse_transcript(agent, &text);
        let limit = if limit < 1 {
            DEFAULT_PAGE_SIZE
        } else {
            limit.min(MAX_PAGE_SIZE)
        };
        let mut end = entries.len();
        if !before.is_empty() {
            for (index, entry) in entries.iter().enumerate() {
                if entry.id == before {
                    end = index;
                    break;
                }
            }
        }
        let start = end.saturating_sub(limit);
        let page_entries: Vec<_> = entries[start..end].to_vec();
        Ok(Page {
            available: true,
            entries: page_entries,
            has_more: start > 0,
            total: entries.len() as i64,
            file_truncated: clipped,
            source_path: location.path.clone(),
            ..Page::default()
        })
    }

    /// `Locate` — pane cwd only.
    pub fn locate(&self, agent: &str, cwd: &str, session_id: &str) -> Location {
        self.locate_with_project(
            agent,
            &ProjectContext {
                cwd: cwd.to_string(),
                foreground_cwd: String::new(),
            },
            session_id,
        )
    }

    /// `LocateWithProject` — cached, TTL-keyed tuple resolution. The Go
    /// implementation single-flights concurrent misses; this synchronous
    /// reader never observes a duplicate scan.
    pub fn locate_with_project(
        &self,
        agent: &str,
        project: &ProjectContext,
        session_id: &str,
    ) -> Location {
        let project = normalize_project_context(agent, project.clone());
        let session_id = session_id.trim();
        let mut agent_key = normalized_agent(agent);
        if is_hermes_agent(agent) {
            agent_key = "hermes".to_string();
        }
        let key = LocationKey {
            agent: agent_key,
            cwd: project.cwd.clone(),
            foreground_cwd: project.foreground_cwd.clone(),
            session_id: session_id.to_string(),
        };
        {
            let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(cached) = inner.locations.get(&key) {
                if Instant::now() < cached.expires {
                    return cached.location.clone();
                }
            }
        }
        let location = self.locate_uncached(agent, &project, session_id);
        let ttl = if location.path.is_empty() {
            LOCATION_MISS_TTL
        } else {
            LOCATION_CACHE_TTL
        };
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if inner.locations.len() >= MAX_LOCATION_CACHE_ENTRIES {
            let now = Instant::now();
            inner.locations.retain(|_, cached| now < cached.expires);
            if inner.locations.len() >= MAX_LOCATION_CACHE_ENTRIES {
                inner.locations.clear();
            }
        }
        inner.locations.insert(
            key,
            LocationCacheEntry {
                location: location.clone(),
                expires: Instant::now() + ttl,
            },
        );
        location
    }

    /// Drop the cached location for `(agent, cwd, session_id)` — the `retry`
    /// path of `read_page`; the tuple is re-resolved on the next read.
    pub(crate) fn evict_location(&self, agent: &str, cwd: &str, session_id: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.locations.retain(|key, _| {
            !(key.agent == agent && key.cwd == cwd && key.session_id == session_id)
        });
    }

    /// `locateWithProject` — the uncached provider dispatch.
    fn locate_uncached(&self, agent: &str, project: &ProjectContext, session_id: &str) -> Location {
        let home = self.home_str();
        match normalized_agent(agent).as_str() {
            "claude" | "claudecode" => {
                if !safe_session_id(session_id) {
                    return Location::default();
                }
                find_project_session_with_project(
                    &roots::claude_roots(&home, self.env()),
                    project,
                    &format!("{session_id}.jsonl"),
                    &|cwd| claude_project_name(cwd),
                )
            }
            "qoder" | "qodercli" => {
                if !safe_session_id(session_id) {
                    return Location::default();
                }
                find_project_session_with_project(
                    &roots::qoder_roots(&home, self.env()),
                    project,
                    &format!("{session_id}.jsonl"),
                    &|_| String::new(),
                )
            }
            "codex" | "openaicodex" => {
                if !canonical_session_id(session_id) {
                    return Location::default();
                }
                find_codex_session(&roots::codex_roots(&home, self.env()), session_id)
            }
            "pi" | "picodingagent" => {
                resolve_path_or_session(&roots::pi_roots(&home, self.env()), session_id, "_")
            }
            "omp" | "ohmypi" => {
                resolve_path_or_session(&roots::omp_roots(&home, self.env()), session_id, "_")
            }
            "hermes" | "hermesagent" => self.hermes_locate(&project.cwd, session_id),
            _ => Location::default(),
        }
    }
}

/// `claudeProjectNonAlphanumeric.ReplaceAllString(cwd, "-")`.
fn claude_project_name(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// `Error` — reader failures are exceptional (I/O), not reason-coded pages.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Error(String);

impl Error {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Error(message.into())
    }
}

/// `isDir` — `os.Stat`-based so symlinks to directories count.
fn is_dir(path: &Path) -> bool {
    path.is_dir()
}

/// `projectDirectoriesForContext`.
fn project_directories_for_context(project: &ProjectContext) -> Vec<String> {
    let mut candidates = Vec::with_capacity(2);
    if !project.foreground_cwd.is_empty() {
        candidates.push(project.foreground_cwd.clone());
    }
    if !project.cwd.trim().is_empty() && (candidates.is_empty() || candidates[0] != project.cwd) {
        candidates.push(project.cwd.clone());
    }
    if candidates.is_empty() {
        // Empty cwd has historical meaning: enumerate every project in the root.
        return vec![String::new()];
    }
    candidates
}

/// `findProjectSessionWithProject` — roots outermost, foreground before cwd.
fn find_project_session_with_project(
    roots: &[String],
    project: &ProjectContext,
    filename: &str,
    preferred_project_name: &dyn Fn(&str) -> String,
) -> Location {
    let candidates = project_directories_for_context(project);
    for root in roots {
        let mut seen = std::collections::HashSet::new();
        for cwd in &candidates {
            let preferred = preferred_project_name(cwd);
            for project_dir in project_directories(root, cwd, &preferred) {
                let project_dir = PathBuf::from(roots::clean_path(&project_dir.to_string_lossy()));
                if !seen.insert(project_dir.clone()) {
                    continue;
                }
                if let Some(path) =
                    contained_regular_file(&project_dir.join(filename), Path::new(root))
                {
                    return Location {
                        path: path.to_string_lossy().into_owned(),
                        root: root.clone(),
                        title: String::new(),
                    };
                }
            }
        }
    }
    Location::default()
}

/// `projectDirectories` — preferred name, `-encoded`, `encoded`, then
/// directories whose `cwd` sidecar file matches.
fn project_directories(root: &str, cwd: &str, preferred_project_name: &str) -> Vec<PathBuf> {
    let root_path = Path::new(root);
    let Ok(entries) = std::fs::read_dir(root_path) else {
        return Vec::new();
    };
    let entries: Vec<_> = entries.flatten().collect();
    if cwd.trim().is_empty() {
        return entries
            .iter()
            .map(|entry| entry.path())
            .filter(|path| is_dir(path))
            .collect();
    }
    let encoded = cwd.trim_start_matches('/').replace('/', "-");
    let candidates = [
        preferred_project_name.to_string(),
        format!("-{encoded}"),
        encoded,
    ];
    let mut directories = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for name in candidates {
        if name.is_empty() {
            continue;
        }
        let path = root_path.join(&name);
        if seen.contains(&path) || !is_dir(&path) {
            continue;
        }
        seen.insert(path.clone());
        directories.push(path);
    }
    for entry in entries {
        let path = entry.path();
        if seen.contains(&path) || !is_dir(&path) {
            continue;
        }
        let Some(cwd_file) = contained_regular_file(&path.join("cwd"), root_path) else {
            continue;
        };
        let Ok(data) = std::fs::read(&cwd_file) else {
            continue;
        };
        if String::from_utf8_lossy(&data).trim() == cwd {
            directories.push(path);
        }
    }
    directories
}

/// `findCodexSession` — `sessions/<y>/<m>/<d>/rollout-*-<lower(id)>.jsonl`,
/// directories newest-first.
fn find_codex_session(roots: &[String], session_id: &str) -> Location {
    let suffix = format!("-{}.jsonl", session_id.to_lowercase());
    for root in roots {
        let root_path = Path::new(root);
        for year in descending_directories(root_path) {
            let year_path = root_path.join(&year);
            for month in descending_directories(&year_path) {
                let month_path = year_path.join(&month);
                for day in descending_directories(&month_path) {
                    let day_path = month_path.join(&day);
                    let Ok(files) = std::fs::read_dir(&day_path) else {
                        continue;
                    };
                    for file in files.flatten() {
                        let name = file.file_name().to_string_lossy().to_lowercase();
                        let file_path = file.path();
                        if is_dir(&file_path)
                            || !name.starts_with("rollout-")
                            || !name.ends_with(&suffix)
                        {
                            continue;
                        }
                        if let Some(path) = contained_regular_file(&file_path, root_path) {
                            return Location {
                                path: path.to_string_lossy().into_owned(),
                                root: root.clone(),
                                title: String::new(),
                            };
                        }
                    }
                }
            }
        }
    }
    Location::default()
}

/// `descendingDirectories` — names sorted reverse-lexicographically.
fn descending_directories(path: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(path) else {
        return Vec::new();
    };
    let mut directories: Vec<String> = entries
        .flatten()
        .filter(|entry| is_dir(&entry.path()))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    directories.sort_unstable_by(|a, b| b.cmp(a));
    directories
}

/// `resolvePathOrSession` — an absolute `.jsonl` session path is contained-
/// checked against each root; otherwise a canonical UUID finds
/// `<projectDir>/*_<id>.jsonl`.
fn resolve_path_or_session(roots: &[String], session_id: &str, separator: &str) -> Location {
    if Path::new(session_id).is_absolute() && session_id.to_lowercase().ends_with(".jsonl") {
        for root in roots {
            if let Some(path) = contained_regular_file(Path::new(session_id), Path::new(root)) {
                return Location {
                    path: path.to_string_lossy().into_owned(),
                    root: root.clone(),
                    title: String::new(),
                };
            }
        }
        return Location::default();
    }
    if !canonical_session_id(session_id) {
        return Location::default();
    }
    let suffix = format!("{}{}.jsonl", separator, session_id.to_lowercase());
    for root in roots {
        let root_path = Path::new(root);
        let Ok(directories) = std::fs::read_dir(root_path) else {
            continue;
        };
        for directory in directories.flatten() {
            let project_dir = directory.path();
            if !is_dir(&project_dir) {
                continue;
            }
            let Ok(files) = std::fs::read_dir(&project_dir) else {
                continue;
            };
            for file in files.flatten() {
                let file_path = file.path();
                if is_dir(&file_path)
                    || !file
                        .file_name()
                        .to_string_lossy()
                        .to_lowercase()
                        .ends_with(&suffix)
                {
                    continue;
                }
                if let Some(path) = contained_regular_file(&file_path, root_path) {
                    return Location {
                        path: path.to_string_lossy().into_owned(),
                        root: root.clone(),
                        title: String::new(),
                    };
                }
            }
        }
    }
    Location::default()
}

/// `containedRegularFile` — resolves symlinks on both path and root, requires
/// strict containment and a regular file; returns the resolved path.
pub(crate) fn contained_regular_file(path: &Path, root: &Path) -> Option<PathBuf> {
    let real_root = std::fs::canonicalize(root).ok()?;
    let real_path = std::fs::canonicalize(path).ok()?;
    let relative = real_path.strip_prefix(&real_root).ok()?;
    // `..` escapes are already excluded by strip_prefix; the oracle also
    // rejects the root itself ("." and "..").
    if relative.as_os_str().is_empty() {
        return None;
    }
    let info = std::fs::metadata(&real_path).ok()?;
    if !info.is_file() {
        return None;
    }
    Some(real_path)
}

/// `loadTail`/`loadTailFile` — reads at most `limit` bytes from the file end;
/// a clipped buffer starts after its first newline (the boundary line is
/// dropped). Returns `(bytes, clipped)`.
pub(crate) fn load_tail(path: &Path, limit: i64) -> std::io::Result<(Vec<u8>, bool)> {
    let file = open_conversation_source(path)?;
    load_tail_file(&file, limit)
}

pub(crate) fn load_tail_file(file: &std::fs::File, limit: i64) -> std::io::Result<(Vec<u8>, bool)> {
    let info = file.metadata()?;
    if !info.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "conversation source is not a regular file",
        ));
    }
    let size = info.len() as i64;
    let clipped = size > limit;
    let start = if clipped { size - limit } else { 0 };
    let mut file = file;
    file.seek(SeekFrom::Start(start as u64))?;
    let mut data = Vec::new();
    file.take(limit as u64).read_to_end(&mut data)?;
    if clipped {
        match data.iter().position(|b| *b == b'\n') {
            Some(newline) => {
                data.drain(..=newline);
            }
            None => data.clear(),
        };
    }
    Ok((data, clipped))
}

/// `openConversationSource` — `O_RDONLY | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK`.
/// `O_NOFOLLOW` is passed via `OpenOptionsExt::custom_flags` so no `libc`
/// dependency is needed; constants match the oracle's `darwin || linux` build.
#[cfg(target_os = "linux")]
pub(crate) fn open_conversation_source(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(0o400000 | 0o4000 | 0o2000000) // O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC
        .open(path)
}

#[cfg(target_os = "macos")]
pub(crate) fn open_conversation_source(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(0x0100 | 0x0004) // O_NOFOLLOW | O_NONBLOCK (O_CLOEXEC is default in Rust)
        .open(path)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn open_conversation_source(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}
