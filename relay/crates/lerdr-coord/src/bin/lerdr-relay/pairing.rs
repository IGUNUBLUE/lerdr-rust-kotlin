//! Pair through the installed user service without opening its auth store for writes.

use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use clap::Parser;
use lerdr_relay::store::{Invitation, STORE_FILENAME};
use serde::Deserialize;
use serde_json::Value;

use super::{bootstrap, config, BoxError};
#[cfg(target_os = "linux")]
use super::{Cli, Commands};

type Result<T, E = BoxError> = std::result::Result<T, E>;

fn output(program: &str, args: &[&str]) -> Result<String> {
    let result = Command::new(program).args(args).output()?;
    if !result.status.success() {
        return Err(format!(
            "{program} failed: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8(result.stdout)?)
}

fn service_pid() -> Result<u32> {
    #[cfg(target_os = "linux")]
    let text = output(
        "systemctl",
        &[
            "--user",
            "show",
            "lerdr.service",
            "--property=MainPID",
            "--value",
        ],
    )?;
    #[cfg(target_os = "macos")]
    let text = {
        let domain = format!("gui/{}/com.lerdr.service", output("id", &["-u"])?.trim());
        let state = output("launchctl", &["print", &domain])?;
        state
            .lines()
            .find_map(|line| line.trim().strip_prefix("pid = "))
            .unwrap_or("0")
            .to_owned()
    };
    let pid = text.trim().parse::<u32>()?;
    if pid == 0 {
        return Err("the installed Lerdr user service is not running".into());
    }
    Ok(pid)
}

fn running_config(pid: u32) -> Result<config::Config> {
    #[cfg(target_os = "linux")]
    let (bytes, overrides) = {
        let bytes = std::fs::read(format!("/proc/{pid}/environ"))?;
        let args = std::fs::read(format!("/proc/{pid}/cmdline"))?;
        let args = args
            .split(|b| *b == 0)
            .filter(|arg| !arg.is_empty())
            .map(std::ffi::OsStr::from_bytes);
        let cli = Cli::try_parse_from(args)?;
        let overrides = match cli.command {
            Some(Commands::Serve(args)) => config::Overrides::from(&args),
            None => config::Overrides::default(),
            _ => return Err("the installed service is not running the relay serve command".into()),
        };
        (bytes, overrides)
    };
    #[cfg(target_os = "macos")]
    let (bytes, overrides) = {
        // The shipped LaunchAgent wrapper sources this trusted operator-owned file.
        let plist = std::path::PathBuf::from(std::env::var("HOME")?)
            .join("Library/LaunchAgents/com.lerdr.service.plist");
        let path = output(
            "/usr/libexec/PlistBuddy",
            &[
                "-c",
                "Print :EnvironmentVariables:LERDR_RELAY_ENV",
                plist.to_str().ok_or("invalid plist path")?,
            ],
        )?;
        let result = Command::new("bash")
            .env_clear()
            .env("HOME", std::env::var("HOME")?)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .args([
                "-c",
                "set -ae; export LERDR_RELAY_ENV=\"$1\"; . \"$1\"; while IFS= read -r key; do printf '%s=%s\\0' \"$key\" \"${!key}\"; done < <(compgen -e)",
                "pairing",
                path.trim(),
            ])
            .output()?;
        if !result.status.success() {
            return Err("could not load the installed LaunchAgent relay environment".into());
        }
        let _ = pid;
        (result.stdout, config::Overrides::default())
    };
    let vars: HashMap<_, _> = bytes
        .split(|b| *b == 0)
        .filter_map(|entry| std::str::from_utf8(entry).ok()?.split_once('='))
        .collect();
    let mut cfg = config::resolve(
        &|key| vars.get(key).map(|value| (*value).to_owned()),
        &Path::is_dir,
        &overrides,
    )?;
    if cfg.device_auth_dir.is_relative() {
        #[cfg(target_os = "linux")]
        let cwd = std::fs::read_link(format!("/proc/{pid}/cwd"))?;
        #[cfg(target_os = "macos")]
        let cwd = {
            let plist = std::path::PathBuf::from(std::env::var("HOME")?)
                .join("Library/LaunchAgents/com.lerdr.service.plist");
            std::path::PathBuf::from(
                output(
                    "/usr/libexec/PlistBuddy",
                    &[
                        "-c",
                        "Print :WorkingDirectory",
                        plist.to_str().ok_or("invalid plist path")?,
                    ],
                )?
                .trim(),
            )
        };
        cfg.device_auth_dir = cwd.join(&cfg.device_auth_dir);
    }
    Ok(cfg)
}

#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStrExt;

fn loopback_target(target: &str, port: u16) -> bool {
    target.rsplit_once(':').is_some_and(|(host, published)| {
        matches!(host, "localhost" | "127.0.0.1" | "[::1]") && published.parse::<u16>() == Ok(port)
    })
}

fn endpoint(status: &Value, serve: &Value, port: u16) -> Result<String> {
    let host = status["Self"]["DNSName"]
        .as_str()
        .map(|s| s.trim_end_matches('.'))
        .filter(|s| !s.is_empty())
        .ok_or("Tailscale has no DNS name for this node")?;
    let tcp = serve["TCP"]
        .as_object()
        .ok_or("Tailscale Serve has no published relay route")?;
    // Prefer an actual HTTPS root proxy, not an assumed port-443 listener.
    if let Some(web) = serve["Web"].as_object() {
        for (authority, routes) in web {
            let Some((name, published_port)) = authority.rsplit_once(':') else {
                continue;
            };
            if name != host
                || tcp
                    .get(published_port)
                    .is_none_or(|route| route["HTTPS"] != true)
            {
                continue;
            }
            let Some(proxy) = routes["Handlers"]["/"]["Proxy"].as_str() else {
                continue;
            };
            if !routes["Handlers"]["/ws"].is_null() || !routes["Handlers"]["/ws/"].is_null() {
                continue;
            }
            if proxy.strip_prefix("http://").is_some_and(|target| {
                loopback_target(target.strip_suffix('/').unwrap_or(target), port)
            }) {
                return Ok(format!("wss://{authority}"));
            }
        }
    }
    let ports: Vec<_> = tcp
        .iter()
        .filter_map(|(published, route)| {
            let published = published.parse::<u16>().ok().filter(|p| *p != 0)?;
            route["TCPForward"]
                .as_str()
                .filter(|target| loopback_target(target, port))
                .map(|_| published)
        })
        .collect();
    match ports.as_slice() {
        [published] => Ok(format!("ws://{host}:{published}")),
        [] => Err("Tailscale Serve has no route to the installed relay port".into()),
        _ => {
            Err("multiple Tailscale TCP routes target the relay; keep one unambiguous route".into())
        }
    }
}

#[derive(Deserialize)]
struct PairingState {
    invitation: Option<Invitation>,
}

fn invitation(path: &Path) -> Result<Option<Invitation>> {
    let state: PairingState = serde_json::from_reader(BufReader::new(File::open(path)?))?;
    Ok(state.invitation)
}

fn fresh(current: &Invitation, previous: Option<&Invitation>, now: i64) -> bool {
    current.expires_at_ms > now
        && current.pending_credential_id.is_empty()
        && previous.is_none_or(|old| {
            current.expires_at_ms != old.expires_at_ms
                || current.invitation_id != old.invitation_id
                || current.version != old.version
                || current.secret != old.secret
        })
}

pub fn qr() -> Result<()> {
    let pid = service_pid()?;
    let cfg = running_config(pid)?;
    let status = serde_json::from_str(&output("tailscale", &["status", "--json"])?)?;
    let serve = serde_json::from_str(&output("tailscale", &["serve", "status", "--json"])?)?;
    let relay_url = endpoint(&status, &serve, cfg.port)?;
    let path = cfg.device_auth_dir.join(STORE_FILENAME);
    let previous = invitation(&path)?;
    #[cfg(target_os = "linux")]
    output(
        "systemctl",
        &[
            "--user",
            "kill",
            "--kill-whom=main",
            "--signal=USR1",
            "lerdr.service",
        ],
    )?;
    #[cfg(target_os = "macos")]
    {
        let domain = format!("gui/{}/com.lerdr.service", output("id", &["-u"])?.trim());
        output("launchctl", &["kill", "SIGUSR1", &domain])?;
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(current) = invitation(&path)? {
            if fresh(&current, previous.as_ref(), bootstrap::now_ms()) {
                if service_pid()? != pid {
                    return Err(
                        "the relay restarted while generating the invitation; run pairing qr again"
                            .into(),
                    );
                }
                let offer = bootstrap::offer_for_invitation(&current, &current.name, &relay_url)
                    .ok_or("the armed invitation cannot produce a pairing link")?;
                bootstrap::print_setup_link(&offer.deep_link());
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            return Err("the relay did not persist a fresh invitation within three seconds".into());
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node() -> Value {
        json!({"Self": {"DNSName": "relay.example.ts.net."}})
    }

    #[test]
    fn tcp_uses_published_port_and_rejects_unrelated_backends() {
        let routes = json!({"TCP": {"9000": {"TCPForward": "localhost:8377"}, "443": {"TCPForward": "localhost:8000"}}});
        assert_eq!(
            endpoint(&node(), &routes, 8377).unwrap(),
            "ws://relay.example.ts.net:9000"
        );
        assert!(endpoint(&node(), &routes, 8375).is_err());
    }

    #[test]
    fn genuine_https_root_proxy_takes_precedence_over_tcp() {
        let routes = json!({"TCP": {"443": {"HTTPS": true}, "8377": {"TCPForward": "127.0.0.1:8377"}}, "Web": {"relay.example.ts.net:443": {"Handlers": {"/": {"Proxy": "http://localhost:8377/"}}}}});
        assert_eq!(
            endpoint(&node(), &routes, 8377).unwrap(),
            "wss://relay.example.ts.net:443"
        );
    }

    #[test]
    fn unrelated_https_and_path_mounts_do_not_hide_tcp() {
        for authority in ["other.example.ts.net:443", "relay.example.ts.net:443"] {
            let routes = json!({"TCP": {"443": {"HTTPS": true}, "8377": {"TCPForward": "[::1]:8377"}}, "Web": {authority: {"Handlers": {"/mounted/": {"Proxy": "http://localhost:8377"}}}}});
            assert_eq!(
                endpoint(&node(), &routes, 8377).unwrap(),
                "ws://relay.example.ts.net:8377"
            );
        }
    }
    #[test]
    fn https_requires_its_listener_and_the_relay_backend() {
        for tcp in [json!({}), json!({"443": {"HTTPS": true}})] {
            let routes = json!({"TCP": tcp, "Web": {"relay.example.ts.net:443": {"Handlers": {"/": {"Proxy": "http://localhost:8000"}}}}});
            assert!(endpoint(&node(), &routes, 8377).is_err());
        }
        let routes = json!({"TCP": {}, "Web": {"relay.example.ts.net:443": {"Handlers": {"/": {"Proxy": "http://localhost:8377"}}}}});
        assert!(endpoint(&node(), &routes, 8377).is_err());
        let routes = json!({"TCP": {"443": {"HTTPS": true}}, "Web": {"relay.example.ts.net:443": {"Handlers": {"/": {"Proxy": "http://localhost:8377////"}}}}});
        assert!(endpoint(&node(), &routes, 8377).is_err());
    }

    #[test]
    fn root_proxy_cannot_advertise_an_overridden_websocket_path() {
        for path in ["/ws", "/ws/"] {
            let routes = json!({"TCP": {"443": {"HTTPS": true}}, "Web": {"relay.example.ts.net:443": {"Handlers": {"/": {"Proxy": "http://localhost:8377"}, path: {"Proxy": "http://localhost:8000"}}}}});
            assert!(endpoint(&node(), &routes, 8377).is_err());
        }
    }

    #[test]
    fn ambiguous_tcp_and_missing_node_dns_are_errors() {
        let routes = json!({"TCP": {"8377": {"TCPForward": "localhost:8377"}, "9000": {"TCPForward": "localhost:8377"}}});
        assert!(endpoint(&node(), &routes, 8377).is_err());
        assert!(endpoint(&json!({"Self": {}}), &routes, 8377).is_err());
    }

    #[test]
    fn attempts_are_not_a_rearm_and_redeemed_or_expired_offers_are_not_printed() {
        let old: Invitation = serde_json::from_value(json!({
            "invitation_id": "test-invite", "version": 1, "secret": "test-secret",
            "expires_at_ms": 1000, "role": "controller"
        }))
        .unwrap();
        let mut current = old.clone();
        current.failed_attempts = 1;
        assert!(!fresh(&current, Some(&old), 100));
        current.expires_at_ms = 1100;
        assert!(fresh(&current, Some(&old), 100));
        assert!(!fresh(&current, Some(&old), 1100));
        current.pending_credential_id = "test-credential".into();
        assert!(!fresh(&current, Some(&old), 100));
    }
}
