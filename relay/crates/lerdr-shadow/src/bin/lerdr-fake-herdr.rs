//! `lerdr-fake-herdr` — static Herdr socket-API endpoint for shadow runs.
//!
//! ```text
//! lerdr-fake-herdr --socket PATH --state FILE [--ops-log FILE]
//! ```
//!
//! See `lerdr_shadow::fake` for the covered methods and the state-file
//! schema (the oracle fake-herdr's `Scenario` plus a `"socket"` block).

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut socket = None;
    let mut state = None;
    let mut ops_log: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => socket = args.next().map(PathBuf::from),
            "--state" => state = args.next().map(PathBuf::from),
            "--ops-log" => ops_log = args.next().map(PathBuf::from),
            other => {
                eprintln!("lerdr-fake-herdr: unknown flag {other}");
                eprintln!("usage: lerdr-fake-herdr --socket PATH --state FILE [--ops-log FILE]");
                return ExitCode::from(2);
            }
        }
    }
    let (Some(socket), Some(state)) = (socket, state) else {
        eprintln!("usage: lerdr-fake-herdr --socket PATH --state FILE [--ops-log FILE]");
        return ExitCode::from(2);
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("lerdr-fake-herdr: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(lerdr_shadow::fake::serve(&socket, &state, ops_log)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("lerdr-fake-herdr: {e:#}");
            ExitCode::FAILURE
        }
    }
}
