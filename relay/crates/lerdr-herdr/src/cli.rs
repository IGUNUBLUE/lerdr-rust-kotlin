//! `herdr` CLI invocation — the port of the Go client's `runCommand`.
//!
//! The socket API has no schema method, so introspection runs
//! `herdr api schema --json` as a subprocess (and `herdr --version` for the
//! installed-binary version evidence). Rules carried over from
//! `internal/herdr/client.go`:
//!
//! * one spawn per call, stdout/stderr each capped (`maxOutputBytes`),
//! * the child inherits `HERDR_SOCKET_PATH` so session-scoped subcommands
//!   would target the same server,
//! * a timeout kills the child (`kill_on_drop` covers the cancelled-future
//!   case; the Go client kills the whole process group — `api schema` and
//!   `--version` spawn no descendants),
//! * binary resolution mirrors `findHerdrBin`: explicit config → `HERDR_BIN`
//!   → `HERDR_BIN_PATH` → `PATH` lookup → known install locations → the
//!   literal `herdr`.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// Hard cap on a CLI stream — the Go client's `maxOutputBytes` (4 MiB).
const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

/// Why a CLI invocation produced no usable output. Internal to the crate —
/// capability collection maps every variant to "schema/version unknown".
#[derive(Debug, thiserror::Error)]
pub(crate) enum CliError {
    /// The binary could not be spawned at all (not installed, not
    /// executable, resource exhaustion).
    #[error("spawn: {0}")]
    Spawn(io::Error),
    /// The child exceeded its deadline and was killed.
    #[error("timed out after {0:?}")]
    Timeout(Duration),
    /// The child exited non-zero; carries the truncated stderr diagnostic.
    #[error("command failed: {0}")]
    Failed(String),
    /// Output exceeded the byte cap.
    #[error("output exceeded {0} bytes")]
    Oversize(usize),
    /// Reading the child's pipes or waiting on it failed.
    #[error("pipe: {0}")]
    Pipe(io::Error),
}

/// Resolve the `herdr` binary — `findHerdrBin` in the oracle's
/// `internal/config`: explicit override → `HERDR_BIN` → `HERDR_BIN_PATH`
/// (doc 09's alias) → `PATH` scan → known install locations → the literal
/// name (spawn failure then reports unavailable).
pub(crate) fn resolve_herdr_bin(explicit: Option<&Path>) -> PathBuf {
    if let Some(path) = explicit.filter(|p| !p.as_os_str().is_empty()) {
        return path.to_path_buf();
    }
    for key in ["HERDR_BIN", "HERDR_BIN_PATH"] {
        if let Some(value) = std::env::var_os(key).filter(|v| !v.is_empty()) {
            return PathBuf::from(value);
        }
    }
    if let Some(found) = look_path("herdr") {
        return found;
    }
    for candidate in [
        home_dir().join(".local/bin/herdr"),
        PathBuf::from("/opt/homebrew/bin/herdr"),
        PathBuf::from("/usr/local/bin/herdr"),
        PathBuf::from("/home/linuxbrew/.linuxbrew/bin/herdr"),
        PathBuf::from("/home/linuxbrew/.linuxbrew/opt/herdr/bin/herdr"),
    ] {
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from("herdr")
}

/// `exec.LookPath` — regular file with any execute bit, searched in PATH
/// order (or treated as a literal path when it contains a separator).
fn look_path(name: &str) -> Option<PathBuf> {
    #[cfg(unix)]
    fn executable(path: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    fn executable(path: &Path) -> bool {
        path.is_file()
    }
    if name.contains(std::path::MAIN_SEPARATOR) || name.contains('/') {
        return executable(Path::new(name)).then(|| PathBuf::from(name));
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// Run `bin args…` with capped stdout/stderr and a deadline. Returns the
/// stdout bytes on a clean exit 0. The child is killed on timeout, and
/// `kill_on_drop` covers a caller that drops this future mid-flight.
pub(crate) async fn run_cli(
    bin: &Path,
    socket_path: Option<&Path>,
    args: &[&str],
    timeout: Duration,
) -> Result<Vec<u8>, CliError> {
    let mut command = Command::new(bin);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(path) = socket_path {
        command.env("HERDR_SOCKET_PATH", path);
    }
    let mut child = command.spawn().map_err(CliError::Spawn)?;
    let mut stdout = child.stdout.take().expect("stdout piped");
    let mut stderr = child.stderr.take().expect("stderr piped");

    // Drain both pipes concurrently — like Go's limitedBuffer, reads never
    // stop at the cap (a full pipe would stall the child); only the kept
    // bytes are bounded.
    let out_task = tokio::spawn(async move { drain_capped(&mut stdout).await });
    let err_task = tokio::spawn(async move { drain_capped(&mut stderr).await });
    let joined = tokio::time::timeout(timeout, async {
        let (out, err, status) = tokio::join!(out_task, err_task, child.wait());
        (out, err, status)
    })
    .await;

    let (out, err, status) = match joined {
        Err(_elapsed) => {
            let _ = child.kill().await;
            return Err(CliError::Timeout(timeout));
        }
        Ok((out, err, status)) => (
            out.map_err(join_err)??,
            err.map_err(join_err)??,
            status.map_err(CliError::Pipe)?,
        ),
    };
    let (stdout, stdout_truncated) = out;
    let (stderr, stderr_truncated) = err;

    if stdout_truncated {
        return Err(CliError::Oversize(MAX_OUTPUT_BYTES));
    }
    if status.success() {
        return Ok(stdout);
    }
    let mut diagnostic = String::from_utf8_lossy(&stderr).trim().to_owned();
    if diagnostic.is_empty() {
        diagnostic = status.to_string();
    }
    if stderr_truncated {
        diagnostic.push_str(" (truncated)");
    }
    // The oracle truncates the stderr diagnostic at 500 chars.
    if diagnostic.len() > 500 {
        diagnostic.truncate(500);
        diagnostic.push_str("...");
    }
    Err(CliError::Failed(diagnostic))
}

fn join_err(err: tokio::task::JoinError) -> CliError {
    CliError::Pipe(io::Error::other(format!("join: {err}")))
}

/// Read to EOF keeping at most `MAX_OUTPUT_BYTES`; returns
/// `(kept, truncated)`.
async fn drain_capped(
    reader: &mut (impl tokio::io::AsyncRead + Unpin),
) -> Result<(Vec<u8>, bool), CliError> {
    let mut buf = Vec::new();
    let mut truncated = false;
    let mut chunk = [0u8; 16 * 1024];
    loop {
        let n = reader.read(&mut chunk).await.map_err(CliError::Pipe)?;
        if n == 0 {
            return Ok((buf, truncated));
        }
        let room = MAX_OUTPUT_BYTES.saturating_sub(buf.len());
        buf.extend_from_slice(&chunk[..n.min(room)]);
        truncated |= n > room;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Path resolution order: explicit > HERDR_BIN > HERDR_BIN_PATH > PATH.
    /// Env vars are process-global, so this test stays a pure-function check
    /// on the explicit branch; env/PATH resolution is exercised through
    /// `collect_capabilities` tests with stub scripts.
    #[test]
    fn explicit_bin_wins() {
        assert_eq!(
            resolve_herdr_bin(Some(Path::new("/opt/x/herdr"))),
            PathBuf::from("/opt/x/herdr")
        );
    }

    #[tokio::test]
    async fn missing_binary_is_spawn_error() {
        let err = run_cli(
            Path::new("/nonexistent/herdr-binary-for-tests"),
            None,
            &["--version"],
            Duration::from_secs(2),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, CliError::Spawn(_)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn captures_stdout_and_reports_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("herdr");
        std::fs::write(
            &bin,
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'herdr 9.9.9'; else echo '{\"bad\":1}' >&2; exit 3; fi\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();

        let out = run_cli(&bin, None, &["--version"], Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(String::from_utf8(out).unwrap().trim(), "herdr 9.9.9");
        let err = run_cli(&bin, None, &["api", "schema"], Duration::from_secs(5))
            .await
            .unwrap_err();
        match err {
            CliError::Failed(msg) => assert!(msg.contains("bad")),
            other => panic!("expected Failed, got {other}"),
        }
    }
}
