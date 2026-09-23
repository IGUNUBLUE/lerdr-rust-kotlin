//! `lerdr-shadow run` — one scripted `herdr-e2ee-v2` connection against a
//! relay, recording every decoded plaintext into a JSONL trace.
//!
//! The handshake mirrors `lerdr-relay/tests/support/mod.rs` verbatim (fresh
//! P-256 keypair, `client_proof` against the relay key as the bootstrap
//! invitation secret, `Session::client` after the server hello), because the
//! test-support module is not importable from a binary.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use lerdr_core::protocol::ENCRYPTED_WEBSOCKET_SUBPROTOCOL;
use lerdr_e2ee::handshake::{
    client_proof, derive_session_keys, ecdh_shared, encode_client_hello, key_salt,
    parse_server_finish, parse_server_hello, server_proof, transcript, AuthKind, AuthSelector,
    CLIENT_FINISH_JSON, NONCE_BYTES, PUBLIC_KEY_BYTES, SECRET_BYTES,
};
use lerdr_e2ee::{Codec, Session};
use p256::elliptic_curve::sec1::ToSec1Point;
use p256::elliptic_curve::Generate;
use p256::SecretKey;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpStream, UnixStream};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

use crate::scenario::{render, render_str, step_scope, Match, Scenario, Step, PROTOCOL_VERSION};
use crate::trace::{now_ms, Record, TraceWriter};
use crate::{Result, ShadowError};

/// The fixed bootstrap selector both relays arm from the relay key
/// (`{kind: invitation, id: "bootstrap", version: 1}` — Go
/// `EnsureBootstrapInvitation`, Rust `mint_invitation`).
pub const BOOTSTRAP_INVITATION_ID: &str = "bootstrap";
pub const BOOTSTRAP_LOCALE: &str = "en";

pub struct RunParams {
    /// `ws(s)://host:port/ws` — the full endpoint.
    pub url: String,
    /// The 32-byte relay key; doubles as the bootstrap invitation secret.
    pub token: [u8; SECRET_BYTES],
    /// Auth selector — defaults are the token bootstrap.
    pub auth_id: String,
    pub auth_version: u64,
    pub locale: String,
    pub scenario: Scenario,
    /// Label written into the trace meta (`go`, `rust`, `rust-a`, …).
    pub side: String,
    pub trace_path: PathBuf,
    /// Whole-handshake deadline.
    pub handshake_timeout: Duration,
    /// Tail capture after the last step (ms).
    pub drain_ms: u64,
    /// Fake-herdr socket for `fake_call` steps — `None` makes them fail
    /// fast with a setup error.
    pub herdr_socket: Option<PathBuf>,
}

/// One received frame: millisecond timestamp relative to run start plus the
/// decoded plaintext object.
type Arrival = (u64, Value);

fn elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis() as u64
}

pub async fn run(params: &RunParams) -> Result<()> {
    let start = Instant::now();
    let mut trace = TraceWriter::create(&params.trace_path)?;
    trace.write(&Record::Meta {
        side: params.side.clone(),
        url: params.url.clone(),
        scenario: params.scenario.name.clone(),
        started_ms: now_ms(),
        compare: params.scenario.compare.clone(),
    })?;

    let (mut sink, mut stream) = connect(params, &mut trace, start).await?;

    // -- handshake ----------------------------------------------------------
    let selector = AuthSelector::new(
        AuthKind::Invitation,
        &params.auth_id,
        params.auth_version,
        &params.locale,
    );
    let private = SecretKey::generate();
    let public_bytes: [u8; PUBLIC_KEY_BYTES] = private
        .public_key()
        .to_sec1_point(false)
        .as_bytes()
        .try_into()
        .expect("P-256 uncompressed point is 65 bytes");
    let nonce: [u8; NONCE_BYTES] = <[u8; NONCE_BYTES]>::generate();
    let proof = client_proof(&params.token, &selector.binding(), &nonce, &public_bytes);
    let hello = encode_client_hello(&selector, &nonce, &public_bytes, &proof);
    sink.send(Message::Text(
        String::from_utf8(hello)
            .map_err(|e| ShadowError::msg(format!("client hello is not utf8: {e}")))?
            .into(),
    ))
    .await?;

    let raw_hello = read_text(&mut stream, params.handshake_timeout).await?;
    let hello_frame: Value = serde_json::from_slice(&raw_hello)?;
    trace.write(&Record::Rx {
        t_ms: elapsed_ms(start),
        step: None,
        frame: hello_frame,
    })?;

    let server = parse_server_hello(&raw_hello)?;
    let transcript = transcript(
        &selector.binding(),
        &nonce,
        &public_bytes,
        &server.nonce,
        &server.public_bytes,
    );
    if server.proof != server_proof(&params.token, &transcript) {
        return Err(ShadowError::msg(
            "e2ee server proof mismatch — wrong relay key or wrong selector",
        ));
    }
    let shared = ecdh_shared(&private, &server.public_key);
    let keys = derive_session_keys(&shared, &key_salt(&params.token, &transcript));
    let session = Arc::new(Mutex::new(
        Session::client(&keys, Codec::Json).map_err(ShadowError::E2ee)?,
    ));

    let finish_frame = {
        session
            .lock()
            .expect("session mutex")
            .seal(CLIENT_FINISH_JSON)?
    };
    sink.send(Message::Text(
        String::from_utf8(finish_frame)
            .map_err(|e| ShadowError::msg(format!("finish frame is not utf8: {e}")))?
            .into(),
    ))
    .await?;

    let raw_finish = read_text(&mut stream, params.handshake_timeout).await?;
    let finish_plain = session.lock().expect("session mutex").open(&raw_finish)?;
    let finish_value: Value = serde_json::from_slice(&finish_plain)?;
    trace.write(&Record::Rx {
        t_ms: elapsed_ms(start),
        step: None,
        frame: finish_value.clone(),
    })?;
    let finish = parse_server_finish(&finish_plain)?;
    trace.write(&Record::Note {
        t_ms: elapsed_ms(start),
        text: format!(
            "handshake complete — device_id={} credential_id={} role={}",
            finish.device_id, finish.credential_id, finish.role
        ),
    })?;

    // -- reader task: ws text → open → (t_ms, value) channel -----------------
    let (tx_chan, rx_chan) = mpsc::unbounded_channel::<Arrival>();
    let reader_session = Arc::clone(&session);
    tokio::spawn(async move {
        while let Some(item) = stream.next().await {
            match item {
                Ok(Message::Text(text)) => {
                    let t = elapsed_ms(start);
                    let opened = reader_session
                        .lock()
                        .expect("session mutex")
                        .open(text.as_str().as_bytes());
                    let value = match opened {
                        Ok(plain) => match serde_json::from_slice::<Value>(&plain) {
                            Ok(v) => v,
                            Err(e) => serde_json::json!({
                                "type": "_decode_error",
                                "error": e.to_string(),
                            }),
                        },
                        Err(e) => serde_json::json!({
                            "type": "_open_error",
                            "error": e.to_string(),
                        }),
                    };
                    if tx_chan.send((t, value)).is_err() {
                        return;
                    }
                }
                Ok(Message::Close(_)) => return,
                Ok(_) => {}
                Err(e) => {
                    let _ = tx_chan.send((
                        elapsed_ms(start),
                        serde_json::json!({"type": "_ws_error", "error": e.to_string()}),
                    ));
                    return;
                }
            }
        }
    });

    // -- step executor --------------------------------------------------------
    let mut exec = Executor {
        rx: rx_chan,
        frames: Vec::new(),
        trace: &mut trace,
        session: &session,
        start,
        herdr_socket: params.herdr_socket.as_deref(),
    };
    let mut run_err: Option<ShadowError> = None;
    for (index, step) in params.scenario.steps.iter().enumerate() {
        let request_id = step.request_id(index);
        exec.trace.write(&Record::Step {
            index,
            label: step.label().to_owned(),
            op: step.op_name().to_owned(),
            request_id: request_id.clone(),
            capture: step.capture().to_vec(),
        })?;
        if let Err(e) = exec
            .step(&params.scenario, index, step, request_id, &mut sink)
            .await
        {
            // Record and abort — an `expect` miss is a protocol failure.
            let _ = exec.trace.write(&Record::Note {
                t_ms: elapsed_ms(start),
                text: format!("step {} {:?} failed: {e}", index, step.label()),
            });
            run_err = Some(e);
            break;
        }
    }

    // Tail capture: anything in flight after the last step.
    if run_err.is_none() && params.drain_ms > 0 {
        exec.collect_tail(params.drain_ms).await?;
    }
    let _ = sink.close().await;
    match run_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Connect + upgrade; notes the negotiated subprotocol.
async fn connect(
    params: &RunParams,
    trace: &mut TraceWriter,
    start: Instant,
) -> Result<(
    futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
        Message,
    >,
    futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
    >,
)> {
    let mut request = params
        .url
        .clone()
        .into_client_request()
        .map_err(|e| ShadowError::msg(format!("bad url {:?}: {e}", params.url)))?;
    request.headers_mut().append(
        "Sec-WebSocket-Protocol",
        ENCRYPTED_WEBSOCKET_SUBPROTOCOL
            .parse()
            .expect("subprotocol is a valid header"),
    );
    let (socket, response) = tokio::time::timeout(
        params.handshake_timeout,
        tokio_tungstenite::connect_async(request),
    )
    .await
    .map_err(|_| ShadowError::msg(format!("connect to {} timed out", params.url)))??;
    let negotiated = response
        .headers()
        .get("Sec-WebSocket-Protocol")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    if negotiated.as_deref() != Some(ENCRYPTED_WEBSOCKET_SUBPROTOCOL) {
        trace.write(&Record::Note {
            t_ms: elapsed_ms(start),
            text: format!("negotiated subprotocol is {negotiated:?}, want {ENCRYPTED_WEBSOCKET_SUBPROTOCOL:?}"),
        })?;
    }
    Ok(socket.split())
}

/// Read one text frame with a deadline.
async fn read_text<S>(stream: &mut S, timeout: Duration) -> Result<Vec<u8>>
where
    S: StreamExt<Item = std::result::Result<Message, tokio_tungstenite::tungstenite::Error>>
        + Unpin,
{
    let item = tokio::time::timeout(timeout, stream.next())
        .await
        .map_err(|_| ShadowError::msg("handshake read timed out"))?
        .ok_or_else(|| ShadowError::msg("socket closed during handshake"))??;
    match item {
        Message::Text(t) => Ok(t.as_str().as_bytes().to_vec()),
        other => Err(ShadowError::msg(format!(
            "expected a text frame during handshake, got {other:?}"
        ))),
    }
}

/// The per-run executor: drains the reader channel into `frames`, writes
/// `rx` records tagged with the current step index.
struct Executor<'a> {
    rx: mpsc::UnboundedReceiver<Arrival>,
    frames: Vec<Value>,
    trace: &'a mut TraceWriter,
    /// Shared with the reader task: `seal` here, `open` there.
    session: &'a Arc<Mutex<Session>>,
    start: Instant,
    /// Fake-herdr socket for `fake_call` steps.
    herdr_socket: Option<&'a Path>,
}

type Sink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
    Message,
>;

impl Executor<'_> {
    /// Pull everything queued right now into the trace + match buffer.
    fn drain(&mut self, step: Option<usize>) -> Result<()> {
        while let Ok((t_ms, frame)) = self.rx.try_recv() {
            self.push(t_ms, step, frame)?;
        }
        Ok(())
    }

    fn push(&mut self, t_ms: u64, step: Option<usize>, frame: Value) -> Result<()> {
        self.trace.write(&Record::Rx {
            t_ms,
            step,
            frame: frame.clone(),
        })?;
        self.frames.push(frame);
        Ok(())
    }

    fn note(&mut self, text: String) -> Result<()> {
        self.trace.write(&Record::Note {
            t_ms: elapsed_ms(self.start),
            text,
        })
    }

    async fn step(
        &mut self,
        scenario: &Scenario,
        index: usize,
        step: &Step,
        request_id: Option<String>,
        sink: &mut Sink,
    ) -> Result<()> {
        match step {
            Step::Fence { label } => {
                self.drain(Some(index))?;
                self.trace.write(&Record::Fence {
                    t_ms: elapsed_ms(self.start),
                    label: label.clone(),
                })?;
            }
            Step::Settle { ms, .. } => {
                self.drain(Some(index))?;
                tokio::time::sleep(Duration::from_millis(*ms)).await;
                self.drain(Some(index))?;
            }
            Step::Expect {
                label,
                r#match,
                timeout_ms,
            } => {
                let want = r#match.render(&scenario.vars, &step_scope(None))?;
                let found = self
                    .wait_match(&want, 0, Some(index), Duration::from_millis(*timeout_ms))
                    .await?;
                if !found {
                    return Err(ShadowError::msg(format!(
                        "expect {:?}: never observed {}",
                        label,
                        want.describe()
                    )));
                }
            }
            Step::Send {
                label,
                frame,
                until,
                timeout_ms,
                quiesce_ms,
                ..
            } => {
                let scope = step_scope(request_id.as_deref());
                let mut outbound = render(frame, &scenario.vars, &scope)?;
                if let (Some(id), Value::Object(map)) = (request_id.clone(), &mut outbound) {
                    map.entry("request_id".to_owned())
                        .or_insert(Value::String(id));
                }
                self.send_frame(index, &outbound, sink).await?;
                let base = self.frames.len();
                let want = match until {
                    Some(m) => Some(m.render(&scenario.vars, &scope)?),
                    None => None,
                };
                self.collect(
                    want.as_ref(),
                    base,
                    Some(index),
                    Duration::from_millis(*timeout_ms),
                    Duration::from_millis(*quiesce_ms),
                    label,
                )
                .await?;
            }
            Step::Collect {
                label,
                until,
                timeout_ms,
                quiesce_ms,
                ..
            } => {
                let base = self.frames.len();
                let want = match until {
                    Some(m) => Some(m.render(&scenario.vars, &step_scope(None))?),
                    None => None,
                };
                self.collect(
                    want.as_ref(),
                    base,
                    Some(index),
                    Duration::from_millis(*timeout_ms),
                    Duration::from_millis(*quiesce_ms),
                    label,
                )
                .await?;
            }
            Step::FakeCall {
                label,
                method,
                params,
                timeout_ms,
            } => {
                let Some(socket) = self.herdr_socket else {
                    return Err(ShadowError::msg(format!(
                        "step {label:?}: fake_call requires --herdr-socket"
                    )));
                };
                // Drain first so a relay push mid-call stays attributed to
                // this step's window.
                self.drain(Some(index))?;
                let scope = step_scope(None);
                let method = render_str(method, &scenario.vars, &scope)?;
                let params = render(params, &scenario.vars, &scope)?;
                let result = fake_call(
                    socket,
                    index,
                    &method,
                    &params,
                    Duration::from_millis(*timeout_ms),
                )
                .await?;
                self.note(format!(
                    "fake_call {method} → {}",
                    crate::normalize::canonical(&result)
                ))?;
            }
            Step::AckPane {
                label,
                pane_id,
                target,
                timeout_ms,
                quiesce_ms,
                ..
            } => {
                let scope = step_scope(request_id.as_deref());
                let pane_id = render_str(pane_id, &scenario.vars, &scope)?;
                let Some(fingerprint) = last_pane_fingerprint(&self.frames, &pane_id) else {
                    return Err(ShadowError::msg(format!(
                        "ack_pane {label:?}: no pane_content/pane_delta seen for {pane_id:?}"
                    )));
                };
                let mut outbound = serde_json::json!({
                    "type": "pane_applied",
                    "protocol": PROTOCOL_VERSION,
                    "pane_id": pane_id,
                    "content_fingerprint": fingerprint,
                });
                if let Value::Object(map) = &mut outbound {
                    // Same `request_id` the Step record advertised — keeps
                    // the tx frame self-describing in the trace.
                    if let Some(id) = &request_id {
                        map.insert("request_id".to_owned(), Value::String(id.clone()));
                    }
                    if let Some(target) = target {
                        let target = render(target, &scenario.vars, &scope)?;
                        // `putTarget` parity — `server_session_id` also rides
                        // top-level on target-bearing actions.
                        if let Some(ssid) = target
                            .get("server_session_id")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            map.insert(
                                "server_session_id".to_owned(),
                                Value::String(ssid.to_owned()),
                            );
                        }
                        map.insert("target".to_owned(), target);
                    }
                }
                self.send_frame(index, &outbound, sink).await?;
                let base = self.frames.len();
                self.collect(
                    None,
                    base,
                    Some(index),
                    Duration::from_millis(*timeout_ms),
                    Duration::from_millis(*quiesce_ms),
                    label,
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Trace `tx`, seal, send — shared by `send` and `ack_pane`.
    async fn send_frame(&mut self, index: usize, outbound: &Value, sink: &mut Sink) -> Result<()> {
        self.trace.write(&Record::Tx {
            t_ms: elapsed_ms(self.start),
            step: Some(index),
            frame: outbound.clone(),
        })?;
        let plaintext = serde_json::to_vec(outbound)?;
        let sealed = self
            .session
            .lock()
            .expect("session mutex")
            .seal(&plaintext)?;
        sink.send(Message::Text(
            String::from_utf8(sealed)
                .map_err(|e| ShadowError::msg(format!("sealed frame not utf8: {e}")))?
                .into(),
        ))
        .await?;
        Ok(())
    }

    /// Post-scenario tail capture: keep recording until `ms` of silence.
    async fn collect_tail(&mut self, ms: u64) -> Result<()> {
        self.collect(
            None,
            self.frames.len(),
            None,
            Duration::from_millis(ms.max(1_000)),
            Duration::from_millis(ms),
            "tail",
        )
        .await
    }

    /// Collect frames into the trace until `until` matches (if given) and the
    /// socket has been quiet for `quiesce` — hard-bounded by `timeout`.
    /// A missed `until` is a `note`, not a failure: one side legitimately
    /// lacking a frame type must not abort the run.
    async fn collect(
        &mut self,
        until: Option<&Match>,
        base: usize,
        step: Option<usize>,
        timeout: Duration,
        quiesce: Duration,
        label: &str,
    ) -> Result<()> {
        let deadline = Instant::now() + timeout;
        let mut matched = until.is_none();
        let mut last_rx = Instant::now();
        loop {
            // Block for the next frame or the nearest boundary: the quiesce
            // horizon once matched, the hard deadline otherwise.
            let boundary = if matched {
                quiesce.min(deadline.saturating_duration_since(Instant::now()))
            } else {
                deadline.saturating_duration_since(Instant::now())
            };
            if boundary.is_zero() {
                break;
            }
            match tokio::time::timeout(boundary, self.rx.recv()).await {
                Ok(Some((t_ms, frame))) => {
                    self.push(t_ms, step, frame)?;
                    last_rx = Instant::now();
                }
                Ok(None) => {
                    self.note(format!("step {label:?}: relay socket closed mid-collect"))?;
                    return Ok(());
                }
                Err(_) => {}
            }
            self.drain(step)?;
            if !matched
                && until.is_some_and(|m| {
                    self.frames[base.min(self.frames.len())..]
                        .iter()
                        .any(|f| m.is_match(f))
                })
            {
                matched = true;
            }
            if Instant::now() >= deadline {
                break;
            }
            if matched && last_rx.elapsed() >= quiesce {
                break;
            }
        }
        if !matched {
            if let Some(m) = until {
                self.note(format!(
                    "step {label:?}: until {} not matched",
                    m.describe()
                ))?;
            }
        }
        Ok(())
    }

    /// Poll until `want` appears in `frames[from..]` (or the whole buffer
    /// when `from == 0`) — `true` when matched before the deadline.
    async fn wait_match(
        &mut self,
        want: &Match,
        from: usize,
        step: Option<usize>,
        timeout: Duration,
    ) -> Result<bool> {
        let deadline = Instant::now() + timeout;
        loop {
            self.drain(step)?;
            if self.frames[from.min(self.frames.len())..]
                .iter()
                .any(|f| want.is_match(f))
            {
                return Ok(true);
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(false);
            }
            match tokio::time::timeout(deadline - now, self.rx.recv()).await {
                Ok(Some((t_ms, frame))) => {
                    self.trace.write(&Record::Rx {
                        t_ms,
                        step,
                        frame: frame.clone(),
                    })?;
                    self.frames.push(frame);
                }
                Ok(None) => {
                    self.note("relay socket closed while waiting".to_owned())?;
                    return Ok(false);
                }
                Err(_) => return Ok(false),
            }
        }
    }
}

/// One NDJSON round-trip on the fake-herdr socket — `control.*` methods
/// mutate fake state (`control.set`) or push events (`control.emit`). A
/// fresh connection per call matches how the relays themselves use the
/// socket for reads; the relay's `events.subscribe` conn stays held by the
/// fake and receives emitted frames.
async fn fake_call(
    socket: &Path,
    index: usize,
    method: &str,
    params: &Value,
    timeout: Duration,
) -> Result<Value> {
    let stream = tokio::time::timeout(timeout, UnixStream::connect(socket))
        .await
        .map_err(|_| ShadowError::msg(format!("fake_call {method}: connect timed out")))??;
    let (read, mut write) = stream.into_split();
    let mut buf = serde_json::to_vec(&serde_json::json!({
        "id": format!("shadow-ctl-{index}"),
        "method": method,
        "params": params,
    }))?;
    buf.push(b'\n');
    tokio::time::timeout(timeout, write.write_all(&buf))
        .await
        .map_err(|_| ShadowError::msg(format!("fake_call {method}: write timed out")))??;
    let mut lines = BufReader::new(read).lines();
    let line = tokio::time::timeout(timeout, lines.next_line())
        .await
        .map_err(|_| ShadowError::msg(format!("fake_call {method}: response timed out")))?
        .map_err(ShadowError::Io)?
        .ok_or_else(|| {
            ShadowError::msg(format!(
                "fake_call {method}: socket closed without a response"
            ))
        })?;
    let reply: Value = serde_json::from_str(&line)?;
    if let Some(error) = reply.get("error") {
        return Err(ShadowError::msg(format!(
            "fake_call {method} refused: {}",
            crate::normalize::canonical(error)
        )));
    }
    Ok(reply.get("result").cloned().unwrap_or(Value::Null))
}

/// The newest `content_fingerprint` observed for `pane_id` — `pane_applied`
/// acks echo it (`handlePaneApplied` matches it against `pending`). The
/// delta path fingerprints the post-image, so a `pane_delta` carries the
/// value its own ack must repeat.
fn last_pane_fingerprint(frames: &[Value], pane_id: &str) -> Option<String> {
    frames.iter().rev().find_map(|frame| {
        let ty = frame.get("type").and_then(Value::as_str)?;
        if ty != "pane_content" && ty != "pane_delta" {
            return None;
        }
        if frame.get("pane_id").and_then(Value::as_str) != Some(pane_id) {
            return None;
        }
        frame
            .get("content_fingerprint")
            .and_then(Value::as_str)
            .filter(|fp| !fp.is_empty())
            .map(str::to_owned)
    })
}
