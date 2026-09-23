//! Pluggable dial layer. Herdr speaks NDJSON over a Unix socket today; the
//! socket-api doc states the Windows transport is a named pipe. `Transport`
//! abstracts `dial()` so a `NamedPipeTransport` can slot in behind
//! `cfg(windows)` without touching the client.

use std::fmt;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use tokio::io::{AsyncRead, AsyncWrite};

/// One live connection: any byte stream Tokio can drive.
pub trait Io: AsyncRead + AsyncWrite + Unpin + Send + 'static {}

impl<T> Io for T where T: AsyncRead + AsyncWrite + Unpin + Send + 'static {}

/// A boxed connection — what a [`Transport`] hands back.
pub type BoxIo = Box<dyn Io>;

/// Object-safe boxed future returned by [`Transport::dial`].
pub type DialFuture = Pin<Box<dyn Future<Output = io::Result<BoxIo>> + Send>>;

/// A way to reach the Herdr server. One dial per request — Herdr closes the
/// connection after each response, so pooling is impossible by design.
///
/// Implementors must return a fresh, unwritten connection per call.
pub trait Transport: Send + Sync + 'static {
    /// Open a new connection to the server.
    fn dial(&self) -> DialFuture;

    /// Short human-readable target for tracing (`unix:/path/herdr.sock`).
    fn describe(&self) -> String;

    /// The socket path this transport dials, when it has one — exported into
    /// the `herdr` CLI's environment as `HERDR_SOCKET_PATH` so session-aware
    /// subcommands (`api schema`, `--version` is unaffected) target the same
    /// server. Default `None` for in-memory test transports.
    fn socket_path_hint(&self) -> Option<PathBuf> {
        None
    }
}

/// Unix domain socket transport (`tokio::net::UnixStream`).
#[derive(Debug, Clone)]
pub struct UnixTransport {
    path: PathBuf,
}

impl UnixTransport {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        UnixTransport { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Transport for UnixTransport {
    fn dial(&self) -> DialFuture {
        let path = self.path.clone();
        Box::pin(async move {
            let stream = tokio::net::UnixStream::connect(path).await?;
            Ok(Box::new(stream) as BoxIo)
        })
    }

    fn describe(&self) -> String {
        format!("unix:{}", self.path.display())
    }

    fn socket_path_hint(&self) -> Option<PathBuf> {
        Some(self.path.clone())
    }
}

impl fmt::Debug for dyn Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Transport({})", self.describe())
    }
}

/// Resolve the default socket path the way the Herdr CLI does
/// (`HERDR_SOCKET_PATH` > `HERDR_SESSION` > `~/.config/herdr/herdr.sock`).
///
/// This mirrors doc 08's resolution order minus `--session` (a CLI flag the
/// relay does not carry). Returns `None` when the home directory cannot be
/// determined for the default fallback.
pub fn default_socket_path() -> Option<PathBuf> {
    default_socket_path_from(|name| std::env::var_os(name))
}

/// Env-var indirection so the resolution order is unit-testable.
pub(crate) fn default_socket_path_from(
    env: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    if let Some(path) = env("HERDR_SOCKET_PATH") {
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    let base = env("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| env("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    let base = base.join("herdr");
    if let Some(session) = env("HERDR_SESSION") {
        if !session.is_empty() {
            return Some(base.join("sessions").join(session).join("herdr.sock"));
        }
    }
    Some(base.join("herdr.sock"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::ffi::OsString;

    fn env(map: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: HashMap<String, OsString> = map
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn socket_path_env_wins() {
        let path = default_socket_path_from(env(&[
            ("HERDR_SOCKET_PATH", "/tmp/x.sock"),
            ("HERDR_SESSION", "abc"),
            ("HOME", "/home/u"),
        ]));
        assert_eq!(path, Some(PathBuf::from("/tmp/x.sock")));
    }

    #[test]
    fn socket_path_session_layout() {
        let path = default_socket_path_from(env(&[("HERDR_SESSION", "abc"), ("HOME", "/home/u")]));
        assert_eq!(
            path,
            Some(PathBuf::from(
                "/home/u/.config/herdr/sessions/abc/herdr.sock"
            ))
        );
    }

    #[test]
    fn socket_path_default_fallback() {
        let path = default_socket_path_from(env(&[("HOME", "/home/u")]));
        assert_eq!(
            path,
            Some(PathBuf::from("/home/u/.config/herdr/herdr.sock"))
        );
    }

    #[test]
    fn socket_path_xdg_wins_over_home() {
        let path =
            default_socket_path_from(env(&[("XDG_CONFIG_HOME", "/xdg"), ("HOME", "/home/u")]));
        assert_eq!(path, Some(PathBuf::from("/xdg/herdr/herdr.sock")));
    }

    #[test]
    fn socket_path_empty_values_ignored() {
        let path = default_socket_path_from(env(&[("HERDR_SOCKET_PATH", ""), ("HOME", "/home/u")]));
        assert_eq!(
            path,
            Some(PathBuf::from("/home/u/.config/herdr/herdr.sock"))
        );
    }

    #[test]
    fn socket_path_none_without_home() {
        let path = default_socket_path_from(env(&[]));
        assert_eq!(path, None);
    }
}
