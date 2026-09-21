//! Per-(client, pane) watch tasks — the `pane_watch.go` port.
//!
//! One task per watched pane per client session. Event-driven (Herdr
//! `pane.*` invalidations) rather than the oracle's fixed poll — same
//! correctness contract, fewer reads:
//!
//! - At most **one unacked frame in flight** per watch — the gate.
//! - `pane_applied` (a `WatchCtl::Ack` signal) clears the gate.
//! - **4 s ack timeout** resets the gate; the next send is a full
//!   `ack_required` frame (the oracle's "gate reset" behavior).
//! - `pane_resync` / rejected deltas force a fresh read + full frame.
//! - Frame selection: same fingerprint → nothing (update noise);
//!   `delta::efficient` → `pane_delta` chained on `base_fingerprint`;
//!   otherwise full `pane_content`.
//!
//! Baseline deviation (doc 10): `pane_applied` is acked coarsely per pane —
//! the typed `Inbound` does not carry `content_fingerprint`, so we cannot
//! match the pending fingerprint the way Go does. With at most one pending
//! frame per pane this is equivalent in practice; the router seam needs a
//! raw-field channel to tighten it.

use std::collections::HashMap;
use std::time::Duration;

use lerdr_core::delta;
use lerdr_core::json::MaybeNull;
use lerdr_core::protocol::{Outbound, PaneContent, PaneDelta, PaneUnchanged, TargetRef};
use lerdr_herdr::{Client, HerdrError, ReadFormat, ReadSource};
use lerdr_relay::session::ClientSink;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn, Instrument};

use crate::actor::Invalidation;
use crate::fingerprint::content_fingerprint;

/// `paneWatchAckTimeout` — the oracle's gate reset window.
pub const ACK_TIMEOUT: Duration = Duration::from_secs(4);

/// Default `lines` when `watch_pane`/`read_pane` carries none (the oracle's
/// default pane read budget).
pub const DEFAULT_LINES: u32 = 400;

/// Control signals a router pushes into a live watch.
#[derive(Debug)]
pub enum WatchCtl {
    /// `pane_applied` — the client committed the pending frame.
    Ack,
    /// `pane_resync` — the client lost the chain; force full re-read.
    Resync,
    /// `unwatch_pane` / session teardown — stop the task.
    Stop,
}

/// One live watch: control channel + join handle.
pub struct WatchEntry {
    /// Router → task signals.
    pub ctl: mpsc::UnboundedSender<WatchCtl>,
    task: JoinHandle<()>,
}

impl WatchEntry {
    /// Abort without ceremony (session teardown).
    pub fn abort(&self) {
        self.task.abort();
    }
}

/// Per-client watch set — owned by that client's router instance.
#[derive(Default)]
pub struct WatchSet {
    entries: HashMap<String, WatchEntry>,
}

impl WatchSet {
    /// `true` when a watch is already live for `pane_id`.
    pub fn watching(&self, pane_id: &str) -> bool {
        self.entries.contains_key(pane_id)
    }

    /// `watch_pane` — spawn the task; idempotent.
    pub fn start(
        &mut self,
        pane_id: String,
        lines: u32,
        client: Client,
        sink: ClientSink,
        invalidations: broadcast::Sender<Invalidation>,
        cancel: CancellationToken,
    ) {
        if self.entries.contains_key(&pane_id) {
            return;
        }
        let (ctl_tx, ctl_rx) = mpsc::unbounded_channel();
        let id = pane_id.clone();
        let task = tokio::spawn(
            watch_loop(
                pane_id,
                lines,
                client,
                sink,
                invalidations.subscribe(),
                ctl_rx,
                cancel,
            )
            .instrument(tracing::info_span!("pane_watch", pane = %id)),
        );
        self.entries.insert(id, WatchEntry { ctl: ctl_tx, task });
    }

    /// `pane_applied` — deliver the ack; no-op when not watching.
    pub fn ack(&self, pane_id: &str) {
        if let Some(entry) = self.entries.get(pane_id) {
            let _ = entry.ctl.send(WatchCtl::Ack);
        }
    }

    /// `pane_resync` — force a full re-send; no-op when not watching.
    pub fn resync(&self, pane_id: &str) {
        if let Some(entry) = self.entries.get(pane_id) {
            let _ = entry.ctl.send(WatchCtl::Resync);
        }
    }

    /// `unwatch_pane` — stop and drop the watch. Returns whether one was
    /// live (the oracle answers unknown panes with a no-op receipt too).
    pub fn stop(&mut self, pane_id: &str) -> bool {
        if let Some(entry) = self.entries.remove(pane_id) {
            let _ = entry.ctl.send(WatchCtl::Stop);
            entry.abort();
            true
        } else {
            false
        }
    }

    /// Session teardown — stop everything.
    pub fn stop_all(&mut self) {
        for (_, entry) in self.entries.drain() {
            entry.abort();
        }
    }
}

/// The watch task's frame state.
struct WatchState {
    /// Fingerprint of the content last sent (and acked or in-flight).
    sent_fingerprint: String,
    /// Content last sent — the `pane_delta` base.
    sent_content: String,
    /// Gate: a frame is awaiting `pane_applied`.
    pending_ack: bool,
    /// After a gate timeout the next frame must be full (`ack_required`
    /// fresh chain), not a delta against a possibly-lost base.
    force_full: bool,
}

impl WatchState {
    fn new() -> Self {
        Self {
            sent_fingerprint: String::new(),
            sent_content: String::new(),
            pending_ack: false,
            force_full: false,
        }
    }
}

/// The watch loop — exits on `Stop`, cancel, closed sink, or closed
/// invalidation feed.
#[allow(clippy::too_many_arguments)]
async fn watch_loop(
    pane_id: String,
    lines: u32,
    client: Client,
    sink: ClientSink,
    mut invalidations: broadcast::Receiver<Invalidation>,
    mut ctl: mpsc::UnboundedReceiver<WatchCtl>,
    cancel: CancellationToken,
) {
    let mut state = WatchState::new();

    // Initial frame — full content, ack-gated.
    match client
        .pane_read(
            &pane_id,
            ReadSource::RecentUnwrapped,
            lines,
            ReadFormat::Text,
        )
        .await
    {
        Ok(read) => send_frame(&pane_id, &sink, &mut state, &read.text, true),
        Err(err) => send_read_error(&pane_id, &sink, &err),
    }

    loop {
        // The ack deadline only runs while a frame is in flight.
        let timeout = async {
            if state.pending_ack {
                tokio::time::sleep(ACK_TIMEOUT).await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            signal = ctl.recv() => {
                match signal {
                    Some(WatchCtl::Ack) => {
                        state.pending_ack = false;
                        debug!("pane_applied — gate cleared");
                    }
                    Some(WatchCtl::Resync) => {
                        state.pending_ack = false;
                        state.force_full = true;
                        refresh(&pane_id, lines, &client, &sink, &mut state).await;
                    }
                    Some(WatchCtl::Stop) | None => break,
                }
            }
            _ = timeout => {
                debug!("ack timeout — gate reset, next frame is full");
                state.pending_ack = false;
                state.force_full = true;
            }
            inv = invalidations.recv() => {
                match inv {
                    Ok(inv) => {
                        if inv.pane_id.as_deref() == Some(pane_id.as_str())
                            || inv.pane_id.is_none()
                        {
                            refresh(&pane_id, lines, &client, &sink, &mut state).await;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        // Lag is correctness-safe: re-read covers the gap.
                        debug!(skipped = n, "invalidation lag — resyncing");
                        refresh(&pane_id, lines, &client, &sink, &mut state).await;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
    debug!("watch stopped");
}

/// Re-read and push the diff/frame when the gate allows.
async fn refresh(
    pane_id: &str,
    lines: u32,
    client: &Client,
    sink: &ClientSink,
    state: &mut WatchState,
) {
    match client
        .pane_read(
            pane_id,
            ReadSource::RecentUnwrapped,
            lines,
            ReadFormat::Text,
        )
        .await
    {
        Ok(read) => send_frame(pane_id, sink, state, &read.text, false),
        Err(err) => send_read_error(pane_id, sink, &err),
    }
}

/// Pick and push the right frame for a fresh read, respecting the gate.
fn send_frame(
    pane_id: &str,
    sink: &ClientSink,
    state: &mut WatchState,
    content: &str,
    initial: bool,
) {
    if state.pending_ack {
        // One unacked frame max — the next ack/timeout/invalidation retries.
        return;
    }
    let fp = content_fingerprint(content);
    if fp == state.sent_fingerprint && !initial {
        return; // unchanged — the oracle emits pane_unchanged only for
                // explicit read_pane answers, not watch ticks.
    }

    let target = Some(MaybeNull::Value(TargetRef {
        pane_id: pane_id.to_owned(),
        ..TargetRef::default()
    }));

    let (message, new_base): (Outbound, String) =
        if !initial && !state.force_full && !state.sent_fingerprint.is_empty() {
            let segments = delta::build(&state.sent_content, content);
            if delta::efficient(&segments, content) {
                (
                    Outbound::PaneDelta(Box::new(PaneDelta {
                        r#type: "pane_delta".to_owned(),
                        pane_id: Some(pane_id.to_owned()),
                        ack_required: Some(true),
                        base_fingerprint: Some(state.sent_fingerprint.clone()),
                        content_fingerprint: Some(fp.clone()),
                        segments: Some(MaybeNull::Value(segments)),
                        target,
                        ..PaneDelta::default()
                    })),
                    content.to_owned(),
                )
            } else {
                full_frame(pane_id, content, fp.clone(), target)
            }
        } else {
            full_frame(pane_id, content, fp.clone(), target)
        };

    if sink.try_send(&message).is_err() {
        warn!("watch push refused — client queue gone/full");
        return;
    }
    state.sent_fingerprint = fp;
    state.sent_content = new_base;
    state.pending_ack = true;
    state.force_full = false;
}

fn full_frame(
    pane_id: &str,
    content: &str,
    fp: String,
    target: Option<MaybeNull<TargetRef>>,
) -> (Outbound, String) {
    (
        Outbound::PaneContent(Box::new(PaneContent {
            r#type: "pane_content".to_owned(),
            pane_id: Some(pane_id.to_owned()),
            content: Some(content.to_owned()),
            ack_required: Some(true),
            content_fingerprint: Some(fp),
            format: Some("text".to_owned()),
            target,
            ..PaneContent::default()
        })),
        content.to_owned(),
    )
}

/// A refused/failed `pane.read` surfaces as the `pane_content` error variant
/// (`{content:"",error,format,pane_id,type}`) — the oracle's shape.
fn send_read_error(pane_id: &str, sink: &ClientSink, err: &HerdrError) {
    let _ = sink.try_send(&Outbound::PaneContent(Box::new(PaneContent {
        r#type: "pane_content".to_owned(),
        pane_id: Some(pane_id.to_owned()),
        content: Some(String::new()),
        error: Some(err.to_string()),
        format: Some("text".to_owned()),
        ..PaneContent::default()
    })));
}

/// `pane_unchanged` — answers `read_pane` when the wire fingerprint already
/// matches the client's (fingerprint-hit path).
pub fn pane_unchanged(pane_id: &str, fingerprint: &str) -> Outbound {
    Outbound::PaneUnchanged(PaneUnchanged {
        r#type: "pane_unchanged".to_owned(),
        pane_id: Some(pane_id.to_owned()),
        content_fingerprint: Some(fingerprint.to_owned()),
        ..PaneUnchanged::default()
    })
}
