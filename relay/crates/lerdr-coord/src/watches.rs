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
//! - `interval_ms` (the oracle's fixed poll period) becomes the *minimum*
//!   gap between pane reads — a trailing-edge throttle on invalidations,
//!   clamped [`MIN_WATCH_INTERVAL`]..=[`MAX_WATCH_INTERVAL`]. Invalidations
//!   already deliver freshness faster than any poll, so the field's
//!   remaining job is bounding read rate.
//! - A `content_fingerprint` matching the initial read emits the oracle's
//!   `knownFingerprint` frame instead of a duplicate `pane_content`: a
//!   copy-everything `pane_delta` (`CopyLines = count("\n") + 1`) that
//!   still engages the ack gate.
//!
//! Baseline deviation (doc 10): `pane_applied` is acked coarsely per pane —
//! `Inbound::content_fingerprint()` exposes the wire fingerprint the
//! oracle matches `pending`/`acknowledged` against; with at most one
//! pending frame per pane the coarse ack is equivalent in practice.

use std::collections::HashMap;
use std::sync::Arc;
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

/// `interval_ms` floor — the oracle whitelists `{100, 250, 500, 1000}` ms
/// (`pane_watch.go:291-299`); an event-driven watch does not need
/// sub-250 ms reads, and the floor keeps an invalidation storm from
/// fd-storming Herdr.
pub(crate) const MIN_WATCH_INTERVAL: Duration = Duration::from_millis(250);

/// `interval_ms` ceiling — slower asks clamp here rather than snapping
/// back to the oracle's whitelist default.
pub(crate) const MAX_WATCH_INTERVAL: Duration = Duration::from_secs(30);

/// `interval_ms` absent or unreadable — the oracle's
/// `defaultPaneWatchInterval`.
pub(crate) const DEFAULT_WATCH_INTERVAL: Duration = Duration::from_millis(250);

/// `requestedPaneWatchInterval` — the wire `interval_ms` resolved to the
/// watch's read cadence. `None`/non-integral values already read as absent
/// on [`Inbound::interval_ms`]; anything left is clamped to
/// [floor, ceiling].
///
/// [`Inbound::interval_ms`]: lerdr_core::protocol::Inbound::interval_ms
pub(crate) fn watch_interval(interval_ms: Option<i64>) -> Duration {
    interval_ms
        .and_then(|ms| u64::try_from(ms).ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_WATCH_INTERVAL)
        .clamp(MIN_WATCH_INTERVAL, MAX_WATCH_INTERVAL)
}

/// The client-bound push endpoint watches and deferred action results
/// write to — `ClientSink` in production. A seam so tests can record
/// pushes without a live session (`ClientSink` is only constructible
/// inside `serve_connection`).
pub(crate) trait FrameSink: Send + Sync + 'static {
    /// `try_send` semantics: `false` = lag/closed — the producer stops.
    fn try_send(&self, message: &Outbound) -> bool;
}

impl FrameSink for ClientSink {
    fn try_send(&self, message: &Outbound) -> bool {
        ClientSink::try_send(self, message).is_ok()
    }
}

/// One `watch_pane` request distilled for the watch task.
#[derive(Debug, Clone)]
pub(crate) struct WatchSpec {
    /// `lines` read budget — the router already applied the default.
    pub(crate) lines: u32,
    /// Minimum gap between pane reads — [`watch_interval`] output.
    pub(crate) interval: Duration,
    /// The request's `content_fingerprint`: matching the initial read
    /// means the client already holds the frame — adopt it as the sent
    /// base instead of pushing a duplicate `pane_content`.
    pub(crate) known_fingerprint: Option<String>,
}

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
    pub(crate) fn start(
        &mut self,
        pane_id: String,
        spec: WatchSpec,
        client: Client,
        sink: Arc<dyn FrameSink>,
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
                spec,
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
async fn watch_loop(
    pane_id: String,
    spec: WatchSpec,
    client: Client,
    sink: Arc<dyn FrameSink>,
    mut invalidations: broadcast::Receiver<Invalidation>,
    mut ctl: mpsc::UnboundedReceiver<WatchCtl>,
    cancel: CancellationToken,
) {
    let mut state = WatchState::new();

    // Initial frame — full content, ack-gated. A `content_fingerprint`
    // matching the fresh read means the client already holds the content:
    // the oracle's `knownFingerprint` branch still ships the frame's
    // metadata as a copy-everything `pane_delta` (base = the known
    // fingerprint) and engages the ack gate (`watch.pending = frame`).
    match client
        .pane_read(
            &pane_id,
            ReadSource::RecentUnwrapped,
            spec.lines,
            ReadFormat::Text,
        )
        .await
    {
        Ok(read) => {
            let fingerprint = content_fingerprint(&read.text);
            if spec.known_fingerprint.as_deref() == Some(fingerprint.as_str()) {
                // `paneWatchUpdate`'s same-fingerprint branch:
                // `CopyLines: strings.Count(content, "\n") + 1`.
                let copy_lines =
                    i64::try_from(read.text.matches('\n').count() + 1).unwrap_or(i64::MAX);
                let message = Outbound::PaneDelta(Box::new(PaneDelta {
                    r#type: "pane_delta".to_owned(),
                    pane_id: Some(pane_id.clone()),
                    ack_required: Some(true),
                    base_fingerprint: Some(fingerprint.clone()),
                    content_fingerprint: Some(fingerprint.clone()),
                    segments: Some(MaybeNull::Value(vec![delta::Segment {
                        copy_lines,
                        ..delta::Segment::default()
                    }])),
                    target: Some(MaybeNull::Value(TargetRef {
                        pane_id: pane_id.clone(),
                        ..TargetRef::default()
                    })),
                    ..PaneDelta::default()
                }));
                if sink.try_send(&message) {
                    debug!("watch fingerprint hit — copy-segment delta");
                    state.sent_fingerprint = fingerprint;
                    state.sent_content = read.text;
                    state.pending_ack = true;
                }
            } else {
                send_frame(&pane_id, &*sink, &mut state, &read.text, true);
            }
        }
        Err(err) => send_read_error(&pane_id, &*sink, &err),
    }

    // `interval_ms` as a trailing-edge throttle: the earliest instant the
    // next pane read may start, and whether an invalidation is waiting on
    // the boundary.
    let mut next_read = tokio::time::Instant::now() + spec.interval;
    let mut deferred = false;

    loop {
        // The ack deadline only runs while a frame is in flight.
        let timeout = async {
            if state.pending_ack {
                tokio::time::sleep(ACK_TIMEOUT).await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        // The cadence boundary only runs while an invalidation is deferred.
        let cadence = async {
            if deferred {
                tokio::time::sleep_until(next_read).await;
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
                        // Client-initiated recovery — bypass the cadence.
                        refresh(&pane_id, spec.lines, &client, &*sink, &mut state).await;
                        next_read = tokio::time::Instant::now() + spec.interval;
                        deferred = false;
                    }
                    Some(WatchCtl::Stop) | None => break,
                }
            }
            _ = timeout => {
                debug!("ack timeout — gate reset, next frame is full");
                state.pending_ack = false;
                state.force_full = true;
            }
            () = cadence => {
                deferred = false;
                if !state.pending_ack {
                    refresh(&pane_id, spec.lines, &client, &*sink, &mut state).await;
                    next_read = tokio::time::Instant::now() + spec.interval;
                }
            }
            inv = invalidations.recv() => {
                let triggered = match inv {
                    Ok(inv) => {
                        inv.pane_id.as_deref() == Some(pane_id.as_str())
                            || inv.pane_id.is_none()
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        // Lag is correctness-safe: re-read covers the gap.
                        debug!(skipped = n, "invalidation lag — resyncing");
                        true
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                if triggered {
                    if tokio::time::Instant::now() >= next_read {
                        refresh(&pane_id, spec.lines, &client, &*sink, &mut state).await;
                        next_read = tokio::time::Instant::now() + spec.interval;
                    } else {
                        // Inside the window — one refresh at the boundary
                        // instead of a read per event.
                        deferred = true;
                    }
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
    sink: &dyn FrameSink,
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
    sink: &dyn FrameSink,
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

    if !sink.try_send(&message) {
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
fn send_read_error(pane_id: &str, sink: &dyn FrameSink, err: &HerdrError) {
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
/// matches the client's (fingerprint-hit path,
/// `unchangedPaneResponse`). The oracle always emits the `target` key —
/// the request's echo, or `null`.
pub fn pane_unchanged(pane_id: &str, fingerprint: &str, target: Option<TargetRef>) -> Outbound {
    Outbound::PaneUnchanged(PaneUnchanged {
        r#type: "pane_unchanged".to_owned(),
        pane_id: Some(pane_id.to_owned()),
        content_fingerprint: Some(fingerprint.to_owned()),
        target: Some(target.map_or(MaybeNull::Null, MaybeNull::Value)),
    })
}

/// Fakes shared by watch/router tests: a canned-response Herdr transport
/// and a recording [`FrameSink`] — `ClientSink` is only constructible
/// inside `lerdr-relay`, so these stand in for the two edges the watch
/// touches.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use lerdr_herdr::{BoxIo, ClientConfig, Event, Transport};
    use serde_json::{json, Value};
    use std::collections::VecDeque;
    use std::future::Future;
    use std::io;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    /// Canned Herdr: each `dial` pops the next `result` body (the last one
    /// replays once the queue drains) and answers `{"id":<req>,"result":…}`
    /// — enough NDJSON for `Client::pane_read`/`call`.
    pub(crate) struct FakeHerdr {
        responses: Mutex<VecDeque<Value>>,
        last: Mutex<Option<Value>>,
        /// One dial per request — the observable "did it re-read" counter.
        pub(crate) dials: AtomicUsize,
    }

    impl FakeHerdr {
        pub(crate) fn serving(responses: Vec<Value>) -> Arc<Self> {
            Arc::new(FakeHerdr {
                responses: Mutex::new(responses.into()),
                last: Mutex::new(None),
                dials: AtomicUsize::new(0),
            })
        }

        pub(crate) fn client(self: &Arc<Self>) -> Client {
            Client::new(
                self.clone(),
                ClientConfig {
                    read_retry: false,
                    ..ClientConfig::default()
                },
            )
        }
    }

    impl Transport for FakeHerdr {
        fn dial(&self) -> Pin<Box<dyn Future<Output = io::Result<BoxIo>> + Send>> {
            self.dials.fetch_add(1, Ordering::Relaxed);
            let result = {
                let mut queue = self.responses.lock().expect("responses poisoned");
                let mut last = self.last.lock().expect("last poisoned");
                match queue.pop_front() {
                    Some(result) => {
                        *last = Some(result.clone());
                        result
                    }
                    None => last.clone().unwrap_or(Value::Null),
                }
            };
            Box::pin(async move {
                let (client, server) = tokio::io::duplex(8192);
                tokio::spawn(async move {
                    let (read, mut write) = tokio::io::split(server);
                    let mut line = String::new();
                    let _ = BufReader::new(read).read_line(&mut line).await;
                    let id = serde_json::from_str::<Value>(&line)
                        .ok()
                        .and_then(|v| v.get("id")?.as_str().map(str::to_owned))
                        .unwrap_or_default();
                    let body = json!({ "id": id, "result": result }).to_string();
                    let _ = write.write_all(body.as_bytes()).await;
                    let _ = write.write_all(b"\n").await;
                    let _ = write.shutdown().await;
                });
                Ok(Box::new(client) as BoxIo)
            })
        }

        fn describe(&self) -> String {
            "fake:herdr".to_owned()
        }
    }

    /// A [`FrameSink`] that hands pushed envelopes to the test.
    pub(crate) struct RecordingSink {
        tx: mpsc::UnboundedSender<Outbound>,
    }

    impl FrameSink for RecordingSink {
        fn try_send(&self, message: &Outbound) -> bool {
            self.tx.send(message.clone()).is_ok()
        }
    }

    pub(crate) fn recording_sink() -> (Arc<RecordingSink>, mpsc::UnboundedReceiver<Outbound>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Arc::new(RecordingSink { tx }), rx)
    }

    /// `{"type":"pane_read","read":{…}}` — a `pane.read` result body.
    pub(crate) fn pane_read_result(pane_id: &str, text: &str) -> Value {
        json!({
            "type": "pane_read",
            "read": {
                "pane_id": pane_id,
                "workspace_id": "wE",
                "tab_id": "wE:tE",
                "source": "recent_unwrapped",
                "format": "text",
                "text": text,
                "revision": 1,
                "truncated": false,
            }
        })
    }

    /// A `pane.updated` invalidation naming `pane_id` (`None` = global).
    pub(crate) fn invalidate(pane_id: Option<&str>) -> Invalidation {
        Invalidation {
            name: "pane.updated".to_owned(),
            pane_id: pane_id.map(str::to_owned),
            event: Event {
                name: "pane.updated".to_owned(),
                data: json!({}),
            },
        }
    }

    pub(crate) fn spec(lines: u32, interval: Duration, known: Option<&str>) -> WatchSpec {
        WatchSpec {
            lines,
            interval,
            known_fingerprint: known.map(str::to_owned),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use std::time::Duration;
    use tokio::sync::broadcast;

    #[test]
    fn interval_defaults_and_clamps() {
        assert_eq!(watch_interval(None), DEFAULT_WATCH_INTERVAL);
        assert_eq!(watch_interval(Some(250)), Duration::from_millis(250));
        assert_eq!(watch_interval(Some(500)), Duration::from_millis(500));
        assert_eq!(watch_interval(Some(1)), MIN_WATCH_INTERVAL);
        assert_eq!(watch_interval(Some(0)), MIN_WATCH_INTERVAL);
        // Unreadable (negative) → the fallback, same as Go's type-assert.
        assert_eq!(watch_interval(Some(-5)), DEFAULT_WATCH_INTERVAL);
        assert_eq!(watch_interval(Some(60_000)), MAX_WATCH_INTERVAL);
        assert_eq!(watch_interval(Some(i64::MAX)), MAX_WATCH_INTERVAL);
    }

    /// `watch_pane` with a matching `content_fingerprint` emits the
    /// oracle's copy-everything `pane_delta`, not a `pane_content` echo.
    #[tokio::test]
    async fn known_fingerprint_emits_copy_delta_not_content() {
        let herdr = FakeHerdr::serving(vec![pane_read_result("wE:pE", "a\nb\n")]);
        let (sink, mut rx) = recording_sink();
        let (invalidations, _) = broadcast::channel(16);
        let cancel = CancellationToken::new();
        let mut watches = WatchSet::default();
        watches.start(
            "wE:pE".to_owned(),
            spec(400, Duration::from_millis(250), Some("911169ddaaf146af")),
            herdr.client(),
            sink,
            invalidations,
            cancel.clone(),
        );

        let frame = rx.recv().await.expect("initial watch frame");
        match frame {
            Outbound::PaneDelta(delta) => {
                assert_eq!(delta.pane_id.as_deref(), Some("wE:pE"));
                assert_eq!(delta.base_fingerprint.as_deref(), Some("911169ddaaf146af"));
                assert_eq!(
                    delta.content_fingerprint.as_deref(),
                    Some("911169ddaaf146af")
                );
                let segments = match delta.segments {
                    Some(MaybeNull::Value(segments)) => segments,
                    other => panic!("expected segments, got {other:?}"),
                };
                assert_eq!(segments.len(), 1);
                // "a\nb\n" → 2 newlines + 1.
                assert_eq!(segments[0].copy_lines, 3);
            }
            other => panic!("expected pane_delta, got {other:?}"),
        }
        watches.stop("wE:pE");
        cancel.cancel();
    }

    /// A mismatched `content_fingerprint` behaves like none: full
    /// `pane_content` initial frame.
    #[tokio::test]
    async fn wrong_fingerprint_sends_full_initial_frame() {
        let herdr = FakeHerdr::serving(vec![pane_read_result("wE:pE", "a\nb\n")]);
        let (sink, mut rx) = recording_sink();
        let (invalidations, _) = broadcast::channel(16);
        let cancel = CancellationToken::new();
        let mut watches = WatchSet::default();
        watches.start(
            "wE:pE".to_owned(),
            spec(400, Duration::from_millis(250), Some("0000000000000000")),
            herdr.client(),
            sink,
            invalidations,
            cancel.clone(),
        );

        let frame = rx.recv().await.expect("initial watch frame");
        match frame {
            Outbound::PaneContent(content) => {
                assert_eq!(content.content.as_deref(), Some("a\nb\n"));
                assert_eq!(
                    content.content_fingerprint.as_deref(),
                    Some("911169ddaaf146af")
                );
                assert_eq!(content.ack_required, Some(true));
            }
            other => panic!("expected pane_content, got {other:?}"),
        }
        watches.stop("wE:pE");
        cancel.cancel();
    }

    /// `interval_ms` bounds the read rate: a burst of invalidations inside
    /// one window collapses into a single read at the boundary.
    #[tokio::test(start_paused = true)]
    async fn interval_throttles_invalidation_burst() {
        let herdr = FakeHerdr::serving(vec![
            pane_read_result("wE:pE", "v1\n"),
            pane_read_result("wE:pE", "v2\n"),
        ]);
        let (sink, mut rx) = recording_sink();
        let (invalidations, _) = broadcast::channel(16);
        let cancel = CancellationToken::new();
        let mut watches = WatchSet::default();
        watches.start(
            "wE:pE".to_owned(),
            spec(400, Duration::from_millis(250), None),
            herdr.client(),
            sink,
            invalidations.clone(),
            cancel.clone(),
        );

        let _initial = rx.recv().await.expect("initial frame");
        watches.ack("wE:pE");

        // Three invalidations inside the first 250ms window → deferred,
        // then one read at the boundary.
        for _ in 0..3 {
            let _ = invalidations.send(invalidate(Some("wE:pE")));
        }
        tokio::task::yield_now().await;
        assert_eq!(herdr.dials.load(std::sync::atomic::Ordering::Relaxed), 1);

        let refresh = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("boundary refresh frame")
            .expect("channel open");
        assert!(matches!(
            refresh,
            Outbound::PaneContent(_) | Outbound::PaneDelta(_)
        ));
        assert_eq!(herdr.dials.load(std::sync::atomic::Ordering::Relaxed), 2);
        watches.stop("wE:pE");
        cancel.cancel();
    }

    /// Outside the window an invalidation still reads immediately — the
    /// throttle bounds rate, never delays past the boundary.
    #[tokio::test(start_paused = true)]
    async fn invalidation_after_window_reads_at_once() {
        let herdr = FakeHerdr::serving(vec![
            pane_read_result("wE:pE", "v1\n"),
            pane_read_result("wE:pE", "v2\n"),
        ]);
        let (sink, mut rx) = recording_sink();
        let (invalidations, _) = broadcast::channel(16);
        let cancel = CancellationToken::new();
        let mut watches = WatchSet::default();
        watches.start(
            "wE:pE".to_owned(),
            spec(400, Duration::from_millis(250), None),
            herdr.client(),
            sink,
            invalidations.clone(),
            cancel.clone(),
        );

        let _initial = rx.recv().await.expect("initial frame");
        watches.ack("wE:pE");
        tokio::time::sleep(Duration::from_millis(300)).await;

        let _ = invalidations.send(invalidate(Some("wE:pE")));
        let frame = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("refresh frame")
            .expect("channel open");
        assert!(matches!(
            frame,
            Outbound::PaneContent(_) | Outbound::PaneDelta(_)
        ));
        assert_eq!(herdr.dials.load(std::sync::atomic::Ordering::Relaxed), 2);
        watches.stop("wE:pE");
        cancel.cancel();
    }

    /// `pane_unchanged` carries the request's `target` echo — `null` when
    /// absent, matching the oracle's `response["target"]` passthrough.
    #[test]
    fn pane_unchanged_echoes_target() {
        let hit = pane_unchanged("wE:pE", "911169ddaaf146af", None);
        match hit {
            Outbound::PaneUnchanged(m) => {
                assert_eq!(m.pane_id.as_deref(), Some("wE:pE"));
                assert_eq!(m.content_fingerprint.as_deref(), Some("911169ddaaf146af"));
                assert!(matches!(m.target, Some(MaybeNull::Null)));
            }
            other => panic!("expected pane_unchanged, got {other:?}"),
        }
        let target = TargetRef {
            pane_id: "wE:pE".to_owned(),
            ..TargetRef::default()
        };
        match pane_unchanged("wE:pE", "911169ddaaf146af", Some(target)) {
            Outbound::PaneUnchanged(m) => {
                assert!(matches!(m.target, Some(MaybeNull::Value(_))));
            }
            other => panic!("expected pane_unchanged, got {other:?}"),
        }
    }
}
