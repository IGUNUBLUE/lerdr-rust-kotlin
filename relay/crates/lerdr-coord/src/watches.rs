//! Per-(client, pane) watch tasks — the `pane_watch.go` port.
//!
//! One task per watched pane per client session. The oracle polls every
//! `interval` (`pollPaneWatch` on a ticker); this port keeps that
//! two-tier tick — a cheap `visible`-source probe, then a full
//! `HandleReadPane` read only when the probe moved — and adds the Herdr
//! `pane.*` invalidation feed as a fast path into the same poll:
//!
//! - **Periodic tick** — every `spec.interval` the loop probes the pane
//!   (`HandleProbePane`: `pane.read` `visible`, 500 lines, the watch's
//!   format) and full-reads only when `paneWatchNeedsFrameRead` says so:
//!   first poll, a moved probe fingerprint, or a committed
//!   `resize_settling` frame. A `pane.updated` missed while the ack gate
//!   was shut is picked up by the first tick after it opens.
//! - **Invalidation fast path** — a `pane.*` event at or past
//!   `next_read` polls immediately; inside the freshness window or gated
//!   it does nothing — the tick covers it.
//! - **Format + source** — `format: "ansi"` rides the spec end to end
//!   (`watchMessage` carries it); [`display_source`] is
//!   `readPaneForDisplay`: text reads stay on `visible` (any other source
//!   makes Herdr harvest scrollback through the operator's real pane via
//!   the mouse-scroll interface), ansi reads pull `recent` —
//!   `recent-unwrapped` only for Claude agents with no active lease.
//! - At most **one unacked frame in flight** per watch — the gate.
//! - `pane_applied` (`WatchCtl::Ack`) carries the wire fingerprint
//!   (`handlePaneApplied`, `pane_watch.go:267-289`): matching the pending
//!   frame clears the gate, matching the last acked frame is a dup,
//!   anything else is foreign and pushes `pane_resync`.
//! - **4 s ack timeout** resets the gate AND the acked fingerprint (the
//!   oracle clears `pending` + `acknowledged` + `probeFingerprint`); the
//!   next send is a full `ack_required` frame.
//! - `pane_resync` forces a fresh read + full frame.
//! - Frame selection (`paneWatchUpdate`): same **frame** fingerprint
//!   → nothing (metadata identical, not just content); same content
//!   fingerprint → a copy-everything `pane_delta` carrying the new
//!   metadata; `delta::efficient` → a real `pane_delta` chained on
//!   `base_fingerprint`; otherwise full `pane_content`. `ack_required`
//!   rides only full frames — `paneDeltaResponse` never sets it.
//! - `interval_ms` resolves through the oracle's whitelist
//!   `{100, 250, 500, 1000}` ms (`requestedPaneWatchInterval`); anything
//!   else is the 250 ms default.
//! - A `content_fingerprint` matching the initial read emits the oracle's
//!   `knownFingerprint` frame instead of a duplicate `pane_content`: a
//!   copy-everything `pane_delta` (`CopyLines = count("\n") + 1`) that
//!   still engages the ack gate.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use lerdr_core::delta;
use lerdr_core::json::MaybeNull;
use lerdr_core::protocol::{
    Inbound, Outbound, PaneContent, PaneDelta, PaneResync, PaneUnchanged, TargetRef,
};
use lerdr_herdr::{ReadFormat, ReadSource};
use lerdr_relay::session::ClientSink;
use sha2::{Digest, Sha256};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn, Instrument};

use crate::actions::{leases::Leases, questions::Questions, Notices};
use crate::actor::{Invalidation, TopologyHandle};
use crate::classify::store::{prepare_pane_response, PaneSemantics};
use crate::fingerprint::content_fingerprint;
use crate::history::Manager as HistoryManager;

/// `paneWatchAckTimeout` — the oracle's gate reset window.
pub const ACK_TIMEOUT: Duration = Duration::from_secs(4);

/// `messageInt(message["lines"], 30)` — the `watch_pane` fallback
/// (`pane_watch.go:61`), shared with `read_pane`'s `intValue(…, 30)`
/// (`dispatch.go:1106`).
pub const DEFAULT_PANE_LINES: u32 = 30;

/// `history.MaxLines` — the `lines` ceiling both Go handlers clamp to.
pub const MAX_PANE_LINES: u32 = 10_000;

/// Retained for the crate-root re-export — superseded by
/// [`DEFAULT_PANE_LINES`].
pub const DEFAULT_LINES: u32 = DEFAULT_PANE_LINES;

/// `defaultPaneWatchInterval` — `interval_ms` absent or outside the
/// whitelist.
pub(crate) const DEFAULT_WATCH_INTERVAL: Duration = Duration::from_millis(250);

/// `WatchCtl` queue depth — bounded like every queue in the relay.
/// `pane_applied` is client-spammable; a lost `Ack` self-heals through
/// the [`ACK_TIMEOUT`] → `pane_resync` chain.
const WATCH_CTL_QUEUE: usize = 16;

/// `requestedPaneWatchInterval` (`pane_watch.go:291-299`): the wire
/// `interval_ms` resolves through the oracle's whitelist — anything not
/// in `{100, 250, 500, 1000}` ms (absent, non-integral, out-of-set)
/// falls back to `defaultPaneWatchInterval`.
///
/// [`Inbound::interval_ms`]: lerdr_core::protocol::Inbound::interval_ms
pub(crate) fn watch_interval(interval_ms: Option<i64>) -> Duration {
    match interval_ms {
        Some(ms @ (100 | 250 | 500 | 1_000)) => Duration::from_millis(ms as u64),
        _ => DEFAULT_WATCH_INTERVAL,
    }
}

/// The `lines` read budget — `messageInt(message["lines"], 30)` for
/// `watch_pane` (`pane_watch.go:61-65`), `intValue(…, 30)` for
/// `read_pane` (`dispatch.go:1106-1112`); both clamp `1..=MaxLines`.
/// `raw_int` applies `messageInt` semantics (integral numbers only — a
/// fractional `lines` reads as absent, a deliberate divergence from
/// `intValue`'s float truncation); the typed field covers messages built
/// without the raw map (`Default`/struct literals).
pub(crate) fn pane_lines(message: &Inbound) -> u32 {
    let raw = match message.raw_int("lines") {
        Some(n) => n,
        // A wire `0` is `Some(0)` above and clamps to 1; `None` + typed
        // `0` is the absent/unreadable case → the default.
        None if message.lines == 0 => return DEFAULT_PANE_LINES,
        None => message.lines,
    };
    raw.clamp(1, i64::from(MAX_PANE_LINES)) as u32
}

/// `HandleProbePane`'s fixed probe depth (`dispatch.go:1178`) — the cheap
/// `visible`-source read every watch tick starts with.
const PROBE_LINES: u32 = 500;

/// `stringValue(message["format"])` normalized — only `"ansi"` survives,
/// absent/null/non-string all read as text (`pane_watch.go:67-70`,
/// `dispatch.go:1113-1116`). The raw seam (not the typed field) keeps
/// `format: 123` a text read instead of a decode-time `invalid_request`.
pub(crate) fn read_format(message: &Inbound) -> ReadFormat {
    match message.raw_str("format") {
        Some("ansi") => ReadFormat::Ansi,
        _ => ReadFormat::Text,
    }
}

/// The wire spelling of a [`ReadFormat`] on outbound frames — the oracle
/// emits the normalized string it read with.
pub(crate) fn format_wire(format: ReadFormat) -> &'static str {
    match format {
        ReadFormat::Ansi => "ansi",
        _ => "text",
    }
}

/// `readPaneForDisplay` (dispatch.go:1077-1098) — the source/format
/// matrix the oracle reads panes through. A non-ansi read is the only
/// shape Herdr can serve by harvesting scrollback through the agent's
/// mouse-scroll interface: `recent`/`recent-unwrapped` in text format
/// scrolls the operator's real pane up and snaps it back, once per read
/// — only `visible` cannot trigger the harvest. Ansi reads pull physical
/// `recent` rows (the visible screen loses rows that scroll past between
/// polls); Claude keeps `recent-unwrapped` — logical lines its
/// alternate-screen history merge depends on — unless the pane was
/// resized for this read (`!resized` only).
pub(crate) fn display_source(format: ReadFormat, resized: bool, agent: &str) -> ReadSource {
    if format != ReadFormat::Ansi {
        return ReadSource::Visible;
    }
    if !resized && agent.to_lowercase().contains("claude") {
        return ReadSource::RecentUnwrapped;
    }
    ReadSource::Recent
}

/// `capPaneContentLines` (dispatch.go:1046-1065) — keep the last `limit`
/// lines; a single trailing newline is discounted first. Herdr may
/// return more than requested (the scrollback sources over-read), so the
/// cap is what the fingerprint and the wire `content` both see.
pub(crate) fn cap_pane_content_lines(content: &str, limit: u32) -> &str {
    if limit < 1 || content.is_empty() {
        return content;
    }
    let bytes = content.as_bytes();
    let mut end = bytes.len();
    if bytes[end - 1] == b'\n' {
        end -= 1;
    }
    let mut remaining = limit;
    let mut index = end;
    while index > 0 {
        index -= 1;
        if bytes[index] != b'\n' {
            continue;
        }
        remaining -= 1;
        if remaining == 0 {
            // `\n` is ASCII — `index + 1` is a char boundary.
            return &content[index + 1..];
        }
    }
    content
}

/// `paneFrameFingerprint` (server.go:2832-2881) — frame-level identity
/// the watch's unchanged-check runs on. The oracle's tagged hash walks
/// `{content, format, truncated, viewport_only, viewport_rows,
/// resize_settling, attention_kind, prompt, command, options,
/// interaction, question_layout}` — `writeField` writes a tag byte
/// (0 absent, 1 string, 2 bool, 3 JSON), the u64-LE length, then the
/// bytes. Internal only — never on the wire — but kept field-faithful so
/// a metadata- or semantics-only change moves the fingerprint and emits
/// the copy-delta exactly like the oracle.
fn frame_fingerprint(frame: &WatchFrame, format: ReadFormat) -> String {
    let mut digest = Sha256::new();
    let mut write_field = |tag: u8, data: &[u8]| {
        digest.update([tag]);
        digest.update((data.len() as u64).to_le_bytes());
        digest.update(data);
    };
    write_field(1, frame.content.as_bytes());
    write_field(1, format_wire(format).as_bytes());
    write_field(2, &[u8::from(frame.truncated)]);
    write_field(2, &[u8::from(frame.viewport_only)]);
    match frame.viewport_rows {
        // `response["viewport_rows"]` absent (no active rows lease) → the
        // `nil` tag; present → `json.Marshal(int64)` (decimal).
        Some(rows) if rows > 0 => write_field(3, rows.to_string().as_bytes()),
        _ => write_field(0, &[]),
    }
    // `resize_settling` is only ever *set* to true — absent otherwise,
    // so a clear flag hashes as the `nil` tag, not a false bool.
    if frame.resize_settling {
        write_field(2, &[1]);
    } else {
        write_field(0, &[]);
    }
    let semantics = &frame.semantics;
    write_field(1, semantics.attention_kind.as_bytes());
    write_field(1, semantics.prompt.as_bytes());
    write_field(1, semantics.command.as_bytes());
    // `[]string(nil)`/`nil *Interaction` both marshal as `null` under
    // the `default` arm — the typed-nil interface never hits `case nil`.
    let options_json = if semantics.options.is_empty() {
        b"null".to_vec()
    } else {
        serde_json::to_vec(&semantics.options).unwrap_or_else(|_| b"null".to_vec())
    };
    write_field(3, &options_json);
    let interaction_json = match &semantics.interaction {
        Some(interaction) => serde_json::to_vec(interaction).unwrap_or_else(|_| b"null".to_vec()),
        None => b"null".to_vec(),
    };
    write_field(3, &interaction_json);
    write_field(2, &[u8::from(semantics.question_layout)]);
    hex::encode(&digest.finalize()[..8])
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
    /// `lines` read budget — the router already applied the
    /// [`pane_lines`] default+clamp.
    pub(crate) lines: u32,
    /// `format` — `"ansi"` honored verbatim, everything else reads as
    /// text (`pane_watch.go:67-70`); `watchMessage` carries it to both
    /// the probe and the full read.
    pub(crate) format: ReadFormat,
    /// Poll period — [`watch_interval`] output.
    pub(crate) interval: Duration,
    /// The request's `content_fingerprint`: matching the initial read
    /// means the client already holds the frame — adopt it as the sent
    /// base instead of pushing a duplicate `pane_content`.
    pub(crate) known_fingerprint: Option<String>,
    /// `watch.target` — `TargetRef{ServerSessionID:"primary", PaneID}`
    /// unless the inbound `target` overrode it (`pane_watch.go:71-74`);
    /// stamped on every emitted frame (`response["target"]`).
    pub(crate) target: TargetRef,
}

/// The ambient handles `readPaneWatchFrame`/`pollPaneWatch` reach for
/// beyond the spec — the lease ledger (`applyPaneReadLease` →
/// `viewport_only`/`viewport_rows`/`resize_settling`), the topology
/// handle (the live committed view: `isClaudeAgent` source choice, the
/// mid-read generation/content fences, `classification_agent`, and the
/// `AcknowledgePane` + `wake` halves of `handleAcknowledge`), the
/// semantic side channels (`customAnswers`, `history.Manager`, the
/// `agent_update` broadcast), and the runtime endpoints (this client's
/// push sink + the session kill switch).
#[derive(Clone)]
pub(crate) struct WatchDeps {
    pub(crate) handle: TopologyHandle,
    pub(crate) leases: Leases,
    /// `s.state`'s custom-answer store — `classify_semantics` records and
    /// fills through it.
    pub(crate) questions: Questions,
    /// `s.historyM` — the claude-like transcript merge ledger.
    pub(crate) history: HistoryManager,
    /// `d.journal` — `handleAcknowledge`'s `d.fail` row on a gone pane.
    pub(crate) activities: crate::actions::activity::Journal,
    /// `d.broadcast` — `agent_update` when the ack moves displayed status.
    pub(crate) notices: Notices,
    /// This client's push endpoint.
    pub(crate) sink: Arc<dyn FrameSink>,
    /// Session-scoped kill switch.
    pub(crate) cancel: CancellationToken,
}

/// Control signals a router pushes into a live watch.
#[derive(Debug)]
pub enum WatchCtl {
    /// `pane_applied` — the client committed a frame; carries the wire
    /// `content_fingerprint` (`""` when the field is absent — the watch
    /// ignores it, matching the oracle).
    Ack(String),
    /// `pane_resync` — the client lost the chain; force full re-read.
    Resync,
    /// `unwatch_pane` / session teardown — stop the task.
    Stop,
}

/// One live watch: control channel + join handle.
pub struct WatchEntry {
    /// Router → task signals.
    pub ctl: mpsc::Sender<WatchCtl>,
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

    /// `watch_pane` — spawn the task. `startPaneWatch` replaces a live
    /// watch on the same pane (`previous.cancel()`): clients re-issue
    /// `watch_pane` to retarget/re-tune rather than `unwatch` first.
    pub(crate) fn start(&mut self, pane_id: String, spec: WatchSpec, deps: WatchDeps) {
        if let Some(previous) = self.entries.remove(&pane_id) {
            let _ = previous.ctl.try_send(WatchCtl::Stop);
            previous.abort();
        }
        let (ctl_tx, ctl_rx) = mpsc::channel(WATCH_CTL_QUEUE);
        let id = pane_id.clone();
        let invalidations = deps.handle.invalidations.subscribe();
        let task = tokio::spawn(
            watch_loop(pane_id, spec, deps, invalidations, ctl_rx)
                .instrument(tracing::info_span!("pane_watch", pane = %id)),
        );
        self.entries.insert(id, WatchEntry { ctl: ctl_tx, task });
    }

    /// `pane_applied` — deliver the ack + wire fingerprint; no-op when
    /// not watching. A full queue drops the signal (the ack timeout →
    /// resync chain self-heals); a closed queue means the task is gone.
    pub fn ack(&self, pane_id: &str, fingerprint: Option<&str>) {
        if let Some(entry) = self.entries.get(pane_id) {
            let signal = WatchCtl::Ack(fingerprint.unwrap_or_default().to_owned());
            if let Err(err) = entry.ctl.try_send(signal) {
                warn!(%err, pane_id, "pane_applied dropped — watch ctl queue");
            }
        }
    }

    /// `pane_resync` — force a full re-send; no-op when not watching.
    pub fn resync(&self, pane_id: &str) {
        if let Some(entry) = self.entries.get(pane_id) {
            if let Err(err) = entry.ctl.try_send(WatchCtl::Resync) {
                warn!(%err, pane_id, "pane_resync dropped — watch ctl queue");
            }
        }
    }

    /// `unwatch_pane` — stop and drop the watch. Returns whether one was
    /// live (the oracle answers unknown panes with a no-op receipt too).
    pub fn stop(&mut self, pane_id: &str) -> bool {
        if let Some(entry) = self.entries.remove(pane_id) {
            // A full queue drops `Stop` — the abort still kills the task.
            if let Err(mpsc::error::TrySendError::Full(_)) = entry.ctl.try_send(WatchCtl::Stop) {
                warn!(pane_id, "watch ctl queue full — stopping via abort");
            }
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

/// `paneWatchFrame` (pane_watch.go:31-38) — one read distilled for the
/// gate: content + its wire fingerprint, the frame-level fingerprint
/// `paneWatchUpdate` skips on, the emitted metadata + semantic fields,
/// the classification agent `paneWatchNeedsFrameRead` re-reads on, and
/// the settle flag.
struct WatchFrame {
    content: String,
    content_fingerprint: String,
    frame_fingerprint: String,
    /// `classificationAgent` — the `s.agentInfo` value the frame's
    /// classification was computed under; a change forces a re-read.
    classification_agent: String,
    truncated: bool,
    viewport_only: bool,
    viewport_rows: Option<i64>,
    resize_settling: bool,
    /// `preparePaneResponse`'s semantic half — emitted verbatim on full
    /// frames and copied onto deltas like every other response key.
    semantics: PaneSemantics,
}

/// The watch task's frame state.
struct WatchState {
    /// Fingerprint of the content last sent (and acked or in-flight).
    sent_fingerprint: String,
    /// Content last sent — the `pane_delta` base.
    sent_content: String,
    /// `acknowledged.frameFingerprint` — the frame-level identity of the
    /// last frame pushed (covers `truncated`/viewport/settle metadata,
    /// not just content): an identical frame sends nothing, a
    /// metadata-only change still emits the copy-segment delta.
    sent_frame_fingerprint: String,
    /// `acknowledged.resizeSettling` — while the committed frame was
    /// read inside the resize-settle window every tick full-reads, so
    /// the flag's clearing itself pushes a metadata delta.
    sent_resize_settling: bool,
    /// `acknowledged.classificationAgent` — the agent the committed
    /// frame's classification was computed under; a topology change to
    /// a different provider re-reads even when the probe fingerprint
    /// holds (`paneWatchNeedsFrameRead`'s second leg).
    sent_classification_agent: String,
    /// `watch.probeFingerprint` — the last `visible`-probe content
    /// fingerprint; empty forces the next tick's full read. Cleared on
    /// ack timeout and resync.
    probe_fingerprint: String,
    /// Fingerprint of the last `pane_applied`-committed frame — the
    /// oracle's `acknowledged.contentFingerprint`. Cleared on ack
    /// timeout and resync so a stale ack reads as foreign.
    acked_fingerprint: String,
    /// Gate: a frame is awaiting `pane_applied`. The companion deadline
    /// is the oracle's `pending.sentAt + ackTimeout` — an absolute
    /// `Instant` because the select arms' futures are rebuilt every loop
    /// turn (a relative `sleep` would restart on every tick).
    pending_ack: bool,
    /// `pending.sentAt + paneWatchAckTimeout` — `Some` only while gated.
    ack_deadline: Option<tokio::time::Instant>,
    /// After a gate timeout the next frame must be full (`ack_required`
    /// fresh chain), not a delta against a possibly-lost base.
    force_full: bool,
}

impl WatchState {
    fn new() -> Self {
        Self {
            sent_fingerprint: String::new(),
            sent_content: String::new(),
            sent_frame_fingerprint: String::new(),
            sent_resize_settling: false,
            sent_classification_agent: String::new(),
            probe_fingerprint: String::new(),
            acked_fingerprint: String::new(),
            pending_ack: false,
            ack_deadline: None,
            force_full: false,
        }
    }

    /// `watch.pending = frame` + the bookkeeping the frame's metadata
    /// feeds later polls (`pane_watch.go:219-221`).
    fn sent(&mut self, frame: &WatchFrame) {
        self.sent_fingerprint = frame.content_fingerprint.clone();
        self.sent_content = frame.content.clone();
        self.sent_frame_fingerprint = frame.frame_fingerprint.clone();
        self.sent_resize_settling = frame.resize_settling;
        self.sent_classification_agent = frame.classification_agent.clone();
        self.gate();
    }

    /// `watch.pending = frame; frame.sentAt = now` — engage the gate and
    /// its deadline.
    fn gate(&mut self) {
        self.pending_ack = true;
        self.ack_deadline = Some(tokio::time::Instant::now() + ACK_TIMEOUT);
    }

    /// Gate open — pending cleared, no deadline.
    fn ungate(&mut self) {
        self.pending_ack = false;
        self.ack_deadline = None;
    }
}

/// The watch loop — exits on `Stop`, cancel, closed sink, or closed
/// invalidation feed.
async fn watch_loop(
    pane_id: String,
    spec: WatchSpec,
    deps: WatchDeps,
    mut invalidations: broadcast::Receiver<Invalidation>,
    mut ctl: mpsc::Receiver<WatchCtl>,
) {
    let sink = &deps.sink;
    let cancel = &deps.cancel;
    let mut state = WatchState::new();

    // Initial frame — `runPaneWatch`'s first pass. A `content_fingerprint`
    // matching the fresh read means the client already holds the content:
    // the oracle's `knownFingerprint` branch still ships the frame's
    // metadata as a copy-everything `pane_delta` (base = the known
    // fingerprint) and engages the ack gate (`watch.pending = frame`).
    // A failed read emits nothing — the oracle sleeps and retries the nil
    // frame every interval; the first tick does the same here (the empty
    // `probe_fingerprint` forces the full read).
    if let Some(frame) = read_watch_frame(&pane_id, &spec, &deps).await {
        if spec.known_fingerprint.as_deref() == Some(frame.content_fingerprint.as_str()) {
            // `paneWatchUpdate`'s same-fingerprint branch:
            // `CopyLines: strings.Count(content, "\n") + 1`.
            let copy_lines =
                i64::try_from(frame.content.matches('\n').count() + 1).unwrap_or(i64::MAX);
            let message = delta_frame(
                &pane_id,
                &spec,
                &frame,
                frame.content_fingerprint.clone(),
                vec![delta::Segment {
                    copy_lines,
                    ..delta::Segment::default()
                }],
            );
            if sink.try_send(&message) {
                debug!("watch fingerprint hit — copy-segment delta");
                state.sent(&frame);
            }
        } else {
            send_frame(&pane_id, &spec, sink.as_ref(), &mut state, frame);
        }
    }

    // `next_tick` is the oracle's ticker — fires every `interval` whether
    // or not a poll ran (gated ticks only advance the schedule), so a
    // `pane.updated` missed while the gate was shut is picked up by the
    // first tick after it opens. `next_read` is the freshness boundary —
    // last poll + `interval`; an invalidation past it polls at
    // once, strictly faster than the tick (gated ticks must NOT advance
    // it — no poll happened, and the boundary is what lets the fast
    // path beat the next tick after the gate opens).
    let mut next_tick = tokio::time::Instant::now() + spec.interval;
    let mut next_read = next_tick;

    loop {
        // The ack deadline only runs while a frame is in flight —
        // `sleep_until` against a stored instant, since this future is
        // rebuilt every loop turn.
        let timeout = async {
            if let Some(deadline) = state.ack_deadline {
                tokio::time::sleep_until(deadline).await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            signal = ctl.recv() => {
                match signal {
                    // `handlePaneApplied` (`pane_watch.go:267-289`):
                    // empty fingerprint → ignored; matching `pending` →
                    // acknowledge; matching `acknowledged` → dup;
                    // anything else is foreign → `pane_resync`.
                    Some(WatchCtl::Ack(fingerprint)) => {
                        if fingerprint.is_empty() {
                            debug!("pane_applied without fingerprint — ignored");
                        } else if state.pending_ack
                            && fingerprint == state.sent_fingerprint
                        {
                            state.acked_fingerprint = fingerprint;
                            state.ungate();
                            debug!("pane_applied — gate cleared");
                        } else if !state.acked_fingerprint.is_empty()
                            && fingerprint == state.acked_fingerprint
                        {
                            debug!("duplicate pane_applied — ignored");
                        } else {
                            debug!("foreign pane_applied fingerprint — pane_resync");
                            let _ = sink.try_send(&Outbound::PaneResync(PaneResync {
                                r#type: "pane_resync".to_owned(),
                                pane_id: Some(pane_id.clone()),
                                target: Some(MaybeNull::Value(spec.target.clone())),
                            }));
                        }
                    }
                    Some(WatchCtl::Resync) => {
                        state.ungate();
                        state.acked_fingerprint.clear();
                        state.probe_fingerprint.clear();
                        state.force_full = true;
                        // Client-initiated recovery — bypass the cadence
                        // AND the probe tier: `force_full` answers with
                        // the full frame whatever the probe would say.
                        if let Some(frame) =
                            read_watch_frame(&pane_id, &spec, &deps).await
                        {
                            send_frame(&pane_id, &spec, sink.as_ref(), &mut state, frame);
                        }
                        next_read = tokio::time::Instant::now() + spec.interval;
                    }
                    Some(WatchCtl::Stop) | None => break,
                }
            }
            _ = timeout => {
                // The ack never landed: drop the gate AND the acked base
                // (the oracle clears `pending`, `acknowledged` and
                // `probeFingerprint`) so a late/stale ack reads as
                // foreign and the next poll's probe always full-reads a
                // fresh `ack_required` rebuild.
                debug!("ack timeout — gate reset, next frame is full");
                state.ungate();
                state.acked_fingerprint.clear();
                state.probe_fingerprint.clear();
                state.force_full = true;
            }
            _ = tokio::time::sleep_until(next_tick) => {
                next_tick = tokio::time::Instant::now() + spec.interval;
                // `pollPaneWatch` returns early on a fresh pending — a
                // gated tick probes nothing; the first tick after the ack
                // lands picks the update up.
                if !state.pending_ack {
                    poll(&pane_id, &spec, &deps, &mut state).await;
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
                // Fast path only: inside the freshness window or gated,
                // the tick covers the update.
                if triggered
                    && !state.pending_ack
                    && tokio::time::Instant::now() >= next_read
                {
                    poll(&pane_id, &spec, &deps, &mut state).await;
                    next_read = tokio::time::Instant::now() + spec.interval;
                }
            }
        }
    }
    debug!("watch stopped");
}

/// `pollPaneWatch` (pane_watch.go:165-223) — the tick: a cheap
/// `visible`-source probe first (`HandleProbePane`), a full
/// `HandleReadPane` read only when `paneWatchNeedsFrameRead` says the
/// frame moved. Probe/read failures are silent — the oracle's poll
/// returns without sending.
async fn poll(pane_id: &str, spec: &WatchSpec, deps: &WatchDeps, state: &mut WatchState) {
    // `HandleProbePane` (dispatch.go:1169-1191) — `pane.read` on the
    // `visible` source at a fixed 500 lines in the watch's format,
    // fenced on the pane's generation mid-read.
    let generation = deps.handle.topology.borrow().generation_of(pane_id);
    let Ok(probe) = deps
        .handle
        .client
        .pane_read(pane_id, ReadSource::Visible, PROBE_LINES, spec.format)
        .await
    else {
        return;
    };
    let (generation_now, classification_agent) = {
        let topology = deps.handle.topology.borrow();
        (
            topology.generation_of(pane_id),
            topology.classification_agent(pane_id),
        )
    };
    if generation_now != generation {
        return;
    }
    let probe_fingerprint = content_fingerprint(&probe.text);
    // `paneWatchNeedsFrameRead` (pane_watch.go:248-259): an empty or
    // moved probe reads; a committed `resize_settling` frame or a
    // `classificationAgent` change keeps reading until both clear.
    let needs_read = state.probe_fingerprint.is_empty()
        || probe_fingerprint != state.probe_fingerprint
        || state.sent_resize_settling
        || (!state.sent_frame_fingerprint.is_empty()
            && state.sent_classification_agent != classification_agent);
    if !needs_read {
        return;
    }
    let Some(frame) = read_watch_frame(pane_id, spec, deps).await else {
        // `frame == nil` → the probe fingerprint stays stale, so the next
        // tick retries the full read.
        return;
    };
    state.probe_fingerprint = probe_fingerprint;
    send_frame(pane_id, spec, deps.sink.as_ref(), state, frame);
}

/// `readPaneWatchFrame` (pane_watch.go:225-246) — the `HandleReadPane`
/// read behind every pushed frame: `handleAcknowledge`, the
/// `applyPaneReadLease` viewport flags, the `readPaneForDisplay`
/// source/format matrix, `capPaneContentLines`, and the mid-read
/// generation fence. `None` = read failed or the pane was replaced
/// under the read — the caller emits nothing (the oracle's `frame ==
/// nil` paths send nothing either: the initial loop retries, the poll
/// just ends).
async fn read_watch_frame(pane_id: &str, spec: &WatchSpec, deps: &WatchDeps) -> Option<WatchFrame> {
    let (generation, content_rev, agent, classification_agent) = {
        let topology = deps.handle.topology.borrow();
        (
            topology.generation_of(pane_id),
            topology.content_rev_of(pane_id),
            topology
                .pane_of(pane_id)
                .and_then(|a| a.agent.clone())
                .unwrap_or_default(),
            topology.classification_agent(pane_id),
        )
    };
    // `handleAcknowledge` — every `HandleReadPane` acks the pane through
    // the shared ledger; the `agent_update` broadcast + `wake` ride
    // inside when the displayed status moved. `paneWatchFrame` builds no
    // `request_id`, so a gone-pane failure row carries `""` like the
    // oracle's `stringValue(message, "request_id")` miss.
    crate::actions::acknowledge_pane_state(
        &deps.handle,
        &deps.notices,
        &deps.activities,
        pane_id,
        "",
    );
    // `applyPaneReadLease` — an active size lease marks the read
    // viewport-only and carries `viewport_rows`; client-sent
    // `terminal_columns`/`terminal_rows` are ignored wholesale (the
    // oracle `delete`s them before applying the lease).
    let viewport_only = deps.leases.active_columns(pane_id).await.is_some();
    let viewport_rows = if viewport_only {
        deps.leases.active_rows(pane_id).await
    } else {
        None
    };
    let read = deps
        .handle
        .client
        .pane_read(
            pane_id,
            display_source(spec.format, viewport_only, &agent),
            spec.lines,
            spec.format,
        )
        .await
        .ok()?;
    // `HandleReadPane`'s mid-read fences — generation (`replaced`) and
    // `ContentRevision` (`changed`).
    {
        let topology = deps.handle.topology.borrow();
        if topology.generation_of(pane_id) != generation
            || topology.content_rev_of(pane_id) != content_rev
        {
            return None;
        }
    }
    // `classifyPaneResponse`'s settle flag — viewport reads inside the
    // window are flagged so the app won't commit possibly-redrawn rows.
    let resize_settling = viewport_only
        && deps
            .leases
            .resized_within(pane_id, crate::actions::leases::RESIZE_SETTLE_WINDOW)
            .await;
    // `preparePaneResponse` — classify the capped raw read, merge
    // claude-like history when warranted, `noecho.Match` the tail.
    let capped = cap_pane_content_lines(&read.text, spec.lines);
    let prepared = prepare_pane_response(
        pane_id,
        capped,
        read.truncated,
        &classification_agent,
        viewport_only,
        spec.lines,
        &deps.questions,
        &deps.history,
    );
    let content_fingerprint = content_fingerprint(&prepared.content);
    let mut frame = WatchFrame {
        frame_fingerprint: String::new(),
        classification_agent,
        content: prepared.content,
        content_fingerprint,
        truncated: prepared.truncated,
        viewport_only,
        viewport_rows,
        resize_settling,
        semantics: prepared.semantics,
    };
    frame.frame_fingerprint = frame_fingerprint(&frame, spec.format);
    Some(frame)
}

/// `paneWatchUpdate` (pane_watch.go:325-351) — pick and push the right
/// frame for a fresh read, respecting the gate.
fn send_frame(
    pane_id: &str,
    spec: &WatchSpec,
    sink: &dyn FrameSink,
    state: &mut WatchState,
    frame: WatchFrame,
) {
    if state.pending_ack {
        // One unacked frame max — the tick retries once it opens.
        return;
    }
    // `acknowledged.frameFingerprint == current.frameFingerprint` → nil:
    // identical frames — content AND metadata — don't re-send, but the
    // oracle still adopts the read as `acknowledged` — a `pane_applied`
    // echoing it then reads as a dup, not a foreign resync.
    if !state.force_full
        && !state.sent_frame_fingerprint.is_empty()
        && frame.frame_fingerprint == state.sent_frame_fingerprint
    {
        state.sent_classification_agent = frame.classification_agent.clone();
        state.sent_resize_settling = frame.resize_settling;
        state.acked_fingerprint = frame.content_fingerprint.clone();
        return;
    }

    let message = if state.force_full || state.sent_fingerprint.is_empty() {
        // `acknowledged == nil` — always the full `ack_required` frame,
        // same fingerprint or not.
        full_frame(pane_id, spec, &frame)
    } else if frame.content_fingerprint == state.sent_fingerprint {
        // Same content, different frame — the metadata refresh branch:
        // a copy-everything delta (`CopyLines = count("\n") + 1`).
        let copy_lines = i64::try_from(frame.content.matches('\n').count() + 1).unwrap_or(i64::MAX);
        delta_frame(
            pane_id,
            spec,
            &frame,
            state.sent_fingerprint.clone(),
            vec![delta::Segment {
                copy_lines,
                ..delta::Segment::default()
            }],
        )
    } else {
        let segments = delta::build(&state.sent_content, &frame.content);
        if delta::efficient(&segments, &frame.content) {
            delta_frame(
                pane_id,
                spec,
                &frame,
                state.sent_fingerprint.clone(),
                segments,
            )
        } else {
            full_frame(pane_id, spec, &frame)
        }
    };

    if !sink.try_send(&message) {
        warn!("watch push refused — client queue gone/full");
        return;
    }
    state.sent(&frame);
    state.force_full = false;
}

/// The full `pane_content` watch frame — `paneWatchUpdate`'s
/// `ack_required` branch — every `preparePaneResponse` key included.
fn full_frame(pane_id: &str, spec: &WatchSpec, frame: &WatchFrame) -> Outbound {
    let semantics = &frame.semantics;
    Outbound::PaneContent(Box::new(PaneContent {
        r#type: "pane_content".to_owned(),
        pane_id: Some(pane_id.to_owned()),
        content: Some(frame.content.clone()),
        ack_required: Some(true),
        content_fingerprint: Some(frame.content_fingerprint.clone()),
        format: Some(format_wire(spec.format).to_owned()),
        truncated: Some(frame.truncated),
        viewport_only: Some(frame.viewport_only),
        viewport_rows: if frame.viewport_only {
            frame.viewport_rows
        } else {
            None
        },
        resize_settling: frame.resize_settling.then_some(true),
        attention_kind: Some(semantics.attention_kind.to_owned()),
        prompt: Some(semantics.prompt.clone()),
        command: Some(semantics.command.clone()),
        options: Some(if semantics.options.is_empty() {
            MaybeNull::Null
        } else {
            MaybeNull::Value(semantics.options.clone())
        }),
        interaction: Some(match &semantics.interaction {
            Some(interaction) => MaybeNull::Value(interaction.clone()),
            None => MaybeNull::Null,
        }),
        question_layout: Some(semantics.question_layout),
        no_echo: Some(semantics.no_echo),
        no_echo_prompt: semantics.no_echo_prompt.clone(),
        target: Some(MaybeNull::Value(spec.target.clone())),
        ..PaneContent::default()
    }))
}

/// `paneDeltaResponse` (pane_watch.go:353-364) — every response key
/// except `content`, plus `type`/`base_fingerprint`/`segments`.
/// `ack_required` is never among the copied keys (the oracle sets it
/// after the response map is built, on the full-frame branches only).
fn delta_frame(
    pane_id: &str,
    spec: &WatchSpec,
    frame: &WatchFrame,
    base_fingerprint: String,
    segments: Vec<delta::Segment>,
) -> Outbound {
    let semantics = &frame.semantics;
    Outbound::PaneDelta(Box::new(PaneDelta {
        r#type: "pane_delta".to_owned(),
        pane_id: Some(pane_id.to_owned()),
        base_fingerprint: Some(base_fingerprint),
        content_fingerprint: Some(frame.content_fingerprint.clone()),
        format: Some(format_wire(spec.format).to_owned()),
        truncated: Some(frame.truncated),
        viewport_only: Some(frame.viewport_only),
        viewport_rows: if frame.viewport_only {
            frame.viewport_rows
        } else {
            None
        },
        resize_settling: frame.resize_settling.then_some(true),
        attention_kind: Some(semantics.attention_kind.to_owned()),
        prompt: Some(semantics.prompt.clone()),
        command: Some(semantics.command.clone()),
        options: Some(if semantics.options.is_empty() {
            MaybeNull::Null
        } else {
            MaybeNull::Value(semantics.options.clone())
        }),
        interaction: Some(match &semantics.interaction {
            Some(interaction) => MaybeNull::Value(interaction.clone()),
            None => MaybeNull::Null,
        }),
        question_layout: Some(semantics.question_layout),
        no_echo: Some(semantics.no_echo),
        no_echo_prompt: semantics.no_echo_prompt.clone(),
        segments: Some(MaybeNull::Value(segments)),
        target: Some(MaybeNull::Value(spec.target.clone())),
        ..PaneDelta::default()
    }))
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
    use crate::topology::Topology;
    use lerdr_herdr::{BoxIo, Client, ClientConfig, Event, Transport};
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
    /// — enough NDJSON for `Client::pane_read`/`call`. Every request body
    /// is recorded so tests can assert the source/format/lines chosen.
    pub(crate) struct FakeHerdr {
        responses: Mutex<VecDeque<Value>>,
        last: Mutex<Option<Value>>,
        /// One dial per request — the observable "did it re-read" counter.
        pub(crate) dials: AtomicUsize,
        /// Every request received, in order.
        pub(crate) requests: Arc<Mutex<Vec<Value>>>,
    }

    impl FakeHerdr {
        pub(crate) fn serving(responses: Vec<Value>) -> Arc<Self> {
            Arc::new(FakeHerdr {
                responses: Mutex::new(responses.into()),
                last: Mutex::new(None),
                dials: AtomicUsize::new(0),
                requests: Arc::new(Mutex::new(Vec::new())),
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

        /// The `params` of every `pane.read` seen, in order.
        pub(crate) fn read_params(&self) -> Vec<Value> {
            self.requests
                .lock()
                .expect("requests poisoned")
                .iter()
                .filter(|req| req.get("method").and_then(Value::as_str) == Some("pane.read"))
                .filter_map(|req| req.get("params").cloned())
                .collect()
        }
    }

    impl Transport for FakeHerdr {
        fn dial(&self) -> Pin<Box<dyn Future<Output = io::Result<BoxIo>> + Send>> {
            self.dials.fetch_add(1, Ordering::Relaxed);
            let requests = self.requests.clone();
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
                    let request = serde_json::from_str::<Value>(&line).unwrap_or(Value::Null);
                    requests
                        .lock()
                        .expect("requests poisoned")
                        .push(request.clone());
                    let id = request
                        .get("id")
                        .and_then(|v| v.as_str().map(str::to_owned))
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
            format: ReadFormat::Text,
            interval,
            known_fingerprint: known.map(str::to_owned),
            // The router's default `watch.target` (`pane_watch.go:71`).
            target: TargetRef {
                server_session_id: "primary".to_owned(),
                pane_id: "wE:pE".to_owned(),
                ..TargetRef::default()
            },
        }
    }

    /// `WatchDeps` over an empty topology + fresh shared ledgers —
    /// the fake client answers the reads; nothing else consults them.
    pub(crate) fn deps(
        client: &Client,
        sink: Arc<dyn FrameSink>,
        invalidations: &broadcast::Sender<Invalidation>,
        cancel: CancellationToken,
    ) -> WatchDeps {
        WatchDeps {
            handle: TopologyHandle::for_test(
                client.clone(),
                Arc::new(Topology::default()),
                invalidations.clone(),
            ),
            leases: Leases::new(client.clone()),
            questions: Questions::default(),
            history: HistoryManager::in_memory(),
            activities: crate::actions::activity::Journal::default(),
            notices: Notices::default(),
            sink,
            cancel,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use serde_json::Value;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use tokio::sync::broadcast;

    /// `requestedPaneWatchInterval`: the whitelist passes through,
    /// everything else — absent, non-integral, out-of-set — is the
    /// 250 ms default (`pane_watch.go:291-299`).
    #[test]
    fn interval_whitelist() {
        let default = Duration::from_millis(250);
        assert_eq!(watch_interval(None), default);
        assert_eq!(watch_interval(Some(100)), Duration::from_millis(100));
        assert_eq!(watch_interval(Some(250)), Duration::from_millis(250));
        assert_eq!(watch_interval(Some(500)), Duration::from_millis(500));
        assert_eq!(watch_interval(Some(1_000)), Duration::from_millis(1_000));
        for off in [0, 1, 2_000, 120_000, -5, i64::MAX] {
            assert_eq!(
                watch_interval(Some(off)),
                default,
                "interval_ms={off} must fall back to the default"
            );
        }
    }

    /// `messageInt`/`intValue(message["lines"], 30)` → clamp
    /// `1..=MaxLines` (`pane_watch.go:61-65`, `dispatch.go:1106-1112`).
    #[test]
    fn pane_lines_default_and_clamp() {
        let msg = |map: serde_json::Value| {
            Inbound::decode_map(&map.as_object().unwrap().clone()).expect("decode")
        };
        assert_eq!(
            pane_lines(&msg(serde_json::json!({"type":"watch_pane"}))),
            30
        );
        assert_eq!(
            pane_lines(&msg(serde_json::json!({"type":"watch_pane","lines":50}))),
            50
        );
        // Integral floats read as ints (`messageInt`'s float64 branch).
        assert_eq!(
            pane_lines(&msg(serde_json::json!({"type":"watch_pane","lines":50.0}))),
            50
        );
        for raw in [0, -3] {
            assert_eq!(
                pane_lines(&msg(serde_json::json!({"type":"watch_pane","lines":raw}))),
                1,
                "lines={raw} clamps to 1"
            );
        }
        assert_eq!(
            pane_lines(&msg(serde_json::json!({"type":"watch_pane","lines":20000}))),
            10_000
        );
        assert_eq!(
            pane_lines(&msg(
                serde_json::json!({"type":"watch_pane","lines":i64::MAX})
            )),
            10_000
        );
        // `null` → the typed field's zero value → default. (Strings and
        // fractional numbers fail `Inbound::decode_map` outright — the
        // decode boundary answers `invalid_request` before the router
        // sees them, so `pane_lines` never observes them.)
        assert_eq!(
            pane_lines(&msg(serde_json::json!({"type":"watch_pane","lines":null}))),
            30
        );
        // Messages built without the raw map (`Default` + field writes)
        // fall back to the typed `lines`.
        let mut typed_only = Inbound::default();
        typed_only.lines = 42;
        assert_eq!(pane_lines(&typed_only), 42);
    }

    /// `watch_pane` with a matching `content_fingerprint` emits the
    /// oracle's copy-everything `pane_delta`, not a `pane_content` echo —
    /// and like every `pane_delta`, carries no `ack_required`.
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
            deps(&herdr.client(), sink, &invalidations, cancel.clone()),
        );

        let frame = rx.recv().await.expect("initial watch frame");
        match frame {
            Outbound::PaneDelta(delta) => {
                assert_eq!(delta.pane_id.as_deref(), Some("wE:pE"));
                assert_eq!(delta.ack_required, None);
                assert_eq!(delta.base_fingerprint.as_deref(), Some("911169ddaaf146af"));
                assert_eq!(
                    delta.content_fingerprint.as_deref(),
                    Some("911169ddaaf146af")
                );
                // `watch.target` is stamped on every frame
                // (`response["target"] = watch.target`).
                match delta.target {
                    Some(MaybeNull::Value(target)) => {
                        assert_eq!(target.server_session_id, "primary");
                        assert_eq!(target.pane_id, "wE:pE");
                    }
                    other => panic!("expected target value, got {other:?}"),
                }
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
            deps(&herdr.client(), sink, &invalidations, cancel.clone()),
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
                match content.target {
                    Some(MaybeNull::Value(target)) => {
                        assert_eq!(target.server_session_id, "primary");
                        assert_eq!(target.pane_id, "wE:pE");
                    }
                    other => panic!("expected target value, got {other:?}"),
                }
            }
            other => panic!("expected pane_content, got {other:?}"),
        }
        watches.stop("wE:pE");
        cancel.cancel();
    }

    /// A failed initial `pane.read` emits no error frame — the oracle's
    /// `frame == nil` → sleep → loop; the first tick retries and sends a
    /// full `pane_content` (`sent_fingerprint` is still empty).
    #[tokio::test(start_paused = true)]
    async fn initial_read_failure_retries_on_tick() {
        let herdr = FakeHerdr::serving(vec![
            // `result` without `read` — `pane_read`'s decode fails.
            serde_json::json!({"type": "pane_read"}),
            pane_read_result("wE:pE", "v1\n"),
        ]);
        let (sink, mut rx) = recording_sink();
        let (invalidations, _) = broadcast::channel(16);
        let cancel = CancellationToken::new();
        let mut watches = WatchSet::default();
        watches.start(
            "wE:pE".to_owned(),
            spec(30, Duration::from_millis(250), None),
            deps(&herdr.client(), sink, &invalidations, cancel.clone()),
        );

        // The first frame to arrive is the tick retry's — an error
        // `pane_content` would surface here first.
        let frame = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("tick retry frame")
            .expect("channel open");
        match frame {
            Outbound::PaneContent(content) => {
                assert_eq!(content.content.as_deref(), Some("v1\n"));
                assert!(content.error.is_none());
                assert_eq!(content.ack_required, Some(true));
            }
            other => panic!("expected pane_content, got {other:?}"),
        }
        // Probe (visible, 500) + the conditional full read.
        assert_eq!(herdr.dials.load(Ordering::Relaxed), 3);
        watches.stop("wE:pE");
        cancel.cancel();
    }

    /// The periodic tick delivers a `pane.updated` that landed while the
    /// ack gate was shut — the lost-update regression. Gate closed →
    /// invalidation + tick do nothing; after the matching `pane_applied`
    /// the next tick reads and pushes the newer frame.
    #[tokio::test(start_paused = true)]
    async fn gated_invalidation_is_picked_up_by_the_tick() {
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
            spec(30, Duration::from_millis(250), None),
            deps(&herdr.client(), sink, &invalidations, cancel.clone()),
        );

        let _initial = rx.recv().await.expect("initial frame");
        let t0 = tokio::time::Instant::now();

        // Invalidation lands while gated — no read, nothing deferred.
        let _ = invalidations.send(invalidate(Some("wE:pE")));
        tokio::task::yield_now().await;
        assert_eq!(herdr.dials.load(Ordering::Relaxed), 1);

        // The 250 ms tick is still gated — still no read.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(herdr.dials.load(Ordering::Relaxed), 1);

        // The ack opens the gate; the next tick (t0+500) picks up v2.
        watches.ack("wE:pE", Some(content_fingerprint("v1\n").as_str()));
        let frame = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("tick refresh")
            .expect("channel open");
        match frame {
            Outbound::PaneContent(content) => {
                assert_eq!(content.content.as_deref(), Some("v2\n"));
            }
            other => panic!("expected pane_content, got {other:?}"),
        }
        assert!(
            t0.elapsed() >= Duration::from_millis(400),
            "the frame came from a tick, not the ack/invalidation"
        );
        // Probe (visible, 500) + the conditional full read.
        assert_eq!(herdr.dials.load(Ordering::Relaxed), 3);
        watches.stop("wE:pE");
        cancel.cancel();
    }

    /// The 4 s gate timeout clears pending AND the acked base — the next
    /// tick re-reads and sends a full `ack_required` `pane_content` even
    /// when the content fingerprint is unchanged (`acknowledged == nil`
    /// bypasses the same-fingerprint skip).
    #[tokio::test(start_paused = true)]
    async fn ack_timeout_forces_full_frame() {
        let herdr = FakeHerdr::serving(vec![
            pane_read_result("wE:pE", "same\n"),
            pane_read_result("wE:pE", "same\n"),
        ]);
        let (sink, mut rx) = recording_sink();
        let (invalidations, _) = broadcast::channel(16);
        let cancel = CancellationToken::new();
        let mut watches = WatchSet::default();
        watches.start(
            "wE:pE".to_owned(),
            spec(30, Duration::from_millis(250), None),
            deps(&herdr.client(), sink, &invalidations, cancel.clone()),
        );

        match rx.recv().await.expect("initial frame") {
            Outbound::PaneContent(content) => {
                assert_eq!(content.ack_required, Some(true));
            }
            other => panic!("expected pane_content, got {other:?}"),
        }

        // No ack. Ticks stay gated until the 4 s timeout resets the gate;
        // the following tick re-reads and ships the full frame again.
        let frame = tokio::time::timeout(Duration::from_secs(6), rx.recv())
            .await
            .expect("post-timeout full frame")
            .expect("channel open");
        match frame {
            Outbound::PaneContent(content) => {
                assert_eq!(content.content.as_deref(), Some("same\n"));
                assert_eq!(content.ack_required, Some(true));
            }
            other => panic!("expected pane_content, got {other:?}"),
        }
        // Probe (visible, 500) + the conditional full read.
        assert_eq!(herdr.dials.load(Ordering::Relaxed), 3);
        watches.stop("wE:pE");
        cancel.cancel();
    }

    /// `handlePaneApplied`: foreign fingerprint → `pane_resync` push;
    /// matching `pending` → gate clears silently; empty → ignored; a
    /// second ack of the committed frame → ignored.
    #[tokio::test(start_paused = true)]
    async fn pane_applied_fingerprint_mismatch_pushes_resync() {
        let herdr = FakeHerdr::serving(vec![pane_read_result("wE:pE", "v1\n")]);
        let (sink, mut rx) = recording_sink();
        let (invalidations, _) = broadcast::channel(16);
        let cancel = CancellationToken::new();
        let mut watches = WatchSet::default();
        watches.start(
            "wE:pE".to_owned(),
            spec(30, Duration::from_millis(250), None),
            deps(&herdr.client(), sink, &invalidations, cancel.clone()),
        );

        let _initial = rx.recv().await.expect("initial frame");
        let fp = content_fingerprint("v1\n");
        // A scheduling point that lets the watch consume queued `Ack`s
        // without reaching the 250 ms tick.
        let settle = || tokio::time::sleep(Duration::from_millis(10));

        // Empty/absent fingerprint — ignored outright.
        watches.ack("wE:pE", Some(""));
        watches.ack("wE:pE", None);
        settle().await;
        assert!(rx.try_recv().is_err(), "empty ack must not push anything");

        // Foreign fingerprint — the oracle pushes `pane_resync`.
        watches.ack("wE:pE", Some("0000000000000000"));
        let pushed = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("pane_resync push")
            .expect("channel open");
        match pushed {
            Outbound::PaneResync(resync) => {
                assert_eq!(resync.pane_id.as_deref(), Some("wE:pE"));
                match resync.target {
                    Some(MaybeNull::Value(target)) => {
                        assert_eq!(target.server_session_id, "primary");
                        assert_eq!(target.pane_id, "wE:pE");
                    }
                    other => panic!("expected target value, got {other:?}"),
                }
            }
            other => panic!("expected pane_resync, got {other:?}"),
        }

        // Matching the pending frame — gate clears, nothing pushed.
        watches.ack("wE:pE", Some(fp.as_str()));
        settle().await;
        assert!(rx.try_recv().is_err(), "matching ack must not resync");

        // Duplicate of the committed frame — ignored.
        watches.ack("wE:pE", Some(fp.as_str()));
        settle().await;
        assert!(rx.try_recv().is_err(), "duplicate ack is ignored");

        // Foreign again — resync.
        watches.ack("wE:pE", Some("1111111111111111"));
        let pushed = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("pane_resync push")
            .expect("channel open");
        assert!(matches!(pushed, Outbound::PaneResync(_)));
        watches.stop("wE:pE");
        cancel.cancel();
    }

    /// `startPaneWatch` replace semantics: a re-issued `watch_pane` on
    /// the same pane aborts the old task and applies the new spec — here
    /// the new `content_fingerprint` turns the fresh initial read into
    /// the copy-everything `pane_delta` instead of `pane_content`.
    #[tokio::test(start_paused = true)]
    async fn watch_reissue_replaces_the_task() {
        let herdr = FakeHerdr::serving(vec![
            pane_read_result("wE:pE", "v1\n"),
            pane_read_result("wE:pE", "v1\n"),
        ]);
        let (sink, mut rx) = recording_sink();
        let (invalidations, _) = broadcast::channel(16);
        let cancel = CancellationToken::new();
        let mut watches = WatchSet::default();
        watches.start(
            "wE:pE".to_owned(),
            spec(30, Duration::from_millis(250), None),
            deps(
                &herdr.client(),
                sink.clone(),
                &invalidations,
                cancel.clone(),
            ),
        );
        match rx.recv().await.expect("initial frame") {
            Outbound::PaneContent(content) => {
                assert_eq!(content.content.as_deref(), Some("v1\n"));
            }
            other => panic!("expected pane_content, got {other:?}"),
        }

        let fp = content_fingerprint("v1\n");
        watches.start(
            "wE:pE".to_owned(),
            spec(30, Duration::from_millis(250), Some(fp.as_str())),
            deps(&herdr.client(), sink, &invalidations, cancel.clone()),
        );
        let frame = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("replacement watch frame")
            .expect("channel open");
        match frame {
            Outbound::PaneDelta(delta) => {
                assert_eq!(delta.base_fingerprint.as_deref(), Some(fp.as_str()));
            }
            other => panic!("expected copy-segment pane_delta, got {other:?}"),
        }
        assert!(watches.watching("wE:pE"));
        watches.stop("wE:pE");
        cancel.cancel();
    }

    /// `paneDeltaResponse` never sets `ack_required` — a chained
    /// `pane_delta` must not carry the key even though the gate still
    /// engages internally.
    #[tokio::test(start_paused = true)]
    async fn pane_delta_carries_no_ack_required() {
        // One changed line in a 40-line pane — `delta::efficient` needs
        // literal bytes + 64B/segment < 3/4 of the content, so the pane
        // must be large enough for a delta to win. The line budget must
        // clear the pane too — `capPaneContentLines` would tail-cap the
        // read and change the fingerprint the test acks.
        let lines: Vec<String> = (0..40).map(|i| format!("line-{i:02}-abcdefghij")).collect();
        let v1 = format!("{}\n", lines.join("\n"));
        let mut v2_lines = lines.clone();
        v2_lines[20] = "line-20-CHANGED!!!".to_owned();
        let v2 = format!("{}\n", v2_lines.join("\n"));
        let herdr = FakeHerdr::serving(vec![
            pane_read_result("wE:pE", &v1),
            pane_read_result("wE:pE", &v2),
        ]);
        let (sink, mut rx) = recording_sink();
        let (invalidations, _) = broadcast::channel(16);
        let cancel = CancellationToken::new();
        let mut watches = WatchSet::default();
        watches.start(
            "wE:pE".to_owned(),
            spec(100, Duration::from_millis(250), None),
            deps(&herdr.client(), sink, &invalidations, cancel.clone()),
        );

        let _initial = rx.recv().await.expect("initial frame");
        watches.ack("wE:pE", Some(content_fingerprint(&v1).as_str()));

        // The 250 ms tick re-reads — one changed line of forty is an
        // efficient delta.
        let frame = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("delta frame")
            .expect("channel open");
        match frame {
            Outbound::PaneDelta(delta) => {
                assert_eq!(delta.ack_required, None);
                let encoded = Outbound::PaneDelta(delta).encode();
                let json = String::from_utf8(encoded).expect("utf8");
                assert!(
                    !json.contains("ack_required"),
                    "pane_delta wire shape must omit ack_required: {json}"
                );
            }
            other => panic!("expected pane_delta, got {other:?}"),
        }
        watches.stop("wE:pE");
        cancel.cancel();
    }

    /// `interval` bounds the read rate: a burst of invalidations inside
    /// one window collapses into the single tick read at the boundary.
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
            deps(&herdr.client(), sink, &invalidations, cancel.clone()),
        );

        let _initial = rx.recv().await.expect("initial frame");
        watches.ack("wE:pE", Some(content_fingerprint("v1\n").as_str()));

        // Three invalidations inside the first 250ms window — the tick at
        // the boundary covers them in one read.
        for _ in 0..3 {
            let _ = invalidations.send(invalidate(Some("wE:pE")));
        }
        tokio::task::yield_now().await;
        assert_eq!(herdr.dials.load(Ordering::Relaxed), 1);

        let refresh = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("boundary refresh frame")
            .expect("channel open");
        assert!(matches!(
            refresh,
            Outbound::PaneContent(_) | Outbound::PaneDelta(_)
        ));
        // Probe (visible, 500) + the conditional full read.
        assert_eq!(herdr.dials.load(Ordering::Relaxed), 3);
        watches.stop("wE:pE");
        cancel.cancel();
    }

    /// Past `next_read` with the gate open an invalidation reads at once —
    /// the fast path beats the next tick.
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
            deps(&herdr.client(), sink, &invalidations, cancel.clone()),
        );

        let _initial = rx.recv().await.expect("initial frame");
        let t0 = tokio::time::Instant::now();

        // The 250 ms tick is gated by the unacked initial frame.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(herdr.dials.load(Ordering::Relaxed), 1);

        // Gate opens, then an invalidation past `next_read` → immediate
        // refresh, ahead of the t0+500 tick.
        watches.ack("wE:pE", Some(content_fingerprint("v1\n").as_str()));
        tokio::task::yield_now().await;
        let _ = invalidations.send(invalidate(Some("wE:pE")));
        let frame = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("refresh frame")
            .expect("channel open");
        assert!(matches!(
            frame,
            Outbound::PaneContent(_) | Outbound::PaneDelta(_)
        ));
        assert!(
            t0.elapsed() < Duration::from_millis(500),
            "the invalidation fast path must not wait for the next tick"
        );
        // Probe (visible, 500) + the conditional full read.
        assert_eq!(herdr.dials.load(Ordering::Relaxed), 3);
        watches.stop("wE:pE");
        cancel.cancel();
    }

    /// `readPaneForDisplay` for a text watch stays on `visible` for BOTH
    /// tiers — probe AND the conditional full read — because only
    /// `visible` cannot trigger Herdr's mouse-scroll harvest on the
    /// operator's pane (dispatch.go:1084-1091). The frame carries
    /// `format:"text"`.
    #[tokio::test(start_paused = true)]
    async fn text_watch_reads_visible_source_on_both_tiers() {
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
            spec(30, Duration::from_millis(250), None),
            deps(&herdr.client(), sink, &invalidations, cancel.clone()),
        );

        match rx.recv().await.expect("initial frame") {
            Outbound::PaneContent(content) => {
                assert_eq!(content.format.as_deref(), Some("text"));
                assert_eq!(content.viewport_only, Some(false));
            }
            other => panic!("expected pane_content, got {other:?}"),
        }
        watches.ack("wE:pE", Some(content_fingerprint("v1\n").as_str()));

        // The 250 ms tick: probe (visible) → moved → full read (visible).
        let frame = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("tick frame")
            .expect("channel open");
        assert!(matches!(
            frame,
            Outbound::PaneContent(_) | Outbound::PaneDelta(_)
        ));

        let reads = herdr.read_params();
        assert_eq!(reads.len(), 3, "initial + probe + full read");
        for params in &reads {
            assert_eq!(
                params.get("source").and_then(Value::as_str),
                Some("visible"),
                "text reads must never leave the visible source: {params}"
            );
            assert_eq!(params.get("format").and_then(Value::as_str), Some("text"));
        }
        // Probe depth is the oracle's fixed 500; the full read is the
        // spec's lines.
        assert_eq!(reads[1].get("lines").and_then(Value::as_u64), Some(500));
        assert_eq!(reads[2].get("lines").and_then(Value::as_u64), Some(30));
        watches.stop("wE:pE");
        cancel.cancel();
    }

    /// An `ansi` watch echoes `format:"ansi"` on every frame and reads
    /// the display source — `recent` rows for a non-Claude/unknown agent
    /// (`readPaneForDisplay`'s ansi branch), while the probe stays on
    /// `visible`.
    #[tokio::test(start_paused = true)]
    async fn ansi_watch_echoes_format_and_reads_recent() {
        let herdr = FakeHerdr::serving(vec![
            pane_read_result("wE:pE", "v1\n"),
            pane_read_result("wE:pE", "v2\n"),
        ]);
        let (sink, mut rx) = recording_sink();
        let (invalidations, _) = broadcast::channel(16);
        let cancel = CancellationToken::new();
        let mut watches = WatchSet::default();
        let mut ansi_spec = spec(30, Duration::from_millis(250), None);
        ansi_spec.format = ReadFormat::Ansi;
        watches.start(
            "wE:pE".to_owned(),
            ansi_spec,
            deps(&herdr.client(), sink, &invalidations, cancel.clone()),
        );

        match rx.recv().await.expect("initial frame") {
            Outbound::PaneContent(content) => {
                assert_eq!(content.format.as_deref(), Some("ansi"));
            }
            other => panic!("expected pane_content, got {other:?}"),
        }
        watches.ack("wE:pE", Some(content_fingerprint("v1\n").as_str()));

        let frame = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("tick frame")
            .expect("channel open");
        assert!(matches!(
            frame,
            Outbound::PaneContent(_) | Outbound::PaneDelta(_)
        ));
        if let Outbound::PaneDelta(delta) = &frame {
            // `paneDeltaResponse` copies `format` from the read response.
            assert_eq!(delta.format.as_deref(), Some("ansi"));
        }

        let reads = herdr.read_params();
        assert_eq!(reads.len(), 3, "initial + probe + full read");
        // The topology feed has no agent for wE:pE → not Claude → ansi
        // reads pull `recent` physical rows.
        assert_eq!(
            reads[0].get("source").and_then(Value::as_str),
            Some("recent")
        );
        assert_eq!(
            reads[1].get("source").and_then(Value::as_str),
            Some("visible"),
            "the probe is always the cheap visible source"
        );
        assert_eq!(
            reads[2].get("source").and_then(Value::as_str),
            Some("recent")
        );
        for params in &reads {
            assert_eq!(params.get("format").and_then(Value::as_str), Some("ansi"));
        }
        watches.stop("wE:pE");
        cancel.cancel();
    }

    /// `paneFrameFingerprint` covers metadata, not just content: a frame
    /// whose `truncated` flag flips with identical content still pushes —
    /// as the copy-everything delta (`paneWatchUpdate`'s same-content
    /// branch), not silence and not a full frame.
    #[test]
    fn metadata_only_change_emits_copy_delta() {
        let (sink, mut rx) = recording_sink();
        let spec = spec(30, Duration::from_millis(250), None);
        let mut state = WatchState::new();
        let frame = |truncated: bool| {
            let mut frame = WatchFrame {
                content: "v1\n".to_owned(),
                content_fingerprint: content_fingerprint("v1\n"),
                frame_fingerprint: String::new(),
                classification_agent: String::new(),
                truncated,
                viewport_only: false,
                viewport_rows: None,
                resize_settling: false,
                semantics: PaneSemantics::default(),
            };
            frame.frame_fingerprint = frame_fingerprint(&frame, ReadFormat::Text);
            frame
        };

        // First frame — full + ack gate.
        send_frame("wE:pE", &spec, &*sink, &mut state, frame(false));
        assert!(matches!(
            rx.try_recv().expect("initial frame"),
            Outbound::PaneContent(_)
        ));
        state.ungate();

        // Identical frame → nothing (the frame-fingerprint skip).
        send_frame("wE:pE", &spec, &*sink, &mut state, frame(false));
        assert!(rx.try_recv().is_err(), "identical frame must not send");
        assert!(!state.pending_ack);

        // `truncated` flipped — same content fingerprint, different frame
        // fingerprint → the copy-everything `pane_delta`.
        send_frame("wE:pE", &spec, &*sink, &mut state, frame(true));
        match rx.try_recv().expect("metadata delta") {
            Outbound::PaneDelta(delta) => {
                assert_eq!(delta.ack_required, None);
                assert_eq!(delta.truncated, Some(true));
                assert_eq!(
                    delta.base_fingerprint.as_deref(),
                    Some(content_fingerprint("v1\n").as_str())
                );
                let segments = match delta.segments {
                    Some(MaybeNull::Value(segments)) => segments,
                    other => panic!("expected segments, got {other:?}"),
                };
                assert_eq!(segments.len(), 1);
                assert_eq!(segments[0].copy_lines, 2); // "v1\n" → 1 + 1
            }
            other => panic!("expected copy-segment pane_delta, got {other:?}"),
        }
    }

    /// `capPaneContentLines` — the last `limit` lines win; a trailing
    /// newline is discounted, fewer lines pass through, `0` disables.
    #[test]
    fn cap_pane_content_lines_keeps_the_tail() {
        assert_eq!(cap_pane_content_lines("a\nb\nc\nd\n", 2), "c\nd\n");
        assert_eq!(cap_pane_content_lines("a\nb\nc", 2), "b\nc");
        assert_eq!(cap_pane_content_lines("a\nb\n", 10), "a\nb\n");
        assert_eq!(cap_pane_content_lines("", 3), "");
        assert_eq!(cap_pane_content_lines("a\nb\n", 0), "a\nb\n");
        assert_eq!(cap_pane_content_lines("single", 1), "single");
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
