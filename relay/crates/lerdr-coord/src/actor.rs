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

use lerdr_core::json::MaybeNull;
use lerdr_core::protocol::{HerdrFeatureStatus, HerdrStatus};
use lerdr_herdr::{
    assert_agent_view, AgentStatus, CapabilityReport, Client, Event, EventSupervisor,
    SessionSnapshot, SupervisorSignal, ViewAssertOutcome,
};
use tokio::sync::{broadcast, mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn, Instrument};

use crate::classify::Classification;
use crate::snapshot::{broadcast_diff, PublishedView};
use crate::topology::{AcceptOutcome, CommitKind, LocalSpeech, Topology};

/// Pane-class invalidation signal for watch tasks. `gap` events (arriving
/// between `subscription_started` and the bootstrap snapshot) are forwarded
/// too — watchers treat them identically since they always re-read.
#[derive(Debug, Clone)]
pub struct Invalidation {
    /// Canonical event name (`pane.updated`, `pane.agent_status_changed`, …).
    pub name: String,
    /// Pane id when the event payload carries one.
    pub pane_id: Option<String>,
    /// The pane's upstream output revision — `pane.output_changed`'s
    /// `data.revision`; `None` on every other event. Watchers dedupe on
    /// it: at or below the served watermark the wake is already covered.
    pub output_revision: Option<u64>,
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

/// The session-title resolver `resolveAgentSessionName` consults — one
/// `conversation::Resolver` shared with the projector's reader. `None`
/// means commits project `session_name = ""` (the pre-resolver shape).
pub(crate) type ResolverSlot = Arc<std::sync::Mutex<Option<Arc<crate::conversation::Resolver>>>>;

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
    /// The title resolver the semantic projector installs — consulted once
    /// per incoming agent ahead of every `accept`.
    resolver: ResolverSlot,
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
    /// Relay-local lane: the speech catalog probe (or a voice-change
    /// handler) delivered fresh `LocalSpeech` facts — the snapshot
    /// adjudicator gates `speech_*` capabilities and fills
    /// `push_config.speech_languages` from them.
    SpeechFacts(LocalSpeech),
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

    /// `NewResolverWithReader` wiring — install the session-title resolver
    /// each snapshot commit consults (`resolveAgentSessionName`,
    /// server.go:1237). Shares the projector's `conversation::Reader` so
    /// title and history consumers agree on transcript locations.
    pub(crate) fn set_resolver(&self, resolver: Arc<crate::conversation::Resolver>) {
        *self.resolver.lock().expect("resolver slot poisoned") = Some(resolver);
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
    /// through this. The returned sender lets a test publish a fresh
    /// `Arc<Topology>` mid-flight — the mid-read fence tests race a
    /// content-revision bump against an in-flight `pane_read` with it.
    #[cfg(test)]
    pub(crate) fn for_test(
        client: Client,
        topology: Arc<Topology>,
        invalidations: broadcast::Sender<Invalidation>,
    ) -> (TopologyHandle, watch::Sender<Arc<Topology>>) {
        let (topology_tx, topology_rx) = watch::channel(topology);
        let (commands, _commands_rx) = mpsc::channel(16);
        (
            TopologyHandle {
                topology: topology_rx,
                invalidations,
                client,
                commands,
                transitions: Default::default(),
                enrich: Default::default(),
                resolver: Default::default(),
            },
            topology_tx,
        )
    }

    /// Herdr `[[startup]]` hook datagram (`lerdr-relay startup-hook`,
    /// delivered over UDP): the session was restored or the server
    /// live-handoff'ed — re-read topology and re-assert the transient
    /// `agent.view.set` projection when the actor was spawned with it
    /// enabled. Cheap to call redundantly; a full inbox already implies
    /// a queued refresh, so a drop just skips one re-assert (the next
    /// hook re-fires it).
    pub async fn startup_hook(&self) {
        let _ = self.commands.try_send(TopologyCommand::StartupHook);
    }

    /// Push relay-local speech catalog facts into the committed view —
    /// the factory's post-construction probe and the voice-change
    /// handlers call this; the snapshot adjudicator reads them for the
    /// `speech_*` capability gates and `push_config.speech_languages`.
    /// Cheap and deduped actor-side (`set_local_speech`), so callers
    /// re-push freely after any catalog-affecting operation.
    pub(crate) fn speech_facts(&self, facts: LocalSpeech) {
        let _ = self.commands.try_send(TopologyCommand::SpeechFacts(facts));
    }
}

/// The one-per-relay topology actor.
pub struct TopologyActor;

impl TopologyActor {
    /// Spawn the supervisor loop. `cancel` terminates the task (wire it to
    /// the relay's shutdown token). Returns the handle immediately — the
    /// first `Synced` may lag; `Topology::default()` is `stale=true` until
    /// then. The `agent.view` assert is off here — the shipped default —
    /// matching `Config::agent_view`; [`spawn_with_agent_view`] opts in.
    pub fn spawn(client: Client, cancel: CancellationToken) -> TopologyHandle {
        Self::spawn_with_agent_view(client, cancel, false)
    }

    /// Same as [`spawn`](Self::spawn) with an explicit `agent.view` choice.
    /// The projection is a courtesy for Herdr's sidebar/mobile ordering —
    /// lerdr's own data path never reads it — and the slot is a single
    /// global last-writer-wins resource, so asserting is opt-in.
    pub fn spawn_with_agent_view(
        client: Client,
        cancel: CancellationToken,
        agent_view: bool,
    ) -> TopologyHandle {
        let (topology_tx, topology_rx) = watch::channel(Arc::new(Topology::default()));
        let (inv_tx, _) = broadcast::channel(256);
        let (cmd_tx, mut cmd_rx) = mpsc::channel(16);
        let transitions: TransitionSink = Default::default();
        let enrich: EnrichSlot = Default::default();
        let resolver: ResolverSlot = Default::default();
        let handle = TopologyHandle {
            topology: topology_rx,
            invalidations: inv_tx.clone(),
            client: client.clone(),
            commands: cmd_tx.clone(),
            transitions: transitions.clone(),
            enrich: enrich.clone(),
            resolver: resolver.clone(),
        };

        tokio::spawn(
            async move {
                let mut state = Topology {
                    resolver,
                    ..Topology::default()
                };
                let mut stream =
                    client.supervise_events(EventSupervisor::topology());
                let mut published = PublishedView::default();
                let mut events_active = false;
                // `agent.view` survives event-stream resubscribes — its
                // documented loss points are restore/handoff (the
                // `[[startup]]` hook) and explicit clear/replace — so
                // only the process's first `Synced` runs the view
                // assert; later resyncs collect capabilities only. The
                // assert itself is opt-in (`agent_view`): an operator
                // running another view writer keeps the slot.
                let mut first_sync = true;
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
                                    // Bootstrap/resync done — the server may
                                    // be a different build (live handoff):
                                    // re-collect capabilities, off-loop so
                                    // slow probes never stall invalidations.
                                    // The view assert rides only on the
                                    // first sync — a resubscribe cannot have
                                    // lost it, and re-asserting would stomp
                                    // a view another writer installed
                                    // mid-session.
                                    if first_sync {
                                        first_sync = false;
                                        spawn_post_sync(&client, &cmd_tx, agent_view);
                                    } else {
                                        spawn_collect(&client, &cmd_tx);
                                    }
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
                                        let pane_id = pane_id_of(&event);
                                        // `pane.output_changed` carries
                                        // the pane's upstream output
                                        // revision — fold it into the
                                        // shared watermark before the
                                        // wake goes out so a mid-flight
                                        // read's post-check observes it.
                                        let output_revision = if event.name
                                            == "pane.output_changed"
                                        {
                                            event
                                                .data
                                                .get("revision")
                                                .and_then(|v| v.as_u64())
                                        } else {
                                            None
                                        };
                                        if let (Some(id), Some(rev)) =
                                            (pane_id.as_deref(), output_revision)
                                        {
                                            state.note_upstream_rev(id, rev);
                                        }
                                        let _ = inv_tx.send(Invalidation {
                                            name: event.name.clone(),
                                            pane_id,
                                            output_revision,
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
                                Some(TopologyCommand::StartupHook) => {
                                    // [[startup]] hook — restore/handoff.
                                    // A poll-kind commit like `d.wake()`
                                    // (the post-handoff sample is
                                    // authoritative), then re-assert
                                    // per-server transient state.
                                    poll_failures = record_poll(
                                        poll_once(&client, &enrich, &mut state, &topology_tx, &transitions, &mut published).await,
                                        poll_failures,
                                    );
                                    poll_timer.as_mut().reset(
                                        tokio::time::Instant::now()
                                            + poll_interval(events_active, poll_failures),
                                    );
                                    spawn_post_sync(&client, &cmd_tx, agent_view);
                                }
                                Some(TopologyCommand::BumpGeneration(pane_id)) => {
                                    state.bump_generation(&pane_id);
                                    publish(&state, &mut published, &topology_tx);
                                }
                                Some(TopologyCommand::CapabilitiesReady(report)) => {
                                    // Unchanged evidence carries unchanged
                                    // generations, so the equality inside
                                    // set_herdr_status suppresses no-change
                                    // republishes. `publish` (not a bare
                                    // send) so the frame joins the dedup'd
                                    // broadcast batch.
                                    if state.set_herdr_status(report_status(&report)) {
                                        publish(&state, &mut published, &topology_tx);
                                    }
                                }
                                Some(TopologyCommand::SpeechFacts(facts)) => {
                                    // Same dedupe + publish pattern as
                                    // CapabilitiesReady — unchanged facts
                                    // carry no republish; changed ones
                                    // ride the broadcast batch (a flipped
                                    // speech cap emits `caps_update`).
                                    if state.set_local_speech(facts) {
                                        publish(&state, &mut published, &topology_tx);
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

/// Post-bootstrap/handoff work, spawned so probe latency never stalls the
/// select loop: refresh the capability ledger and hand the report back
/// through the command lane (deduped there), then re-assert the canonical
/// `agent.view.set` projection — transient per-server state that session
/// restore and `server.live_handoff` drop — when `agent_view` is on.
/// Called from the first `Synced` and the `[[startup]]` hook only: an
/// event-stream resubscribe does not kill the server-side view, so
/// asserting there would stomp a view another writer legitimately
/// installed mid-session. Overlapping collects serialize inside the
/// client (`refreshMu`); view asserts are idempotent.
fn spawn_post_sync(client: &Client, commands: &mpsc::Sender<TopologyCommand>, agent_view: bool) {
    let client = client.clone();
    let commands = commands.clone();
    tokio::spawn(async move {
        let report = client.collect_capabilities().await;
        // Bounded wait — the lane drains while the actor lives; a dead
        // actor makes the send fail, which is fine.
        let _ = commands
            .send(TopologyCommand::CapabilitiesReady(report))
            .await;
        if !agent_view {
            return;
        }
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
        // The attention ledger is shared, not copied — projector commits
        // must be visible to every already-published clone.
        attention: state.attention.clone(),
        // The resolver slot is shared too — installs land on the live
        // state's slot and clones only ever read it inside `accept`.
        resolver: state.resolver.clone(),
        accepted_at: state.accepted_at,
        attempted_at: state.attempted_at,
        inventory_ready: state.inventory_ready,
        inventory_error_code: state.inventory_error_code.clone(),
        inventory_message: state.inventory_message.clone(),
        herdr_status: state.herdr_status.clone(),
        local_speech: state.local_speech.clone(),
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

    /// A scripted in-memory Herdr: the subscription handshake then an open
    /// stream, `session.snapshot`, `ping`, `agent.view.set`, and the probe
    /// methods (validation-refused). Counts `agent.view.set` and
    /// `session.snapshot` requests so re-assertion is observable.
    /// `events` are written onto the subscription socket right after the
    /// handshake — the `control.emit`-style injection path.
    struct MiniHerdr {
        view_sets: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        snapshots: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        /// `events.subscribe` conns seen — the first
        /// `drop_subscriptions` close right after the handshake so the
        /// supervisor walks its resubscribe → `Synced` path.
        subscriptions: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        drop_subscriptions: usize,
        events: std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
        /// Pane rows every `session.snapshot` replies with.
        snapshot_panes: std::sync::Arc<Vec<serde_json::Value>>,
    }

    impl MiniHerdr {
        fn new() -> Self {
            Self::with_events(Vec::new())
        }

        fn with_events(events: Vec<Vec<u8>>) -> Self {
            Self::with_parts(events, Vec::new())
        }

        fn with_parts(events: Vec<Vec<u8>>, snapshot_panes: Vec<serde_json::Value>) -> Self {
            Self {
                view_sets: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                snapshots: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                subscriptions: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                drop_subscriptions: 0,
                events: std::sync::Arc::new(std::sync::Mutex::new(events)),
                snapshot_panes: std::sync::Arc::new(snapshot_panes),
            }
        }

        /// The first `drops` subscription conns close right after the
        /// handshake — each clean close drives the supervisor through
        /// `Reconnecting` → re-bootstrap → `Synced`.
        fn resyncing(drops: usize) -> Self {
            Self {
                drop_subscriptions: drops,
                ..Self::new()
            }
        }

        fn view_sets(&self) -> usize {
            self.view_sets.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn snapshots(&self) -> usize {
            self.snapshots.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn subscriptions(&self) -> usize {
            self.subscriptions.load(std::sync::atomic::Ordering::SeqCst)
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
            let subscriptions = self.subscriptions.clone();
            let drop_subscriptions = self.drop_subscriptions;
            let events = self.events.clone();
            let snapshot_panes = self.snapshot_panes.clone();
            Box::pin(async move {
                let (client_end, server_end) = tokio::io::duplex(64 * 1024);
                tokio::spawn(serve_conn(
                    server_end,
                    view_sets,
                    snapshots,
                    subscriptions,
                    drop_subscriptions,
                    events,
                    snapshot_panes,
                ));
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
        subscriptions: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        drop_subscriptions: usize,
        events: std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
        snapshot_panes: std::sync::Arc<Vec<serde_json::Value>>,
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
                // Scripted event lines land right after the handshake —
                // a resubscribe drains nothing (the queue is one-shot).
                let pending: Vec<Vec<u8>> =
                    events.lock().expect("events poisoned").drain(..).collect();
                for line in pending {
                    if conn.write_all(&line).await.is_err() {
                        return;
                    }
                    if conn.write_all(b"\n").await.is_err() {
                        return;
                    }
                }
                // A scripted drop closes the conn cleanly — the client
                // sees EOF and walks its resubscribe path.
                if subscriptions.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                    < drop_subscriptions
                {
                    return;
                }
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
                                "workspaces": [], "tabs": [],
                                "panes": *snapshot_panes,
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
        let handle = TopologyActor::spawn_with_agent_view(client, cancel.clone(), true);
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
        let handle = TopologyActor::spawn_with_agent_view(client, cancel.clone(), true);

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

    /// `agent.view` survives event-stream resubscribes — the documented
    /// loss points are restore/handoff (`[[startup]]`) and explicit
    /// clear/replace — so the assert rides the first `Synced` only.
    /// Two forced stream drops here produce two resync `Synced`s, which
    /// must run capabilities collect without touching the view; the
    /// `[[startup]]` hook remains a loss point and re-asserts.
    #[tokio::test]
    async fn resyncs_do_not_reassert_view() {
        let server = Arc::new(MiniHerdr::resyncing(2));
        let client = mini_client(&server);
        let cancel = CancellationToken::new();
        let handle = TopologyActor::spawn_with_agent_view(client, cancel.clone(), true);

        until(5, "initial view assert", || server.view_sets() == 1).await;
        // The first two subscription conns close post-handshake; the
        // third holds open — three `Synced`s total.
        until(15, "forced resyncs completed", || {
            server.subscriptions() >= 3
        })
        .await;
        // Let the last resync's post-sync collect run to completion —
        // a buggy re-assert lands right after it.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(
            server.view_sets(),
            1,
            "resubscribe Synceds must not re-assert agent.view"
        );

        handle.startup_hook().await;
        until(5, "startup-hook view re-assert", || server.view_sets() == 2).await;
        cancel.cancel();
    }

    /// `agent_view` off — the shipped default — never writes the slot:
    /// no bootstrap assert and no `[[startup]]` re-assert, while
    /// capability collect still runs on both (the snapshot counts move).
    #[tokio::test]
    async fn agent_view_disabled_never_asserts() {
        let server = Arc::new(MiniHerdr::new());
        let client = mini_client(&server);
        let cancel = CancellationToken::new();
        let handle = TopologyActor::spawn(client, cancel.clone());

        until(5, "bootstrap snapshot committed", || {
            server.snapshots() >= 1
        })
        .await;
        // Let post-sync work settle — a stray assert lands right after it.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(server.view_sets(), 0, "default spawn must not assert");

        let snapshots_before = server.snapshots();
        handle.startup_hook().await;
        until(5, "startup-hook snapshot re-read", || {
            server.snapshots() > snapshots_before
        })
        .await;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(server.view_sets(), 0, "startup hook must not assert");
        cancel.cancel();
    }

    /// `pane_output_changed{pane_id,revision}` is a pure watch nudge: the
    /// actor folds `data.revision` into the shared upstream watermark and
    /// forwards the revision on the invalidation — no snapshot refresh
    /// (unlike `pane.updated`, which re-reads topology). The snapshot
    /// keeps `wE:pE` at revision 3, so the refresh also proves the fold
    /// max-merges instead of being reseeded down.
    #[tokio::test]
    async fn pane_output_changed_folds_revision_and_broadcasts() {
        let server = Arc::new(MiniHerdr::with_parts(
            vec![
                br#"{"event":"pane_output_changed","data":{"pane_id":"wE:pE","revision":7}}"#
                    .to_vec(),
                // A lifecycle event too — it must trigger the snapshot
                // refresh `pane_output_changed` does not.
                br#"{"event":"pane_updated","data":{"pane_id":"wE:pE"}}"#.to_vec(),
            ],
            vec![serde_json::json!({
                "pane_id": "wE:pE", "terminal_id": "term_E",
                "workspace_id": "wE", "tab_id": "wE:t1",
                "focused": false, "agent_status": "unknown",
                "revision": 3
            })],
        ));
        let client = mini_client(&server);
        let cancel = CancellationToken::new();
        let handle = TopologyActor::spawn(client, cancel.clone());
        // Subscribe before the event can land: the invalidation send is
        // the last step of the Invalidated arm, so a receiver created
        // before the observable watermark fold always sees it.
        let mut inv_rx = handle.invalidations.subscribe();

        // The event path's fold lands on the shared ledger — visible
        // through the last published topology immediately.
        until(5, "upstream revision folded", || {
            handle.topology.borrow().upstream_rev_of("wE:pE") == 7
        })
        .await;

        let inv = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match inv_rx.recv().await {
                    Ok(inv) if inv.name == "pane.output_changed" => break inv,
                    Ok(_) => continue,
                    Err(e) => panic!("invalidation feed: {e}"),
                }
            }
        })
        .await
        .expect("pane.output_changed invalidation");
        assert_eq!(inv.pane_id.as_deref(), Some("wE:pE"));
        assert_eq!(inv.output_revision, Some(7));

        // `pane_output_changed` itself is not a topology trigger — the
        // only snapshot re-read is the `pane_updated` lifecycle one
        // (bootstrap's snapshot plus that refresh).
        until(5, "pane.updated snapshot refresh", || {
            server.snapshots() >= 2
        })
        .await;
        cancel.cancel();
    }
}
