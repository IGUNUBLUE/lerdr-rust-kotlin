//! `sqlite3` CLI execution — shared by the OpenCode and Hermes readers, which
//! (like the Go oracle) run `sqlite3 -readonly -batch -json` rather than
//! linking a SQLite library. Output is bounded; a 3 s timeout kills a wedged
//! query. `stderr` is discarded — the oracle captures it only for diagnostics.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Duration;

use super::roots::EnvLookup;

/// Failure codes matching the oracle's query errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SqliteError {
    /// spawn failure, non-zero exit, or timeout — `query_failed`
    QueryFailed,
    /// stdout exceeded the bounded buffer — `output_limit`
    OutputLimit,
}

/// `exec.LookPath` — does `binary` resolve to an executable file on PATH (or
/// as a path containing a slash)?
pub(crate) fn look_path(binary: &str, env: &EnvLookup) -> bool {
    let is_executable = |path: &std::path::Path| -> bool {
        let Ok(meta) = std::fs::metadata(path) else {
            return false;
        };
        if !meta.is_file() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            meta.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            true
        }
    };
    if binary.contains('/') {
        return is_executable(std::path::Path::new(binary));
    }
    let path = env("PATH").unwrap_or_else(|| "/usr/bin:/bin".to_string());
    path.split(':')
        .filter(|dir| !dir.is_empty())
        .any(|dir| is_executable(&std::path::Path::new(dir).join(binary)))
}

/// Run `sqlite3 -readonly -batch -json <database> <query>`, returning stdout
/// on success. Reads on a helper thread so a `timeout` kill cannot be blocked
/// by a writer that stops producing output.
pub(crate) fn run_json_query(
    binary: &str,
    database: &str,
    query: &str,
    max_output: usize,
    timeout: Duration,
) -> Result<Vec<u8>, SqliteError> {
    let mut child = Command::new(binary)
        .args(["-readonly", "-batch", "-json", database, query])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| SqliteError::QueryFailed)?;
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(SqliteError::QueryFailed);
    };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut overflow = false;
        let mut chunk = [0u8; 64 * 1024];
        loop {
            match stdout.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if buf.len() + n > max_output {
                        overflow = true;
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
            }
        }
        let _ = tx.send((buf, overflow));
    });
    match rx.recv_timeout(timeout) {
        Ok((buf, overflow)) => {
            if overflow {
                let _ = child.kill();
            }
            let status = child.wait();
            if overflow {
                return Err(SqliteError::OutputLimit);
            }
            match status {
                Ok(status) if status.success() => Ok(buf),
                _ => Err(SqliteError::QueryFailed),
            }
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(SqliteError::QueryFailed)
        }
    }
}
