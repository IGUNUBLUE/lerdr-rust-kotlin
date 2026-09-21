//! Plugin-facing helper subcommands — the surface `plugin/scripts/*` invokes
//! on the installed binary. Ports of `internal/setuphelper`,
//! `internal/eventhook`, and `internal/support` from the reference
//! implementation; byte-compatible where scripts parse the output.

use std::env;
use std::fmt;
use std::fs;
use std::io;
use std::net::UdpSocket;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::json;

/// `relayEnv` — prefer `LERDR_*`, accept legacy `HERDR_*`.
fn relay_env(key: &str) -> Option<(String, String)> {
    if let Ok(value) = env::var(format!("LERDR_{key}")) {
        return Some((format!("LERDR_{key}"), value));
    }
    env::var(format!("HERDR_{key}"))
        .ok()
        .map(|v| (format!("HERDR_{key}"), v))
}

/// Load `relay.env` from the plugin config dir without overriding existing
/// vars — `loadEnvironment` in the oracle.
fn load_environment() {
    let mut filename = relay_env("RELAY_ENV").map(|(_, v)| PathBuf::from(v));
    if filename.is_none() {
        if let Ok(dir) = env::var("HERDR_PLUGIN_CONFIG_DIR") {
            filename = Some(PathBuf::from(dir).join("relay.env"));
        }
    }
    let Some(filename) = filename else { return };
    let Ok(data) = fs::read_to_string(&filename) else {
        return;
    };
    for line in data.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim().trim_matches('"').trim_matches('\'');
        if !key.is_empty() && env::var_os(key).is_none() {
            // SAFETY-free: single-threaded CLI entry before tokio starts.
            env::set_var(key, value);
        }
    }
}

/// UDP plugin port — `LERDR_RELAY_PLUGIN_PORT` > `HERDR_RELAY_PLUGIN_PORT` >
/// 8376.
fn plugin_port() -> Result<u16, HookError> {
    match relay_env("RELAY_PLUGIN_PORT") {
        Some((key, value)) => value
            .parse::<u16>()
            .ok()
            .filter(|p| *p >= 1)
            .ok_or_else(|| HookError::msg(format!("invalid {key} {value:?}"))),
        None => Ok(8376),
    }
}

fn default_socket_path() -> PathBuf {
    if let Ok(path) = env::var("HERDR_SOCKET_PATH") {
        return PathBuf::from(path);
    }
    let config_home = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    config_home.join("herdr").join("herdr.sock")
}

fn send_udp(payload: serde_json::Value) -> Result<(), HookError> {
    let port = plugin_port()?;
    let socket = UdpSocket::bind("127.0.0.1:0")?;
    socket.set_write_timeout(Some(std::time::Duration::from_secs(1)))?;
    socket.send_to(payload.to_string().as_bytes(), ("127.0.0.1", port))?;
    Ok(())
}

/// `setup-fragment TOKEN LABEL [RELAY]` → `setup=…&label=…[&relay=…]`.
pub fn setup_fragment(token: &str, label: &str, relay: Option<&str>) -> String {
    let mut values = vec![("setup", token.to_string()), ("label", label.to_string())];
    if let Some(relay) = relay {
        if !relay.is_empty() {
            values.push(("relay", relay.to_string()));
        }
    }
    values
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencoding(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// `application/x-www-form-urlencoded` component encoding — same alphabet the
/// oracle's `url.Values.Encode` emits (space → `+` is *not* what we want for
/// setup links; Go encodes space as `+` in query values. Match it exactly.)
fn urlencoding(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// `normalize-origin [--allow-loopback-http] ORIGIN` — port of
/// `setuphelper.NormalizeOrigin`: adds `https://` when no scheme, rejects
/// credentials/paths/queries/fragments, enforces HTTPS except loopback HTTP
/// when allowed, strips default port.
pub fn normalize_origin(value: &str, allow_loopback_http: bool) -> Result<String, HookError> {
    let mut value = value.trim().to_string();
    if !value.contains("://") {
        value = format!("https://{value}");
    }
    let parsed = url::Url::parse(&value)?;

    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.host_str().is_none_or(|h| h.is_empty())
        || (parsed.path() != "" && parsed.path() != "/")
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(HookError::msg(
            "origin must not contain credentials, a path, query, or fragment",
        ));
    }
    if parsed
        .host_str()
        .is_some_and(|h| h.chars().any(|c| (c as u32) < 33))
    {
        return Err(HookError::msg("origin has an invalid host"));
    }

    let hostname = parsed.host_str().unwrap_or_default().to_lowercase();
    let is_loopback = hostname == "localhost"
        || hostname
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false);
    let loopback_http = allow_loopback_http && parsed.scheme() == "http" && is_loopback;
    if parsed.scheme() != "https" && !loopback_http {
        return Err(HookError::msg("origin must use HTTPS"));
    }

    let port = parsed.port();
    let host = match port {
        Some(p) if !(parsed.scheme() == "https" && p == 443) => {
            if hostname.contains(':') {
                format!("[{hostname}]:{p}")
            } else {
                format!("{hostname}:{p}")
            }
        }
        _ => {
            if hostname.contains(':') {
                format!("[{hostname}]")
            } else {
                hostname
            }
        }
    };
    Ok(format!("{}://{host}", parsed.scheme()))
}

/// `qr [--columns N] VALUE` — EC level M, half-block rows, quiet zone;
/// refuses when the rendered width exceeds the terminal budget (matching
/// `setuphelper.TerminalQR`).
pub fn terminal_qr(value: &str, max_columns: usize) -> Result<String, HookError> {
    if value.is_empty() {
        return Err(HookError::msg("QR value is required"));
    }
    let code = qrcode::QrCode::with_error_correction_level(value, qrcode::EcLevel::M)
        .map_err(|e| HookError::msg(e.to_string()))?;
    let rendered = code
        .render::<qrcode::render::unicode::Dense1x2>()
        .quiet_zone(true)
        .build();
    if max_columns > 0 {
        let width = rendered
            .lines()
            .next()
            .map(|l| l.chars().count())
            .unwrap_or(0);
        if width > max_columns {
            return Err(HookError::msg(format!(
                "QR code needs {width} columns, terminal has {max_columns}"
            )));
        }
    }
    Ok(rendered)
}

#[derive(Debug, Default, Deserialize)]
struct EventEnvelope {
    #[serde(default)]
    data: EventData,
}

#[derive(Debug, Default, Deserialize)]
struct EventData {
    #[serde(default)]
    pane_id: String,
    #[serde(default)]
    tab_id: String,
    #[serde(default)]
    tab_label: String,
    #[serde(default)]
    tab_name: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    tab_number: Option<i64>,
    #[serde(default)]
    workspace_id: String,
    #[serde(default)]
    agent_status: String,
    #[serde(default)]
    agent: String,
    #[serde(default)]
    display_agent: String,
    #[serde(default)]
    cwd: String,
}

fn first_nonempty<'a>(values: impl IntoIterator<Item = &'a String>) -> &'a str {
    values
        .into_iter()
        .map(String::as_str)
        .find(|v| !v.is_empty())
        .unwrap_or("")
}

/// `event-hook` — parse `HERDR_PLUGIN_EVENT_JSON`, build the `agent_event`
/// payload, UDP-send to the relay's plugin port. Exit 0 even when no relay
/// listens (UDP is fire-and-forget by design).
pub fn event_hook() -> Result<(), HookError> {
    load_environment();
    let raw = env::var("HERDR_PLUGIN_EVENT_JSON").unwrap_or_else(|_| "{}".into());
    let envelope: EventEnvelope = serde_json::from_str(&raw)
        .map_err(|e| HookError::msg(format!("parse HERDR_PLUGIN_EVENT_JSON: {e}")))?;
    let data = envelope.data;

    let socket_path = default_socket_path();
    let absolute_socket = socket_path.canonicalize().unwrap_or(socket_path);
    let hostname = hostname_short();
    let payload = json!({
        "type": "agent_event",
        "socket_path": absolute_socket,
        "pane_id": data.pane_id,
        "tab_id": data.tab_id,
        "tab_label": first_nonempty([&data.tab_label, &data.tab_name, &data.label]),
        "tab_number": data.tab_number,
        "workspace_id": data.workspace_id,
        "status": data.agent_status.to_lowercase(),
        "agent": first_nonempty([&data.agent, &data.display_agent]).to_lowercase(),
        "project": Path::new(&data.cwd)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        "cwd": data.cwd,
        "host": hostname,
    });
    send_udp(payload)
}

/// `startup-hook` — fired by Herdr after session restore / live_handoff.
/// Sends a `startup` datagram so the running relay re-reads topology and
/// re-asserts its `agent.view.set` projection (transient, per-server state).
/// Exits 0 when no relay is running — the next `serve` boot asserts anyway.
pub fn startup_hook() -> Result<(), HookError> {
    load_environment();
    let socket_path = default_socket_path();
    let payload = json!({
        "type": "startup",
        "socket_path": socket_path.canonicalize().unwrap_or(socket_path),
        "host": hostname_short(),
    });
    match send_udp(payload) {
        Ok(()) => Ok(()),
        // ConnectionRefused = nobody listening — benign for a fire-and-
        // forget hook; only real socket errors propagate.
        Err(HookError::Io(e)) if e.kind() == io::ErrorKind::ConnectionRefused => Ok(()),
        Err(e) => Err(e),
    }
}

fn hostname_short() -> String {
    let host = gethostname::gethostname().to_string_lossy().into_owned();
    host.split('.').next().unwrap_or("").to_string()
}

/// `support` — print the relay's `support-state.json` written by `serve`.
/// Diagnostics shape mirrors `internal/support` — raw passthrough so the
/// running relay owns the schema.
pub fn support(runtime_dir: &Path) -> Result<(), HookError> {
    let path = runtime_dir.join("support-state.json");
    let data = fs::read_to_string(&path).map_err(|e| {
        HookError::msg(format!(
            "read {}: {e} (is the relay running?)",
            path.display()
        ))
    })?;
    println!("{data}");
    Ok(())
}

/// CLI error: usage → exit 2, runtime → exit 1. `HookError::msg` is always
/// runtime-class.
#[derive(Debug)]
pub enum HookError {
    Io(io::Error),
    Msg(String),
}

impl HookError {
    pub fn msg(text: impl Into<String>) -> Self {
        Self::Msg(text.into())
    }
}

impl fmt::Display for HookError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Msg(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for HookError {}

impl From<io::Error> for HookError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<url::ParseError> for HookError {
    fn from(e: url::ParseError) -> Self {
        Self::Msg(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_fragment_encodes() {
        assert_eq!(
            setup_fragment("tok en", "my phone", None),
            "setup=tok+en&label=my+phone"
        );
        assert_eq!(
            setup_fragment("t", "l", Some("https://x.example")),
            "setup=t&label=l&relay=https%3A%2F%2Fx.example"
        );
    }

    #[test]
    fn normalize_origin_adds_https() {
        assert_eq!(
            normalize_origin("example.ts.net", false).unwrap(),
            "https://example.ts.net"
        );
        assert_eq!(
            normalize_origin("https://Example.com:443/", false).unwrap(),
            "https://example.com"
        );
        assert_eq!(
            normalize_origin("https://example.com:8443", false).unwrap(),
            "https://example.com:8443"
        );
    }

    #[test]
    fn normalize_origin_rejects() {
        assert!(normalize_origin("https://u:p@x.com", false).is_err());
        assert!(normalize_origin("https://x.com/path", false).is_err());
        assert!(normalize_origin("http://x.com", false).is_err());
        assert!(normalize_origin("https://x.com?q=1", false).is_err());
    }

    #[test]
    fn normalize_origin_loopback_http() {
        assert_eq!(
            normalize_origin("http://localhost:8375", true).unwrap(),
            "http://localhost:8375"
        );
        assert_eq!(
            normalize_origin("http://127.0.0.1", true).unwrap(),
            "http://127.0.0.1"
        );
        assert!(normalize_origin("http://localhost", false).is_err());
    }

    #[test]
    fn qr_rejects_empty_and_oversized() {
        assert!(terminal_qr("", 80).is_err());
        assert!(terminal_qr("lerdr://pair#x", 0).is_ok());
        // A real link needs ~40 columns; a 10-column budget must refuse.
        assert!(terminal_qr("lerdr://pair#setup=abc123", 10).is_err());
    }
}
