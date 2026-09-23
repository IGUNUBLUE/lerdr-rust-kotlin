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

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use lerdr_herdr::{AgentStatus, Client, Event, EventSupervisor, SessionSnapshot, SupervisorSignal};
use tokio::sync::{broadcast, mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn, Instrument};

use crate::classify::Classification;
use crate::snapshot::{broadcast_diff, PublishedView};
use crate::topology::{AcceptOutcome, CommitKind, Topology};

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

/// `SetOnTransition` — the projector's transition channel. The callback
/// shape is a bounded queue of [`AcceptOutcome`]s (one per committed
/// snapshot: transitions, removed panes) rather than a spawned
/// goroutine per record — the drain decides what to spawn, matching
/// `transitionTasks.Start` ordering without blocking the accept loop
/// when no consumer is attached.
type TransitionSink = Arc<std::sync::Mutex<Option<mpsc::Sender<AcceptOutcome>>>>;

/// `poller.SetEnrich` (server.go:1235-1255) — one fresh classification
/// per blocked pane, produced ahead of each topology commit so the
/// commit itself observes kind/options drift (`attentionChanged`, the
/// blocked→blocked refire). The projector installs it; `None` means the
/// commit runs unenriched like a nil `SetEnrich`.
pub(crate) type EnrichHook = Arc<
    dyn Fn(String, String) -> Pin<Box<dyn Future<Output = Classification> + Send>> + Send + Sync,
>;
type EnrichSlot = Arc<std::sync::Mutex<Option<EnrichHook>>>;

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
    /// The transition sink `SetOnTransition` installs (`None` until the
    /// semantic projector subscribes — `accept` then costs nothing).
    transitions: TransitionSink,
    /// The classification hook `SetEnrich` installs — consulted once per
    /// blocked incoming agent ahead of every `accept`.
    enrich: EnrichSlot,
}

/// External triggers the actor honors beside the event stream.
#[derive(Debug)]
enum TopologyCommand {
    /// Re-read `session.snapshot` unconditionally — the UDP event hook and
    /// the Herdr `[[startup]]` hook use this as their poke.
    Refresh,
    /// `d.state.BumpGeneration` — a lifecycle mutation replaced the pane's
    /// session; advance its epoch and republish so stale exact targets
    /// stop validating.
    BumpGeneration(String),
}

impl TopologyHandle {
    /// Ask the actor to re-read the session snapshot. Cheap to call
    /// redundantly — refreshes coalesce behind the in-flight read.
    pub async fn refresh(&self) {
        self.try_refresh();
    }

    /// `try_send` half of [`refresh`](Self::refresh) — `d.wake()` is a
    /// synchronous poke in the oracle (the poller's channel); spawn-free
    /// callers (the ack path inside `HandleReadPane`) use this.
    pub fn try_refresh(&self) {
        // Full inbox = a refresh is already queued; dropping is correct.
        let _ = self.commands.try_send(TopologyCommand::Refresh);
    }

    /// `state.SetOnTransition` — install the channel each committed
    /// snapshot's [`AcceptOutcome`] is pushed to. Latest wins; the
    /// projector owns the receiver.
    pub(crate) fn set_transition_sink(&self, sink: mpsc::Sender<AcceptOutcome>) {
        *self.transitions.lock().expect("transition sink poisoned") = Some(sink);
    }

    /// `poller.SetEnrich` — install the blocked-pane classification hook
    /// each snapshot commit consults. Latest wins; the semantic
    /// projector owns the implementation.
    pub(crate) fn set_enrich(&self, hook: EnrichHook) {
        *self.enrich.lock().expect("enrich hook poisoned") = Some(hook);
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

    /// A handle over a caller-supplied topology with no actor behind it —
    /// commands and the transition sink are inert (the command queue's
    /// receiver is dropped, so `try_send`/`send` fail silently like a
    /// stopped actor). Tests that drive `WatchDeps`/`ProjectorDeps` build
    /// through this.
    #[cfg(test)]
    pub(crate) fn for_test(
        client: Client,
        topology: Arc<Topology>,
        invalidations: broadcast::Sender<Invalidation>,
    ) -> TopologyHandle {
        let (_topology_tx, topology_rx) = watch::channel(topology);
        let (commands, _commands_rx) = mpsc::channel(16);
        TopologyHandle {
            topology: topology_rx,
            invalidations,
            client,
            commands,
            transitions: Default::default(),
            enrich: Default::default(),
        }
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
        let transitions: TransitionSink = Default::default();
        let enrich: EnrichSlot = Default::default();
        let handle = TopologyHandle {
            topology: topology_rx,
            invalidations: inv_tx.clone(),
            client: client.clone(),
            commands: cmd_tx,
            transitions: transitions.clone(),
            enrich: enrich.clone(),
        };

        tokio::spawn(
            async move {
                let mut state = Topology::default();
                let mut stream =
                    client.supervise_events(EventSupervisor::topology());
                let mut published = PublishedView::default();
                let mut events_active = false;
                // `Poller.Run`'s leading poll (poller.go:92) — one
                // inventory reconcile up front so the committed view does
                // not wait on the event bootstrap; failures fold into the
                // retry backoff below.
                let mut poll_failures = record_poll(
                    poll_once(
                        &client,
                        &enrich,
                        &mut state,
                        &topology_tx,
                        &transitions,
                        &mut published,
                    )
                    .await,
                    0,
                );
                let mut poll_timer = Box::pin(tokio::time::sleep(poll_interval(
                    events_active,
                    poll_failures,
                )));
                loop {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => break,
                        signal = stream.next_signal() => {
                            let Some(signal) = signal else { break };
                            match signal {
                                SupervisorSignal::Synced(snapshot) => {
                                    // `commitEventTopology` on bootstrap —
                                    // the event path never lets a sampled
                                    // status overwrite the committed
                                    // stream for an existing pane.
                                    let enrichments =
                                        collect_enrichments(&enrich, &snapshot).await;
                                    let outcome = state.accept_enriched(
                                        *snapshot,
                                        &enrichments,
                                        CommitKind::Event,
                                    );
                                    events_active = true;
                                    publish(&state, &mut published, &topology_tx);
                                    forward_outcome(&transitions, outcome).await;
                                }
                                SupervisorSignal::Invalidated { event, .. } => {
                                    // Pane lifecycle events mutate the
                                    // oracle's `SessionCache` (events.go:
                                    // `Apply` — pane.{created,updated,
                                    // moved,closed,exited,agent_detected}),
                                    // so they are topology triggers too:
                                    // re-read, never apply the payload. The
                                    // remaining `pane.*` names stay pure
                                    // watch nudges.
                                    if event.is_topology() || is_pane_topology(&event) {
                                        match client.session_snapshot().await {
                                            Ok(fresh) => {
                                                let enrichments =
                                                    collect_enrichments(&enrich, &fresh).await;
                                                let outcome = state.accept_enriched(
                                                    fresh,
                                                    &enrichments,
                                                    CommitKind::Event,
                                                );
                                                publish(&state, &mut published, &topology_tx);
                                                forward_outcome(&transitions, outcome).await;
                                            }
                                            Err(err) => {
                                                warn!(error = %err, "snapshot refresh failed; staying on last state");
                                            }
                                        }
                                    }
                                    if event.is_pane() {
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
                                    events_active = false;
                                    if state.mark_stale() {
                                        publish(&state, &mut published, &topology_tx);
                                    }
                                }
                            }
                        }
                        () = &mut poll_timer => {
                            // `case <-timer.C` (poller.go:111-118): the
                            // reconcile poll — the only status-adopting
                            // commit besides `d.wake()`.
                            poll_failures = record_poll(
                                poll_once(&client, &enrich, &mut state, &topology_tx, &transitions, &mut published).await,
                                poll_failures,
                            );
                            poll_timer.as_mut().reset(
                                tokio::time::Instant::now()
                                    + poll_interval(events_active, poll_failures),
                            );
                        }
                        command = cmd_rx.recv() => {
                            match command {
                                Some(TopologyCommand::Refresh) => {
                                    // `d.wake()` (dispatch.go:1040) — the
                                    // poller poke behind reads, acks, and
                                    // command side effects. Polls adopt
                                    // live status; the oracle resets its
                                    // timer on a wake, and so does this.
                                    poll_failures = record_poll(
                                        poll_once(&client, &enrich, &mut state, &topology_tx, &transitions, &mut published).await,
                                        poll_failures,
                                    );
                                    poll_timer.as_mut().reset(
                                        tokio::time::Instant::now()
                                            + poll_interval(events_active, poll_failures),
                                    );
                                }
                                Some(TopologyCommand::BumpGeneration(pane_id)) => {
                                    state.bump_generation(&pane_id);
                                    publish(&state, &mut published, &topology_tx);
                                }
                                None => break,
                            }
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

/// `idlePollInterval` (poller.go:17) — while the event stream is healthy
/// the reconcile poll runs on this fixed cadence. The oracle's configured
/// interval only applies while the stream is down, and even then
/// `normalizePollInterval` clamps it to ≤ 15s — the relay configures no
/// shorter value, so 15s is the base either way.
const IDLE_POLL_INTERVAL: Duration = Duration::from_secs(15);
/// `maxPollRetryInterval` (poller.go:18) — consecutive failed polls
/// double the interval up to this cap.
const MAX_POLL_RETRY_INTERVAL: Duration = Duration::from_secs(60);
/// `maxPollRetryFailures` (poller.go:20) — the backoff counter stops
/// doubling here.
const MAX_POLL_RETRY_FAILURES: u32 = 64;

/// `pollRetryInterval` (poller.go:434-445) — the healthy interval doubles
/// once per consecutive failed poll, capped at the outage interval.
/// `currentInterval` collapses to the 15s base in both stream states
/// (see [`IDLE_POLL_INTERVAL`]), so `events_active` is carried only to
/// keep the shape of the oracle's computation visible.
fn poll_interval(_events_active: bool, failures: u32) -> Duration {
    let mut interval = IDLE_POLL_INTERVAL;
    for _ in 0..failures {
        if interval >= MAX_POLL_RETRY_INTERVAL || interval > MAX_POLL_RETRY_INTERVAL / 2 {
            return MAX_POLL_RETRY_INTERVAL;
        }
        interval *= 2;
    }
    interval
}

/// `pollRetryFailures` bookkeeping — a successful poll clears the
/// backoff; a failed one doubles the next interval (poller.go:382-390).
fn record_poll(ok: bool, failures: u32) -> u32 {
    if ok {
        0
    } else {
        failures.saturating_add(1).min(MAX_POLL_RETRY_FAILURES)
    }
}

/// One `poll()` (poller.go:119-183): re-read the session snapshot and
/// commit on the inventory path — the fresh sample is authoritative here
/// (`CommitPoll`), unlike the event-path accepts. `CommitPoll`'s
/// topology-generation fence has no counterpart: this actor is the sole
/// writer, so a sampled inventory can never race an event commit.
/// Returns `false` on a fetch failure — the caller folds it into the
/// retry backoff.
async fn poll_once(
    client: &Client,
    enrich: &EnrichSlot,
    state: &mut Topology,
    topology_tx: &watch::Sender<Arc<Topology>>,
    transitions: &TransitionSink,
    published: &mut PublishedView,
) -> bool {
    match client.session_snapshot().await {
        Ok(fresh) => {
            let enrichments = collect_enrichments(enrich, &fresh).await;
            let outcome = state.accept_enriched(fresh, &enrichments, CommitKind::Poll);
            publish(state, published, topology_tx);
            forward_outcome(transitions, outcome).await;
            true
        }
        Err(err) => {
            warn!(error = %err, "inventory poll failed; staying on last state");
            // `MarkInventoryFailure` — inventory-not-ready + the
            // `command_failed` pair; the transport `stale` flag is the
            // event stream's, not the poll's.
            if state.mark_inventory_failure() {
                publish(state, published, topology_tx);
            }
            false
        }
    }
}

/// Stamp the dedup'd broadcast batch onto a fresh projection and publish
/// it — `publishCurrentInventory`'s batch + `commit` in one step
/// (snapshot.rs `broadcast_diff` holds the `stateViewMu` comparison).
fn publish(
    state: &Topology,
    published: &mut PublishedView,
    topology_tx: &watch::Sender<Arc<Topology>>,
) {
    let mut projected = clone_topology(state);
    projected.broadcast_frames = broadcast_diff(state, published);
    let _ = topology_tx.send(Arc::new(projected));
}

/// The `onTransition` fan-out — push the commit's outcome to the
/// projector once the publish landed (the sink's fences then see at
/// least this revision, like the oracle's post-commit task start). No
/// sink = no projector attached; the outcome is dropped, matching the
/// oracle's nil `SetOnTransition`.
async fn forward_outcome(transitions: &TransitionSink, outcome: AcceptOutcome) {
    let sink = transitions
        .lock()
        .expect("transition sink poisoned")
        .clone();
    if let Some(sink) = sink {
        // A dead projector closes the channel — drop, don't block.
        let _ = sink.send(outcome).await;
    }
}

/// `p.enrich(ctx, agents)` (poller.go:169-170, 340-341) — classify every
/// blocked incoming agent's live content ahead of the commit, sequential
/// like the oracle's per-agent loop. The hook owns the read timeout and
/// the error fallback; an absent hook commits unenriched (nil
/// `SetEnrich`).
async fn collect_enrichments(
    slot: &EnrichSlot,
    snapshot: &SessionSnapshot,
) -> BTreeMap<String, Classification> {
    let hook = slot.lock().expect("enrich hook poisoned").clone();
    let Some(hook) = hook else {
        return BTreeMap::new();
    };
    let mut out = BTreeMap::new();
    for agent in &snapshot.agents {
        if agent.agent_status != AgentStatus::Blocked {
            continue;
        }
        let pane_id = agent.pane_id.clone();
        out.insert(
            pane_id.clone(),
            hook(pane_id, agent.agent.clone().unwrap_or_default()).await,
        );
    }
    out
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
        // The attention ledger is shared, not copied — projector commits
        // must be visible to every already-published clone.
        attention: state.attention.clone(),
        accepted_at: state.accepted_at,
        attempted_at: state.attempted_at,
        inventory_ready: state.inventory_ready,
        inventory_error_code: state.inventory_error_code.clone(),
        inventory_message: state.inventory_message.clone(),
        herdr_features: state.herdr_features.clone(),
        // Stamped by `publish` — clones carry the batch decided for the
        // commit that produced them, never the previous one.
        broadcast_frames: Vec::new(),
    }
}

/// The `pane.*` names the oracle's `SessionCache.Apply` mutates
/// (events.go:620-848): pane lifecycle changes that commit topology.
/// `pane.output_changed`, `pane.scroll_changed`, `pane.output_matched`,
/// `pane.focused`, and `pane.agent_status_changed` are *not* in that set —
/// they stay watch nudges only.
fn is_pane_topology(event: &Event) -> bool {
    matches!(
        event.name.as_str(),
        "pane.created"
            | "pane.updated"
            | "pane.moved"
            | "pane.closed"
            | "pane.exited"
            | "pane.agent_detected"
    )
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

    /// Answers every request on its per-dial connection: `events.subscribe`
    /// gets `subscription_started` (the stream then sits silent — no events
    /// arrive), `session.snapshot` gets an empty snapshot so the actor's
    /// leading poll resolves, anything else an `ok`. The command lane is
    /// the only live driver once the bootstrap lands.
    struct EmptyTransport;

    impl lerdr_herdr::Transport for EmptyTransport {
        fn dial(
            &self,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = std::io::Result<lerdr_herdr::BoxIo>> + Send>,
        > {
            Box::pin(async {
                let (client_end, server_end) = tokio::io::duplex(4096);
                tokio::spawn(async move {
                    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
                    let mut reader = tokio::io::BufReader::new(server_end);
                    let mut line = String::new();
                    loop {
                        line.clear();
                        match reader.read_line(&mut line).await {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {
                                let Ok(request) = serde_json::from_str::<serde_json::Value>(&line)
                                else {
                                    continue;
                                };
                                let id = request
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default()
                                    .to_owned();
                                let method = request
                                    .get("method")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default();
                                let result = match method {
                                    "events.subscribe" => {
                                        serde_json::json!({"type": "subscription_started"})
                                    }
                                    "session.snapshot" => serde_json::json!({
                                        "type": "session_snapshot",
                                        "snapshot": {"version": "test", "protocol": 22}
                                    }),
                                    _ => serde_json::json!({"type": "ok"}),
                                };
                                let mut bytes = serde_json::to_vec(
                                    &serde_json::json!({"id": id, "result": result}),
                                )
                                .unwrap_or_default();
                                bytes.push(b'\n');
                                if reader.get_mut().write_all(&bytes).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                });
                Ok(Box::new(client_end) as lerdr_herdr::BoxIo)
            })
        }

        fn describe(&self) -> String {
            "empty".to_owned()
        }
    }

    #[tokio::test]
    async fn bump_generation_reaches_the_published_topology() {
        let client = Client::new(
            Arc::new(EmptyTransport),
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
}
