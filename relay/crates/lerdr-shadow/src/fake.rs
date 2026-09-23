//! `lerdr-fake-herdr` — a static Herdr socket-API endpoint.
//!
//! Speaks the newline-delimited `{id, method, params}` → `{id, result}` /
//! `{id, error:{code,message}}` protocol both relays consume. The state file
//! is the oracle fake-herdr's `Scenario` JSON plus a `"socket"` extension
//! block it ignores, so one file seeds the CLI fake (`HERDR_BIN` for the Go
//! relay) and this socket fake (`HERDR_SOCKET_PATH` for both relays).
//!
//! Covered surface (everything the relays' startup + poller + actions hit):
//!
//! | method                    | response                                    |
//! |---------------------------|---------------------------------------------|
//! | `ping`                    | `{"type":"pong",…}`                         |
//! | `events.subscribe`        | `{"type":"subscription_started"}`, held open |
//! | `session.snapshot`        | `{"type":"session_snapshot","snapshot":{…}}` |
//! | `agent.list`              | `{"type":"agent_list","agents":[…]}`        |
//! | `pane.list`               | `{"type":"pane_list","panes":[…]}`          |
//! | `workspace.list`          | `{"type":"workspace_list","workspaces":[…]}`|
//! | `tab.list`                | `{"type":"tab_list","tabs":[…]}`            |
//! | `pane.read`               | `{"type":"pane_read","read":{…}}`           |
//! | `pane.send_input`/`text`/`keys` | `{"type":"ok"}`                       |
//! | `worktree.list`           | `{"type":"worktree_list",…}` per workspace  |
//! | `workspace.move_block`    | `workspace_move_block_failed` (probe → supported) |
//! | `tab.move`                | `tab_not_found` (probe → supported)         |
//! | `control.set`             | `{"type":"ok"}` — sets `content[pane_id]`   |
//! | `control.emit`            | `{"type":"ok","delivered":N}` — broadcasts  |
//! |                           | `{"event":name,"data":…}` to subscribers    |
//! | anything else             | `unknown_method` error                      |
//!
//! `socket.errors`/`socket.methods` in the state file override per method —
//! `"errors": {"pane.send_input": {"code": "pane_not_found", "message": …}}`
//! scripts a failure without rebuilding.
//!
//! ## `control.*` — the fake's scenario lever
//!
//! `control.*` methods are a fake-internal extension the relays never call;
//! the shadow *scenario client* issues them on a normal (non-subscribe)
//! connection to drive server-side change mid-run:
//!
//! - `control.set {pane_id, text}` — replaces the `pane.read` text for the
//!   pane. The next read observes the new text, which is what lets a watch
//!   emit a delta. Reset content to a known baseline at the top of a
//!   scenario: the socket fake is shared across both runs of a diff.
//! - `control.emit {name, data}` — pushes one NDJSON event frame,
//!   `{"event":"<name>","data":<data>}` (the exact envelope both relays'
//!   event clients decode — Go `herdr.Event`, Rust `lerdr_herdr::Event`),
//!   to every held `events.subscribe` connection. Names are written
//!   verbatim; subscribers canonicalize snake_case → dotted themselves.
//!   Emission to zero subscribers is an error — that's a scenario bug.
//!
//! The broadcast does not filter by each connection's `subscriptions`
//! list — scenarios emit only names the relays subscribe to.

use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;

use crate::{Result, ShadowError};

const DEFAULT_VERSION: &str = "0.9.1";
const DEFAULT_PROTOCOL: u32 = 22;

/// The shared state file — oracle `Scenario` fields plus `socket`.
#[derive(Debug, Deserialize)]
pub struct StateFile {
    /// Herdr server version reported by `pong`/`session.snapshot`.
    #[serde(default = "default_version")]
    pub version: String,
    /// Herdr protocol number — must be > 0 or the Go pong decoder rejects it.
    #[serde(default = "default_protocol")]
    pub protocol: u32,
    /// `Pane`-shaped records (Go CLI tags; superset fields fine — both sides
    /// ignore what they don't model).
    #[serde(default)]
    pub panes: Vec<Value>,
    /// `WorkspaceInfo`-shaped records — verbatim into `workspace.list` and
    /// `session.snapshot`.
    #[serde(default)]
    pub workspaces: Vec<Value>,
    /// `TabInfo`-shaped records.
    #[serde(default)]
    pub tabs: Vec<Value>,
    /// `pane_id` → read text for `pane read`/`pane.read`.
    #[serde(default)]
    pub content: BTreeMap<String, String>,
    /// Socket-only extensions.
    #[serde(default)]
    pub socket: SocketExt,
}

fn default_version() -> String {
    DEFAULT_VERSION.to_owned()
}

fn default_protocol() -> u32 {
    DEFAULT_PROTOCOL
}

#[derive(Debug, Default, Deserialize)]
pub struct SocketExt {
    /// `pong.capabilities` — e.g. `{"health_check": true}`.
    #[serde(default)]
    pub capabilities: Option<Value>,
    /// `session.snapshot.agents` / `agent.list` records (pane/agent superset).
    #[serde(default)]
    pub agents: Vec<Value>,
    /// `session.snapshot.layouts`.
    #[serde(default)]
    pub layouts: Vec<Value>,
    #[serde(default)]
    pub focused_workspace_id: Option<String>,
    #[serde(default)]
    pub focused_tab_id: Option<String>,
    #[serde(default)]
    pub focused_pane_id: Option<String>,
    /// `workspace_id` → `worktree.list` result payload (`{source, worktrees}`).
    #[serde(default)]
    pub worktree_list: BTreeMap<String, Value>,
    /// `method` → canned `result` payload (verbatim).
    #[serde(default)]
    pub methods: BTreeMap<String, Value>,
    /// `method` → `{"code": …, "message": …}` error response.
    #[serde(default)]
    pub errors: BTreeMap<String, Value>,
    /// `pane_id` → `pane.read` `read` object overrides.
    #[serde(default)]
    pub pane_read: BTreeMap<String, Value>,
}

impl StateFile {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        serde_json::from_str(&raw)
            .map_err(|e| ShadowError::msg(format!("state {}: {e}", path.display())))
    }

    fn capabilities(&self) -> Value {
        self.socket
            .capabilities
            .clone()
            .unwrap_or_else(|| json!({"health_check": true}))
    }

    fn snapshot(&self) -> Value {
        let mut snap = json!({
            "version": self.version,
            "protocol": self.protocol,
            "workspaces": self.workspaces,
            "tabs": self.tabs,
            "panes": self.panes,
            "layouts": self.socket.layouts,
            "agents": self.socket.agents,
        });
        if let Value::Object(map) = &mut snap {
            if let Some(id) = &self.socket.focused_workspace_id {
                map.insert("focused_workspace_id".into(), json!(id));
            }
            if let Some(id) = &self.socket.focused_tab_id {
                map.insert("focused_tab_id".into(), json!(id));
            }
            if let Some(id) = &self.socket.focused_pane_id {
                map.insert("focused_pane_id".into(), json!(id));
            }
        }
        snap
    }
}

/// The mutable runtime — everything `control.*` calls touch. `StateFile`
/// stays the immutable seed; `Live` is seeded from it at startup.
pub struct Live {
    /// `pane_id` → current `pane.read` text — `control.set` writes,
    /// `pane.read` reads.
    content: Mutex<BTreeMap<String, String>>,
    /// Raw event frames (`{"event":…,"data":…}`) broadcast to every held
    /// `events.subscribe` connection — `control.emit` sends.
    events: broadcast::Sender<Value>,
}

/// Event-channel capacity per subscriber — a burst buffer, not a queue of
/// record; a lagging subscriber skips (watchers re-read on the next event).
const EVENT_CHANNEL: usize = 64;

impl Live {
    fn new(state: &StateFile) -> Self {
        Self {
            content: Mutex::new(state.content.clone()),
            events: broadcast::channel(EVENT_CHANNEL).0,
        }
    }
}

/// A method call outcome: `Ok(result)` or `Err((code, message))`.
type Dispatch = std::result::Result<Value, (String, String)>;

fn dispatch(state: &StateFile, live: &Live, method: &str, params: &Map<String, Value>) -> Dispatch {
    if let Some(err) = state.socket.errors.get(method) {
        let code = err
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("injected_error")
            .to_owned();
        let message = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("injected error")
            .to_owned();
        return Err((code, message));
    }
    if let Some(result) = state.socket.methods.get(method) {
        return Ok(result.clone());
    }
    match method {
        "ping" => Ok(json!({
            "type": "pong",
            "version": state.version,
            "protocol": state.protocol,
            "capabilities": state.capabilities(),
        })),
        "events.subscribe" => Ok(json!({"type": "subscription_started"})),
        "session.snapshot" => Ok(json!({
            "type": "session_snapshot",
            "snapshot": state.snapshot(),
        })),
        "agent.list" => Ok(json!({"type": "agent_list", "agents": state.socket.agents})),
        "pane.list" => Ok(json!({"type": "pane_list", "panes": state.panes})),
        "workspace.list" => Ok(json!({"type": "workspace_list", "workspaces": state.workspaces})),
        "tab.list" => {
            let tabs: Vec<Value> = match params.get("workspace_id").and_then(Value::as_str) {
                Some(ws) if !ws.is_empty() => state
                    .tabs
                    .iter()
                    .filter(|t| t.get("workspace_id").and_then(Value::as_str) == Some(ws))
                    .cloned()
                    .collect(),
                _ => state.tabs.clone(),
            };
            Ok(json!({"type": "tab_list", "tabs": tabs}))
        }
        "pane.read" => pane_read(state, live, params),
        "pane.send_input" | "pane.send_text" | "pane.send_keys" => Ok(json!({"type": "ok"})),
        "control.set" => control_set(live, params),
        "control.emit" => control_emit(live, params),
        "worktree.list" => worktree_list(state, params),
        // Capability probes (the Go client interprets these codes as
        // "method supported, arguments refused" — keep them stable).
        "workspace.move_block" => Err((
            "workspace_move_block_failed".to_owned(),
            "workspace_ids is required".to_owned(),
        )),
        "tab.move" => Err(("tab_not_found".to_owned(), "tab not found".to_owned())),
        _ => Err((
            "unknown_method".to_owned(),
            format!("unknown method {method}"),
        )),
    }
}

/// `control.set {pane_id, text}` — mutate the `pane.read` text so a later
/// read (a watch tick/probe) observes new content.
fn control_set(live: &Live, params: &Map<String, Value>) -> Dispatch {
    let pane_id = params.get("pane_id").and_then(Value::as_str).unwrap_or("");
    let Some(text) = params.get("text").and_then(Value::as_str) else {
        return Err((
            "invalid_params".to_owned(),
            "control.set requires pane_id and text".to_owned(),
        ));
    };
    if pane_id.is_empty() {
        return Err((
            "invalid_params".to_owned(),
            "control.set requires pane_id".to_owned(),
        ));
    }
    live.content
        .lock()
        .expect("content mutex")
        .insert(pane_id.to_owned(), text.to_owned());
    Ok(json!({"type": "ok"}))
}

/// `control.emit {name, data}` — push one event frame onto every held
/// `events.subscribe` connection. The wire shape is the exact envelope the
/// relays' event clients decode: `{"event":"<name>","data":<data>}`.
fn control_emit(live: &Live, params: &Map<String, Value>) -> Dispatch {
    let name = params
        .get("name")
        .or_else(|| params.get("event"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if name.is_empty() {
        return Err((
            "invalid_params".to_owned(),
            "control.emit requires name".to_owned(),
        ));
    }
    let frame = json!({
        "event": name,
        "data": params.get("data").cloned().unwrap_or_else(|| json!({})),
    });
    // `send` fails only with zero receivers — an emit that reaches no
    // relay is a scenario-ordering bug, so report it as an error.
    match live.events.send(frame) {
        Ok(delivered) => Ok(json!({"type": "ok", "delivered": delivered})),
        Err(_) => Err((
            "no_subscribers".to_owned(),
            format!("control.emit {name}: no events.subscribe connection is held"),
        )),
    }
}

fn pane_read(state: &StateFile, live: &Live, params: &Map<String, Value>) -> Dispatch {
    let pane_id = params.get("pane_id").and_then(Value::as_str).unwrap_or("");
    if pane_id.is_empty() {
        return Err(("pane_not_found".to_owned(), "pane is required".to_owned()));
    }
    if let Some(over) = state.socket.pane_read.get(pane_id) {
        return Ok(json!({"type": "pane_read", "read": over}));
    }
    let pane = state
        .panes
        .iter()
        .find(|p| p.get("pane_id").and_then(Value::as_str) == Some(pane_id));
    let Some(pane) = pane else {
        return Err((
            "pane_not_found".to_owned(),
            format!("pane {pane_id} not found"),
        ));
    };
    let text = live
        .content
        .lock()
        .expect("content mutex")
        .get(pane_id)
        .cloned()
        .unwrap_or_else(|| format!("fake terminal content for pane {pane_id}"));
    Ok(json!({
        "type": "pane_read",
        "read": {
            "pane_id": pane_id,
            "workspace_id": pane.get("workspace_id").cloned().unwrap_or(Value::String(String::new())),
            "tab_id": pane.get("tab_id").cloned().unwrap_or(Value::String(String::new())),
            "source": params.get("source").cloned().unwrap_or(json!("recent_unwrapped")),
            "format": params.get("format").cloned().unwrap_or(json!("text")),
            "text": text,
            "revision": pane.get("revision").cloned().unwrap_or(json!(1)),
            "truncated": false,
        }
    }))
}

fn worktree_list(state: &StateFile, params: &Map<String, Value>) -> Dispatch {
    let workspace_id = params
        .get("workspace_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    match state.socket.worktree_list.get(workspace_id) {
        Some(payload) => {
            let mut result = payload.clone();
            if let Value::Object(map) = &mut result {
                map.insert("type".to_owned(), json!("worktree_list"));
            }
            Ok(result)
        }
        None => Err((
            "workspace_not_found".to_owned(),
            format!("workspace {workspace_id} not found"),
        )),
    }
}

struct Request {
    id: String,
    method: String,
    params: Map<String, Value>,
}

fn parse_request(line: &str) -> std::result::Result<Request, String> {
    let value: Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
    let id = value
        .get("id")
        .and_then(|v| {
            v.as_str()
                .map(str::to_owned)
                .or_else(|| Some(v.to_string()))
        })
        .unwrap_or_default();
    let method = value
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing method".to_owned())?
        .to_owned();
    let params = value
        .get("params")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    Ok(Request { id, method, params })
}

/// JSONL operation log — who called what, for post-run forensics.
struct OpsLog {
    inner: Mutex<Option<std::io::BufWriter<std::fs::File>>>,
    start: Instant,
}

impl OpsLog {
    fn open(path: Option<&Path>) -> Self {
        let writer = path.and_then(|p| {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            std::fs::File::create(p).ok().map(std::io::BufWriter::new)
        });
        Self {
            inner: Mutex::new(writer),
            start: Instant::now(),
        }
    }

    fn log(&self, conn: u64, method: &str, params: &Map<String, Value>, outcome: &str) {
        use std::io::Write;
        let mut guard = self.inner.lock().expect("ops log mutex");
        let Some(w) = guard.as_mut() else { return };
        let line = json!({
            "t_ms": self.start.elapsed().as_millis() as u64,
            "conn": conn,
            "method": method,
            "params": params,
            "outcome": outcome,
        });
        let _ = writeln!(w, "{line}");
        let _ = w.flush();
    }
}

/// Serve the socket API until killed.
pub async fn serve(socket: &Path, state: &Path, ops_log: Option<PathBuf>) -> Result<()> {
    let state = Arc::new(StateFile::load(state)?);
    let live = Arc::new(Live::new(&state));
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if socket.exists() {
        std::fs::remove_file(socket)?;
    }
    let listener = UnixListener::bind(socket)?;
    let ops = Arc::new(OpsLog::open(ops_log.as_deref()));
    eprintln!(
        "lerdr-fake-herdr: serving {} (version {} protocol {})",
        socket.display(),
        state.version,
        state.protocol
    );
    let mut next_conn = 0u64;
    loop {
        let (stream, _) = listener.accept().await?;
        next_conn += 1;
        tokio::spawn(serve_conn(
            stream,
            Arc::clone(&state),
            Arc::clone(&live),
            Arc::clone(&ops),
            next_conn,
        ));
    }
}

/// Serialize + write one NDJSON value; `false` = the peer is gone.
async fn write_line(write: &mut tokio::net::unix::OwnedWriteHalf, value: &Value) -> bool {
    let mut buf = Vec::new();
    if serde_json::to_writer(&mut buf, value).is_err() {
        return false;
    }
    buf.push(b'\n');
    write.write_all(&buf).await.is_ok()
}

/// The held-open `events.subscribe` tail: the handshake reply already went
/// out; now forward `control.emit` broadcasts while discarding inbound
/// lines until the relay drops the connection.
async fn pump_events(
    lines: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    write: &mut tokio::net::unix::OwnedWriteHalf,
    events: &broadcast::Sender<Value>,
) {
    let mut rx = events.subscribe();
    loop {
        tokio::select! {
            line = lines.next_line() => {
                match line {
                    Ok(Some(_)) => {}
                    // EOF or decode error — the relay hung up.
                    _ => return,
                }
            }
            event = rx.recv() => {
                match event {
                    Ok(frame) => {
                        if !write_line(write, &frame).await {
                            return;
                        }
                    }
                    // Lagged receivers skip; the next event re-reads state.
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        }
    }
}

async fn serve_conn(
    stream: UnixStream,
    state: Arc<StateFile>,
    live: Arc<Live>,
    ops: Arc<OpsLog>,
    conn: u64,
) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    loop {
        let Ok(Some(line)) = lines.next_line().await else {
            return;
        };
        if line.trim().is_empty() {
            continue;
        }
        let response = match parse_request(&line) {
            Err(e) => json!({
                "id": "",
                "error": {"code": "invalid_request", "message": format!("invalid request: {e}")},
            }),
            Ok(req) => {
                let subscribed = req.method == "events.subscribe";
                let response = match dispatch(&state, &live, &req.method, &req.params) {
                    Ok(result) => {
                        ops.log(conn, &req.method, &req.params, "ok");
                        json!({"id": req.id, "result": result})
                    }
                    Err((code, message)) => {
                        ops.log(conn, &req.method, &req.params, &code);
                        json!({"id": req.id, "error": {"code": code, "message": message}})
                    }
                };
                if subscribed {
                    // The subscription handshake is the only reply on this
                    // connection — then the pump forwards `control.emit`
                    // frames until the relay drops it. A refused subscribe
                    // (injected error) holds the conn without broadcasts.
                    if !write_line(&mut write, &response).await {
                        return;
                    }
                    if response.get("result").is_some() {
                        pump_events(&mut lines, &mut write, &live.events).await;
                    } else {
                        while let Ok(Some(_)) = lines.next_line().await {}
                    }
                    return;
                }
                response
            }
        };
        if !write_line(&mut write, &response).await {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live(state: &StateFile) -> Live {
        Live::new(state)
    }

    fn state() -> StateFile {
        serde_json::from_value(json!({
            "version": "9.9.9",
            "protocol": 42,
            "panes": [{"pane_id": "wE:p1", "workspace_id": "wE", "tab_id": "tE",
                       "agent": "claude", "revision": 7}],
            "workspaces": [{"workspace_id": "wE", "label": "main", "number": 1}],
            "tabs": [{"tab_id": "tE", "workspace_id": "wE", "number": 1}],
            "content": {"wE:p1": "hello pane"},
            "socket": {
                "agents": [{"pane_id": "wE:p1", "agent": "claude", "name": "a"}],
                "focused_workspace_id": "wE",
                "worktree_list": {"wE": {"source": {"repo_key": "k"}, "worktrees": []}},
                "errors": {"pane.send_input": {"code": "pane_not_found", "message": "gone"}}
            }
        }))
        .unwrap()
    }

    #[test]
    fn ping_pong() {
        let s = state();
        let l = live(&s);
        let r = dispatch(&s, &l, "ping", &Map::new()).unwrap();
        assert_eq!(r["type"], "pong");
        assert_eq!(r["version"], "9.9.9");
        assert_eq!(r["protocol"], 42);
    }

    #[test]
    fn snapshot_shape() {
        let s = state();
        let l = live(&s);
        let r = dispatch(&s, &l, "session.snapshot", &Map::new()).unwrap();
        assert_eq!(r["type"], "session_snapshot");
        assert_eq!(r["snapshot"]["focused_workspace_id"], "wE");
        assert_eq!(r["snapshot"]["agents"][0]["agent"], "claude");
    }

    #[test]
    fn lists_and_reads() {
        let s = state();
        let l = live(&s);
        assert_eq!(
            dispatch(&s, &l, "agent.list", &Map::new()).unwrap()["type"],
            "agent_list"
        );
        let read = dispatch(
            &s,
            &l,
            "pane.read",
            &Map::from_iter([("pane_id".into(), json!("wE:p1"))]),
        )
        .unwrap();
        assert_eq!(read["read"]["text"], "hello pane");
        assert_eq!(read["read"]["revision"], 7);
        assert!(dispatch(
            &s,
            &l,
            "pane.read",
            &Map::from_iter([("pane_id".into(), json!("nope"))])
        )
        .is_err());
    }

    #[test]
    fn send_input_injected_error() {
        let s = state();
        let l = live(&s);
        let err = dispatch(&s, &l, "pane.send_input", &Map::new()).unwrap_err();
        assert_eq!(err.0, "pane_not_found");
    }

    #[test]
    fn worktree_lookup() {
        let s = state();
        let l = live(&s);
        let ok = dispatch(
            &s,
            &l,
            "worktree.list",
            &Map::from_iter([("workspace_id".into(), json!("wE"))]),
        )
        .unwrap();
        assert_eq!(ok["type"], "worktree_list");
        assert_eq!(ok["source"]["repo_key"], "k");
        assert!(dispatch(
            &s,
            &l,
            "worktree.list",
            &Map::from_iter([("workspace_id".into(), json!("nope"))])
        )
        .is_err());
    }

    #[test]
    fn probes_report_supported() {
        let s = state();
        let l = live(&s);
        assert_eq!(
            dispatch(&s, &l, "workspace.move_block", &Map::new())
                .unwrap_err()
                .0,
            "workspace_move_block_failed"
        );
        assert_eq!(
            dispatch(&s, &l, "tab.move", &Map::new()).unwrap_err().0,
            "tab_not_found"
        );
        assert_eq!(
            dispatch(&s, &l, "no.such", &Map::new()).unwrap_err().0,
            "unknown_method"
        );
    }

    #[test]
    fn control_set_mutates_pane_read() {
        let s = state();
        let l = live(&s);
        let set = |text: &str| {
            dispatch(
                &s,
                &l,
                "control.set",
                &Map::from_iter([
                    ("pane_id".into(), json!("wE:p1")),
                    ("text".into(), json!(text)),
                ]),
            )
            .unwrap()
        };
        set("v1\n");
        let read = dispatch(
            &s,
            &l,
            "pane.read",
            &Map::from_iter([("pane_id".into(), json!("wE:p1"))]),
        )
        .unwrap();
        assert_eq!(read["read"]["text"], "v1\n");
        set("v1\nappended\n");
        let read = dispatch(
            &s,
            &l,
            "pane.read",
            &Map::from_iter([("pane_id".into(), json!("wE:p1"))]),
        )
        .unwrap();
        assert_eq!(read["read"]["text"], "v1\nappended\n");
        // Missing args are errors, not silent mutations.
        assert!(dispatch(&s, &l, "control.set", &Map::new()).is_err());
        assert!(dispatch(
            &s,
            &l,
            "control.set",
            &Map::from_iter([("pane_id".into(), json!("wE:p1"))])
        )
        .is_err());
    }

    #[test]
    fn control_emit_broadcasts_event_envelope() {
        let s = state();
        let l = live(&s);
        // No subscribers → the emit is a scenario bug, reported as an error.
        assert_eq!(
            dispatch(
                &s,
                &l,
                "control.emit",
                &Map::from_iter([("name".into(), json!("pane.updated"))]),
            )
            .unwrap_err()
            .0,
            "no_subscribers"
        );
        let mut rx = l.events.subscribe();
        let ok = dispatch(
            &s,
            &l,
            "control.emit",
            &Map::from_iter([
                ("name".into(), json!("pane.updated")),
                ("data".into(), json!({"pane_id": "wE:p1"})),
            ]),
        )
        .unwrap();
        assert_eq!(ok["delivered"], 1);
        let frame = rx.try_recv().unwrap();
        assert_eq!(
            frame,
            json!({"event": "pane.updated", "data": {"pane_id": "wE:p1"}})
        );
        // `data` defaults to an empty object.
        let mut rx = l.events.subscribe();
        dispatch(
            &s,
            &l,
            "control.emit",
            &Map::from_iter([("name".into(), json!("workspace.updated"))]),
        )
        .unwrap();
        assert_eq!(
            rx.try_recv().unwrap(),
            json!({"event": "workspace.updated", "data": {}})
        );
        assert!(dispatch(&s, &l, "control.emit", &Map::new()).is_err());
    }

    #[test]
    fn socket_overrides_apply_to_control_methods() {
        let mut s = state();
        s.socket.errors.insert(
            "control.emit".into(),
            json!({"code": "emit_blocked", "message": "scripted"}),
        );
        let l = live(&s);
        let err = dispatch(
            &s,
            &l,
            "control.emit",
            &Map::from_iter([("name".into(), json!("pane.updated"))]),
        )
        .unwrap_err();
        assert_eq!(err.0, "emit_blocked");
    }
}
