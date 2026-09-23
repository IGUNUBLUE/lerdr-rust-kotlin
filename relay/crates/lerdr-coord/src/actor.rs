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

use lerdr_herdr::{Client, Event, EventSupervisor, SupervisorSignal};
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
            commands: cmd_tx,
        };

        tokio::spawn(
            async move {
                let mut state = Topology::default();
                let mut stream =
                    client.supervise_events(EventSupervisor::topology());
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
                                }
                                SupervisorSignal::Invalidated { event, .. } => {
                                    if event.is_topology() {
                                        // Invalidation semantics: re-read,
                                        // never apply the payload.
                                        match client.session_snapshot().await {
                                            Ok(fresh) => {
                                                state.accept(fresh);
                                                let _ = topology_tx
                                                    .send(Arc::new(clone_topology(&state)));
                                            }
                                            Err(err) => {
                                                warn!(error = %err, "snapshot refresh failed; staying on last state");
                                            }
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
                                    match client.session_snapshot().await {
                                        Ok(fresh) => {
                                            state.accept(fresh);
                                            let _ = topology_tx
                                                .send(Arc::new(clone_topology(&state)));
                                        }
                                        Err(err) => {
                                            warn!(error = %err, "refresh-command snapshot failed; staying on last state");
                                        }
                                    }
                                }
                                Some(TopologyCommand::BumpGeneration(pane_id)) => {
                                    state.bump_generation(&pane_id);
                                    let _ = topology_tx
                                        .send(Arc::new(clone_topology(&state)));
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
}
