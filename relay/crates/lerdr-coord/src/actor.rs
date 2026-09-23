//! `TopologyActor` — the single driver of the Herdr event stream.
//!
//! One actor per relay owns the `events.subscribe` lifecycle via
//! [`Client::supervise_events`], projects each `Synced` snapshot into a
//! [`Topology`], and republishes on two axes:
//!
//! - `watch` — full [`Topology`] for snapshot composition and broadcasts
//!   (routers forward `workspaces`/`agents`/`herdr_status` to their client).
//! - `invalidations` — `pane.*`-class [`Event`]s as a broadcast channel for
//!   per-(client, pane) watch tasks; topology-class events never reach this
//!   channel because the actor consumes them as snapshot-refresh triggers.
//!
//! Recovery follows doc 08 verbatim: `Synced` replaces state,
//! `Invalidated` topology events trigger a fresh `session.snapshot` (events
//! are never applied as payloads), `Reconnecting` marks the view stale.

use std::sync::Arc;

use lerdr_core::json::MaybeNull;
use lerdr_core::protocol::{HerdrFeatureStatus, HerdrStatus};
use lerdr_herdr::{
    assert_agent_view, CapabilityReport, Client, Event, EventSupervisor, SupervisorSignal,
    ViewAssertOutcome,
};
use tokio::sync::{broadcast, mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn, Instrument};

use crate::topology::Topology;

/// Pane-class invalidation signal for watch tasks. `gap` events (arriving
/// between `subscription_started` and the bootstrap snapshot) are forwarded
/// too — watchers treat them identically since they always re-read.
#[derive(Debug, Clone)]
pub struct Invalidation {
    /// Canonical event name (`pane.updated`, `pane.agent_status_changed`, …).
    pub name: String,
    /// Pane id when the event payload carries one.
    pub pane_id: Option<String>,
    /// Raw event for consumers needing fields beyond the id.
    pub event: Event,
}

/// Handle every consumer holds: the projected topology plus the pane
/// invalidation feed.
#[derive(Clone)]
pub struct TopologyHandle {
    /// Latest [`Topology`] — `watch::Receiver::borrow()`.
    pub topology: watch::Receiver<Arc<Topology>>,
    /// Pane-class invalidations (broadcast; lagged receivers resync by
    /// re-reading — lag is correctness-safe, not lossy).
    pub invalidations: broadcast::Sender<Invalidation>,
    /// Herdr client for RPC (clone-cheap: the inner is `Arc`).
    pub client: Client,
    /// Command lane — external triggers (UDP event hook, startup hook)
    /// request a topology re-read through here.
    commands: mpsc::Sender<TopologyCommand>,
}

/// External triggers the actor honors beside the event stream.
#[derive(Debug)]
enum TopologyCommand {
    /// Re-read `session.snapshot` unconditionally — the UDP event hook's
    /// `agent_event` datagram uses this as its poke.
    Refresh,
    /// The Herdr `[[startup]]` hook fired (session restore or
    /// `server.live_handoff`): transient per-server state may have been
    /// dropped, so beside the snapshot re-read the actor re-collects
    /// capabilities and re-asserts the canonical `agent.view.set`
    /// projection.
    StartupHook,
    /// `d.state.BumpGeneration` — a lifecycle mutation replaced the pane's
    /// session; advance its epoch and republish so stale exact targets
    /// stop validating.
    BumpGeneration(String),
    /// Internal lane: a spawned post-sync capability collect finished;
    /// install its report (`set_herdr_status` dedupes).
    CapabilitiesReady(CapabilityReport),
}

impl TopologyHandle {
    /// Ask the actor to re-read the session snapshot. Cheap to call
    /// redundantly — refreshes coalesce behind the in-flight read.
    pub async fn refresh(&self) {
        // Full inbox = a refresh is already queued; dropping is correct.
        let _ = self.commands.try_send(TopologyCommand::Refresh);
    }

    /// `d.state.BumpGeneration(paneID)` after a successful lifecycle
    /// mutation (`agent_stop`, `agent_clear`/`agent_restart`). Unlike
    /// [`refresh`](Self::refresh) this waits on the bounded inbox — a
    /// dropped bump would leave a stale exact target validating against
    /// the replaced session.
    pub async fn bump_generation(&self, pane_id: String) {
        let _ = self
            .commands
            .send(TopologyCommand::BumpGeneration(pane_id))
            .await;
    }

    /// Herdr `[[startup]]` hook datagram (`lerdr-relay startup-hook`,
    /// delivered over UDP): the session was restored or the server
    /// live-handoff'ed — re-read topology and re-assert the transient
    /// `agent.view.set` projection. Cheap to call redundantly; a full
    /// inbox already implies a queued refresh, so a drop just skips one
    /// re-assert (the next `Synced`/hook re-fires it).
    pub async fn startup_hook(&self) {
        let _ = self.commands.try_send(TopologyCommand::StartupHook);
    }
}

/// The one-per-relay topology actor.
pub struct TopologyActor;

impl TopologyActor {
    /// Spawn the supervisor loop. `cancel` terminates the task (wire it to
    /// the relay's shutdown token). Returns the handle immediately — the
    /// first `Synced` may lag; `Topology::default()` is `stale=true` until
    /// then.
    pub fn spawn(client: Client, cancel: CancellationToken) -> TopologyHandle {
        let (topology_tx, topology_rx) = watch::channel(Arc::new(Topology::default()));
        let (inv_tx, _) = broadcast::channel(256);
        let (cmd_tx, mut cmd_rx) = mpsc::channel(16);
        let handle = TopologyHandle {
            topology: topology_rx,
            invalidations: inv_tx.clone(),
            client: client.clone(),
            commands: cmd_tx.clone(),
        };

        tokio::spawn(
            async move {
                let mut state = Topology::default();
                let mut stream =
                    client.supervise_events(EventSupervisor::topology());
                // `RunCapabilityRefresh(30s)` — the oracle's periodic
                // re-probe; catches server changes that never drop the
                // socket. First tick deferred so it can't race bootstrap.
                let mut capability_tick = tokio::time::interval_at(
                    tokio::time::Instant::now() + std::time::Duration::from_secs(30),
                    std::time::Duration::from_secs(30),
                );
                capability_tick
                    .set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => break,
                        signal = stream.next_signal() => {
                            let Some(signal) = signal else { break };
                            match signal {
                                SupervisorSignal::Synced(snapshot) => {
                                    state.accept(*snapshot);
                                    let _ = topology_tx.send(Arc::new(clone_topology(&state)));
                                    // Bootstrap/resync done — the server may
                                    // be a different build (live handoff):
                                    // re-collect capabilities and re-assert
                                    // the transient agent view, off-loop so
                                    // slow probes never stall invalidations.
                                    spawn_post_sync(&client, &cmd_tx);
                                }
                                SupervisorSignal::Invalidated { event, .. } => {
                                    if event.is_topology() {
                                        // Invalidation semantics: re-read,
                                        // never apply the payload.
                                        if refresh_snapshot(&client, &mut state).await {
                                            let _ = topology_tx
                                                .send(Arc::new(clone_topology(&state)));
                                        }
                                    } else if event.is_pane() {
                                        let _ = inv_tx.send(Invalidation {
                                            name: event.name.clone(),
                                            pane_id: pane_id_of(&event),
                                            event,
                                        });
                                    }
                                    // Non-pane, non-topology events
                                    // (agent.*, server.*) — baseline drops
                                    // them; attention projection lands with
                                    // the notification subsystem.
                                }
                                SupervisorSignal::Reconnecting { attempt, delay, cause } => {
                                    debug!(attempt, ?delay, %cause, "herdr event stream reconnecting");
                                    if state.mark_stale() {
                                        let _ = topology_tx
                                            .send(Arc::new(clone_topology(&state)));
                                    }
                                }
                            }
                        }
                        command = cmd_rx.recv() => {
                            match command {
                                Some(TopologyCommand::Refresh) => {
                                    // External poke (UDP hook): same
                                    // invalidation semantics — re-read,
                                    // never trust the payload.
                                    if refresh_snapshot(&client, &mut state).await {
                                        let _ = topology_tx
                                            .send(Arc::new(clone_topology(&state)));
                                    }
                                }
                                Some(TopologyCommand::StartupHook) => {
                                    // [[startup]] hook — restore/handoff.
                                    // Refresh like a poke, then re-assert
                                    // per-server transient state.
                                    if refresh_snapshot(&client, &mut state).await {
                                        let _ = topology_tx
                                            .send(Arc::new(clone_topology(&state)));
                                    }
                                    spawn_post_sync(&client, &cmd_tx);
                                }
                                Some(TopologyCommand::BumpGeneration(pane_id)) => {
                                    state.bump_generation(&pane_id);
                                    let _ = topology_tx
                                        .send(Arc::new(clone_topology(&state)));
                                }
                                Some(TopologyCommand::CapabilitiesReady(report)) => {
                                    // Unchanged evidence carries unchanged
                                    // generations, so the equality inside
                                    // set_herdr_status suppresses no-change
                                    // republishes.
                                    if state.set_herdr_status(report_status(&report)) {
                                        let _ = topology_tx
                                            .send(Arc::new(clone_topology(&state)));
                                    }
                                }
                                None => break,
                            }
                        }
                        _ = capability_tick.tick() => {
                            // Periodic re-probe — collect only; the view
                            // assert stays on bootstrap/startup paths.
                            spawn_collect(&client, &cmd_tx);
                        }
                    }
                }
                info!("topology actor stopped");
            }
            .instrument(tracing::info_span!("topology_actor")),
        );

        handle
    }
}

/// The shared re-read used by topology invalidations, refresh pokes, and
/// the startup hook: `session.snapshot` replaces state wholesale (never an
/// event payload). Returns `true` when the caller should republish.
async fn refresh_snapshot(client: &Client, state: &mut Topology) -> bool {
    match client.session_snapshot().await {
        Ok(fresh) => {
            state.accept(fresh);
            true
        }
        Err(err) => {
            warn!(error = %err, "snapshot refresh failed; staying on last state");
            false
        }
    }
}

/// Post-bootstrap/handoff work, spawned so probe latency never stalls the
/// select loop: refresh the capability ledger and hand the report back
/// through the command lane (deduped there), then re-assert the canonical
/// `agent.view.set` projection — transient per-server state that session
/// restore and `server.live_handoff` drop. Overlapping collects serialize
/// inside the client (`refreshMu`); view asserts are idempotent.
fn spawn_post_sync(client: &Client, commands: &mpsc::Sender<TopologyCommand>) {
    let client = client.clone();
    let commands = commands.clone();
    tokio::spawn(async move {
        let report = client.collect_capabilities().await;
        // Bounded wait — the lane drains while the actor lives; a dead
        // actor makes the send fail, which is fine.
        let _ = commands
            .send(TopologyCommand::CapabilitiesReady(report))
            .await;
        match assert_agent_view(&client).await {
            ViewAssertOutcome::Installed => debug!("canonical agent view asserted"),
            ViewAssertOutcome::KnownUnsupported => {
                debug!("agent.view.set known-unsupported on this server; view assert skipped")
            }
            ViewAssertOutcome::Failed(err) => {
                warn!(error = %err, "agent.view.set reassert failed after bounded retries")
            }
        }
    });
}

/// The periodic collect (`RunCapabilityRefresh`'s tick) — same as
/// [`spawn_post_sync`] minus the view assert.
fn spawn_collect(client: &Client, commands: &mpsc::Sender<TopologyCommand>) {
    let client = client.clone();
    let commands = commands.clone();
    tokio::spawn(async move {
        let report = client.collect_capabilities().await;
        let _ = commands
            .send(TopologyCommand::CapabilitiesReady(report))
            .await;
    });
}

/// `CapabilityReport` → the `herdrStatusPayload` wire shape — every field
/// the oracle fills from `ServerStatus`. `features` is always
/// `MaybeNull::Value`: Kotlin decodes the map non-nullable.
fn report_status(report: &CapabilityReport) -> HerdrStatus {
    HerdrStatus {
        installed_client_version: report.installed_client_version.clone(),
        server_version: report.server_version.clone(),
        server_protocol: report.server_protocol,
        server_protocol_known: report.server_protocol_known,
        endpoint_protocol_generation: report.endpoint_protocol_generation,
        surface_interest: report.surface_interest,
        health_check: report.health_check,
        generation: report.generation,
        features: MaybeNull::Value(
            report
                .features
                .iter()
                .map(|(name, evidence)| {
                    (
                        name.clone(),
                        HerdrFeatureStatus {
                            state: evidence.state.as_str().to_owned(),
                            reason: evidence.reason.clone(),
                            generation: evidence.generation,
                        },
                    )
                })
                .collect(),
        ),
    }
}

/// `Topology` is not `Clone` (its snapshot is big); publish a fresh `Arc`
/// rather than mutating in place so watchers never observe a half-applied
/// state.
fn clone_topology(state: &Topology) -> Topology {
    Topology {
        snapshot: state.snapshot.clone(),
        revision: state.revision,
        stale: state.stale,
        generations: state.generations.clone(),
        agent_times: state.agent_times.clone(),
        accepted_at: state.accepted_at,
        herdr_status: state.herdr_status.clone(),
    }
}

/// Best-effort `pane_id` extraction — event payloads are schema-varied;
/// `pane_id`/`pane`/`id` cover the documented shapes.
fn pane_id_of(event: &Event) -> Option<String> {
    event
        .data
        .get("pane_id")
        .or_else(|| event.data.get("pane"))
        .or_else(|| event.data.get("id"))
        .and_then(|v| v.as_str())
        .map(str::to_owned)
}

/// mpsc helper kept for symmetry with future request lanes (the actor core
/// stays a synchronous state machine — see rust-relay skill rule 3).
#[allow(dead_code)]
type ActorInbox<T> = mpsc::Sender<T>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_id_extraction() {
        let event = |data: serde_json::Value| Event {
            name: "pane.updated".into(),
            data,
        };
        assert_eq!(
            pane_id_of(&event(serde_json::json!({"pane_id": "wE:p1"}))),
            Some("wE:p1".into())
        );
        assert_eq!(
            pane_id_of(&event(serde_json::json!({"pane": "wE:p2"}))),
            Some("wE:p2".into())
        );
        assert_eq!(pane_id_of(&event(serde_json::json!({}))), None);
    }

    /// Accepts the socket then holds it silent — the supervisor never
    /// finishes subscribing, so only the command lane drives the actor.
    struct SilentTransport;

    impl lerdr_herdr::Transport for SilentTransport {
        fn dial(
            &self,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = std::io::Result<lerdr_herdr::BoxIo>> + Send>,
        > {
            Box::pin(async {
                let (client_end, mut server_end) = tokio::io::duplex(1024);
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    while tokio::io::AsyncReadExt::read(&mut server_end, &mut buf)
                        .await
                        .unwrap_or(0)
                        > 0
                    {}
                });
                Ok(Box::new(client_end) as lerdr_herdr::BoxIo)
            })
        }

        fn describe(&self) -> String {
            "silent".to_owned()
        }
    }

    #[tokio::test]
    async fn bump_generation_reaches_the_published_topology() {
        let client = Client::new(
            Arc::new(SilentTransport),
            lerdr_herdr::ClientConfig::default(),
        );
        let cancel = CancellationToken::new();
        let handle = TopologyActor::spawn(client, cancel.clone());
        let mut rx = handle.topology.clone();

        handle.bump_generation("wE:p1".to_owned()).await;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while rx.borrow().generation_of("wE:p1") == 0 {
                rx.changed().await.expect("topology channel closed");
            }
        })
        .await
        .expect("generation bump was not published");
        assert_eq!(rx.borrow().generation_of("wE:p1"), 1);
        cancel.cancel();
    }

    /// A scripted in-memory Herdr: the subscription handshake then an open
    /// stream, `session.snapshot`, `ping`, `agent.view.set`, and the probe
    /// methods (validation-refused). Counts `agent.view.set` and
    /// `session.snapshot` requests so re-assertion is observable.
    struct MiniHerdr {
        view_sets: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        snapshots: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl MiniHerdr {
        fn new() -> Self {
            Self {
                view_sets: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                snapshots: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }
        }
        fn view_sets(&self) -> usize {
            self.view_sets.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn snapshots(&self) -> usize {
            self.snapshots.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl lerdr_herdr::Transport for MiniHerdr {
        fn dial(
            &self,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = std::io::Result<lerdr_herdr::BoxIo>> + Send>,
        > {
            let view_sets = self.view_sets.clone();
            let snapshots = self.snapshots.clone();
            Box::pin(async move {
                let (client_end, server_end) = tokio::io::duplex(64 * 1024);
                tokio::spawn(serve_conn(server_end, view_sets, snapshots));
                Ok(Box::new(client_end) as lerdr_herdr::BoxIo)
            })
        }

        fn describe(&self) -> String {
            "mini".to_owned()
        }
    }

    async fn serve_conn(
        mut conn: tokio::io::DuplexStream,
        view_sets: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        snapshots: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // One NDJSON request line per connection.
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        let line = loop {
            match conn.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                        break buf[..pos].to_vec();
                    }
                }
            }
        };
        let request: serde_json::Value = match serde_json::from_slice(&line) {
            Ok(v) => v,
            Err(_) => return,
        };
        let id = request["id"].as_str().unwrap_or_default().to_owned();
        let reply = |result: serde_json::Value| {
            serde_json::json!({"id": id, "result": result}).to_string() + "\n"
        };
        match request["method"].as_str().unwrap_or_default() {
            "events.subscribe" => {
                let _ = conn
                    .write_all(
                        serde_json::json!({"id": id, "result": {"type": "subscription_started"}})
                            .to_string()
                            .as_bytes(),
                    )
                    .await;
                let _ = conn.write_all(b"\n").await;
                // Hold the subscription open until the client drops it.
                let mut sink = [0u8; 256];
                while conn.read(&mut sink).await.map(|n| n > 0).unwrap_or(false) {}
            }
            "session.snapshot" => {
                snapshots.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = conn
                    .write_all(
                        reply(serde_json::json!({
                            "type": "session_snapshot",
                            "snapshot": {
                                "version": "0.9.1", "protocol": 22,
                                "workspaces": [], "tabs": [], "panes": [],
                                "layouts": [], "agents": []
                            }
                        }))
                        .as_bytes(),
                    )
                    .await;
            }
            "ping" => {
                let _ = conn
                    .write_all(
                        reply(serde_json::json!({
                            "type": "pong", "version": "0.9.1", "protocol": 22
                        }))
                        .as_bytes(),
                    )
                    .await;
            }
            "agent.view.set" => {
                view_sets.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = conn
                    .write_all(
                        reply(serde_json::json!({
                            "type": "agent_view", "active": true,
                            "source": "plugin:lerdr.events"
                        }))
                        .as_bytes(),
                    )
                    .await;
            }
            // The optimistic probes: each recognized validation refusal
            // proves the method without touching real state.
            m @ ("workspace.move_block" | "tab.move" | "pane.read") => {
                let code = match m {
                    "workspace.move_block" => "workspace_move_block_failed",
                    "tab.move" => "tab_not_found",
                    _ => "pane_not_found",
                };
                let _ = conn
                    .write_all(
                        (serde_json::json!({"id": id, "error": {
                            "code": code, "message": "validation refused"
                        }})
                        .to_string()
                            + "\n")
                            .as_bytes(),
                    )
                    .await;
            }
            _ => {
                let _ = conn
                    .write_all(
                        (serde_json::json!({"id": id, "error": {
                            "code": "unknown_method", "message": "nope"
                        }})
                        .to_string()
                            + "\n")
                            .as_bytes(),
                    )
                    .await;
            }
        }
    }

    /// Poll `f` with a real-time deadline — small, deterministic, and does
    /// not depend on topology publishes (post-sync work happens off-loop).
    async fn until(deadline_secs: u64, what: &str, f: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(deadline_secs);
        while !f() {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    fn mini_client(server: &Arc<MiniHerdr>) -> Client {
        Client::new(
            server.clone() as Arc<dyn lerdr_herdr::Transport>,
            lerdr_herdr::ClientConfig {
                // Probe path only — never touch the host's herdr binary.
                herdr_bin: Some(std::path::PathBuf::from("/nonexistent/herdr")),
                schema_source: lerdr_herdr::SchemaSource::Disabled,
                ..lerdr_herdr::ClientConfig::default()
            },
        )
    }

    #[tokio::test]
    async fn sync_publishes_capabilities_and_asserts_view() {
        let server = Arc::new(MiniHerdr::new());
        let client = mini_client(&server);
        let cancel = CancellationToken::new();
        let handle = TopologyActor::spawn(client, cancel.clone());
        let mut rx = handle.topology.clone();

        // Synced → snapshot accepted; capability evidence lands a moment
        // later through the post-sync task.
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while rx.borrow().stale {
                rx.changed().await.expect("topology channel closed");
            }
        })
        .await
        .expect("never synced");
        until(5, "herdr_status published", || {
            handle
                .topology
                .borrow()
                .herdr_status
                .features
                .value()
                .is_some_and(|m| !m.is_empty())
        })
        .await;
        until(5, "agent.view.set asserted", || server.view_sets() >= 1).await;

        let status = handle.topology.borrow().herdr_status.clone();
        // The pong fields project through too (server_version/protocol).
        assert_eq!(status.server_version, "0.9.1");
        assert_eq!(status.server_protocol, 22);
        assert!(status.server_protocol_known);
        let features = status.features.value().cloned().unwrap_or_default();
        assert_eq!(features["ordinary_json"].state, "supported");
        // pane.read was validation-refused — probe semantics say supported.
        assert_eq!(features["pane.read"].state, "supported");
        assert_eq!(
            features["pane.read"].reason,
            "recognized_validation_refusal"
        );
        cancel.cancel();
    }

    #[tokio::test]
    async fn startup_hook_reasserts_view_and_refreshes() {
        let server = Arc::new(MiniHerdr::new());
        let client = mini_client(&server);
        let cancel = CancellationToken::new();
        let handle = TopologyActor::spawn(client, cancel.clone());

        until(5, "initial view assert", || server.view_sets() >= 1).await;
        let snapshots_before = server.snapshots();

        // The [[startup]] datagram path — post-restore/handoff reassert.
        handle.startup_hook().await;
        until(5, "startup-hook snapshot re-read", || {
            server.snapshots() > snapshots_before
        })
        .await;
        until(5, "startup-hook view re-assert", || server.view_sets() >= 2).await;
        cancel.cancel();
    }
}
