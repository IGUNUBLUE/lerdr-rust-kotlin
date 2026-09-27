//! `terminal session observe` — the live ANSI frame stream behind the
//! read-only attach. Herdr renders the pane's terminal onto a surface of the
//! requested geometry (default: the pane's real size — the host pane is
//! never touched either way) and pushes NDJSON `terminal.frame` records:
//! `seq` monotonic, `full` marks a complete re-render, `bytes` is
//! base64-encoded ANSI, `width`/`height` the surface geometry. Pane exit
//! arrives as a `terminal.closed` record; a stalled consumer may be dropped
//! after ~30 s without it (EOF still terminates the stream).
//!
//! The relay consumes frames into a `vt100` screen and re-renders the
//! surface as the same ANSI text `pane.read` would have produced — the
//! fingerprint/delta/ack machinery downstream is unchanged.

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdout, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// One decoded `terminal.frame` record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalFrame {
    pub seq: u64,
    /// `full` = a complete re-render (first frame, resyncs).
    pub full: bool,
    /// Decoded ANSI bytes.
    pub bytes: Vec<u8>,
    /// Surface geometry the bytes were rendered at.
    pub width: u16,
    pub height: u16,
}

/// What the observe stream yielded next.
#[derive(Debug)]
pub enum ObserveEvent {
    /// A `terminal.frame` record.
    Frame(TerminalFrame),
    /// `terminal.closed` — the pane is gone.
    Closed,
    /// The child exited or the stream broke — caller decides whether to
    /// fall back to snapshot reads.
    Ended,
}

#[derive(Deserialize)]
struct WireRecord {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    seq: u64,
    #[serde(default)]
    full: bool,
    #[serde(default)]
    bytes: String,
    #[serde(default)]
    width: u16,
    #[serde(default)]
    height: u16,
}

/// A spawned `herdr terminal session observe` child yielding decoded frames.
pub struct ObserveStream {
    child: Option<Child>,
    rx: mpsc::UnboundedReceiver<ObserveEvent>,
    pump: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for ObserveStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObserveStream").finish_non_exhaustive()
    }
}

impl ObserveStream {
    /// Spawn `herdr terminal session observe <pane>` against `socket_path`,
    /// invoking `bin` (the client's resolved `herdr_bin`). `None` when the
    /// CLI can't be spawned (missing binary, permissions) — callers fall
    /// back to snapshot reads.
    pub fn spawn(
        bin: &std::path::Path,
        pane_id: &str,
        socket_path: &std::path::Path,
    ) -> Option<Self> {
        Self::spawn_sized(bin, pane_id, socket_path, None, None)
    }

    /// `observe --cols --rows` — a virtual re-render at phone geometry, or
    /// the pane's real geometry when both are `None`.
    pub fn spawn_sized(
        bin: &std::path::Path,
        pane_id: &str,
        socket_path: &std::path::Path,
        cols: Option<u16>,
        rows: Option<u16>,
    ) -> Option<Self> {
        let mut command = Command::new(bin);
        command
            .arg("terminal")
            .arg("session")
            .arg("observe")
            .arg(pane_id)
            .env("HERDR_SOCKET_PATH", socket_path)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            // The observer is detached from the relay's lifetime — kill it
            // when the watch drops rather than leaving orphans in herdr.
            .kill_on_drop(true);
        if let Some(cols) = cols {
            command.arg("--cols").arg(cols.to_string());
        }
        if let Some(rows) = rows {
            command.arg("--rows").arg(rows.to_string());
        }
        let mut child = command.spawn().ok()?;
        let stdout = child.stdout.take()?;
        let (tx, rx) = mpsc::unbounded_channel();
        let pump = tokio::spawn(pump_frames(stdout, tx));
        Some(ObserveStream {
            child: Some(child),
            rx,
            pump: Some(pump),
        })
    }

    /// Next event from the stream; `Ended` once the child/pipe is done.
    pub async fn next(&mut self) -> ObserveEvent {
        self.rx.recv().await.unwrap_or(ObserveEvent::Ended)
    }
}

impl Drop for ObserveStream {
    fn drop(&mut self) {
        if let Some(pump) = self.pump.take() {
            pump.abort();
        }
        if let Some(mut child) = self.child.take() {
            // `kill_on_drop` covers it; `start_kill` reaps without a wait.
            let _ = child.start_kill();
        }
    }
}

/// Read NDJSON lines off the child's stdout and forward decoded records.
async fn pump_frames(stdout: ChildStdout, tx: mpsc::UnboundedSender<ObserveEvent>) {
    let mut lines = BufReader::new(stdout).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                let Some(event) = decode_record(&line) else {
                    continue;
                };
                if tx.send(event).is_err() {
                    return;
                }
            }
            // EOF or I/O error — the stream is over either way.
            _ => {
                let _ = tx.send(ObserveEvent::Ended);
                return;
            }
        }
    }
}

fn decode_record(line: &str) -> Option<ObserveEvent> {
    let record: WireRecord = serde_json::from_str(line).ok()?;
    match record.kind.as_str() {
        "terminal.frame" => {
            use base64::Engine;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(record.bytes.as_bytes())
                .ok()?;
            Some(ObserveEvent::Frame(TerminalFrame {
                seq: record.seq,
                full: record.full,
                bytes,
                width: record.width,
                height: record.height,
            }))
        }
        "terminal.closed" => Some(ObserveEvent::Closed),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_frame_record() {
        let line = r#"{"bytes":"aGVsbG8=","encoding":"ansi","full":true,"height":24,"seq":1,"type":"terminal.frame","width":80}"#;
        let event = decode_record(line).expect("frame");
        match event {
            ObserveEvent::Frame(frame) => {
                assert_eq!(frame.seq, 1);
                assert!(frame.full);
                assert_eq!(frame.bytes, b"hello");
                assert_eq!((frame.width, frame.height), (80, 24));
            }
            _ => panic!("expected frame"),
        }
    }

    #[test]
    fn decode_closed_record() {
        let line = r#"{"type":"terminal.closed"}"#;
        assert!(matches!(decode_record(line), Some(ObserveEvent::Closed)));
    }

    #[test]
    fn decode_skips_unknown_and_malformed() {
        assert!(decode_record(r#"{"type":"other"}"#).is_none());
        assert!(decode_record("not json").is_none());
        assert!(decode_record("").is_none());
    }
}
