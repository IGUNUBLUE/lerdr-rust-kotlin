//! `pane.report_metadata` / `workspace.report_metadata` watch
//! annotations plus the `client.window_title` driver — the "phone is
//! watching / N devices connected" chrome signals reported to Herdr.
//!
//! One [`WatchAnnotations`] lives per relay (`ActionShared`): Herdr
//! sequences metadata reports per `(target, source)` and every client
//! watch reports under the shared `"lerdr-relay"` source, so the seq
//! ledger and watcher refcounts must be relay-wide — two sessions
//! watching one pane share the annotation rather than fighting over it.
//!
//! Reports travel one unbounded queue drained by a single task: seqs are
//! issued at enqueue time, so FIFO draining keeps every target's seq
//! stream monotonic even when watch events race the refresh tick.
//! `ttl_ms` (~5 min) bounds every annotation — the refresh task re-issues
//! reports well inside the TTL while watchers live, and a dead relay
//! simply lets the tokens expire server-side.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lerdr_herdr::capabilities::features;
use lerdr_herdr::{Client, FeatureState, PaneReportMetadataParams, WorkspaceReportMetadataParams};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::actor::TopologyHandle;

/// `source` on every report — Herdr's seq/merge bookkeeping is per
/// `(target, source)`; all lerdr watches share this identity.
const METADATA_SOURCE: &str = "lerdr-relay";

/// `ttl_ms` on reports — ~5 min after the last refresh, so a dead
/// relay's annotations expire server-side without an explicit teardown.
const METADATA_TTL_MS: u64 = 5 * 60 * 1000;

/// Refresh cadence — comfortably inside the TTL so live watches never
/// expire between reports.
const REFRESH_INTERVAL: Duration = Duration::from_secs(150);

/// The pane token a phone watch sets. Token keys must match
/// `^[A-Za-z0-9_-]{1,32}$` — dotted spellings are refused with
/// `invalid_metadata_token` (verified on herdr 0.9.1).
const WATCHING_TOKEN: &str = "lerdr_watching";

/// The workspace token carrying the live controller count.
const DEVICES_TOKEN: &str = "lerdr_devices";

/// Live connected-controller count — `Relay::connected_clients` behind a
/// lookup (the router factory is built before the Relay exists).
pub type ClientCountLookup = Arc<dyn Fn() -> usize + Send + Sync>;

/// One queued metadata report — `watching: false` clears the token.
enum Report {
    Pane {
        pane_id: String,
        watching: bool,
        seq: u64,
    },
    Workspace {
        workspace_id: String,
        /// `None` clears `lerdr_devices`.
        devices: Option<usize>,
        seq: u64,
    },
}

#[derive(Default)]
struct AnnotationState {
    /// pane_id → watchers + the pane's workspace ("" until topology
    /// names it — a watch admitted before the first snapshot).
    panes: BTreeMap<String, PaneAnnotation>,
    /// workspace_id → watching client ids (any pane inside it).
    workspaces: BTreeMap<String, BTreeSet<String>>,
    /// target → last issued seq. Herdr's per-`(target, source)` ledger
    /// only moves forward, so seqs floor at the wall clock — a restarted
    /// relay must not replay a seq the previous process's ledger already
    /// consumed (the server drops stale reports).
    seqs: BTreeMap<String, u64>,
}

impl AnnotationState {
    /// `last+1`, floored at `now_unix_ms` — strictly monotonic in-process
    /// and across restarts.
    fn next_seq(&mut self, target: &str) -> u64 {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let seq = self.seqs.entry(target.to_owned()).or_insert(0);
        *seq = (*seq + 1).max(now_ms);
        *seq
    }
}

struct PaneAnnotation {
    workspace_id: String,
    watchers: BTreeSet<String>,
}

/// The shared annotation ledger + ordered report queue.
#[derive(Clone)]
pub(crate) struct WatchAnnotations {
    inner: Arc<Inner>,
}

struct Inner {
    client: Client,
    handle: TopologyHandle,
    devices_of: ClientCountLookup,
    tx: mpsc::UnboundedSender<Report>,
    state: Mutex<AnnotationState>,
}

impl WatchAnnotations {
    /// Spawn the drain + refresh tasks. `cancel` is the relay shutdown —
    /// queued reports drop with it (TTL cleans up server-side anyway).
    pub(crate) fn spawn(
        handle: TopologyHandle,
        devices_of: ClientCountLookup,
        cancel: CancellationToken,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let inner = Arc::new(Inner {
            client: handle.client.clone(),
            handle,
            devices_of,
            tx,
            state: Mutex::new(AnnotationState::default()),
        });
        {
            let inner = inner.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move { drain(inner, rx, cancel).await });
        }
        {
            let inner = inner.clone();
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(REFRESH_INTERVAL);
                // The first tick fires immediately — consume it; a watch
                // that just started already reported.
                ticker.tick().await;
                loop {
                    tokio::select! {
                        () = cancel.cancelled() => break,
                        _ = ticker.tick() => inner.refresh_all(),
                    }
                }
            });
        }
        Self { inner }
    }

    /// `watch_pane` admission — the first watcher on a pane reports
    /// `lerdr_watching=1` and joins its workspace's device count.
    pub(crate) fn watch_started(&self, client_id: &str, pane_id: &str) {
        if client_id.is_empty() || pane_id.is_empty() {
            return;
        }
        let workspace_id = self.inner.workspace_of(pane_id);
        let mut state = self.inner.state.lock().expect("annotations poisoned");
        let first_for_pane = match state.panes.entry(pane_id.to_owned()) {
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                entry.get_mut().watchers.insert(client_id.to_owned());
                false
            }
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(PaneAnnotation {
                    workspace_id: workspace_id.clone().unwrap_or_default(),
                    watchers: BTreeSet::from([client_id.to_owned()]),
                });
                true
            }
        };
        if first_for_pane {
            self.inner.enqueue_pane(&mut state, pane_id, true);
        }
        let Some(workspace_id) = workspace_id else {
            return;
        };
        let watchers = state.workspaces.entry(workspace_id.clone()).or_default();
        if watchers.insert(client_id.to_owned()) && watchers.len() == 1 {
            self.inner.enqueue_workspace(&mut state, &workspace_id);
        }
    }

    /// `unwatch_pane` / session teardown — the last watcher off a pane
    /// clears its token; the last watcher in a workspace clears
    /// `lerdr_devices`.
    pub(crate) fn watch_stopped(&self, client_id: &str, pane_id: &str) {
        let mut state = self.inner.state.lock().expect("annotations poisoned");
        let Some(annotation) = state.panes.get_mut(pane_id) else {
            return;
        };
        annotation.watchers.remove(client_id);
        if !annotation.watchers.is_empty() {
            return;
        }
        let annotation = state.panes.remove(pane_id).expect("checked above");
        self.inner.enqueue_pane(&mut state, pane_id, false);
        let workspace_id = annotation.workspace_id;
        if workspace_id.is_empty() {
            return;
        }
        // The client stays counted while another of its watches sits in
        // the same workspace.
        let still_watching = state
            .panes
            .values()
            .any(|a| a.workspace_id == workspace_id && a.watchers.contains(client_id));
        if still_watching {
            return;
        }
        let Some(watchers) = state.workspaces.get_mut(&workspace_id) else {
            return;
        };
        watchers.remove(client_id);
        if watchers.is_empty() {
            state.workspaces.remove(&workspace_id);
            self.inner
                .enqueue_workspace_clear(&mut state, &workspace_id);
        }
    }
}

impl Inner {
    /// Queue `lerdr_watching=1` (or the clearing `null`) — gated on the
    /// published capability: a known-absent method never reaches the
    /// socket.
    fn enqueue_pane(&self, state: &mut AnnotationState, pane_id: &str, watching: bool) {
        if self.client.feature(features::PANE_REPORT_METADATA).state == FeatureState::Unsupported {
            return;
        }
        let seq = state.next_seq(pane_id);
        let _ = self.tx.send(Report::Pane {
            pane_id: pane_id.to_owned(),
            watching,
            seq,
        });
    }

    /// Queue `lerdr.devices=<count>` — reported only while >0 (the clear
    /// path is [`Inner::enqueue_workspace_clear`]).
    fn enqueue_workspace(&self, state: &mut AnnotationState, workspace_id: &str) {
        let devices = (self.devices_of)();
        if devices == 0 {
            return;
        }
        self.enqueue_workspace_report(state, workspace_id, Some(devices));
    }

    /// Queue the `lerdr.devices` clear — same capability gate as the set.
    fn enqueue_workspace_clear(&self, state: &mut AnnotationState, workspace_id: &str) {
        self.enqueue_workspace_report(state, workspace_id, None);
    }

    /// The shared gate + send: a known-absent method never reaches the
    /// socket; `devices: None` serializes the clearing `null`.
    fn enqueue_workspace_report(
        &self,
        state: &mut AnnotationState,
        workspace_id: &str,
        devices: Option<usize>,
    ) {
        if self
            .client
            .feature(features::WORKSPACE_REPORT_METADATA)
            .state
            == FeatureState::Unsupported
        {
            return;
        }
        let seq = state.next_seq(workspace_id);
        let _ = self.tx.send(Report::Workspace {
            workspace_id: workspace_id.to_owned(),
            devices,
            seq,
        });
    }

    /// pane → workspace via the committed topology — `snapshot.panes`
    /// covers non-agent panes too (`pane_of` is agent rows only).
    fn workspace_of(&self, pane_id: &str) -> Option<String> {
        let topology = self.handle.topology.borrow();
        topology
            .snapshot
            .panes
            .iter()
            .find(|p| p.pane_id == pane_id)
            .map(|p| p.workspace_id.clone())
            .filter(|id| !id.is_empty())
    }

    /// The periodic refresh — re-issue every live annotation inside the
    /// TTL window. Late workspace resolution also lands here: a pane
    /// watched before the first snapshot carries "" until this pass
    /// re-resolves it.
    fn refresh_all(&self) {
        let mut state = self.state.lock().expect("annotations poisoned");
        let panes: Vec<String> = state.panes.keys().cloned().collect();
        for pane_id in panes {
            // Late workspace resolution — computed outside the panes
            // borrow so the workspace merge can take the map mutably.
            let resolved = match state.panes.get(&pane_id) {
                Some(a) if a.workspace_id.is_empty() => self.workspace_of(&pane_id),
                _ => None,
            };
            if let Some(ws) = resolved {
                let watchers: Vec<String> = {
                    let annotation = state.panes.get_mut(&pane_id).expect("checked above");
                    annotation.workspace_id = ws.clone();
                    annotation.watchers.iter().cloned().collect()
                };
                let entry = state.workspaces.entry(ws.clone()).or_default();
                let was_empty = entry.is_empty();
                entry.extend(watchers);
                if was_empty && !entry.is_empty() {
                    self.enqueue_workspace(&mut state, &ws);
                }
            }
            self.enqueue_pane(&mut state, &pane_id, true);
        }
        let workspaces: Vec<String> = state.workspaces.keys().cloned().collect();
        for workspace_id in workspaces {
            self.enqueue_workspace(&mut state, &workspace_id);
        }
    }
}

/// The report queue's consumer — one socket call at a time so every
/// target sees its seq stream in issue order.
async fn drain(
    inner: Arc<Inner>,
    mut rx: mpsc::UnboundedReceiver<Report>,
    cancel: CancellationToken,
) {
    loop {
        let report = tokio::select! {
            () = cancel.cancelled() => break,
            report = rx.recv() => match report {
                Some(report) => report,
                None => break,
            },
        };
        match report {
            Report::Pane {
                pane_id,
                watching,
                seq,
            } => {
                let mut tokens = BTreeMap::new();
                tokens.insert(WATCHING_TOKEN.to_owned(), watching.then(|| "1".to_owned()));
                let params = PaneReportMetadataParams {
                    pane_id: pane_id.clone(),
                    source: METADATA_SOURCE.to_owned(),
                    tokens,
                    ttl_ms: Some(METADATA_TTL_MS),
                    seq: Some(seq),
                    ..PaneReportMetadataParams::default()
                };
                if let Err(err) = inner.client.pane_report_metadata(&params).await {
                    debug!(%err, pane_id, "pane.report_metadata failed");
                }
            }
            Report::Workspace {
                workspace_id,
                devices,
                seq,
            } => {
                let mut tokens = BTreeMap::new();
                tokens.insert(DEVICES_TOKEN.to_owned(), devices.map(|n| n.to_string()));
                let params = WorkspaceReportMetadataParams {
                    workspace_id: workspace_id.clone(),
                    source: METADATA_SOURCE.to_owned(),
                    tokens,
                    ttl_ms: Some(METADATA_TTL_MS),
                    seq: Some(seq),
                };
                if let Err(err) = inner.client.workspace_report_metadata(&params).await {
                    debug!(%err, workspace_id, "workspace.report_metadata failed");
                }
            }
        }
    }
}

/// `client.window_title.{set,clear}` — "lerdr: N device(s)" while
/// controllers are connected; cleared when the last one leaves.
/// Capability-gated like the watch annotations.
pub async fn update_window_title(client: &Client, devices: usize) {
    if devices == 0 {
        if client.feature(features::CLIENT_WINDOW_TITLE_CLEAR).state == FeatureState::Unsupported {
            return;
        }
        if let Err(err) = client.client_window_title_clear().await {
            debug!(%err, "client.window_title.clear failed");
        }
        return;
    }
    if client.feature(features::CLIENT_WINDOW_TITLE_SET).state == FeatureState::Unsupported {
        return;
    }
    let title = if devices == 1 {
        "lerdr: 1 device".to_owned()
    } else {
        format!("lerdr: {devices} devices")
    };
    if let Err(err) = client.client_window_title_set(&title).await {
        warn!(%err, "client.window_title.set failed");
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use lerdr_herdr::{BoxIo, ClientConfig, PaneInfo, SessionSnapshot, Transport, WorkspaceInfo};
    use serde_json::{json, Value};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::broadcast;

    use super::*;
    use crate::topology::Topology;

    /// One scripted connection outcome.
    enum Step {
        Reply(Value),
        Refuse(&'static str, &'static str),
    }

    /// Recording/scripted `Transport` — every (method, params) pair is
    /// kept; each method pops its reply queue, defaulting to `ok`.
    #[derive(Clone, Default)]
    struct Recorder {
        requests: Arc<Mutex<Vec<(String, Value)>>>,
        replies: Arc<Mutex<HashMap<String, Vec<Step>>>>,
    }

    impl Recorder {
        fn scripted(steps: &[(&'static str, Step)]) -> Self {
            let recorder = Recorder::default();
            for (method, step) in steps {
                // Queue order: the script list is written in call order.
                recorder
                    .replies
                    .lock()
                    .unwrap()
                    .entry((*method).to_owned())
                    .or_default()
                    .push(match step {
                        Step::Reply(v) => Step::Reply(v.clone()),
                        Step::Refuse(c, m) => Step::Refuse(c, m),
                    });
            }
            recorder
        }

        fn all(&self) -> Vec<(String, Value)> {
            self.requests.lock().unwrap().clone()
        }

        /// Poll until at least `n` requests were recorded.
        async fn wait_for(&self, n: usize) -> Vec<(String, Value)> {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                let requests = self.all();
                if requests.len() >= n {
                    return requests;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "only {} requests recorded, wanted {n}",
                    requests.len()
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
    }

    impl Transport for Recorder {
        fn dial(
            &self,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<BoxIo>> + Send>>
        {
            let recorder = self.clone();
            Box::pin(async move {
                let (client_end, server_end) = tokio::io::duplex(64 * 1024);
                tokio::spawn(serve_recorder(server_end, recorder));
                Ok(Box::new(client_end) as BoxIo)
            })
        }

        fn describe(&self) -> String {
            "recorder".to_owned()
        }
    }

    async fn serve_recorder(mut conn: tokio::io::DuplexStream, recorder: Recorder) {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let end = loop {
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
        let request: Value = serde_json::from_slice(&buf[..end]).unwrap_or_default();
        let method = request["method"].as_str().unwrap_or_default().to_owned();
        recorder
            .requests
            .lock()
            .unwrap()
            .push((method.clone(), request["params"].clone()));
        let step = recorder
            .replies
            .lock()
            .unwrap()
            .get_mut(&method)
            .and_then(|queue| {
                if queue.is_empty() {
                    None
                } else {
                    Some(queue.remove(0))
                }
            })
            .unwrap_or(Step::Reply(json!({ "type": "ok" })));
        let id = request["id"].as_str().unwrap_or_default().to_owned();
        match step {
            Step::Reply(result) => {
                let _ = conn
                    .write_all(json!({ "id": id, "result": result }).to_string().as_bytes())
                    .await;
            }
            Step::Refuse(code, message) => {
                let _ = conn
                    .write_all(
                        json!({ "id": id, "error": { "code": code, "message": message } })
                            .to_string()
                            .as_bytes(),
                    )
                    .await;
            }
        }
        let _ = conn.write_all(b"\n").await;
    }

    /// Two panes in `ws`, one elsewhere — exercises the pane→workspace
    /// projection both ways.
    fn fixture(devices: usize, steps: &[(&'static str, Step)]) -> (WatchAnnotations, Recorder) {
        let recorder = Recorder::scripted(steps);
        let client = Client::new(Arc::new(recorder.clone()), ClientConfig::default());
        let mut topology = Topology::default();
        topology.accept(SessionSnapshot {
            workspaces: vec![WorkspaceInfo {
                workspace_id: "ws".to_owned(),
                ..WorkspaceInfo::default()
            }],
            panes: vec![
                PaneInfo {
                    pane_id: "ws:p1".to_owned(),
                    workspace_id: "ws".to_owned(),
                    ..PaneInfo::default()
                },
                PaneInfo {
                    pane_id: "ws:p2".to_owned(),
                    workspace_id: "ws".to_owned(),
                    ..PaneInfo::default()
                },
                PaneInfo {
                    pane_id: "other:p".to_owned(),
                    workspace_id: "other".to_owned(),
                    ..PaneInfo::default()
                },
            ],
            ..SessionSnapshot::default()
        });
        let (invalidations, _) = broadcast::channel(8);
        let (handle, _topology_tx) =
            crate::TopologyHandle::for_test(client, Arc::new(topology), invalidations);
        let annotations =
            WatchAnnotations::spawn(handle, Arc::new(move || devices), CancellationToken::new());
        (annotations, recorder)
    }

    #[tokio::test]
    async fn first_watch_reports_pane_and_workspace_tokens() {
        let (annotations, recorder) = fixture(2, &[]);
        annotations.watch_started("c1", "ws:p1");
        let requests = recorder.wait_for(2).await;

        let (method, params) = &requests[0];
        assert_eq!(method, "pane.report_metadata");
        assert_eq!(params["pane_id"], "ws:p1");
        assert_eq!(params["source"], "lerdr-relay");
        assert_eq!(params["tokens"]["lerdr_watching"], "1");
        assert_eq!(params["ttl_ms"], 300_000);
        assert!(params["seq"].as_u64().unwrap_or(0) > 0);

        let (method, params) = &requests[1];
        assert_eq!(method, "workspace.report_metadata");
        assert_eq!(params["workspace_id"], "ws");
        assert_eq!(params["source"], "lerdr-relay");
        assert_eq!(params["tokens"]["lerdr_devices"], "2");
        assert!(params["seq"].as_u64().unwrap_or(0) > 0);
    }

    #[tokio::test]
    async fn last_unwatch_clears_with_monotonic_seq() {
        let (annotations, recorder) = fixture(1, &[]);
        annotations.watch_started("c1", "ws:p1");
        recorder.wait_for(2).await;
        annotations.watch_stopped("c1", "ws:p1");
        let requests = recorder.wait_for(4).await;

        // Clear rides the same target seq — strictly ahead of the set,
        // so herdr's per-source ordering always accepts it.
        let set_seq = requests[0].1["seq"].as_u64().unwrap_or(0);
        assert_eq!(requests[2].0, "pane.report_metadata");
        assert_eq!(requests[2].1["tokens"]["lerdr_watching"], Value::Null);
        assert!(requests[2].1["seq"].as_u64().unwrap_or(0) > set_seq);
        let ws_seq = requests[1].1["seq"].as_u64().unwrap_or(0);
        assert_eq!(requests[3].0, "workspace.report_metadata");
        assert_eq!(requests[3].1["tokens"]["lerdr_devices"], Value::Null);
        assert!(requests[3].1["seq"].as_u64().unwrap_or(0) > ws_seq);
    }

    #[tokio::test]
    async fn second_watcher_shares_the_annotation() {
        let (annotations, recorder) = fixture(2, &[]);
        annotations.watch_started("c1", "ws:p1");
        annotations.watch_started("c2", "ws:p1");
        recorder.wait_for(2).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            recorder
                .all()
                .iter()
                .filter(|(m, _)| m == "pane.report_metadata")
                .count(),
            1,
            "the shared annotation is reported once per pane"
        );

        // First leaver keeps the annotation; the last clears it.
        annotations.watch_stopped("c1", "ws:p1");
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            recorder
                .all()
                .iter()
                .filter(|(m, _)| m == "pane.report_metadata")
                .count(),
            1
        );
        annotations.watch_stopped("c2", "ws:p1");
        let requests = recorder.wait_for(3).await;
        assert_eq!(requests[2].1["tokens"]["lerdr_watching"], Value::Null);
    }

    #[tokio::test]
    async fn same_workspace_panes_share_one_devices_report() {
        let (annotations, recorder) = fixture(3, &[]);
        annotations.watch_started("c1", "ws:p1");
        annotations.watch_started("c1", "ws:p2");
        recorder.wait_for(3).await;
        assert_eq!(
            recorder
                .all()
                .iter()
                .filter(|(m, _)| m == "workspace.report_metadata")
                .count(),
            1,
            "one lerdr_devices report per workspace"
        );
        // c1 still watches p2 — unwatched p1 must not clear the workspace.
        annotations.watch_stopped("c1", "ws:p1");
        tokio::time::sleep(Duration::from_millis(20)).await;
        let cleared = recorder.all().iter().any(|(m, p)| {
            m == "workspace.report_metadata" && p["tokens"]["lerdr_devices"].is_null()
        });
        assert!(!cleared);
    }

    #[tokio::test]
    async fn refresh_re_reports_with_monotonic_seq() {
        let (annotations, recorder) = fixture(2, &[]);
        annotations.watch_started("c1", "ws:p1");
        recorder.wait_for(2).await;
        annotations.inner.refresh_all();
        let requests = recorder.wait_for(4).await;
        let pane_seq = requests[0].1["seq"].as_u64().unwrap_or(0);
        let ws_seq = requests[1].1["seq"].as_u64().unwrap_or(0);
        assert!(
            requests[2].1["seq"].as_u64().unwrap_or(0) > pane_seq,
            "pane refresh seq climbs"
        );
        assert_eq!(requests[2].1["tokens"]["lerdr_watching"], "1");
        assert!(
            requests[3].1["seq"].as_u64().unwrap_or(0) > ws_seq,
            "workspace refresh seq climbs"
        );
        assert_eq!(requests[3].1["tokens"]["lerdr_devices"], "2");
    }

    #[tokio::test]
    async fn unknown_method_marks_unsupported_and_stops_enqueuing() {
        let (annotations, recorder) = fixture(
            1,
            &[(
                "pane.report_metadata",
                Step::Refuse("unknown_method", "no such method"),
            )],
        );
        annotations.watch_started("c1", "ws:p1");
        recorder.wait_for(1).await; // the refused first report
        tokio::time::sleep(Duration::from_millis(20)).await;

        // After the definitive refusal the feature reads Unsupported, so
        // the workspace report (different method, still unknown) went out
        // but pane reports stop being attempted entirely.
        let pane_reports = recorder
            .all()
            .iter()
            .filter(|(m, _)| m == "pane.report_metadata")
            .count();
        assert_eq!(pane_reports, 1);
        annotations.watch_stopped("c1", "ws:p1");
        annotations.watch_started("c1", "ws:p2");
        annotations.watch_stopped("c1", "ws:p2");
        tokio::time::sleep(Duration::from_millis(20)).await;
        let pane_reports = recorder
            .all()
            .iter()
            .filter(|(m, _)| m == "pane.report_metadata")
            .count();
        assert_eq!(pane_reports, 1, "refused methods are never re-attempted");
    }

    #[tokio::test]
    async fn window_title_set_and_clear() {
        let recorder = Recorder::default();
        let client = Client::new(Arc::new(recorder.clone()), ClientConfig::default());

        update_window_title(&client, 2).await;
        update_window_title(&client, 1).await;
        update_window_title(&client, 0).await;
        let requests = recorder.all();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0].0, "client.window_title.set");
        assert_eq!(requests[0].1["title"], "lerdr: 2 devices");
        assert_eq!(requests[1].1["title"], "lerdr: 1 device");
        assert_eq!(requests[2].0, "client.window_title.clear");
    }

    #[tokio::test]
    async fn window_title_refusal_stops_future_calls() {
        let recorder = Recorder::scripted(&[
            (
                "client.window_title.set",
                Step::Refuse("unknown_method", "no such method"),
            ),
            (
                "client.window_title.clear",
                Step::Refuse("unknown_method", "no such method"),
            ),
        ]);
        let client = Client::new(Arc::new(recorder.clone()), ClientConfig::default());

        update_window_title(&client, 1).await;
        update_window_title(&client, 2).await;
        update_window_title(&client, 0).await;
        update_window_title(&client, 0).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            recorder.all().len(),
            2,
            "each refused method is tried once, then noted Unsupported"
        );
    }
}
