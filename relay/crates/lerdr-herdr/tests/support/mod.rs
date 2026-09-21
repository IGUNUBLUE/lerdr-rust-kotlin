//! A fake Herdr server on a real Unix socket: one NDJSON request per
//! connection, scripted responses, recorded request log. Exercises the client
//! end-to-end without the real daemon.
//!
//! Shared by several test binaries — each compiles it separately, so helpers
//! unused by a given binary would warn.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinHandle;

/// One recorded request line, decoded.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub id: String,
    pub method: String,
    pub params: Value,
}

/// Per-connection behavior the server applies to a request.
pub enum Action {
    /// Reply `{"id":<echoed>,"result":<value>}` then close.
    Reply(Value),
    /// Reply `{"id":<echoed>,"error":{code,message}}` then close.
    Refuse(&'static str, &'static str),
    /// Reply with a fixed id regardless of the request (id mismatch).
    ReplyForeignId(&'static str, Value),
    /// Close the connection without writing (post-dispatch silence).
    HangUp,
    /// Read the request, write a partial response, then close mid-write.
    Truncated,
    /// Keep the connection open and stream the given lines (subscription
    /// sockets); holds until the peer disconnects.
    Stream(Vec<Vec<u8>>),
    /// Write the given lines then close immediately — a subscription socket
    /// that ends right after its scripted output.
    StreamThenClose(Vec<Vec<u8>>),
    /// Delegates to a function for bespoke behavior; the connection is
    /// counted open until the returned future completes.
    Custom(fn(UnixStream, RecordedRequest) -> HandlerFut),
}

/// Boxed future returned by `Action::Custom` handlers.
pub type HandlerFut = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

/// What the fake server does with a request. `Once` applies to the next
/// connection; the sticky default handles the rest.
struct Script {
    default: Action,
    queue: Vec<Action>,
}

pub struct FakeHerdr {
    pub sock_path: PathBuf,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    script: Arc<Mutex<Script>>,
    accept_count: Arc<AtomicUsize>,
    live_conns: Arc<AtomicUsize>,
    max_conns: Arc<AtomicUsize>,
    task: JoinHandle<()>,
    _dir: tempfile::TempDir,
}

impl FakeHerdr {
    /// Start a server whose default action applies to every connection.
    pub async fn start(default: Action) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let sock_path = dir.path().join("herdr.sock");
        let listener = UnixListener::bind(&sock_path).expect("bind unix listener");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let script = Arc::new(Mutex::new(Script {
            default,
            queue: Vec::new(),
        }));
        let accept_count = Arc::new(AtomicUsize::new(0));
        let live_conns = Arc::new(AtomicUsize::new(0));
        let max_conns = Arc::new(AtomicUsize::new(0));

        let (req_tx, script_tx, count_tx) =
            (requests.clone(), script.clone(), accept_count.clone());
        let live_tx = live_conns.clone();
        let max_tx = max_conns.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((conn, _)) = listener.accept().await else {
                    return;
                };
                count_tx.fetch_add(1, Ordering::SeqCst);
                let now = live_tx.fetch_add(1, Ordering::SeqCst) + 1;
                max_tx.fetch_max(now, Ordering::SeqCst);
                let req_tx = req_tx.clone();
                let script_tx = script_tx.clone();
                let live_tx = live_tx.clone();
                tokio::spawn(async move {
                    handle_conn(conn, req_tx, script_tx).await;
                    live_tx.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });

        FakeHerdr {
            sock_path,
            requests,
            script,
            accept_count,
            live_conns,
            max_conns,
            task,
            _dir: dir,
        }
    }

    /// Enqueue a one-shot action for the next connection (FIFO).
    pub fn push(&self, action: Action) {
        self.script.lock().unwrap().queue.push(action);
    }

    /// Replace the default action for subsequent connections.
    pub fn set_default(&self, action: Action) {
        self.script.lock().unwrap().default = action;
    }

    /// All recorded requests, in accept order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// How many connections were accepted — the fresh-connection-per-request
    /// check and singleflight's dedupe counter.
    pub fn accept_count(&self) -> usize {
        self.accept_count.load(Ordering::SeqCst)
    }

    /// Peak simultaneous connections — the dial-semaphore bound check.
    pub fn max_concurrent(&self) -> usize {
        self.max_conns.load(Ordering::SeqCst)
    }

    /// Wait until at least `n` connections were accepted (for subscribe
    /// sockets that sit open).
    pub async fn wait_accepts(&self, n: usize) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while self.accept_count() < n {
            assert!(
                std::time::Instant::now() < deadline,
                "server accepted {} connections, wanted {n}",
                self.accept_count()
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }
}

impl Drop for FakeHerdr {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Read one NDJSON line, record it, apply the scripted action.
async fn handle_conn(
    mut conn: UnixStream,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    script: Arc<Mutex<Script>>,
) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let line = loop {
        let n = match conn.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.iter().position(|b| *b == b'\n') {
            break buf[..pos].to_vec();
        }
        if buf.len() > 8 * 1024 * 1024 {
            return;
        }
    };
    let request: Value = match serde_json::from_slice(&line) {
        Ok(v) => v,
        Err(_) => return,
    };
    let recorded = RecordedRequest {
        id: request["id"].as_str().unwrap_or_default().to_string(),
        method: request["method"].as_str().unwrap_or_default().to_string(),
        params: request["params"].clone(),
    };
    requests.lock().unwrap().push(recorded.clone());

    let action = {
        let mut script = script.lock().unwrap();
        if script.queue.is_empty() {
            // Cloneable stand-in for queue-drain semantics: actions that can
            // repeat are taken by value where possible.
            match &mut script.default {
                Action::Reply(v) => Action::Reply(v.clone()),
                Action::Refuse(c, m) => Action::Refuse(c, m),
                Action::ReplyForeignId(id, v) => Action::ReplyForeignId(id, v.clone()),
                Action::HangUp => Action::HangUp,
                Action::Truncated => Action::Truncated,
                Action::Stream(lines) => Action::Stream(lines.clone()),
                Action::StreamThenClose(lines) => Action::StreamThenClose(lines.clone()),
                Action::Custom(f) => Action::Custom(*f),
            }
        } else {
            script.queue.remove(0)
        }
    };

    match action {
        Action::Reply(result) => {
            let _ = conn
                .write_all(
                    json!({"id": recorded.id, "result": result})
                        .to_string()
                        .as_bytes(),
                )
                .await;
            let _ = conn.write_all(b"\n").await;
        }
        Action::Refuse(code, message) => {
            let _ = conn
                .write_all(
                    json!({"id": recorded.id, "error": {"code": code, "message": message}})
                        .to_string()
                        .as_bytes(),
                )
                .await;
            let _ = conn.write_all(b"\n").await;
        }
        Action::ReplyForeignId(id, result) => {
            let _ = conn
                .write_all(json!({"id": id, "result": result}).to_string().as_bytes())
                .await;
            let _ = conn.write_all(b"\n").await;
        }
        Action::HangUp => {}
        Action::Truncated => {
            let _ = conn.write_all(br#"{"id":"#.as_slice()).await;
        }
        Action::Stream(lines) => {
            for line in lines {
                if conn.write_all(&line).await.is_err() {
                    return;
                }
                if conn.write_all(b"\n").await.is_err() {
                    return;
                }
            }
            // Subscription sockets stay open until the peer disconnects.
            let mut sink = [0u8; 256];
            while conn.read(&mut sink).await.map(|n| n > 0).unwrap_or(false) {}
        }
        Action::StreamThenClose(lines) => {
            for line in lines {
                if conn.write_all(&line).await.is_err() {
                    return;
                }
                if conn.write_all(b"\n").await.is_err() {
                    return;
                }
            }
        }
        Action::Custom(f) => f(conn, recorded).await,
    }
}

/// Convenience: the `session.snapshot` result body the tests reuse.
pub fn snapshot_result() -> Value {
    json!({
        "type": "session_snapshot",
        "snapshot": {
            "version": "0.9.1",
            "protocol": 22,
            "workspaces": [{"workspace_id": "wE", "number": 1, "label": "main",
                            "focused": true, "pane_count": 1, "tab_count": 1,
                            "active_tab_id": "wE:t1", "agent_status": "idle"}],
            "tabs": [{"tab_id": "wE:t1", "workspace_id": "wE", "number": 1,
                      "label": "tab", "focused": true, "pane_count": 1,
                      "agent_status": "idle"}],
            "panes": [{"pane_id": "wE:pE", "terminal_id": "t1",
                       "workspace_id": "wE", "tab_id": "wE:t1",
                       "focused": true, "agent_status": "idle"}],
            "layouts": [],
            "agents": [],
            "focused_workspace_id": "wE",
            "focused_tab_id": "wE:t1",
            "focused_pane_id": "wE:pE"
        }
    })
}

/// Convenience: the subscription handshake result line.
pub fn subscription_started_line() -> Vec<u8> {
    br#"{"id":"lerdr-events","result":{"type":"subscription_started"}}"#.to_vec()
}
