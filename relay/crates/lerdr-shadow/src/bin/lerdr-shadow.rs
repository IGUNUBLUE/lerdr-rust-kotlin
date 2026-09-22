//! `lerdr-shadow` — shadow-diff client + trace differ.
//!
//! ```text
//! lerdr-shadow run  --url ws://127.0.0.1:8375/ws --token <32-byte key>
//!                   --scenario tools/shadow/scenarios/core.json
//!                   --trace run/rust.jsonl --side rust
//!                   [--auth-id bootstrap] [--auth-version 1] [--locale en]
//!                   [--handshake-timeout-ms 10000] [--drain-ms 600]
//!
//! lerdr-shadow diff --a run/go.jsonl --b run/rust.jsonl
//!                   [--config scenario.json] [--out report.txt]
//! ```
//!
//! `run` exits nonzero on connect/handshake/`expect` failure — a missed
//! `until` is recorded in the trace, not fatal. `diff` exits 0 when the
//! normalized streams are identical, 1 when they differ.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use lerdr_shadow::client::{self, BOOTSTRAP_INVITATION_ID, BOOTSTRAP_LOCALE};
use lerdr_shadow::compare::diff_traces;
use lerdr_shadow::scenario::{CompareConfig, Scenario};
use lerdr_shadow::Result;

fn usage() -> ! {
    eprintln!(
        "usage:\n  \
         lerdr-shadow run --url URL --token KEY --scenario FILE --trace FILE --side NAME \\\n         \
             [--auth-id ID] [--auth-version N] [--locale LOCALE] \\\n         \
             [--handshake-timeout-ms N] [--drain-ms N]\n  \
         lerdr-shadow diff --a FILE --b FILE [--config FILE] [--out FILE]"
    );
    std::process::exit(2);
}

/// Minimal `--flag value` parser — the harness has two verbs; clap is
/// overkill here.
struct Args {
    args: std::collections::VecDeque<String>,
}

impl Args {
    fn parse() -> Self {
        Self {
            args: std::env::args().skip(1).collect(),
        }
    }

    fn next_flag(&mut self) -> Option<String> {
        self.args.pop_front()
    }

    /// `--name value` or `--name=value`.
    fn take(&mut self, name: &str) -> Option<String> {
        let mut idx = None;
        for (i, arg) in self.args.iter().enumerate() {
            if arg == name || arg.starts_with(&format!("{name}=")) {
                idx = Some(i);
                break;
            }
        }
        let i = idx?;
        let arg = self.args.remove(i).unwrap();
        if let Some((_, v)) = arg.split_once('=') {
            Some(v.to_owned())
        } else {
            self.args.remove(i)
        }
    }
}

fn main() -> ExitCode {
    let mut args = Args::parse();
    let Some(cmd) = args.next_flag() else {
        usage();
    };
    let result = match cmd.as_str() {
        "run" => cmd_run(&mut args),
        "diff" => cmd_diff(&mut args),
        _ => {
            usage();
        }
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("lerdr-shadow: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_run(args: &mut Args) -> Result<ExitCode> {
    let url = args.take("--url");
    let token = args.take("--token");
    let scenario = args.take("--scenario");
    let trace = args.take("--trace");
    let side = args.take("--side").unwrap_or_else(|| "side".to_owned());
    let auth_id = args
        .take("--auth-id")
        .unwrap_or_else(|| BOOTSTRAP_INVITATION_ID.to_owned());
    let auth_version: u64 = args
        .take("--auth-version")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let locale = args
        .take("--locale")
        .unwrap_or_else(|| BOOTSTRAP_LOCALE.to_owned());
    let handshake_timeout_ms: u64 = args
        .take("--handshake-timeout-ms")
        .and_then(|v| v.parse().ok())
        .unwrap_or(10_000);
    let drain_ms: u64 = args
        .take("--drain-ms")
        .and_then(|v| v.parse().ok())
        .unwrap_or(600);
    let (Some(url), Some(token), Some(scenario), Some(trace)) = (url, token, scenario, trace)
    else {
        usage();
    };
    let token_bytes = token.as_bytes();
    if token_bytes.len() != 32 {
        return Err(lerdr_shadow::ShadowError::msg(
            "--token must be exactly 32 bytes (the relay key)",
        ));
    }
    let mut token = [0u8; 32];
    token.copy_from_slice(token_bytes);
    let scenario = Scenario::load(&PathBuf::from(scenario))?;
    let params = client::RunParams {
        url,
        token,
        auth_id,
        auth_version,
        locale,
        scenario,
        side,
        trace_path: PathBuf::from(trace),
        handshake_timeout: Duration::from_millis(handshake_timeout_ms),
        drain_ms,
    };
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(client::run(&params))?;
    Ok(ExitCode::SUCCESS)
}

fn cmd_diff(args: &mut Args) -> Result<ExitCode> {
    let a = args.take("--a");
    let b = args.take("--b");
    let config = args.take("--config");
    let out = args.take("--out");
    let (Some(a), Some(b)) = (a, b) else {
        usage();
    };
    let override_cfg: Option<CompareConfig> = match config {
        Some(path) => {
            let raw = std::fs::read_to_string(&path)?;
            // The file may be a full scenario (use its `compare` section) or
            // a bare compare object.
            #[derive(serde::Deserialize)]
            struct Wrapper {
                #[serde(default)]
                compare: Option<CompareConfig>,
            }
            let parsed: Wrapper = serde_json::from_str(&raw)?;
            match parsed.compare {
                Some(c) => Some(c),
                None => Some(serde_json::from_str(&raw)?),
            }
        }
        None => None,
    };
    let report = diff_traces(&PathBuf::from(a), &PathBuf::from(b), override_cfg.as_ref())?;
    match &out {
        Some(path) => std::fs::write(path, &report.text)?,
        None => print!("{}", report.text),
    }
    if let Some(path) = &out {
        println!("report written to {path}");
    }
    Ok(if report.identical {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
