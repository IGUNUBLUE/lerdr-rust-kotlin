//! Workspace inspection — `workspace_tree`/`workspace_file`/
//! `workspace_git_status`/`workspace_git_diff`, the `workspace/inspector.go`
//! port.
//!
//! All four are local-OS (filesystem walk, bounded `git` exec) — no Herdr
//! dispatch — and answer with a bare `command_result`. The oracle keys the
//! workspace off the pane's current `cwd` and re-checks that it did not
//! change mid-inspection; the topology snapshot at admission is compared
//! with the live watch afterwards for the same guard.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use lerdr_core::protocol::Inbound;
use serde::Serialize;
use tokio::sync::Semaphore;

use super::local::command_result;
use super::ActionContext;

// inspector.go bounds.
const MAX_TREE_ENTRIES: usize = 4000;
const MAX_TEXT_BYTES: u64 = 1024 * 1024;
const MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024;
const MAX_DIFF_BYTES: usize = 1024 * 1024;
const MAX_GIT_BYTES: usize = 8 * 1024 * 1024;
const GIT_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_STATUS_FILES: usize = 2000;

/// `ignoredDirectories`.
const IGNORED_DIRECTORIES: &[&str] = &[
    ".git",
    ".expo",
    ".next",
    ".turbo",
    ".vite",
    "Pods",
    "build",
    "coverage",
    "dist",
    "node_modules",
];

/// `gitSlots` — at most four concurrent git invocations.
static GIT_SLOTS: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(4));

#[derive(Serialize)]
struct TreeEntry {
    path: String,
    name: String,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
}

#[derive(Serialize)]
struct Tree {
    root: String,
    entries: Vec<TreeEntry>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    truncated: bool,
}

#[derive(Serialize)]
struct FilePreview {
    path: String,
    media_type: String,
    kind: &'static str,
    #[serde(skip_serializing_if = "String::is_empty")]
    text: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    data_url: String,
    size: u64,
}

#[derive(Serialize)]
struct GitFile {
    path: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    original_path: String,
    status: String,
}

#[derive(Serialize)]
struct GitStatus {
    available: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    branch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    ahead: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    behind: Option<i64>,
    files: Vec<GitFile>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    truncated: bool,
}

#[derive(Serialize)]
struct GitDiff {
    path: String,
    diff: String,
}

/// The four actions share one envelope: resolve the pane's cwd, inspect,
/// guard against a mid-inspection workspace change, answer.
pub(crate) async fn inspect(
    ctx: ActionContext,
    request_id: &str,
    action: &str,
    message: &Inbound,
) -> lerdr_core::protocol::Outbound {
    let pane_id = message.pane_id.as_str();
    let cwd = ctx
        .topology
        .pane_of(pane_id)
        .and_then(|a| a.cwd.clone())
        .map(|c| c.trim().to_owned())
        .unwrap_or_default();
    if pane_id.is_empty() || cwd.is_empty() {
        return command_result(
            request_id,
            action,
            false,
            "failed",
            "Agent pane not found",
            pane_id,
            None,
        );
    }
    let inspected = match action {
        "workspace_tree" => tree_for(&cwd).map(|v| serde_json::to_value(v).unwrap_or_default()),
        "workspace_file" => {
            read_file(&cwd, &message.path).map(|v| serde_json::to_value(v).unwrap_or_default())
        }
        "workspace_git_status" => git_status_for(&cwd)
            .await
            .map(|v| serde_json::to_value(v).unwrap_or_default()),
        _ => git_diff_for(&cwd, &message.path)
            .await
            .map(|v| serde_json::to_value(v).unwrap_or_default()),
    };
    // The oracle's generation+cwd guard: the live topology (not the
    // admission snapshot) must still show this pane at the same cwd.
    let still_same = ctx
        .handle
        .topology
        .borrow()
        .pane_of(pane_id)
        .and_then(|a| a.cwd.clone())
        .map(|c| c.trim() == cwd)
        .unwrap_or(false);
    if !still_same {
        return command_result(
            request_id,
            action,
            false,
            "failed",
            "Agent workspace changed during inspection",
            pane_id,
            None,
        );
    }
    match inspected {
        Err(message) => {
            command_result(request_id, action, false, "failed", &message, pane_id, None)
        }
        Ok(data) => command_result(
            request_id,
            action,
            true,
            "completed",
            "",
            pane_id,
            Some(data),
        ),
    }
}

// ── workspace root + safe paths ──────────────────────────────────────────

/// `openWorkspace` — canonicalize the workspace; it must exist and be a
/// directory. Rust has no `os.Root`; `symlink_metadata`/`canonicalize`
/// checks on every touched path keep the same never-escape guarantee
/// without following links.
fn open_workspace(path: &str) -> Result<PathBuf, String> {
    let path = path.trim();
    if path.is_empty() {
        return Err("Workspace path is unavailable".to_owned());
    }
    let canonical =
        std::fs::canonicalize(path).map_err(|_| "Workspace is unavailable".to_owned())?;
    let meta = std::fs::metadata(&canonical).map_err(|_| "Workspace is unavailable".to_owned())?;
    if !meta.is_dir() {
        return Err("Workspace is unavailable".to_owned());
    }
    Ok(canonical)
}

/// `safePath` — workspace-relative, slash-separated, no escapes.
fn safe_path(path: &str) -> Result<String, String> {
    let path = path.trim().replace('\\', "/");
    if path.is_empty() || path == "." || !valid_relative_path(&path) {
        return Err("Invalid workspace-relative path".to_owned());
    }
    Ok(path)
}

/// `fs.ValidPath` — relative, no `.`/`..` components, no leading slash.
fn valid_relative_path(path: &str) -> bool {
    if path.starts_with('/') || path.is_empty() {
        return false;
    }
    path.split('/')
        .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// `regularFile` — the path must resolve to a regular file that is not a
/// symlink, still inside the workspace.
fn regular_file(root: &Path, path: &str) -> Result<(PathBuf, u64), String> {
    let joined = root.join(path);
    let meta = std::fs::symlink_metadata(&joined)
        .map_err(|_| "Workspace file was not found".to_owned())?;
    if meta.file_type().is_symlink() || !meta.file_type().is_file() {
        return Err("Workspace preview only supports regular files".to_owned());
    }
    // Canonicalize must agree the file sits under the workspace root (a
    // symlinked intermediate directory would show up here).
    let canonical =
        std::fs::canonicalize(&joined).map_err(|_| "Workspace file was not found".to_owned())?;
    if !canonical.starts_with(root) {
        return Err("Workspace preview only supports regular files".to_owned());
    }
    Ok((joined, meta.len()))
}

// ── tree ─────────────────────────────────────────────────────────────────

fn tree_for(workspace: &str) -> Result<Tree, String> {
    let root = open_workspace(workspace)?;
    let mut entries: Vec<TreeEntry> = Vec::with_capacity(256);
    let mut truncated = false;
    let mut stack: Vec<PathBuf> = vec![root.clone()];
    // Iterative walk — `fs.WalkDir` order is not load-bearing (the entries
    // are sorted afterwards) and the ignored-directory / symlink rules are
    // the observable behavior.
    while let Some(dir) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if meta.is_dir() && IGNORED_DIRECTORIES.contains(&name.as_str()) {
                continue;
            }
            if meta.file_type().is_symlink() {
                continue;
            }
            let entry_path = entry.path();
            let Ok(relative) = entry_path.strip_prefix(&root) else {
                continue;
            };
            if entries.len() >= MAX_TREE_ENTRIES {
                truncated = true;
                break;
            }
            if meta.is_dir() {
                stack.push(entry.path());
                entries.push(TreeEntry {
                    path: slash_path(relative),
                    name,
                    kind: "directory",
                    size: None,
                });
            } else {
                if !meta.is_file() {
                    continue;
                }
                entries.push(TreeEntry {
                    path: slash_path(relative),
                    name,
                    kind: "file",
                    size: Some(meta.len()),
                });
            }
        }
        if truncated {
            break;
        }
    }
    entries.sort_by(|a, b| {
        let parent_a = Path::new(&a.path).parent().map(|p| p.to_path_buf());
        let parent_b = Path::new(&b.path).parent().map(|p| p.to_path_buf());
        if parent_a != parent_b {
            return a.path.cmp(&b.path);
        }
        if a.kind != b.kind {
            return if a.kind == "directory" {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            };
        }
        a.name.to_lowercase().cmp(&b.name.to_lowercase())
    });
    Ok(Tree {
        root: root.to_string_lossy().into_owned(),
        entries,
        truncated,
    })
}

fn slash_path(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

// ── file preview ─────────────────────────────────────────────────────────

/// `mime.TypeByExtension` for the handful of media types the preview
/// distinguishes; empty → content sniffing.
fn extension_media_type(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .as_deref()
        .unwrap_or_default()
    {
        "png" => "image/png",
        "gif" => "image/gif",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "avif" => "image/avif",
        "svg" => "image/svg+xml",
        "json" => "application/json",
        "pdf" => "application/pdf",
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "txt" | "md" | "log" => "text/plain; charset=utf-8",
        _ => "",
    }
}

/// `http.DetectContentType` approximation — enough to keep `media_type`
/// honest for unknown extensions: NUL bytes are binary; common magic
/// headers get their type; the rest is text.
fn detect_media_type(data: &[u8]) -> &'static str {
    let head = &data[..data.len().min(512)];
    const MAGICS: &[(&[u8], &str)] = &[
        (b"\x89PNG\r\n\x1a\n", "image/png"),
        (b"GIF87a", "image/gif"),
        (b"GIF89a", "image/gif"),
        (b"\xff\xd8\xff", "image/jpeg"),
        (b"%PDF", "application/pdf"),
        (b"PK\x03\x04", "application/zip"),
        (b"RIFF", "application/octet-stream"), // webp needs RIFF....WEBP; octet-stream is honest
    ];
    for (magic, media) in MAGICS {
        if head.starts_with(magic) {
            return media;
        }
    }
    if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        return "image/webp";
    }
    if head.contains(&0) {
        return "application/octet-stream";
    }
    "text/plain; charset=utf-8"
}

fn read_file(workspace: &str, path: &str) -> Result<FilePreview, String> {
    let root = open_workspace(workspace)?;
    let path = safe_path(path)?;
    let (file, size) = regular_file(&root, &path)?;

    let extension_type = extension_media_type(&path);
    let mut limit = MAX_TEXT_BYTES;
    let mut kind = "text";
    if extension_type.starts_with("image/") && extension_type != "image/svg+xml" {
        limit = MAX_IMAGE_BYTES;
        kind = "image";
    }
    if size > limit {
        return Err(format!(
            "Workspace file exceeds the {} MB preview limit",
            limit / (1024 * 1024)
        ));
    }
    let data = std::fs::read(&file)
        .map_err(|_| "Workspace file could not be read within the preview limit".to_owned())?;
    if data.len() as u64 > limit {
        return Err("Workspace file could not be read within the preview limit".to_owned());
    }
    let mut media_type = extension_type.to_owned();
    if media_type.is_empty() {
        media_type = detect_media_type(&data).to_owned();
    }
    let media_type = media_type.split(';').next().unwrap_or_default().to_owned();
    if kind == "image" {
        if !media_type.starts_with("image/") {
            return Err("Workspace image type is not supported".to_owned());
        }
        use base64::Engine;
        return Ok(FilePreview {
            path,
            media_type: media_type.clone(),
            kind,
            text: String::new(),
            data_url: format!(
                "data:{media_type};base64,{}",
                base64::engine::general_purpose::STANDARD.encode(&data)
            ),
            size,
        });
    }
    if data.contains(&0) {
        return Err("Binary workspace files cannot be previewed".to_owned());
    }
    Ok(FilePreview {
        path,
        media_type,
        kind,
        text: String::from_utf8_lossy(&data).into_owned(),
        data_url: String::new(),
        size,
    })
}

// ── git ──────────────────────────────────────────────────────────────────

/// `gitEnvironment` — scrub `GIT_*`, force the non-interactive config set.
fn git_environment() -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(key, _)| !key.starts_with("GIT_"))
        .collect();
    for (key, value) in [
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_NO_LAZY_FETCH", "1"),
        ("GIT_OPTIONAL_LOCKS", "0"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_PAGER", "cat"),
        ("GIT_EXTERNAL_DIFF", ""),
        ("LC_ALL", "C"),
    ] {
        env.retain(|(k, _)| k != key);
        env.push((key.to_owned(), value.to_owned()));
    }
    env
}

/// `runGit` — slot-semaphore, 8s timeout, bounded stdout/stderr.
/// Returns (stdout, exit_code); `-1` exit codes are the oracle's own
/// timeout/bounds failures.
async fn run_git(root: &Path, limit: usize, args: &[&str]) -> Result<(String, i32), String> {
    let permit = tokio::time::timeout(GIT_TIMEOUT, GIT_SLOTS.acquire())
        .await
        .map_err(|_| "Git inspection timed out".to_owned())?
        .map_err(|_| "Git inspection timed out".to_owned())?;
    let mut command = tokio::process::Command::new("git");
    command.args([
        "--literal-pathspecs",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "color.ui=false",
        "-c",
        "color.diff=false",
        "-c",
        "diff.ignoreSubmodules=all",
        "-c",
        "status.relativePaths=true",
        "-C",
    ]);
    command.arg(root);
    command.args(args);
    command.env_clear();
    for (key, value) in git_environment() {
        command.env(key, value);
    }
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    let output = match tokio::time::timeout(GIT_TIMEOUT, command.output()).await {
        Err(_) => return Err("Git inspection timed out".to_owned()),
        Ok(Err(_)) => return Err("Git inspection could not start".to_owned()),
        Ok(Ok(out)) => out,
    };
    drop(permit);
    if output.stdout.len() > limit || output.stderr.len() > 64 * 1024 {
        return Err("Git output exceeded the preview limit".to_owned());
    }
    Ok((
        String::from_utf8_lossy(&output.stdout).into_owned(),
        output.status.code().unwrap_or(-1),
    ))
}

async fn git_status_for(workspace: &str) -> Result<GitStatus, String> {
    let root = open_workspace(workspace)?;
    let (output, code) = run_git(
        &root,
        MAX_GIT_BYTES,
        &[
            "status",
            "--porcelain=v1",
            "--branch",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=all",
            "--",
            ".",
        ],
    )
    .await?;
    if code != 0 {
        return Ok(GitStatus {
            available: false,
            branch: String::new(),
            ahead: None,
            behind: None,
            files: Vec::new(),
            truncated: false,
        });
    }
    let parts: Vec<&str> = output.split('\0').collect();
    let mut status = GitStatus {
        available: true,
        branch: String::new(),
        ahead: None,
        behind: None,
        files: Vec::new(),
        truncated: false,
    };
    let mut index = 0;
    while index < parts.len() {
        let part = parts[index];
        index += 1;
        if part.is_empty() {
            continue;
        }
        if let Some(branch) = part.strip_prefix("## ") {
            let branch = branch
                .strip_prefix("No commits yet on ")
                .or_else(|| branch.strip_prefix("Initial commit on "))
                .unwrap_or(branch);
            status.branch = branch.split("...").next().unwrap_or(branch).to_owned();
            continue;
        }
        if part.len() < 4 {
            continue;
        }
        let mut entry = GitFile {
            status: part[..2].to_owned(),
            path: part[3..].to_owned(),
            original_path: String::new(),
        };
        if entry.status.contains(['R', 'C']) && index < parts.len() {
            entry.original_path = parts[index].to_owned();
            index += 1;
        }
        if status.files.len() >= MAX_STATUS_FILES {
            status.truncated = true;
            continue;
        }
        status.files.push(entry);
    }
    let (counts, count_code) = run_git(
        &root,
        128,
        &["rev-list", "--left-right", "--count", "HEAD...@{upstream}"],
    )
    .await?;
    if count_code == 0 {
        let trimmed = counts.trim();
        let mut pieces = trimmed.split_whitespace();
        let ahead = pieces.next().and_then(|v| v.parse::<i64>().ok());
        let behind = pieces.next().and_then(|v| v.parse::<i64>().ok());
        if let (Some(ahead), Some(behind)) = (ahead, behind) {
            if ahead >= 0 && behind >= 0 {
                status.ahead = Some(ahead);
                status.behind = Some(behind);
            }
        }
    }
    Ok(status)
}

async fn git_diff_for(workspace: &str, path: &str) -> Result<GitDiff, String> {
    let root = open_workspace(workspace)?;
    let path = safe_path(path)?;
    let status = git_status_for(workspace).await?;
    if !status.available {
        return Err("Workspace is not inside a Git repository".to_owned());
    }
    let Some(changed) = status.files.iter().find(|f| f.path == path) else {
        return Err("Workspace file is no longer reported as changed".to_owned());
    };
    if changed.status == "??" {
        regular_file(&root, &path)?;
        let (output, code) = run_git(
            &root,
            MAX_DIFF_BYTES,
            &[
                "diff",
                "--no-index",
                "--no-ext-diff",
                "--no-textconv",
                "--",
                "/dev/null",
                &path,
            ],
        )
        .await?;
        if code != 0 && code != 1 {
            return Err("Git diff could not be read".to_owned());
        }
        return Ok(GitDiff { path, diff: output });
    }
    let (staged, staged_code) = run_git(
        &root,
        MAX_DIFF_BYTES,
        &[
            "diff",
            "--cached",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--",
            &path,
        ],
    )
    .await?;
    let (unstaged, unstaged_code) = run_git(
        &root,
        MAX_DIFF_BYTES,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--",
            &path,
        ],
    )
    .await?;
    if staged_code != 0 || unstaged_code != 0 {
        return Err("Git diff could not be read".to_owned());
    }
    let mut combined = staged;
    if !combined.is_empty() && !unstaged.is_empty() {
        combined.push('\n');
    }
    combined.push_str(&unstaged);
    if combined.len() > MAX_DIFF_BYTES {
        return Err("Git diff exceeded the preview limit".to_owned());
    }
    Ok(GitDiff {
        path,
        diff: combined,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lerdr-inspect-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn safe_path_validates() {
        assert_eq!(safe_path("src/main.rs").unwrap(), "src/main.rs");
        assert!(safe_path("../x").is_err());
        assert!(safe_path("/abs").is_err());
        assert!(safe_path("").is_err());
        assert!(safe_path("a//b").is_err());
        assert!(safe_path("a/./b").is_err());
    }

    #[test]
    fn tree_skips_ignored_and_symlinks() {
        let dir = tempdir("tree");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("node_modules/pkg")).unwrap();
        std::fs::write(dir.join("src/main.rs"), b"fn main() {}").unwrap();
        std::fs::write(dir.join("node_modules/pkg/x.js"), b"x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("src"), dir.join("link")).unwrap();
        let tree = tree_for(dir.to_str().unwrap()).expect("tree");
        let paths: Vec<&str> = tree.entries.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"src"));
        assert!(paths.contains(&"src/main.rs"));
        assert!(!paths.iter().any(|p| p.starts_with("node_modules")));
        assert!(!paths.iter().any(|p| p.starts_with("link")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_file_text_and_binary() {
        let dir = tempdir("file");
        std::fs::write(dir.join("a.txt"), b"hello").unwrap();
        std::fs::write(dir.join("bin.dat"), b"\x00\x01").unwrap();
        let preview = read_file(dir.to_str().unwrap(), "a.txt").expect("preview");
        assert_eq!(preview.text, "hello");
        assert_eq!(preview.media_type, "text/plain");
        assert!(read_file(dir.to_str().unwrap(), "bin.dat").is_err());
        assert!(read_file(dir.to_str().unwrap(), "../outside").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
