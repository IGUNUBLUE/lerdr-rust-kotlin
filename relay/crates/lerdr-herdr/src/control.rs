//! A writable native-terminal size controller, without input forwarding or
//! forced takeover. Geometry is confirmed by `terminal.frame` records, not
//! by the successful write of a resize command.

use std::io;
use std::path::Path;
use std::process::Stdio;

use serde::Deserialize;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::{AbortHandle, JoinHandle};

use crate::MAX_LINE_BYTES;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Geometry {
    seq: u64,
    cols: u16,
    rows: u16,
}

#[derive(Default)]
struct StreamState {
    latest: Option<Geometry>,
    failure: Option<io::Error>,
}

impl StreamState {
    fn observe(&mut self, frame: Geometry) -> bool {
        if self.latest.is_some_and(|latest| frame.seq <= latest.seq) {
            return false;
        }
        self.latest = Some(frame);
        true
    }

    fn confirms(&self, cols: u16, rows: u16, after: Option<u64>) -> io::Result<bool> {
        if let Some(error) = &self.failure {
            return Err(copy_error(error));
        }
        Ok(self.latest.is_some_and(|frame| {
            frame.cols == cols && frame.rows == rows && after.is_none_or(|seq| frame.seq > seq)
        }))
    }
}

enum ControlCommand {
    Resize {
        cols: u16,
        rows: u16,
        reply: oneshot::Sender<io::Result<()>>,
    },
    Release,
}

struct PendingResize {
    cols: u16,
    rows: u16,
    after_seq: u64,
    // A partially received record began before dispatch and is not evidence
    // for this resize, even if its sequence is newer than the last full line.
    skip_partial: bool,
    reply: oneshot::Sender<io::Result<()>>,
}

impl PendingResize {
    fn confirms(&mut self, state: &StreamState, fresh: bool) -> io::Result<bool> {
        if std::mem::take(&mut self.skip_partial) {
            return Ok(false);
        }
        Ok(fresh && state.confirms(self.cols, self.rows, Some(self.after_seq))?)
    }
}

/// Owns one `herdr terminal session control` subprocess. Its driver drains
/// frames continuously into a latest-value watch cell; ANSI payloads are ignored.
///
/// Callers supply their own operation deadlines. Cancellation of an in-flight
/// operation or dropping this owner aborts the driver and kills its child.
/// A cancelled or failed resize cannot leave a usable controller behind.
pub struct ControlStream {
    commands: mpsc::Sender<ControlCommand>,
    state: watch::Receiver<StreamState>,
    driver: Option<JoinHandle<io::Result<()>>>,
}

impl std::fmt::Debug for ControlStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControlStream")
            .field("live", &self.is_live())
            .finish_non_exhaustive()
    }
}

impl ControlStream {
    /// Claim the native controller without `--takeover`, and wait until an
    /// emitted frame confirms the requested native grid. The optional socket
    /// hint follows the client's CLI convention; `None` preserves inheritance.
    pub async fn spawn_sized(
        bin: &Path,
        pane_id: &str,
        socket_path: Option<&Path>,
        cols: u16,
        rows: u16,
    ) -> io::Result<Self> {
        validate_size(cols, rows)?;
        let mut command = Command::new(bin);
        command
            .args(["terminal", "session", "control"])
            .arg(pane_id)
            .arg("--cols")
            .arg(cols.to_string())
            .arg("--rows")
            .arg(rows.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(path) = socket_path {
            command.env("HERDR_SOCKET_PATH", path);
        }
        let mut child = command.spawn()?;
        let stdin = child.stdin.take().expect("control stdin piped");
        let stdout = child.stdout.take().expect("control stdout piped");
        let (commands, requests) = mpsc::channel(1);
        let (states, state) = watch::channel(StreamState::default());
        let driver = tokio::spawn(async move {
            let result = drive_control(child, stdin, stdout, requests, &states).await;
            states.send_modify(|state| {
                state.failure = Some(match &result {
                    Err(error) => copy_error(error),
                    Ok(()) => ended("terminal controller released"),
                });
            });
            result
        });
        let mut stream = Self {
            commands,
            state,
            driver: Some(driver),
        };
        stream.wait_ready(cols, rows).await?;
        Ok(stream)
    }

    /// Whether a confirmed stream still has a running child and open output.
    /// This is a latest-value projection, not a subprocess probe or repaint.
    pub fn is_live(&self) -> bool {
        self.driver
            .as_ref()
            .is_some_and(|driver| !driver.is_finished())
            && self.state.has_changed().is_ok()
            && {
                let state = self.state.borrow();
                state.latest.is_some() && state.failure.is_none()
            }
    }

    /// Resize the native VT and process PTY, awaiting a newer matching frame.
    /// Renewing the already confirmed live dimensions sends no command.
    pub async fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        validate_size(cols, rows)?;
        if !self.is_live() {
            return Err(self.stream_error());
        }
        if self.state.borrow().confirms(cols, rows, None)? {
            return Ok(());
        }
        let mut cancellation = AbortOnDrop::new(self.driver.as_ref().expect("live driver"));
        let (reply, response) = oneshot::channel();
        self.commands
            .send(ControlCommand::Resize { cols, rows, reply })
            .await
            .map_err(|_| self.stream_error())?;
        response.await.map_err(|_| self.stream_error())??;
        if !self.is_live() {
            return Err(self.stream_error());
        }
        cancellation.disarm();
        Ok(())
    }

    /// Explicitly release ownership and wait for a successful child exit.
    /// Dropping or timing out this future still kills the controller process.
    pub async fn release(mut self) -> io::Result<()> {
        if !self.is_live() {
            return Err(self.stream_error());
        }
        let driver = self.driver.take().expect("live driver");
        let mut cancellation = AbortOnDrop::new(&driver);
        self.commands
            .send(ControlCommand::Release)
            .await
            .map_err(|_| self.stream_error())?;
        let result = driver
            .await
            .map_err(|_| ended("terminal controller driver stopped"))?;
        cancellation.disarm();
        result
    }

    async fn wait_ready(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        loop {
            if self.state.borrow_and_update().confirms(cols, rows, None)? {
                return if self.is_live() {
                    Ok(())
                } else {
                    Err(self.stream_error())
                };
            }
            self.state
                .changed()
                .await
                .map_err(|_| self.stream_error())?;
        }
    }

    fn stream_error(&self) -> io::Error {
        self.state
            .borrow()
            .failure
            .as_ref()
            .map(copy_error)
            .unwrap_or_else(|| ended("terminal controller is not live"))
    }
}

impl Drop for ControlStream {
    fn drop(&mut self) {
        if let Some(driver) = &self.driver {
            driver.abort();
        }
    }
}

struct AbortOnDrop(Option<AbortHandle>);

impl AbortOnDrop {
    fn new(driver: &JoinHandle<io::Result<()>>) -> Self {
        Self(Some(driver.abort_handle()))
    }

    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(driver) = &self.0 {
            driver.abort();
        }
    }
}

async fn drive_control(
    mut child: Child,
    mut stdin: ChildStdin,
    stdout: ChildStdout,
    mut requests: mpsc::Receiver<ControlCommand>,
    states: &watch::Sender<StreamState>,
) -> io::Result<()> {
    let mut reader = BufReader::new(stdout);
    let mut line = Vec::new();
    let mut pending: Option<PendingResize> = None;
    let mut releasing = false;
    let mut output_open = true;
    loop {
        tokio::select! {
            // Process exit and already available output precede dispatch. This
            // drains queued old frames before recording a resize boundary.
            biased;
            status = child.wait() => {
                let status = status?;
                return if releasing && status.success() {
                    Ok(())
                } else if status.success() {
                    Err(ended("terminal controller exited before release"))
                } else {
                    Err(io::Error::other(format!("terminal controller exited with {status}")))
                };
            }
            record = read_line(&mut reader, &mut line), if output_open => {
                if !record? {
                    if releasing {
                        output_open = false;
                        continue;
                    }
                    return Err(ended("terminal controller output closed"));
                }
                let record = decode_record(&line)?;
                line.clear();
                if let Some(status) = child.try_wait()? {
                    return if releasing && status.success() {
                        Ok(())
                    } else {
                        Err(ended("terminal controller exited while emitting a frame"))
                    };
                }
                match record {
                    Record::Frame(frame) => {
                        let fresh = states.send_if_modified(|state| state.observe(frame));
                        if let Some(request) = &mut pending {
                            if request.confirms(&states.borrow(), fresh)? {
                                let request = pending.take().expect("pending resize");
                                let _ = request.reply.send(Ok(()));
                            }
                        }
                    }
                    Record::Closed if releasing => output_open = false,
                    Record::Closed => return Err(ended("controlled terminal closed")),
                    Record::Other => {
                        if let Some(request) = &mut pending {
                            request.skip_partial = false;
                        }
                    }
                }
            }
            request = requests.recv(), if pending.is_none() && !releasing => {
                match request.ok_or_else(|| ended("terminal controller owner dropped"))? {
                    ControlCommand::Resize { cols, rows, reply } => {
                        if states.borrow().confirms(cols, rows, None)? {
                            let _ = reply.send(Ok(()));
                            continue;
                        }
                        let after_seq = states.borrow().latest
                            .ok_or_else(|| ended("terminal controller has no geometry"))?.seq;
                        let (bytes, length) = resize_command(cols, rows);
                        stdin.write_all(&bytes[..length]).await?;
                        stdin.flush().await?;
                        pending = Some(PendingResize {
                            cols, rows, after_seq, skip_partial: !line.is_empty(), reply,
                        });
                    }
                    ControlCommand::Release => {
                        stdin.write_all(b"{\"type\":\"terminal.release\"}\n").await?;
                        stdin.flush().await?;
                        releasing = true;
                    }
                }
            }
        }
    }
}

// Reuse a single bounded line buffer. Unlike read_line(String), read_until
// semantics remain cancellation-safe when a command wins the driver's select.
async fn read_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    line: &mut Vec<u8>,
) -> io::Result<bool> {
    loop {
        let bytes = reader.fill_buf().await?;
        if bytes.is_empty() {
            return if line.is_empty() {
                Ok(false)
            } else {
                Err(ended("incomplete terminal controller record"))
            };
        }
        let newline = bytes.iter().position(|byte| *byte == b'\n');
        let count = newline.map_or(bytes.len(), |index| index + 1);
        if count > MAX_LINE_BYTES.saturating_sub(line.len()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "terminal frame exceeds line limit",
            ));
        }
        line.extend_from_slice(&bytes[..count]);
        reader.consume(count);
        if newline.is_some() {
            return Ok(true);
        }
    }
}

#[derive(Deserialize)]
enum WireKind {
    #[serde(rename = "terminal.frame")]
    Frame,
    #[serde(rename = "terminal.closed")]
    Closed,
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct WireRecord {
    #[serde(rename = "type")]
    kind: WireKind,
    seq: Option<u64>,
    width: Option<u16>,
    height: Option<u16>,
    // Unknown fields, including base64 `bytes`, are skipped by serde without
    // allocating a String, decoding ANSI, or copying the payload a second time.
}

enum Record {
    Frame(Geometry),
    Closed,
    Other,
}

fn decode_record(line: &[u8]) -> io::Result<Record> {
    let record: WireRecord = serde_json::from_slice(line)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    match record.kind {
        WireKind::Frame => {
            let (Some(seq), Some(cols), Some(rows)) = (record.seq, record.width, record.height)
            else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "terminal frame has no geometry or sequence",
                ));
            };
            validate_size(cols, rows)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            Ok(Record::Frame(Geometry { seq, cols, rows }))
        }
        WireKind::Closed => Ok(Record::Closed),
        WireKind::Other => Ok(Record::Other),
    }
}

fn resize_command(cols: u16, rows: u16) -> ([u8; 80], usize) {
    use std::io::Write;

    let mut bytes = [0; 80];
    let mut remaining = &mut bytes[..];
    writeln!(
        remaining,
        "{{\"type\":\"terminal.resize\",\"cols\":{cols},\"rows\":{rows}}}"
    )
    .expect("u16 resize command fits fixed buffer");
    let length = 80 - remaining.len();
    (bytes, length)
}

fn validate_size(cols: u16, rows: u16) -> io::Result<()> {
    if cols == 0 || rows == 0 {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "terminal dimensions must be positive",
        ))
    } else {
        Ok(())
    }
}

fn ended(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, message)
}

fn copy_error(error: &io::Error) -> io::Error {
    io::Error::new(error.kind(), error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_geometry_cannot_confirm_or_replace_newer_state() {
        let mut state = StreamState::default();
        assert!(state.observe(Geometry {
            seq: 10,
            cols: 80,
            rows: 24
        }));
        for seq in [9, 10] {
            assert!(!state.observe(Geometry {
                seq,
                cols: 57,
                rows: 12
            }));
            assert!(!state.confirms(57, 12, Some(10)).unwrap());
        }
        assert!(state.observe(Geometry {
            seq: 11,
            cols: 57,
            rows: 36
        }));
        assert!(!state.confirms(57, 12, Some(10)).unwrap());
        assert!(state.observe(Geometry {
            seq: 12,
            cols: 57,
            rows: 12
        }));
        assert!(state.confirms(57, 12, Some(10)).unwrap());
        assert!(!state.confirms(57, 12, Some(12)).unwrap());
    }

    #[test]
    fn a_record_started_before_dispatch_cannot_confirm_a_resize() {
        let (reply, _response) = oneshot::channel();
        let mut request = PendingResize {
            cols: 57,
            rows: 12,
            after_seq: 10,
            skip_partial: true,
            reply,
        };
        let mut state = StreamState::default();
        state.observe(Geometry {
            seq: 11,
            cols: 57,
            rows: 12,
        });
        assert!(!request.confirms(&state, true).unwrap());
        assert!(!request.confirms(&state, false).unwrap());
        state.observe(Geometry {
            seq: 12,
            cols: 57,
            rows: 12,
        });
        assert!(request.confirms(&state, true).unwrap());
    }

    #[test]
    fn failure_overrides_a_retained_matching_frame() {
        let mut state = StreamState::default();
        state.observe(Geometry {
            seq: 1,
            cols: 57,
            rows: 12,
        });
        state.failure = Some(ended("test output closed"));
        assert_eq!(
            state.confirms(57, 12, None).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn frame_confirmation_requires_geometry_and_sequence_not_ansi() {
        let frame = br#"{"type":"terminal.frame","seq":0,"width":57,"height":12,"bytes":"not base64","full":true}"#;
        assert!(matches!(
            decode_record(frame).unwrap(),
            Record::Frame(Geometry {
                seq: 0,
                cols: 57,
                rows: 12
            })
        ));
        for frame in [
            &br#"{"type":"terminal.frame","width":57,"height":12}"#[..],
            &br#"{"type":"terminal.frame","seq":1,"width":57,"height":0}"#[..],
        ] {
            assert_eq!(
                decode_record(frame).err().unwrap().kind(),
                io::ErrorKind::InvalidData
            );
        }
    }

    #[tokio::test]
    async fn incomplete_record_is_not_a_frame_at_eof() {
        let mut reader = BufReader::new(&b"{\"type\":\"terminal.frame\"}"[..]);
        assert_eq!(
            read_line(&mut reader, &mut Vec::new())
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelling_an_operation_kills_its_controller() {
        use tokio::io::AsyncReadExt;

        // A real silent child keeps stderr open until it exits. EOF on that
        // independent pipe proves aborting the driver also terminates its child.
        let mut child = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let (_commands, requests) = mpsc::channel(1);
        let (states, _state) = watch::channel(StreamState::default());
        let driver =
            tokio::spawn(
                async move { drive_control(child, stdin, stdout, requests, &states).await },
            );
        let cancellation = AbortOnDrop::new(&driver);
        drop(cancellation);
        assert!(driver.await.unwrap_err().is_cancelled());
        let mut byte = [0];
        let read = tokio::time::timeout(std::time::Duration::from_secs(5), stderr.read(&mut byte))
            .await
            .expect("cancelled controller exits")
            .unwrap();
        assert_eq!(read, 0);
    }
}
