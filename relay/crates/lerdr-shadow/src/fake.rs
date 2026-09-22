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
//! | anything else             | `unknown_method` error                      |
//!
//! `socket.errors`/`socket.methods` in the state file override per method —
//! `"errors": {"pane.send_input": {"code": "pane_not_found", "message": …}}`
//! scripts a failure without rebuilding.

use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

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

/// A method call outcome: `Ok(result)` or `Err((code, message))`.
type Dispatch = std::result::Result<Value, (String, String)>;

fn dispatch(state: &StateFile, method: &str, params: &Map<String, Value>) -> Dispatch {
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
        "pane.read" => pane_read(state, params),
        "pane.send_input" | "pane.send_text" | "pane.send_keys" => Ok(json!({"type": "ok"})),
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

fn pane_read(state: &StateFile, params: &Map<String, Value>) -> Dispatch {
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
    let text = state
        .content
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
            Arc::clone(&ops),
            next_conn,
        ));
    }
}

async fn serve_conn(stream: UnixStream, state: Arc<StateFile>, ops: Arc<OpsLog>, conn: u64) {
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
                let response = match dispatch(&state, &req.method, &req.params) {
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
                    // connection — keep reading (and discarding) so the
                    // stream stays open until the relay drops it.
                    let mut buf = Vec::new();
                    let _ = serde_json::to_writer(&mut buf, &response);
                    buf.push(b'\n');
                    if write.write_all(&buf).await.is_err() {
                        return;
                    }
                    while let Ok(Some(_)) = lines.next_line().await {}
                    return;
                }
                response
            }
        };
        let mut buf = Vec::new();
        let _ = serde_json::to_writer(&mut buf, &response);
        buf.push(b'\n');
        if write.write_all(&buf).await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let r = dispatch(&s, "ping", &Map::new()).unwrap();
        assert_eq!(r["type"], "pong");
        assert_eq!(r["version"], "9.9.9");
        assert_eq!(r["protocol"], 42);
    }

    #[test]
    fn snapshot_shape() {
        let s = state();
        let r = dispatch(&s, "session.snapshot", &Map::new()).unwrap();
        assert_eq!(r["type"], "session_snapshot");
        assert_eq!(r["snapshot"]["focused_workspace_id"], "wE");
        assert_eq!(r["snapshot"]["agents"][0]["agent"], "claude");
    }

    #[test]
    fn lists_and_reads() {
        let s = state();
        assert_eq!(
            dispatch(&s, "agent.list", &Map::new()).unwrap()["type"],
            "agent_list"
        );
        let read = dispatch(
            &s,
            "pane.read",
            &Map::from_iter([("pane_id".into(), json!("wE:p1"))]),
        )
        .unwrap();
        assert_eq!(read["read"]["text"], "hello pane");
        assert_eq!(read["read"]["revision"], 7);
        assert!(dispatch(
            &s,
            "pane.read",
            &Map::from_iter([("pane_id".into(), json!("nope"))])
        )
        .is_err());
    }

    #[test]
    fn send_input_injected_error() {
        let s = state();
        let err = dispatch(&s, "pane.send_input", &Map::new()).unwrap_err();
        assert_eq!(err.0, "pane_not_found");
    }

    #[test]
    fn worktree_lookup() {
        let s = state();
        let ok = dispatch(
            &s,
            "worktree.list",
            &Map::from_iter([("workspace_id".into(), json!("wE"))]),
        )
        .unwrap();
        assert_eq!(ok["type"], "worktree_list");
        assert_eq!(ok["source"]["repo_key"], "k");
        assert!(dispatch(
            &s,
            "worktree.list",
            &Map::from_iter([("workspace_id".into(), json!("nope"))])
        )
        .is_err());
    }

    #[test]
    fn probes_report_supported() {
        let s = state();
        assert_eq!(
            dispatch(&s, "workspace.move_block", &Map::new())
                .unwrap_err()
                .0,
            "workspace_move_block_failed"
        );
        assert_eq!(
            dispatch(&s, "tab.move", &Map::new()).unwrap_err().0,
            "tab_not_found"
        );
        assert_eq!(
            dispatch(&s, "no.such", &Map::new()).unwrap_err().0,
            "unknown_method"
        );
    }
}
