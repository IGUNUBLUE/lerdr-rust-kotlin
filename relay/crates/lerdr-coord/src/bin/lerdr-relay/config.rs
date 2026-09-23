//! Relay configuration — `internal/config/config.go` ported for the serve
//! path.
//!
//! Resolution order mirrors the oracle: command-line flags win, then
//! `LERDR_<key>`, then the pre-rename `HERDR_<key>` spelling a service file
//! or operator shell may still set (`relayEnv`). Host-injected variables
//! (`HERDR_SOCKET_PATH`, `HERDR_PLUGIN_CONFIG_DIR`, `XDG_*`, `HOME`) are read
//! directly, never through the alias chain.
//!
//! [`resolve`] is pure: the environment and the directory probe come in as
//! parameters so tests never touch the process environment.

use std::path::{Path, PathBuf};

/// `relayEnvOr("RELAY_HOST", "127.0.0.1")`.
pub const DEFAULT_HOST: &str = "127.0.0.1";
/// `relayEnvIntOr("RELAY_PORT", 8375)`.
pub const DEFAULT_PORT: u16 = 8375;

/// The pairing link's app base. The oracle joins the fragment onto the
/// phone-app page origin (`<appOrigin>/#<fragment>`); this relay serves no
/// web app, so the Android deep link takes that place.
pub const DEEP_LINK_BASE: &str = "lerdr://pair";

/// Resolved relay configuration (`config.Config`, serve-path subset).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// `cfg.Host`.
    pub host: String,
    /// `cfg.Port`.
    pub port: u16,
    /// `cfg.Token` — the 32-byte relay key; also the bootstrap pairing secret.
    pub token: Option<String>,
    /// `cfg.SocketPath` — the Herdr API socket (`HERDR_SOCKET_PATH`).
    pub socket_path: PathBuf,
    /// `cfg.RuntimeDir` — pid file and relay.env's home.
    pub runtime_dir: PathBuf,
    /// `RuntimeDir/device-auth` — the `FileAuthStore` directory.
    pub device_auth_dir: PathBuf,
    /// `cfg.RearmBootstrap`.
    pub rearm_bootstrap: bool,
    /// `cfg.InstanceID` — `RELAY_INSTANCE_ID`, echoed back in
    /// `X-Herdr-Relay-Instance` and the healthz `instance` key.
    pub instance_id: String,
    /// `cfg.LogLevel` — validated `debug`/`info`/`warn`/`error`.
    pub log_level: Option<String>,
    /// The `ws(s)://` origin printed inside pairing links (`relay=` param).
    /// Defaults to `ws://<host>:<port>`; override where the reachable address
    /// differs from the bind address (Tailscale serve, port forwards).
    pub advertised_url: Option<String>,
}

/// Command-line overrides — a set field always wins over the environment.
#[derive(Debug, Default, Clone)]
pub struct Overrides {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub token: Option<String>,
    pub socket_path: Option<PathBuf>,
    pub runtime_dir: Option<PathBuf>,
    pub device_auth_dir: Option<PathBuf>,
    pub advertised_url: Option<String>,
    pub rearm_bootstrap: bool,
}

/// `config.Load`/`validate` failures — reported verbatim to stderr.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// `refusing to bind tokenless relay to non-loopback address %s`.
    #[error("refusing to bind tokenless relay to non-loopback address {0}")]
    TokenlessNonLoopback(String),
    /// `relay key must be exactly 32 bytes`.
    #[error("relay key must be exactly 32 bytes")]
    TokenLength,
    /// `invalid port %d`.
    #[error("invalid port {0}")]
    Port(i64),
    /// `invalid LERDR_RELAY_LOG_LEVEL %q: want debug, info, warn, or error` —
    /// the oracle's message names the new spelling even when `HERDR_` was read.
    #[error("invalid LERDR_RELAY_LOG_LEVEL {0:?}: want debug, info, warn, or error")]
    LogLevel(String),
}

/// Resolve the effective configuration.
///
/// - `vars` reads a raw process variable (`Some("")` counts as unset,
///   matching `relayEnv`/`envOr`).
/// - `dir_exists` answers `os.Stat`-is-dir for [`adopt_legacy_dir`].
/// - `cli` carries the `serve` flags; every set flag wins over env.
pub fn resolve(
    vars: &dyn Fn(&str) -> Option<String>,
    dir_exists: &dyn Fn(&Path) -> bool,
    cli: &Overrides,
) -> Result<Config, ConfigError> {
    let host = cli
        .host
        .clone()
        .or_else(|| relay_env(vars, "RELAY_HOST"))
        .unwrap_or_else(|| DEFAULT_HOST.to_owned());
    let port = cli
        .port
        .map(i64::from)
        .or_else(|| relay_env_int(vars, "RELAY_PORT"))
        .unwrap_or_else(|| i64::from(DEFAULT_PORT));
    let token = cli
        .token
        .clone()
        .or_else(|| relay_env(vars, "RELAY_TOKEN"))
        .filter(|t| !t.is_empty());
    let rearm_bootstrap =
        cli.rearm_bootstrap || relay_env_bool_or(vars, "RELAY_REARM_BOOTSTRAP", false);
    // `InstanceID: relayEnv("RELAY_INSTANCE_ID")` — env-only, no flag.
    let instance_id = relay_env(vars, "RELAY_INSTANCE_ID").unwrap_or_default();
    let log_level = match relay_env(vars, "RELAY_LOG_LEVEL") {
        Some(raw) => match raw.trim().to_lowercase().as_str() {
            "" | "info" => None,
            level @ ("debug" | "warn" | "error") => Some(level.to_owned()),
            _ => return Err(ConfigError::LogLevel(raw)),
        },
        None => None,
    };

    let config_home = env(vars, "XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir(vars).join(".config"));

    // `resolveRuntimeDir` — RELAY_ENV's directory, then the Herdr plugin
    // config dir, then the renamed config dir with legacy adoption.
    let runtime_dir = cli
        .runtime_dir
        .clone()
        .or_else(|| relay_env(vars, "RELAY_ENV").map(|p| dirname(&p)))
        .or_else(|| env(vars, "HERDR_PLUGIN_CONFIG_DIR").map(PathBuf::from))
        .unwrap_or_else(|| {
            adopt_legacy_dir(
                config_home.join("lerdr"),
                config_home.join("herdr-mobile-relay"),
                dir_exists,
            )
        });

    let device_auth_dir = cli
        .device_auth_dir
        .clone()
        .or_else(|| relay_env(vars, "RELAY_DEVICE_AUTH_DIR").map(PathBuf::from))
        .unwrap_or_else(|| runtime_dir.join("device-auth"));

    let socket_path = cli
        .socket_path
        .clone()
        .or_else(|| env(vars, "HERDR_SOCKET_PATH").map(PathBuf::from))
        .unwrap_or_else(|| config_home.join("herdr").join("herdr.sock"));

    let advertised_url = cli
        .advertised_url
        .clone()
        .or_else(|| relay_env(vars, "RELAY_URL"))
        .filter(|u| !u.is_empty());

    // `validate`: tokenless relays bind loopback only, and a configured key
    // is exactly 32 bytes (it doubles as the bootstrap secret).
    if token.is_none() && !matches!(host.as_str(), "127.0.0.1" | "::1" | "localhost") {
        return Err(ConfigError::TokenlessNonLoopback(host));
    }
    if token.as_ref().is_some_and(|t| t.len() != 32) {
        return Err(ConfigError::TokenLength);
    }
    if !(1..=65535).contains(&port) {
        return Err(ConfigError::Port(port));
    }

    Ok(Config {
        host,
        port: port as u16,
        token,
        socket_path,
        runtime_dir,
        device_auth_dir,
        rearm_bootstrap,
        instance_id,
        log_level,
        advertised_url,
    })
}

impl Config {
    /// The socket URL pairing links carry (`relay=`): the advertised override
    /// when set, else `ws://` on the bind address — an unspecified bind
    /// (`0.0.0.0`/`::`) advertises loopback rather than an unreachable host.
    pub fn socket_url(&self) -> String {
        if let Some(url) = &self.advertised_url {
            return url.clone();
        }
        let host = match self.host.as_str() {
            "0.0.0.0" => "127.0.0.1",
            "::" => "::1",
            host => host,
        };
        let host = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host.to_owned()
        };
        format!("ws://{host}:{}", self.port)
    }
}

/// `relayEnv` — `LERDR_<key>` first, then `HERDR_<key>`; empty counts as unset.
fn relay_env(env: &dyn Fn(&str) -> Option<String>, key: &str) -> Option<String> {
    env(&format!("LERDR_{key}"))
        .filter(|v| !v.is_empty())
        .or_else(|| env(&format!("HERDR_{key}")).filter(|v| !v.is_empty()))
}

/// A directly-read host variable (`envOr`): empty counts as unset.
fn env(get: &dyn Fn(&str) -> Option<String>, key: &str) -> Option<String> {
    get(key).filter(|v| !v.is_empty())
}

/// `relayEnvIntOr` — a non-numeric value falls back silently, like the oracle.
fn relay_env_int(env: &dyn Fn(&str) -> Option<String>, key: &str) -> Option<i64> {
    relay_env(env, key).and_then(|v| v.parse::<i64>().ok())
}

/// `relayEnvBoolOr` — `strconv.ParseBool` accepts 1/t/T/TRUE/true/True and
/// 0/f/F/FALSE/false/False; anything else falls back.
fn relay_env_bool_or(env: &dyn Fn(&str) -> Option<String>, key: &str, fallback: bool) -> bool {
    relay_env(env, key)
        .and_then(|v| match v.as_str() {
            "1" | "t" | "T" | "true" | "TRUE" | "True" => Some(true),
            "0" | "f" | "F" | "false" | "FALSE" | "False" => Some(false),
            _ => None,
        })
        .unwrap_or(fallback)
}

/// `filepath.Dir` — a bare filename resolves to `.`.
fn dirname(path: &str) -> PathBuf {
    let parent = Path::new(path).parent().unwrap_or_else(|| Path::new(""));
    if parent.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        parent.to_path_buf()
    }
}

/// `adoptLegacyDir` — the renamed dir wins when present, a pre-rename install
/// stays reachable, and a fresh install lands on the new name.
fn adopt_legacy_dir(dir: PathBuf, legacy: PathBuf, dir_exists: &dyn Fn(&Path) -> bool) -> PathBuf {
    if dir_exists(&dir) || !dir_exists(&legacy) {
        dir
    } else {
        legacy
    }
}

/// `homeDir` — `$HOME`, `/tmp` when the platform can't answer.
fn home_dir(env: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    env("HOME").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Injected environment: `HOME` is set to a stable value in every test so
    /// results don't depend on the machine running them.
    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key| map.get(key).cloned()
    }

    fn no_dirs() -> impl Fn(&Path) -> bool {
        |_| false
    }

    fn resolve_env(vars: &[(&str, &str)]) -> Result<Config, ConfigError> {
        resolve(&env(vars), &no_dirs(), &Overrides::default())
    }

    #[test]
    fn defaults_match_oracle() {
        let cfg = resolve_env(&[("HOME", "/home/op")]).unwrap();
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.port, 8375);
        assert_eq!(cfg.token, None);
        // Fresh install lands on the renamed dir (`adoptLegacyDir` default).
        assert_eq!(cfg.runtime_dir, Path::new("/home/op/.config/lerdr"));
        assert_eq!(
            cfg.device_auth_dir,
            Path::new("/home/op/.config/lerdr/device-auth")
        );
        assert_eq!(
            cfg.socket_path,
            Path::new("/home/op/.config/herdr/herdr.sock")
        );
        assert_eq!(cfg.socket_url(), "ws://127.0.0.1:8375");
    }

    #[test]
    fn legacy_runtime_dir_is_adopted() {
        let vars = env(&[("HOME", "/home/op")]);
        let dir_exists = |p: &Path| p == Path::new("/home/op/.config/herdr-mobile-relay");
        let cfg = resolve(&vars, &dir_exists, &Overrides::default()).unwrap();
        // Dev machines that ran the Go relay keep one state directory.
        assert_eq!(
            cfg.device_auth_dir,
            Path::new("/home/op/.config/herdr-mobile-relay/device-auth")
        );
    }

    #[test]
    fn lerdr_spelling_beats_herdr() {
        let cfg = resolve_env(&[
            ("HOME", "/h"),
            ("LERDR_RELAY_PORT", "9001"),
            ("HERDR_RELAY_PORT", "9002"),
            ("HERDR_RELAY_HOST", "localhost"),
        ])
        .unwrap();
        assert_eq!(cfg.port, 9001);
        assert_eq!(cfg.host, "localhost");
    }

    #[test]
    fn empty_lerdr_falls_through_to_herdr() {
        let cfg = resolve_env(&[
            ("HOME", "/h"),
            ("LERDR_RELAY_PORT", ""),
            ("HERDR_RELAY_PORT", "9100"),
        ])
        .unwrap();
        assert_eq!(cfg.port, 9100);
    }

    #[test]
    fn cli_overrides_env() {
        let vars = env(&[("HOME", "/h"), ("LERDR_RELAY_PORT", "9001")]);
        let cli = Overrides {
            host: Some("127.0.0.1".into()),
            port: Some(8443),
            device_auth_dir: Some(PathBuf::from("/tmp/devices")),
            ..Overrides::default()
        };
        let cfg = resolve(&vars, &no_dirs(), &cli).unwrap();
        assert_eq!(cfg.port, 8443);
        assert_eq!(cfg.device_auth_dir, Path::new("/tmp/devices"));
    }

    #[test]
    fn instance_id_is_env_only() {
        // `InstanceID: relayEnv("RELAY_INSTANCE_ID")` — no flag, LERDR
        // beats HERDR, empty/unset resolves to "".
        let cfg = resolve_env(&[
            ("HOME", "/h"),
            ("LERDR_RELAY_INSTANCE_ID", "lerdr-1"),
            ("HERDR_RELAY_INSTANCE_ID", "herdr-1"),
        ])
        .unwrap();
        assert_eq!(cfg.instance_id, "lerdr-1");
        let cfg = resolve_env(&[("HOME", "/h"), ("HERDR_RELAY_INSTANCE_ID", "herdr-1")]).unwrap();
        assert_eq!(cfg.instance_id, "herdr-1");
        let cfg = resolve_env(&[("HOME", "/h")]).unwrap();
        assert_eq!(cfg.instance_id, "");
    }

    #[test]
    fn tokenless_refuses_non_loopback() {
        let err = resolve_env(&[("HOME", "/h"), ("LERDR_RELAY_HOST", "0.0.0.0")]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "refusing to bind tokenless relay to non-loopback address 0.0.0.0"
        );
    }

    #[test]
    fn token_unlocks_non_loopback() {
        let cfg = resolve_env(&[
            ("HOME", "/h"),
            ("LERDR_RELAY_HOST", "0.0.0.0"),
            ("LERDR_RELAY_TOKEN", &"k".repeat(32)),
        ])
        .unwrap();
        assert_eq!(cfg.host, "0.0.0.0");
        // A wildcard bind advertises loopback — an unspecified address is
        // unreachable from a phone anyway.
        assert_eq!(cfg.socket_url(), "ws://127.0.0.1:8375");
    }

    #[test]
    fn token_must_be_32_bytes() {
        let err = resolve_env(&[("HOME", "/h"), ("LERDR_RELAY_TOKEN", "short")]).unwrap_err();
        assert_eq!(err.to_string(), "relay key must be exactly 32 bytes");
    }

    #[test]
    fn env_port_out_of_range_fails_but_garbage_defaults() {
        let err = resolve_env(&[("HOME", "/h"), ("LERDR_RELAY_PORT", "70000")]).unwrap_err();
        assert_eq!(err.to_string(), "invalid port 70000");
        // Non-numeric values fall back silently — `relayEnvIntOr`.
        let cfg = resolve_env(&[("HOME", "/h"), ("LERDR_RELAY_PORT", "abc")]).unwrap();
        assert_eq!(cfg.port, 8375);
    }

    #[test]
    fn relay_env_dirname_wins_for_runtime_dir() {
        let cfg = resolve_env(&[
            ("HOME", "/h"),
            ("HERDR_PLUGIN_CONFIG_DIR", "/plugin"),
            ("LERDR_RELAY_ENV", "/service/relay.env"),
        ])
        .unwrap();
        assert_eq!(cfg.runtime_dir, Path::new("/service"));
    }

    #[test]
    fn plugin_config_dir_beats_adopted_default() {
        let cfg = resolve_env(&[("HOME", "/h"), ("HERDR_PLUGIN_CONFIG_DIR", "/plugin")]).unwrap();
        assert_eq!(cfg.runtime_dir, Path::new("/plugin"));
    }

    #[test]
    fn log_level_validated() {
        let cfg = resolve_env(&[("HOME", "/h"), ("LERDR_RELAY_LOG_LEVEL", "DEBUG")]).unwrap();
        assert_eq!(cfg.log_level.as_deref(), Some("debug"));
        let err = resolve_env(&[("HOME", "/h"), ("LERDR_RELAY_LOG_LEVEL", "trace")]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid LERDR_RELAY_LOG_LEVEL \"trace\": want debug, info, warn, or error"
        );
    }

    #[test]
    fn rearm_bootstrap_parses_go_bool() {
        let cfg = resolve_env(&[("HOME", "/h"), ("LERDR_RELAY_REARM_BOOTSTRAP", "1")]).unwrap();
        assert!(cfg.rearm_bootstrap);
        let cfg = resolve_env(&[("HOME", "/h"), ("LERDR_RELAY_REARM_BOOTSTRAP", "False")]).unwrap();
        assert!(!cfg.rearm_bootstrap);
    }

    #[test]
    fn socket_path_from_host_env() {
        let cfg = resolve_env(&[("HOME", "/h"), ("HERDR_SOCKET_PATH", "/run/herdr.sock")]).unwrap();
        assert_eq!(cfg.socket_path, Path::new("/run/herdr.sock"));
    }
}
