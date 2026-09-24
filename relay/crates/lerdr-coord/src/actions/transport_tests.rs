//! Fake-Herdr transport tests: a scripted [`Transport`] records every
//! request line and replies per method, so handler tests can assert the
//! exact RPC method and params that reach the socket plus the outcome
//! classification the reply produces.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use lerdr_core::protocol::Inbound;
use lerdr_herdr::{AgentInfo, BoxIo, Client, ClientConfig, SessionSnapshot, Transport};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use crate::topology::Topology;
use crate::TopologyActor;

use super::{leases::Leases, profiles::Resolver, ActionContext};

/// One scripted connection outcome.
enum Step {
    /// `{"id":…,"result":<value>}` then close.
    Reply(Value),
    /// `{"id":…,"error":{"code","message"}}` then close — a confirmed refusal.
    Refuse(&'static str, &'static str),
    /// Read the request then close without replying — bytes reached the
    /// socket, the outcome is unknown.
    HangUp,
}

/// Scripted server state shared by every dialed connection.
struct ScriptInner {
    /// Per-method FIFO of steps; an empty queue answers
    /// `Reply({"type":"ok"})`.
    by_method: HashMap<String, VecDeque<Step>>,
    /// Every decoded request line, in accept order.
    requests: Vec<(String, Value)>,
}

/// Clone-cheap handle the transport and the test share.
#[derive(Clone)]
struct Script {
    inner: Arc<Mutex<ScriptInner>>,
}

impl Script {
    fn new(steps: impl IntoIterator<Item = (&'static str, Step)>) -> Self {
        let mut by_method: HashMap<String, VecDeque<Step>> = HashMap::new();
        for (method, step) in steps {
            by_method
                .entry(method.to_owned())
                .or_default()
                .push_back(step);
        }
        Script {
            inner: Arc::new(Mutex::new(ScriptInner {
                by_method,
                requests: Vec::new(),
            })),
        }
    }

    /// Recorded `(method, params)` pairs minus the topology supervisor's
    /// `events.subscribe`/`session.snapshot` sync traffic, in accept order.
    fn requests(&self) -> Vec<(String, Value)> {
        self.inner
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|(method, _)| {
                !matches!(method.as_str(), "events.subscribe" | "session.snapshot")
            })
            .cloned()
            .collect()
    }

    fn record(&self, request: &Value) {
        self.inner.lock().unwrap().requests.push((
            request["method"].as_str().unwrap_or_default().to_owned(),
            request["params"].clone(),
        ));
    }

    /// Client wired to this script.
    fn client(&self) -> Client {
        Client::new(
            Arc::new(ScriptTransport {
                script: self.clone(),
            }),
            ClientConfig::default(),
        )
    }
}

struct ScriptTransport {
    script: Script,
}

impl Transport for ScriptTransport {
    fn dial(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<BoxIo>> + Send>> {
        let script = self.script.clone();
        Box::pin(async move {
            let (client_end, server_end) = tokio::io::duplex(64 * 1024);
            tokio::spawn(serve(server_end, script));
            Ok(Box::new(client_end) as BoxIo)
        })
    }

    fn describe(&self) -> String {
        "script".to_owned()
    }
}

/// Read one request line, record it, apply the method's scripted step.
async fn serve(mut conn: tokio::io::DuplexStream, script: Script) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let line_end = loop {
        match conn.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                    break pos;
                }
            }
        }
    };
    let request: Value = serde_json::from_slice(&buf[..line_end]).unwrap_or_default();
    script.record(&request);
    let method = request["method"].as_str().unwrap_or_default().to_owned();
    let id = request["id"].as_str().unwrap_or_default().to_owned();
    let step = script
        .inner
        .lock()
        .unwrap()
        .by_method
        .get_mut(&method)
        .and_then(VecDeque::pop_front)
        .unwrap_or(Step::Reply(json!({ "type": "ok" })));
    match step {
        Step::Reply(result) => {
            let line = json!({ "id": id, "result": result });
            let _ = conn.write_all(line.to_string().as_bytes()).await;
            let _ = conn.write_all(b"\n").await;
        }
        Step::Refuse(code, message) => {
            let line = json!({
                "id": id,
                "error": { "code": code, "message": message },
            });
            let _ = conn.write_all(line.to_string().as_bytes()).await;
            let _ = conn.write_all(b"\n").await;
        }
        Step::HangUp => {}
    }
}

/// An `ActionContext` wired to the script — see
/// [`context_with_client`].
fn context(script: &Script, agents: Vec<AgentInfo>) -> ActionContext {
    context_full(script.client(), agents, Vec::new(), Vec::new(), &[])
}

/// Same, with workspaces projected into the topology snapshot.
fn context_topo(
    script: &Script,
    agents: Vec<AgentInfo>,
    workspaces: Vec<lerdr_herdr::WorkspaceInfo>,
) -> ActionContext {
    context_full(script.client(), agents, workspaces, Vec::new(), &[])
}

/// Same, with pane rows — `PaneInfo.revision` seeds the upstream
/// `content_revision` watermark and `PaneInfo.scroll` the link actions'
/// `offset_from_bottom`.
fn context_panes(
    script: &Script,
    agents: Vec<AgentInfo>,
    panes: Vec<lerdr_herdr::PaneInfo>,
) -> ActionContext {
    context_full(script.client(), agents, Vec::new(), panes, &[])
}

/// Same, plus capability-ledger rows (`(method, state)` — e.g.
/// `("pane.copy_search", "unsupported")` refutes the method before
/// dispatch).
fn context_features(
    script: &Script,
    agents: Vec<AgentInfo>,
    features: &[(&str, &str)],
) -> ActionContext {
    context_full(script.client(), agents, Vec::new(), Vec::new(), features)
}

/// `ActionContext` over an arbitrary client — the plain snapshot path.
fn context_with_client(
    client: Client,
    agents: Vec<AgentInfo>,
    workspaces: Vec<lerdr_herdr::WorkspaceInfo>,
) -> ActionContext {
    context_full(client, agents, workspaces, Vec::new(), &[])
}

/// `ActionContext` over an arbitrary client: the topology snapshot carries
/// `agents`/`workspaces`/`panes`, `features` seeds the capability ledger
/// rows `herdr_status.features` projects, the profile resolver points at
/// an empty config dir, and the spawned supervisor shares the transport
/// (its `events.subscribe` requests are filtered out of
/// [`Script::requests`]).
fn context_full(
    client: Client,
    agents: Vec<AgentInfo>,
    workspaces: Vec<lerdr_herdr::WorkspaceInfo>,
    panes: Vec<lerdr_herdr::PaneInfo>,
    features: &[(&str, &str)],
) -> ActionContext {
    let mut topology = Topology::default();
    topology.accept(SessionSnapshot {
        agents,
        workspaces,
        panes,
        ..SessionSnapshot::default()
    });
    if !features.is_empty() {
        topology.herdr_status.features = lerdr_core::json::MaybeNull::Value(
            features
                .iter()
                .map(|(name, state)| {
                    (
                        (*name).to_owned(),
                        lerdr_core::protocol::HerdrFeatureStatus {
                            state: (*state).to_owned(),
                            reason: "schema_absent".to_owned(),
                            generation: 1,
                        },
                    )
                })
                .collect(),
        );
    }
    ActionContext {
        handle: TopologyActor::spawn(client.clone(), CancellationToken::new()),
        leases: Leases::new(client.clone()),
        profiles: Resolver::with_config_home(tempfile::tempdir().expect("tempdir").keep()),
        questions: crate::actions::questions::Questions::default(),
        uploads: crate::actions::uploads::Uploads::new(
            tempfile::tempdir().expect("tempdir").keep(),
        ),
        activities: crate::actions::activity::Journal::default(),
        push: crate::actions::push::Push::default(),
        speech: crate::actions::speech::Speech::default(),
        notices: crate::actions::Notices::default(),
        audit: None,
        device_id: "test-device".to_owned(),
        client,
        topology: Arc::new(topology),
        client_id: "test-client".to_owned(),
    }
}

fn agent(pane_id: &str, tab_id: &str, agent: &str) -> AgentInfo {
    AgentInfo {
        pane_id: pane_id.to_owned(),
        tab_id: tab_id.to_owned(),
        agent: Some(agent.to_owned()),
        ..AgentInfo::default()
    }
}

fn message(map: serde_json::Map<String, Value>) -> Inbound {
    Inbound::decode_map(&map).expect("decode inbound")
}

/// Decoded frame pair: `(ok, command_result phase, receipt phase, receipt
/// error code, data)`.
struct Frames {
    ok: bool,
    phase: String,
    receipt_phase: String,
    receipt_code: Option<String>,
    data: Option<Value>,
}

fn frames_of(frames: Vec<lerdr_core::protocol::Outbound>) -> Frames {
    let mut iter = frames.into_iter();
    let (ok, phase, data) = match iter.next() {
        Some(lerdr_core::protocol::Outbound::CommandResult(m)) => (
            m.ok.unwrap_or(false),
            m.phase.unwrap_or_default(),
            m.data
                .and_then(|d| d.into_value())
                .map(|r| serde_json::from_str::<Value>(r.get()).unwrap_or_default()),
        ),
        other => panic!("expected command_result, got {other:?}"),
    };
    let (receipt_phase, receipt_code) = match iter.next() {
        Some(lerdr_core::protocol::Outbound::ActionReceipt(m)) => {
            let receipt = m.receipt.expect("receipt payload");
            (
                receipt.phase.as_str().to_owned(),
                receipt.error.map(|e| e.code),
            )
        }
        other => panic!("expected action_receipt, got {other:?}"),
    };
    assert!(iter.next().is_none(), "trailing frame after receipt");
    Frames {
        ok,
        phase,
        receipt_phase,
        receipt_code,
        data,
    }
}

// ── input actions ---------------------------------------------------------

#[tokio::test]
async fn send_text_dispatches_pane_send_text() {
    let script = Script::new([]);
    let ctx = context(&script, vec![]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("send_text")),
        ("pane_id".into(), json!("wE:p1")),
        ("text".into(), json!("hello")),
    ]));
    let frames = frames_of(super::input::send_text(ctx, "r1", "a1", &msg).await);
    assert!(frames.ok);
    assert_eq!(frames.phase, "completed");
    assert_eq!(frames.receipt_phase, "confirmed");
    let requests = script.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "pane.send_text");
    assert_eq!(
        requests[0].1,
        json!({ "pane_id": "wE:p1", "text": "hello" })
    );
}

#[tokio::test]
async fn send_keys_lone_shift_tab_routes_through_send_text() {
    let script = Script::new([]);
    let ctx = context(&script, vec![]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("send_keys")),
        ("pane_id".into(), json!("wE:p1")),
        ("keys".into(), json!(["shift+tab"])),
    ]));
    let frames = frames_of(super::input::send_keys(ctx, "r1", "a1", &msg).await);
    assert!(frames.ok);
    let requests = script.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "pane.send_text");
    assert_eq!(
        requests[0].1,
        json!({ "pane_id": "wE:p1", "text": "\u{1b}[Z" })
    );
}

#[tokio::test]
async fn send_keys_normalized_ctrl_letter() {
    let script = Script::new([]);
    let ctx = context(&script, vec![]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("send_keys")),
        ("pane_id".into(), json!("wE:p1")),
        ("keys".into(), json!(["ctrl+C"])),
    ]));
    let frames = frames_of(super::input::send_keys(ctx, "r1", "a1", &msg).await);
    assert!(frames.ok);
    let requests = script.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "pane.send_keys");
    assert_eq!(
        requests[0].1,
        json!({ "pane_id": "wE:p1", "keys": ["ctrl+c"] })
    );
}

#[tokio::test]
async fn send_secret_sends_one_key_per_rune_plus_enter() {
    let script = Script::new([]);
    let ctx = context(&script, vec![]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("send_secret")),
        ("pane_id".into(), json!("wE:p1")),
        ("text".into(), json!("hé")),
    ]));
    let frames = frames_of(super::input::send_secret(ctx, "r1", "a1", &msg).await);
    assert!(frames.ok);
    let requests = script.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "pane.send_keys");
    assert_eq!(
        requests[0].1,
        json!({ "pane_id": "wE:p1", "keys": ["h", "é", "Enter"] })
    );
}

#[tokio::test]
async fn send_input_dispatches_normalized_input() {
    let script = Script::new([]);
    let ctx = context(&script, vec![]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("send_input")),
        ("pane_id".into(), json!("wE:p1")),
        ("text".into(), json!("ls")),
        ("keys".into(), json!(["enter", "shift+ctrl+up"])),
    ]));
    let frames = frames_of(super::input::send_input(ctx, "r1", "a1", &msg).await);
    assert!(frames.ok);
    let requests = script.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "pane.send_input");
    assert_eq!(
        requests[0].1,
        json!({
            "pane_id": "wE:p1",
            "text": "ls",
            "keys": ["Enter", "ctrl+shift+Up"],
        })
    );
}

#[tokio::test]
async fn submit_prompt_uses_agent_prompt_for_regular_agents() {
    let script = Script::new([]);
    let ctx = context(&script, vec![agent("wE:p1", "wE:t1", "claude")]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("submit_prompt")),
        ("pane_id".into(), json!("wE:p1")),
        ("text".into(), json!("do it")),
    ]));
    let frames = frames_of(super::input::submit_prompt(ctx, "r1", "a1", &msg).await);
    assert!(frames.ok);
    let requests = script.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "agent.prompt");
    assert_eq!(requests[0].1, json!({ "target": "wE:p1", "text": "do it" }));
}

#[tokio::test]
async fn submit_prompt_qoder_sends_text_then_enter() {
    let script = Script::new([]);
    let ctx = context(&script, vec![agent("wE:p1", "wE:t1", "QoderCLI")]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("submit_prompt")),
        ("pane_id".into(), json!("wE:p1")),
        ("text".into(), json!("do it")),
    ]));
    let frames = frames_of(super::input::submit_prompt(ctx, "r1", "a1", &msg).await);
    assert!(frames.ok);
    let requests = script.requests();
    assert_eq!(
        requests,
        vec![
            (
                "pane.send_text".to_owned(),
                json!({ "pane_id": "wE:p1", "text": "do it" })
            ),
            (
                "pane.send_keys".to_owned(),
                json!({ "pane_id": "wE:p1", "keys": ["Enter"] })
            ),
        ]
    );
}

#[tokio::test]
async fn submit_prompt_qoder_enter_failure_is_partially_applied() {
    let script = Script::new([
        ("pane.send_text", Step::Reply(json!({ "type": "ok" }))),
        ("pane.send_keys", Step::HangUp),
    ]);
    let ctx = context(&script, vec![agent("wE:p1", "wE:t1", "qoder")]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("submit_prompt")),
        ("pane_id".into(), json!("wE:p1")),
        ("text".into(), json!("do it")),
    ]));
    let frames = frames_of(super::input::submit_prompt(ctx, "r1", "a1", &msg).await);
    assert!(!frames.ok);
    assert_eq!(frames.phase, "dispatched_unknown");
    assert_eq!(frames.receipt_phase, "dispatched_unknown");
    assert_eq!(frames.data, Some(json!({ "dispatched_unknown": true })));
}

#[tokio::test]
async fn agent_stop_dispatches_pane_close() {
    let script = Script::new([]);
    let ctx = context(&script, vec![agent("wE:p1", "wE:t1", "claude")]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("agent_stop")),
        ("pane_id".into(), json!("wE:p1")),
    ]));
    let frames = frames_of(super::input::agent_stop(ctx, "r1", "a1", &msg).await);
    assert!(frames.ok);
    let requests = script.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "pane.close");
    assert_eq!(requests[0].1, json!({ "pane_id": "wE:p1" }));
}

// ── tabs ------------------------------------------------------------------

#[tokio::test]
async fn agent_rename_targets_the_panes_tab() {
    let script = Script::new([]);
    let ctx = context(&script, vec![agent("wE:p1", "wE:t1", "claude")]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("agent_rename")),
        ("pane_id".into(), json!("wE:p1")),
        ("name".into(), json!("  new label  ")),
    ]));
    let frames = frames_of(super::tabs::agent_rename(ctx, "r1", "a1", &msg).await);
    assert!(frames.ok);
    let requests = script.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "tab.rename");
    assert_eq!(
        requests[0].1,
        json!({ "tab_id": "wE:t1", "label": "new label" })
    );
}

#[tokio::test]
async fn tab_reorder_moves_the_panes_tab() {
    let script = Script::new([]);
    let ctx = context(&script, vec![agent("wE:p1", "wE:t1", "claude")]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("tab_reorder")),
        ("pane_id".into(), json!("wE:p1")),
        ("insert_index".into(), json!(3)),
    ]));
    let frames = frames_of(super::tabs::tab_reorder(ctx, "r1", "a1", &msg).await);
    assert!(frames.ok);
    assert_eq!(frames.data, Some(json!({ "insert_index": 3 })));
    let requests = script.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "tab.move");
    assert_eq!(
        requests[0].1,
        json!({ "tab_id": "wE:t1", "insert_index": 3 })
    );
}

// ── outcome classification -------------------------------------------------

#[tokio::test]
async fn refusal_is_confirmed_receipt_with_not_started_result() {
    let script = Script::new([("pane.send_text", Step::Refuse("pane_not_found", "gone"))]);
    let ctx = context(&script, vec![]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("send_text")),
        ("pane_id".into(), json!("wE:p1")),
        ("text".into(), json!("hi")),
    ]));
    let frames = frames_of(super::input::send_text(ctx, "r1", "a1", &msg).await);
    assert!(!frames.ok);
    assert_eq!(frames.phase, "not_started");
    assert_eq!(frames.receipt_phase, "confirmed");
    assert_eq!(frames.receipt_code.as_deref(), Some("pane_not_found"));
    assert_eq!(frames.data, Some(json!({ "code": "pane_not_found" })));
}

#[tokio::test]
async fn transient_refusal_carries_no_code_data() {
    let script = Script::new([("pane.send_text", Step::Refuse("agent_pane_busy", "busy"))]);
    let ctx = context(&script, vec![]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("send_text")),
        ("pane_id".into(), json!("wE:p1")),
        ("text".into(), json!("hi")),
    ]));
    let frames = frames_of(super::input::send_text(ctx, "r1", "a1", &msg).await);
    assert!(!frames.ok);
    assert_eq!(frames.phase, "not_started");
    assert_eq!(frames.receipt_phase, "confirmed");
    assert_eq!(frames.receipt_code.as_deref(), Some("agent_pane_busy"));
    assert_eq!(frames.data, None);
}

#[tokio::test]
async fn hangup_after_write_is_dispatched_unknown() {
    let script = Script::new([("pane.send_text", Step::HangUp)]);
    let ctx = context(&script, vec![]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("send_text")),
        ("pane_id".into(), json!("wE:p1")),
        ("text".into(), json!("hi")),
    ]));
    let frames = frames_of(super::input::send_text(ctx, "r1", "a1", &msg).await);
    assert!(!frames.ok);
    assert_eq!(frames.phase, "dispatched_unknown");
    assert_eq!(frames.receipt_phase, "dispatched_unknown");
    assert_eq!(
        frames.receipt_code.as_deref(),
        Some("dispatch_outcome_unknown")
    );
}

#[tokio::test]
async fn unreachable_socket_fails_before_dispatch() {
    let dir = tempfile::tempdir().expect("tempdir");
    let client = Client::unix(dir.path().join("missing.sock"));
    let ctx = context_with_client(client, vec![], Vec::new());
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("send_text")),
        ("pane_id".into(), json!("wE:p1")),
        ("text".into(), json!("hi")),
    ]));
    let frames = frames_of(super::input::send_text(ctx, "r1", "a1", &msg).await);
    assert!(!frames.ok);
    assert_eq!(frames.phase, "not_started");
    assert_eq!(frames.receipt_phase, "failed_before_dispatch");
    assert_eq!(frames.receipt_code.as_deref(), Some("herdr_unreachable"));
}

// ── workspace / created-target extraction ---------------------------------

#[tokio::test]
async fn workspace_create_extracts_nested_result_ids() {
    let home = super::workspace::home_dir().expect("home");
    let cwd = tempfile::tempdir_in(&home).expect("tempdir in home");
    let resolved = std::fs::canonicalize(cwd.path()).expect("canonical");
    let script = Script::new([(
        "workspace.create",
        Step::Reply(json!({
            "type": "workspace_created",
            "workspace": { "workspace_id": "wT" },
            "tab": { "tab_id": "wT:t1" },
            "root_pane": { "pane_id": "wT:p1" },
        })),
    )]);
    let ctx = context(&script, vec![]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("workspace_create")),
        ("label".into(), json!("work")),
        ("cwd".into(), json!(resolved.to_string_lossy())),
    ]));
    let frames = frames_of(super::workspace::workspace_create(ctx, "r1", "a1", &msg).await);
    assert!(frames.ok, "frames: {:?}", frames.data);
    let requests = script.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "workspace.create");
    assert_eq!(
        requests[0].1,
        json!({
            "cwd": resolved.to_string_lossy().to_string(),
            "label": "work",
            "focus": false,
        })
    );
    let data = frames.data.expect("create data");
    assert_eq!(data["workspace_id"], json!("wT"));
    assert_eq!(data["tab_id"], json!("wT:t1"));
    assert_eq!(data["pane_id"], json!("wT:p1"));
}

#[tokio::test]
async fn workspace_create_without_root_pane_is_dispatched_unknown() {
    let home = super::workspace::home_dir().expect("home");
    let cwd = tempfile::tempdir_in(&home).expect("tempdir in home");
    let resolved = std::fs::canonicalize(cwd.path()).expect("canonical");
    let script = Script::new([(
        "workspace.create",
        Step::Reply(json!({ "type": "workspace_created" })),
    )]);
    let ctx = context(&script, vec![]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("workspace_create")),
        ("label".into(), json!("work")),
        ("cwd".into(), json!(resolved.to_string_lossy())),
    ]));
    let frames = frames_of(super::workspace::workspace_create(ctx, "r1", "a1", &msg).await);
    assert!(!frames.ok);
    assert_eq!(frames.phase, "dispatched_unknown");
    assert_eq!(frames.receipt_phase, "dispatched_unknown");
}

// ── agent_start lifecycle ---------------------------------------------------

/// `Lifecycle.Start` through the argv path: profile `sh` resolves on PATH
/// (kind is empty → `pane.send_input` + `agent.get` + `agent.rename`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_start_argv_lifecycle_matches_oracle_call_sequence() {
    let home = super::workspace::home_dir().expect("home");
    let cwd = tempfile::tempdir_in(&home).expect("tempdir in home");
    let resolved = std::fs::canonicalize(cwd.path()).expect("canonical");

    // Profile `sh` via INI — `binary_path("sh")` resolves on every host.
    let config_home = tempfile::tempdir().expect("config tempdir");
    std::fs::create_dir_all(config_home.path().join("herdr")).unwrap();
    std::fs::write(
        config_home.path().join("herdr/agent-profiles.ini"),
        "[config]\nreplace_profiles = true\n[profiles]\nsh = Shell\n",
    )
    .unwrap();

    // The lifecycle reads `agent.list` twice — `reconcileExisting`, then
    // the workspace-selection inventory.
    let script = Script::new([
        (
            "agent.list",
            Step::Reply(json!({ "type": "agent_list", "agents": [] })),
        ),
        (
            "agent.list",
            Step::Reply(json!({ "type": "agent_list", "agents": [] })),
        ),
        (
            "workspace.list",
            Step::Reply(json!({ "type": "workspace_list", "workspaces": [] })),
        ),
        (
            "workspace.create",
            Step::Reply(json!({
                "type": "workspace_created",
                "workspace": { "workspace_id": "wT" },
                "tab": { "tab_id": "wT:t1" },
                "root_pane": { "pane_id": "wT:p1" },
            })),
        ),
        (
            "agent.get",
            Step::Reply(json!({
                "type": "agent_info",
                "agent": {
                    "pane_id": "wT:p1",
                    "terminal_id": "term_1",
                    "workspace_id": "wT",
                    "tab_id": "wT:t1",
                    "focused": false,
                    "agent_status": "idle",
                    "agent": "sh",
                    "revision": 1,
                },
            })),
        ),
    ]);
    let client = script.client();
    let ctx = {
        let mut topology = Topology::default();
        topology.accept(SessionSnapshot::default());
        ActionContext {
            handle: TopologyActor::spawn(client.clone(), CancellationToken::new()),
            leases: Leases::new(client.clone()),
            profiles: Resolver::with_config_home(config_home.path().to_owned()),
            questions: crate::actions::questions::Questions::default(),
            uploads: crate::actions::uploads::Uploads::new(
                tempfile::tempdir().expect("tempdir").keep(),
            ),
            activities: crate::actions::activity::Journal::default(),
            push: crate::actions::push::Push::default(),
            speech: crate::actions::speech::Speech::default(),
            notices: crate::actions::Notices::default(),
            audit: None,
            device_id: "test-device".to_owned(),
            client,
            topology: Arc::new(topology),
            client_id: "test-client".to_owned(),
        }
    };
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("agent_start")),
        ("profile_id".into(), json!("sh")),
        ("name".into(), json!("shell-1")),
        ("cwd".into(), json!(resolved.to_string_lossy())),
    ]));
    let frames = frames_of(super::agents::agent_start(ctx, "r1", "a1", &msg).await);
    assert!(frames.ok, "frames: {:?}", frames.data);
    assert_eq!(frames.phase, "completed");
    assert_eq!(frames.receipt_phase, "confirmed");

    let requests = script.requests();
    let methods: Vec<&str> = requests.iter().map(|(m, _)| m.as_str()).collect();
    assert_eq!(
        methods,
        vec![
            "integration.list",
            "agent.list",
            "agent.list",
            "workspace.list",
            "workspace.create",
            "tab.rename",
            "pane.send_input",
            "agent.get",
            "agent.rename",
        ]
    );
    // `workspace.create` gets the cwd + basename label, never focus.
    assert_eq!(requests[4].1["cwd"], json!(resolved.to_string_lossy()));
    assert_eq!(requests[4].1["focus"], json!(false));
    // The fresh tab is renamed to the agent name.
    assert_eq!(
        requests[5].1,
        json!({ "tab_id": "wT:t1", "label": "shell-1" })
    );
    // The argv profile runs through pane input — shell-joined + Enter.
    assert_eq!(requests[6].0, "pane.send_input");
    assert_eq!(requests[6].1["pane_id"], json!("wT:p1"));
    assert_eq!(requests[6].1["keys"], json!(["Enter"]));
    assert!(requests[6].1["text"].as_str().unwrap().ends_with("sh"));
    // Detection poll then rename.
    assert_eq!(requests[7].1, json!({ "target": "wT:p1" }));
    assert_eq!(
        requests[8].1,
        json!({ "target": "wT:p1", "name": "shell-1" })
    );
}

/// A kind profile goes through `agent.start` with the transient-refusal
/// retry: one `agent_pane_busy`, then success.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_start_kind_retries_pane_busy() {
    let home = super::workspace::home_dir().expect("home");
    let cwd = tempfile::tempdir_in(&home).expect("tempdir in home");
    let resolved = std::fs::canonicalize(cwd.path()).expect("canonical");

    // `integration.list` supplies the `claude` profile — integrations get
    // `kind = id` without needing a PATH binary.
    let script = Script::new([
        (
            "integration.list",
            Step::Reply(json!({ "integrations": [{ "target": "claude", "state": "current" }] })),
        ),
        (
            "agent.list",
            Step::Reply(json!({ "type": "agent_list", "agents": [] })),
        ),
        (
            "agent.list",
            Step::Reply(json!({ "type": "agent_list", "agents": [] })),
        ),
        (
            "workspace.list",
            Step::Reply(json!({ "type": "workspace_list", "workspaces": [] })),
        ),
        (
            "workspace.create",
            Step::Reply(json!({
                "type": "workspace_created",
                "workspace": { "workspace_id": "wT" },
                "tab": { "tab_id": "wT:t1" },
                "root_pane": { "pane_id": "wT:p1" },
            })),
        ),
        (
            "agent.start",
            Step::Refuse("agent_pane_busy", "pane is still starting"),
        ),
        ("agent.start", Step::Reply(json!({ "type": "ok" }))),
    ]);
    let client = script.client();
    let ctx = {
        let mut topology = Topology::default();
        topology.accept(SessionSnapshot::default());
        ActionContext {
            handle: TopologyActor::spawn(client.clone(), CancellationToken::new()),
            leases: Leases::new(client.clone()),
            profiles: Resolver::with_config_home(tempfile::tempdir().expect("tempdir").keep()),
            questions: crate::actions::questions::Questions::default(),
            uploads: crate::actions::uploads::Uploads::new(
                tempfile::tempdir().expect("tempdir").keep(),
            ),
            activities: crate::actions::activity::Journal::default(),
            push: crate::actions::push::Push::default(),
            speech: crate::actions::speech::Speech::default(),
            notices: crate::actions::Notices::default(),
            audit: None,
            device_id: "test-device".to_owned(),
            client,
            topology: Arc::new(topology),
            client_id: "test-client".to_owned(),
        }
    };
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("agent_start")),
        ("profile_id".into(), json!("claude")),
        ("name".into(), json!("agent-1")),
        ("cwd".into(), json!(resolved.to_string_lossy())),
    ]));
    let frames = frames_of(super::agents::agent_start(ctx, "r1", "a1", &msg).await);
    assert!(frames.ok, "frames: {:?}", frames.data);
    let requests = script.requests();
    let methods: Vec<&str> = requests.iter().map(|(m, _)| m.as_str()).collect();
    assert_eq!(
        methods,
        vec![
            "integration.list",
            "agent.list",
            "agent.list",
            "workspace.list",
            "workspace.create",
            "tab.rename",
            "agent.start",
            "agent.start",
        ]
    );
    // Both attempts carry name/kind/pane_id/timeout_ms.
    for request in &requests[6..] {
        assert_eq!(request.1["name"], json!("agent-1"));
        assert_eq!(request.1["kind"], json!("claude"));
        assert_eq!(request.1["pane_id"], json!("wT:p1"));
        assert!(request.1["timeout_ms"].as_u64().unwrap_or_default() > 0);
    }
}

// ── worktree / workspace_close ---------------------------------------------

fn workspace(id: &str) -> lerdr_herdr::WorkspaceInfo {
    lerdr_herdr::WorkspaceInfo {
        workspace_id: id.to_owned(),
        ..lerdr_herdr::WorkspaceInfo::default()
    }
}

fn linked_workspace(id: &str, repo_key: &str) -> lerdr_herdr::WorkspaceInfo {
    lerdr_herdr::WorkspaceInfo {
        workspace_id: id.to_owned(),
        worktree: Some(lerdr_herdr::WorkspaceWorktreeInfo {
            repo_key: repo_key.to_owned(),
            is_linked_worktree: true,
            ..lerdr_herdr::WorkspaceWorktreeInfo::default()
        }),
        ..lerdr_herdr::WorkspaceInfo::default()
    }
}

#[tokio::test]
async fn worktree_open_dispatches_path_params() {
    let script = Script::new([(
        "worktree.open",
        Step::Reply(json!({
            "type": "worktree_opened",
            "workspace": { "workspace_id": "wX" },
            "root_pane": { "pane_id": "wX:p1" },
            "tab": { "tab_id": "wX:t1" },
        })),
    )]);
    let ctx = context_topo(&script, vec![], vec![workspace("wE")]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("worktree_open")),
        ("workspace_id".into(), json!("wE")),
        ("path".into(), json!("/tmp/wt")),
    ]));
    let frames = frames_of(super::worktree::worktree_open(ctx, "r1", "a1", &msg).await);
    assert!(frames.ok);
    let requests = script.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "worktree.open");
    assert_eq!(
        requests[0].1,
        json!({ "workspace_id": "wE", "path": "/tmp/wt", "focus": false })
    );
    // The `type` envelope tag is stripped from the forwarded data.
    assert_eq!(frames.data.expect("data")["type"], Value::Null);
}

#[tokio::test]
async fn worktree_remove_dirty_refusal_offers_force() {
    let script = Script::new([(
        "worktree.remove",
        Step::Refuse("dirty_worktree_requires_force", "worktree is dirty"),
    )]);
    let ctx = context_topo(&script, vec![], vec![linked_workspace("wE", "repo")]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("worktree_remove")),
        ("workspace_id".into(), json!("wE")),
        ("force".into(), json!(false)),
    ]));
    let frames = frames_of(super::worktree::worktree_remove(ctx, "r1", "a1", &msg).await);
    assert!(!frames.ok);
    assert_eq!(frames.phase, "not_started");
    assert_eq!(frames.receipt_phase, "confirmed");
    assert_eq!(
        frames.receipt_code.as_deref(),
        Some("dirty_worktree_requires_force")
    );
    assert_eq!(
        frames.data,
        Some(json!({
            "code": "dirty_worktree_requires_force",
            "force_available": true,
        }))
    );
    assert_eq!(
        script.requests()[0].1,
        json!({ "workspace_id": "wE", "force": false })
    );
}

#[tokio::test]
async fn workspace_close_dispatches_close_params_and_affected_ids() {
    let script = Script::new([(
        "workspace.list",
        Step::Reply(json!({
            "type": "workspace_list",
            "workspaces": [{
                "workspace_id": "wE",
                "number": 1,
                "label": "work",
                "focused": false,
                "pane_count": 1,
                "tab_count": 1,
                "active_tab_id": "",
                "agent_status": "unknown",
            }],
        })),
    )]);
    let ctx = context(&script, vec![]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("workspace_close")),
        ("workspace_id".into(), json!("wE")),
    ]));
    let frames = frames_of(super::workspace::workspace_close(ctx, "r1", "a1", &msg).await);
    assert!(frames.ok, "frames: {:?}", frames.data);
    let requests = script.requests();
    assert_eq!(
        requests.iter().map(|(m, _)| m.as_str()).collect::<Vec<_>>(),
        vec!["workspace.list", "workspace.close"]
    );
    assert_eq!(
        requests[1].1,
        json!({ "workspace_id": "wE", "close_group": false })
    );
    let data = frames.data.expect("close data");
    assert_eq!(data["workspace_id"], json!("wE"));
    assert_eq!(data["workspace_ids"], json!(["wE"]));
}

// ── focus actions (Phase-5 §1.1) -------------------------------------------

#[tokio::test]
async fn focus_pane_dispatches_pane_focus() {
    let script = Script::new([(
        "pane.focus",
        Step::Reply(json!({
            "type": "pane_info",
            "pane": {
                "pane_id": "wE:p1", "terminal_id": "term-1",
                "workspace_id": "wE", "tab_id": "wE:t1",
                "focused": true, "agent_status": "idle"
            }
        })),
    )]);
    let ctx = context(&script, vec![]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("focus_pane")),
        ("pane_id".into(), json!("wE:p1")),
        ("target".into(), json!({"pane_id": "wE:p1"})),
    ]));
    let frames = frames_of(super::focus::focus_pane(ctx, "r1", "a1", &msg).await);
    assert!(frames.ok);
    assert_eq!(frames.phase, "completed");
    assert_eq!(frames.receipt_phase, "confirmed");
    let requests = script.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "pane.focus");
    assert_eq!(requests[0].1, json!({ "pane_id": "wE:p1" }));
}

#[tokio::test]
async fn focus_tab_dispatches_tab_focus() {
    let script = Script::new([(
        "tab.focus",
        Step::Reply(json!({
            "type": "tab_info",
            "tab": {
                "tab_id": "wE:p1:t2", "workspace_id": "wE", "number": 2,
                "label": "build", "focused": true, "pane_count": 1,
                "agent_status": "idle"
            }
        })),
    )]);
    let ctx = context(&script, vec![]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("focus_tab")),
        (
            "target".into(),
            json!({"pane_id": "wE:p1", "tab_id": "wE:p1:t2"}),
        ),
    ]));
    let frames = frames_of(super::focus::focus_tab(ctx, "r1", "a1", &msg).await);
    assert!(frames.ok);
    assert_eq!(frames.phase, "completed");
    let requests = script.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "tab.focus");
    assert_eq!(requests[0].1, json!({ "tab_id": "wE:p1:t2" }));
}

#[tokio::test]
async fn focus_workspace_dispatches_workspace_focus() {
    let script = Script::new([(
        "workspace.focus",
        Step::Reply(json!({
            "type": "workspace_info",
            "workspace": {
                "workspace_id": "wE", "number": 1, "label": "main",
                "focused": true, "pane_count": 2, "tab_count": 3,
                "active_tab_id": "wE:t2", "agent_status": "idle"
            }
        })),
    )]);
    let ctx = context(&script, vec![]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("focus_workspace")),
        ("target".into(), json!({"workspace_id": "wE"})),
    ]));
    let frames = frames_of(super::focus::focus_workspace(ctx, "r1", "a1", &msg).await);
    assert!(frames.ok);
    assert_eq!(frames.phase, "completed");
    let requests = script.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "workspace.focus");
    assert_eq!(requests[0].1, json!({ "workspace_id": "wE" }));
}

#[tokio::test]
async fn focus_agent_resolves_session_to_hosting_pane() {
    let script = Script::new([(
        "agent.focus",
        Step::Reply(json!({
            "type": "agent_info",
            "agent": {
                "pane_id": "wE:p1", "terminal_id": "term-1",
                "workspace_id": "wE", "tab_id": "wE:t1",
                "focused": true, "agent_status": "idle"
            }
        })),
    )]);
    let ctx = context(
        &script,
        vec![AgentInfo {
            pane_id: "wE:p1".into(),
            agent_session: Some(lerdr_herdr::AgentSessionInfo {
                source: "sess".into(),
                agent: "devin".into(),
                kind: lerdr_herdr::AgentSessionRefKind::Id,
                value: "wE:a3".into(),
            }),
            ..AgentInfo::default()
        }],
    );
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("focus_agent")),
        ("target".into(), json!({"agent_session_id": "wE:a3"})),
    ]));
    let frames = frames_of(super::focus::focus_agent(ctx, "r1", "a1", &msg).await);
    assert!(frames.ok);
    assert_eq!(frames.phase, "completed");
    let requests = script.requests();
    assert_eq!(requests.len(), 1);
    // `agent.focus` targets the hosting pane — session references do not
    // resolve on the Herdr side.
    assert_eq!(requests[0].0, "agent.focus");
    assert_eq!(requests[0].1, json!({ "target": "wE:p1" }));
}

#[tokio::test]
async fn focus_agent_unknown_session_fails_before_dispatch() {
    let script = Script::new([]);
    let ctx = context(&script, vec![]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("focus_agent")),
        ("target".into(), json!({"agent_session_id": "wE:a9"})),
    ]));
    let frames = frames_of(super::focus::focus_agent(ctx, "r1", "a1", &msg).await);
    assert!(!frames.ok);
    assert_eq!(frames.phase, "failed");
    assert_eq!(frames.receipt_phase, "failed_before_dispatch");
    assert!(script.requests().is_empty(), "no socket traffic");
}

#[tokio::test]
async fn focus_pane_refusal_maps_through_dispatch_failure() {
    let script = Script::new([(
        "pane.focus",
        Step::Refuse("unknown_method", "no such method"),
    )]);
    let ctx = context(&script, vec![]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("focus_pane")),
        ("pane_id".into(), json!("wE:p1")),
        ("target".into(), json!({"pane_id": "wE:p1"})),
    ]));
    let frames = frames_of(super::focus::focus_pane(ctx, "r1", "a1", &msg).await);
    assert!(!frames.ok);
    // A structured refusal reaches Herdr and back — `confirmed` with the
    // upstream code (docs/13: `herdr_error` with upstream message).
    assert_eq!(frames.receipt_phase, "confirmed");
    assert_eq!(frames.receipt_code.as_deref(), Some("unknown_method"));
    assert_eq!(frames.data, Some(json!({ "code": "unknown_method" })));
    assert_eq!(script.requests().len(), 1);
}

/// Live evidence that `workspace.focus` is unsupported must refuse
/// `focus_workspace` without a socket call — while the rest of the
/// family keeps dispatching (partial families stay advertised).
#[tokio::test]
async fn refuted_method_gaps_without_socket_call() {
    let script = Script::new([(
        "pane.focus",
        Step::Reply(json!({
            "type": "pane_info",
            "pane": {
                "pane_id": "wE:p1", "terminal_id": "term-1",
                "workspace_id": "wE", "tab_id": "wE:t1",
                "focused": true, "agent_status": "idle"
            }
        })),
    )]);
    let mut ctx = context(&script, vec![]);
    let features = std::collections::BTreeMap::from([(
        "workspace.focus".to_owned(),
        lerdr_core::protocol::HerdrFeatureStatus {
            state: "unsupported".to_owned(),
            reason: "schema_absent".to_owned(),
            generation: 1,
        },
    )]);
    Arc::get_mut(&mut ctx.topology)
        .expect("single topology reference")
        .herdr_status
        .features = lerdr_core::json::MaybeNull::Value(features);

    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("focus_workspace")),
        ("target".into(), json!({"workspace_id": "wE"})),
    ]));
    let frames = frames_of(super::focus::focus_workspace(ctx.clone(), "r1", "a1", &msg).await);
    assert!(!frames.ok);
    assert_eq!(frames.phase, "not_started");
    assert_eq!(frames.receipt_phase, "failed_before_dispatch");
    assert_eq!(
        frames.receipt_code.as_deref(),
        Some("capability_unsupported")
    );
    assert_eq!(
        frames.data,
        Some(json!({ "code": "capability_unsupported" }))
    );
    assert!(
        script.requests().is_empty(),
        "gap refuses before the socket"
    );

    // The rest of the family still dispatches on this Herdr build.
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("focus_pane")),
        ("pane_id".into(), json!("wE:p1")),
        ("target".into(), json!({"pane_id": "wE:p1"})),
    ]));
    let frames = frames_of(super::focus::focus_pane(ctx, "r2", "a2", &msg).await);
    assert!(frames.ok);
    assert_eq!(script.requests()[0].0, "pane.focus");
}

#[tokio::test]
async fn focus_workspace_requires_a_workspace_id() {
    let script = Script::new([]);
    let ctx = context(&script, vec![]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("focus_workspace")),
        ("target".into(), json!({})),
    ]));
    let frames = frames_of(super::focus::focus_workspace(ctx, "r1", "a1", &msg).await);
    assert!(!frames.ok);
    assert_eq!(frames.phase, "failed");
    assert!(script.requests().is_empty());
}

// ── Phase-5 pane content (docs/13 §1.2-1.5) ----------------------------------

/// A `panes` row — `revision` seeds the upstream `content_revision`
/// watermark the fenced copy family injects.
fn pane_row(
    pane_id: &str,
    revision: u64,
    scroll: Option<lerdr_herdr::PaneScrollInfo>,
) -> lerdr_herdr::PaneInfo {
    lerdr_herdr::PaneInfo {
        pane_id: pane_id.to_owned(),
        terminal_id: "term-1".to_owned(),
        workspace_id: "wE".to_owned(),
        tab_id: "wE:t1".to_owned(),
        revision,
        scroll,
        ..lerdr_herdr::PaneInfo::default()
    }
}

fn search_reply() -> serde_json::Value {
    json!({
        "type": "pane_copy_search",
        "pane_id": "wE:p1",
        "content_revision": 930,
        "matches": [
            {"start": {"row": 2, "col": 12}, "end": {"row": 2, "col": 16}},
            {"start": {"row": 12, "col": 6}, "end": {"row": 12, "col": 10}}
        ],
        "total": 2,
        "current": 1,
        "current_global": 1
    })
}

/// No watermark yet — the handler learns the fence through the unfenced
/// `pane.copy_motion` probe, then dispatches `pane.copy_search` with it.
#[tokio::test]
async fn pane_search_probes_the_revision_then_searches() {
    let script = Script::new([
        (
            "pane.copy_motion",
            Step::Reply(json!({
                "type": "pane_copy_motion",
                "pane_id": "wE:p1",
                "cursor": {"row": 0, "col": 0},
                "content_revision": 930
            })),
        ),
        ("pane.copy_search", Step::Reply(search_reply())),
    ]);
    let ctx = context(&script, vec![]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("pane_search")),
        ("pane_id".into(), json!("wE:p1")),
        ("query".into(), json!("panic")),
        ("direction".into(), json!("backward")),
        ("cursor".into(), json!({"row": 0, "col": 0})),
        (
            "previous".into(),
            json!({"start": {"row": 2, "col": 12}, "end": {"row": 2, "col": 16}}),
        ),
    ]));
    let frames = frames_of(super::content::pane_search(ctx, "r1", "a1", &msg).await);
    assert!(frames.ok, "data: {:?}", frames.data);
    assert_eq!(frames.receipt_phase, "confirmed");
    let requests = script.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].0, "pane.copy_motion");
    assert_eq!(
        requests[0].1,
        json!({
            "pane_id": "wE:p1",
            "cursor": {"row": 0, "col": 0},
            "motion": "line_end"
        })
    );
    assert_eq!(requests[1].0, "pane.copy_search");
    assert_eq!(
        requests[1].1,
        json!({
            "pane_id": "wE:p1",
            "query": "panic",
            "direction": "backward",
            "cursor": {"row": 0, "col": 0},
            "content_revision": 930,
            "previous": {"start": {"row": 2, "col": 12}, "end": {"row": 2, "col": 16}}
        })
    );
    let data = frames.data.expect("search data");
    assert_eq!(data["content_revision"], 930);
    assert_eq!(data["total"], 2);
    assert_eq!(data["current"], 1);
    assert_eq!(data["matches"].as_array().unwrap().len(), 2);
    // The observed revision folded into the served watermark.
    // (the shared ledger — the next action sees it without a probe)
}

/// A known watermark dispatches `pane.copy_search` directly — no probe.
#[tokio::test]
async fn pane_search_uses_the_served_watermark_without_probing() {
    let script = Script::new([("pane.copy_search", Step::Reply(search_reply()))]);
    let ctx = context_panes(&script, vec![], vec![pane_row("wE:p1", 12, None)]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("pane_search")),
        ("pane_id".into(), json!("wE:p1")),
        ("query".into(), json!("panic")),
        ("direction".into(), json!("forward")),
        ("cursor".into(), json!({"row": 3, "col": 4})),
    ]));
    let frames = frames_of(super::content::pane_search(ctx, "r1", "a1", &msg).await);
    assert!(frames.ok, "data: {:?}", frames.data);
    let requests = script.requests();
    assert_eq!(requests.len(), 1, "no probe once the watermark is known");
    assert_eq!(requests[0].0, "pane.copy_search");
    assert_eq!(requests[0].1["content_revision"], 12);
    // `previous` absent on the wire stays absent upstream.
    assert!(requests[0].1.get("previous").is_none());
}

/// A `stale_content` refusal re-probes and retries once at the fresh mark.
#[tokio::test]
async fn pane_search_stale_fence_reprobes_and_retries() {
    let script = Script::new([
        (
            "pane.copy_search",
            Step::Refuse("stale_content", "content changed"),
        ),
        (
            "pane.copy_motion",
            Step::Reply(json!({
                "type": "pane_copy_motion",
                "pane_id": "wE:p1",
                "cursor": {"row": 0, "col": 0},
                "content_revision": 40
            })),
        ),
        ("pane.copy_search", Step::Reply(search_reply())),
    ]);
    let ctx = context_panes(&script, vec![], vec![pane_row("wE:p1", 12, None)]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("pane_search")),
        ("pane_id".into(), json!("wE:p1")),
        ("query".into(), json!("panic")),
        ("cursor".into(), json!({"row": 0, "col": 0})),
    ]));
    let frames = frames_of(super::content::pane_search(ctx, "r1", "a1", &msg).await);
    assert!(frames.ok, "data: {:?}", frames.data);
    let requests = script.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].1["content_revision"], 12);
    assert_eq!(requests[1].0, "pane.copy_motion");
    assert_eq!(requests[2].0, "pane.copy_search");
    assert_eq!(requests[2].1["content_revision"], 40);
}

/// A definitive refusal reaches Herdr and back — `confirmed` receipt with
/// the upstream code, `not_started` result.
#[tokio::test]
async fn pane_search_refusal_maps_to_not_started() {
    let script = Script::new([("pane.copy_search", Step::Refuse("pane_not_found", "gone"))]);
    let ctx = context_panes(&script, vec![], vec![pane_row("wE:p1", 12, None)]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("pane_search")),
        ("pane_id".into(), json!("wE:p1")),
        ("query".into(), json!("panic")),
        ("cursor".into(), json!({"row": 0, "col": 0})),
    ]));
    let frames = frames_of(super::content::pane_search(ctx, "r1", "a1", &msg).await);
    assert!(!frames.ok);
    assert_eq!(frames.phase, "not_started");
    assert_eq!(frames.receipt_phase, "confirmed");
    assert_eq!(frames.receipt_code.as_deref(), Some("pane_not_found"));
    assert_eq!(frames.data, Some(json!({ "code": "pane_not_found" })));
}

/// Bad shapes fail before the socket: missing query, missing cursor,
/// malformed `previous`, unknown direction.
#[tokio::test]
async fn pane_search_validation_failures_stay_local() {
    let script = Script::new([]);
    let ctx = context(&script, vec![]);
    for map in [
        // no query
        serde_json::Map::from_iter([
            ("type".into(), json!("pane_search")),
            ("pane_id".into(), json!("wE:p1")),
            ("cursor".into(), json!({"row": 0, "col": 0})),
        ]),
        // no cursor
        serde_json::Map::from_iter([
            ("type".into(), json!("pane_search")),
            ("pane_id".into(), json!("wE:p1")),
            ("query".into(), json!("panic")),
        ]),
        // malformed previous
        serde_json::Map::from_iter([
            ("type".into(), json!("pane_search")),
            ("pane_id".into(), json!("wE:p1")),
            ("query".into(), json!("panic")),
            ("cursor".into(), json!({"row": 0, "col": 0})),
            ("previous".into(), json!("yes")),
        ]),
        // unknown direction
        serde_json::Map::from_iter([
            ("type".into(), json!("pane_search")),
            ("pane_id".into(), json!("wE:p1")),
            ("query".into(), json!("panic")),
            ("cursor".into(), json!({"row": 0, "col": 0})),
            ("direction".into(), json!("sideways")),
        ]),
    ] {
        let msg = message(map);
        let frames = frames_of(super::content::pane_search(ctx.clone(), "r1", "a1", &msg).await);
        assert!(!frames.ok, "map: {msg:?}");
        assert_eq!(frames.phase, "failed");
        assert_eq!(frames.receipt_phase, "failed_before_dispatch");
    }
    assert!(script.requests().is_empty());
}

/// Live evidence that `pane.copy_search` is unsupported refutes the action
/// without a socket call — same `capability_unsupported` code the session
/// gate emits.
#[tokio::test]
async fn refuted_copy_method_gaps_without_socket_call() {
    let script = Script::new([]);
    let ctx = context_features(&script, vec![], &[("pane.copy_search", "unsupported")]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("pane_search")),
        ("pane_id".into(), json!("wE:p1")),
        ("query".into(), json!("panic")),
        ("cursor".into(), json!({"row": 0, "col": 0})),
    ]));
    let frames = frames_of(super::content::pane_search(ctx, "r1", "a1", &msg).await);
    assert!(!frames.ok);
    assert_eq!(frames.phase, "not_started");
    assert_eq!(frames.receipt_phase, "failed_before_dispatch");
    assert_eq!(
        frames.receipt_code.as_deref(),
        Some("capability_unsupported")
    );
    assert!(script.requests().is_empty());
}

/// `pane_selection_read` rides the watermark fence and reports the range.
#[tokio::test]
async fn pane_selection_read_fences_and_decodes() {
    let script = Script::new([(
        "pane.selection.read",
        Step::Reply(json!({
            "type": "pane_selection",
            "pane_id": "wE:p1",
            "text": "selected text"
        })),
    )]);
    let ctx = context_panes(&script, vec![], vec![pane_row("wE:p1", 41, None)]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("pane_selection_read")),
        ("pane_id".into(), json!("wE:p1")),
        ("anchor".into(), json!({"row": 1, "col": 3})),
        ("cursor".into(), json!({"row": 4, "col": 9})),
    ]));
    let frames = frames_of(super::content::pane_selection_read(ctx, "r1", "a1", &msg).await);
    assert!(frames.ok, "data: {:?}", frames.data);
    let requests = script.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "pane.selection.read");
    assert_eq!(
        requests[0].1,
        json!({
            "pane_id": "wE:p1",
            "anchor": {"row": 1, "col": 3},
            "cursor": {"row": 4, "col": 9},
            "content_revision": 41
        })
    );
    let data = frames.data.expect("selection data");
    assert_eq!(data["text"], "selected text");
    assert_eq!(data["content_revision"], 41);
}

/// An unobserved watermark reads unfenced — no `content_revision` upstream
/// and `0` reported relay-side.
#[tokio::test]
async fn pane_selection_read_unfenced_when_unobserved() {
    let script = Script::new([(
        "pane.selection.read",
        Step::Reply(json!({
            "type": "pane_selection",
            "pane_id": "wE:p1",
            "text": ""
        })),
    )]);
    let ctx = context(&script, vec![]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("pane_selection_read")),
        ("pane_id".into(), json!("wE:p1")),
        ("anchor".into(), json!({"row": 0, "col": 0})),
        ("cursor".into(), json!({"row": 0, "col": 5})),
    ]));
    let frames = frames_of(super::content::pane_selection_read(ctx, "r1", "a1", &msg).await);
    assert!(frames.ok);
    let requests = script.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].1.get("content_revision").is_none());
    assert_eq!(frames.data.expect("data")["content_revision"], 0);
}

/// `pane_link_resolve` translates `row`/`col` to upstream's
/// `viewport_row`/`col`, carries the pane's scroll offset, and fences at
/// the served watermark.
#[tokio::test]
async fn pane_link_resolve_maps_viewport_cell_and_decodes_regions() {
    let script = Script::new([(
        "pane.link.resolve",
        Step::Reply(json!({
            "type": "pane_link_resolved",
            "regions": [{"row": 5, "start_col": 9, "end_col": 27}]
        })),
    )]);
    let ctx = context_panes(
        &script,
        vec![],
        vec![pane_row(
            "wE:p1",
            41,
            Some(lerdr_herdr::PaneScrollInfo {
                offset_from_bottom: 3,
                max_offset_from_bottom: 60,
                viewport_rows: 24,
            }),
        )],
    );
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("pane_link_resolve")),
        ("pane_id".into(), json!("wE:p1")),
        ("row".into(), json!(5)),
        ("col".into(), json!(12)),
    ]));
    let frames = frames_of(super::content::pane_link_resolve(ctx, "r1", "a1", &msg).await);
    assert!(frames.ok, "data: {:?}", frames.data);
    let requests = script.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "pane.link.resolve");
    assert_eq!(
        requests[0].1,
        json!({
            "pane_id": "wE:p1",
            "viewport_row": 5,
            "col": 12,
            "content_revision": 41,
            "offset_from_bottom": 3
        })
    );
    let regions = frames.data.expect("regions")["regions"].clone();
    assert_eq!(regions[0]["row"], 5);
    assert_eq!(regions[0]["start_col"], 9);
    assert_eq!(regions[0]["end_col"], 27);
}

/// `pane_link_activate` is the mutating leg — `{handled,url}` reports who
/// took the link and the resolved target.
#[tokio::test]
async fn pane_link_activate_reports_handled_and_url() {
    let script = Script::new([(
        "pane.link.activate",
        Step::Reply(json!({
            "type": "pane_link_activated",
            "handled": true,
            "url": "https://example.com"
        })),
    )]);
    let ctx = context_panes(&script, vec![], vec![pane_row("wE:p1", 41, None)]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("pane_link_activate")),
        ("pane_id".into(), json!("wE:p1")),
        ("row".into(), json!(5)),
        ("col".into(), json!(12)),
    ]));
    let frames = frames_of(super::content::pane_link_activate(ctx, "r1", "a1", &msg).await);
    assert!(frames.ok, "data: {:?}", frames.data);
    let requests = script.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "pane.link.activate");
    assert_eq!(requests[0].1["content_revision"], 41);
    let data = frames.data.expect("activate data");
    assert_eq!(data["handled"], true);
    assert_eq!(data["url"], "https://example.com");
}

/// Links addressed outside the viewport bounds fail before the socket.
#[tokio::test]
async fn pane_link_coordinate_validation_stays_local() {
    let script = Script::new([]);
    let ctx = context(&script, vec![]);
    for map in [
        serde_json::Map::from_iter([
            ("type".into(), json!("pane_link_resolve")),
            ("pane_id".into(), json!("wE:p1")),
            ("col".into(), json!(4)), // row missing
        ]),
        serde_json::Map::from_iter([
            ("type".into(), json!("pane_link_resolve")),
            ("pane_id".into(), json!("wE:p1")),
            ("row".into(), json!(70_000)), // over u16
            ("col".into(), json!(4)),
        ]),
        serde_json::Map::from_iter([
            ("type".into(), json!("pane_link_activate")),
            ("pane_id".into(), json!("wE:p1")),
            ("row".into(), json!(-1)), // negative
            ("col".into(), json!(4)),
        ]),
    ] {
        let msg = message(map);
        let action = msg.r#type.clone();
        let frames = frames_of(if action == "pane_link_resolve" {
            super::content::pane_link_resolve(ctx.clone(), "r1", "a1", &msg).await
        } else {
            super::content::pane_link_activate(ctx.clone(), "r1", "a1", &msg).await
        });
        assert!(!frames.ok);
        assert_eq!(frames.phase, "failed");
        assert_eq!(frames.receipt_phase, "failed_before_dispatch");
    }
    assert!(script.requests().is_empty());
}

/// `layout_export` addresses by tab or pane and projects the description's
/// `root`.
#[tokio::test]
async fn layout_export_addresses_by_tab_and_projects_root() {
    let root = json!({
        "type": "split",
        "direction": "right",
        "ratio": 0.5,
        "first": {"type": "pane", "pane_id": "wE:p1"},
        "second": {"type": "pane", "pane_id": "wE:p2"}
    });
    let script = Script::new([(
        "layout.export",
        Step::Reply(json!({
            "type": "layout_export",
            "layout": {
                "workspace_id": "wE",
                "tab_id": "wE:t1",
                "zoomed": false,
                "focused_pane_id": "wE:p1",
                "root": root
            }
        })),
    )]);
    let ctx = context(&script, vec![]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("layout_export")),
        ("target".into(), json!({"tab_id": "wE:t1"})),
    ]));
    let frames = frames_of(super::content::layout_export(ctx, "r1", "a1", &msg).await);
    assert!(frames.ok, "data: {:?}", frames.data);
    let requests = script.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "layout.export");
    assert_eq!(requests[0].1, json!({ "tab_id": "wE:t1" }));
    let data = frames.data.expect("export data");
    assert_eq!(data["root"]["type"], "split");
    assert_eq!(data["root"]["first"]["pane_id"], "wE:p1");
}

/// `layout_apply` serializes the root tree plus its addressing/option
/// fields and reports the realized layout.
#[tokio::test]
async fn layout_apply_serializes_root_and_options() {
    let script = Script::new([(
        "layout.apply",
        Step::Reply(json!({
            "type": "layout_apply",
            "layout": {
                "workspace_id": "wE",
                "tab_id": "wE:t2",
                "zoomed": false,
                "focused_pane_id": "wE:p3",
                "root": {"type": "pane", "pane_id": "wE:p3"}
            }
        })),
    )]);
    let ctx = context(&script, vec![]);
    let msg = message(serde_json::Map::from_iter([
        ("type".into(), json!("layout_apply")),
        (
            "root".into(),
            json!({
                "type": "split",
                "direction": "down",
                "ratio": 0.3,
                "first": {"type": "pane", "pane_id": "wE:p1"},
                "second": {"type": "pane", "command": ["bash"], "cwd": "/tmp"}
            }),
        ),
        ("tab_id".into(), json!("wE:t2")),
        ("tab_label".into(), json!("rebuilt")),
        ("focus".into(), json!(true)),
    ]));
    let frames = frames_of(super::content::layout_apply(ctx, "r1", "a1", &msg).await);
    assert!(frames.ok, "data: {:?}", frames.data);
    let requests = script.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "layout.apply");
    assert_eq!(
        requests[0].1,
        json!({
            "root": {
                "type": "split",
                "direction": "down",
                "ratio": 0.3,
                "first": {"type": "pane", "pane_id": "wE:p1"},
                "second": {"type": "pane", "command": ["bash"], "cwd": "/tmp"}
            },
            "tab_id": "wE:t2",
            "tab_label": "rebuilt",
            "focus": true
        })
    );
    let data = frames.data.expect("apply data");
    assert_eq!(data["layout"]["tab_id"], "wE:t2");
}

/// A missing or malformed `root` fails before the socket.
#[tokio::test]
async fn layout_apply_requires_a_valid_root() {
    let script = Script::new([]);
    let ctx = context(&script, vec![]);
    for map in [
        serde_json::Map::from_iter([("type".into(), json!("layout_apply"))]),
        serde_json::Map::from_iter([
            ("type".into(), json!("layout_apply")),
            ("root".into(), json!("nope")),
        ]),
        serde_json::Map::from_iter([
            ("type".into(), json!("layout_apply")),
            ("root".into(), json!({"type": "portal"})),
        ]),
    ] {
        let msg = message(map);
        let frames = frames_of(super::content::layout_apply(ctx.clone(), "r1", "a1", &msg).await);
        assert!(!frames.ok);
        assert_eq!(frames.phase, "failed");
        assert_eq!(frames.receipt_phase, "failed_before_dispatch");
    }
    assert!(script.requests().is_empty());
}
