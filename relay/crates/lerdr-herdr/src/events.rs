//! Event stream: the reactive spine of the Herdr boundary.
//!
//! Wire facts (verified against herdr 0.9.1, protocol 22):
//!
//! * `events.subscribe` takes **dotted** names (`{"type":"workspace.created"}`).
//!   Three subscriptions are per-pane and carry extra fields
//!   (`pane.output_matched`, `pane.agent_status_changed`, `pane.scroll_changed`);
//!   the rest are global `{"type": name}` entries.
//! * The first line back is the handshake
//!   `{"id":"lerdr-events","result":{"type":"subscription_started"}}`. A
//!   refusal arrives as `{"id":"","error":{"code":"invalid_request","message":
//!   "invalid request: unknown variant `…`"}}` — the pre-dispatch shape.
//! * Events on the wire carry **snake_case** names (`pane_updated`); the client
//!   canonicalizes to dotted via the 26-alias table the Go client maintains.
//! * `events_lost` arrives mid-stream as an error envelope echoing the
//!   subscription id, then Herdr closes the connection. Recovery is the
//!   documented loop: resubscribe → `subscription_started` →
//!   `session.snapshot` → treat subsequent events as invalidation signals.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use futures_core::Stream;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::instrument;

use crate::error::{EventStreamError, HerdrError, SubscribeError};
use crate::transport::{BoxIo, Transport};
use crate::types::{AgentStatus, OutputMatch, ReadSource, SessionSnapshot};
use crate::wire;

/// Request id for the event subscription — the Go client's constant; an
/// `events_lost` envelope echoes it.
pub const EVENTS_REQUEST_ID: &str = "lerdr-events";

/// Legacy (snake_case) → canonical (dotted) event name aliases — the 26-entry
/// table the Go client applies to every inbound event.
const EVENT_NAME_ALIASES: &[(&str, &str)] = &[
    ("workspace_created", "workspace.created"),
    ("workspace_updated", "workspace.updated"),
    ("workspace_metadata_updated", "workspace.metadata_updated"),
    ("workspace_closed", "workspace.closed"),
    ("workspace_renamed", "workspace.renamed"),
    ("workspace_moved", "workspace.moved"),
    ("workspace_reordered", "workspace.reordered"),
    ("workspace_focused", "workspace.focused"),
    ("worktree_created", "worktree.created"),
    ("worktree_opened", "worktree.opened"),
    ("worktree_removed", "worktree.removed"),
    ("tab_created", "tab.created"),
    ("tab_closed", "tab.closed"),
    ("tab_renamed", "tab.renamed"),
    ("tab_moved", "tab.moved"),
    ("tab_focused", "tab.focused"),
    ("pane_created", "pane.created"),
    ("pane_closed", "pane.closed"),
    ("pane_updated", "pane.updated"),
    ("pane_focused", "pane.focused"),
    ("pane_moved", "pane.moved"),
    ("pane_output_changed", "pane.output_changed"),
    ("pane_exited", "pane.exited"),
    ("pane_agent_detected", "pane.agent_detected"),
    ("pane_agent_status_changed", "pane.agent_status_changed"),
    ("layout_updated", "layout.updated"),
];

/// Canonicalize a wire event name: legacy snake_case aliases map to dotted;
/// already-canonical names (and the dotted-only `pane.output_matched`,
/// `pane.scroll_changed`) pass through unchanged.
pub fn canonical_event_name(name: &str) -> &str {
    EVENT_NAME_ALIASES
        .iter()
        .find(|(legacy, _)| *legacy == name)
        .map(|(_, canonical)| *canonical)
        .unwrap_or(name)
}

/// The outbound spelling for `events.wait` match clauses, which use the
/// legacy snake_case names. Dotted input maps back; unknown names pass
/// through untouched.
pub fn wire_event_name(name: &str) -> &str {
    if name.contains('.') {
        EVENT_NAME_ALIASES
            .iter()
            .find(|(_, canonical)| *canonical == name)
            .map(|(legacy, _)| *legacy)
            .unwrap_or(name)
    } else {
        name
    }
}

/// One event from the subscription stream. `name` is always the canonical
/// (dotted) form; `data` is the event's raw payload object (e.g.
/// `{"pane": {…}}` for `pane.updated`).
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub name: String,
    pub data: Value,
}

impl Event {
    /// Decode `data` into the per-event payload shape the caller expects.
    pub fn data_as<T: serde::de::DeserializeOwned>(&self) -> serde_json::Result<T> {
        serde_json::from_value(self.data.clone())
    }

    /// `true` for `pane.*` events.
    pub fn is_pane(&self) -> bool {
        self.name.starts_with("pane.")
    }

    /// `true` for `workspace.*`/`worktree.*`/`tab.*`/`layout.*` topology
    /// events.
    pub fn is_topology(&self) -> bool {
        self.name.starts_with("workspace.")
            || self.name.starts_with("worktree.")
            || self.name.starts_with("tab.")
            || self.name.starts_with("layout.")
    }
}

impl<'de> Deserialize<'de> for Event {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Envelope {
            event: String,
            #[serde(default)]
            data: Option<Value>,
        }
        let env = Envelope::deserialize(deserializer)?;
        if env.event.is_empty() {
            return Err(serde::de::Error::custom("herdr event has no event kind"));
        }
        Ok(Event {
            name: canonical_event_name(&env.event).to_owned(),
            data: env.data.unwrap_or(Value::Null),
        })
    }
}

/// One `events.subscribe` entry. Global subscriptions serialize as
/// `{"type":"<dotted-name>"}`; the three per-pane subscriptions carry their
/// filter fields.
#[derive(Debug, Clone, PartialEq)]
pub enum Subscription {
    /// A global lifecycle event — `workspace.created`, `pane.updated`, …
    Named(&'static str),
    /// `pane.output_matched` — server-side output match on one pane.
    PaneOutputMatched {
        pane_id: String,
        source: ReadSource,
        match_: OutputMatch,
        strip_ansi: Option<bool>,
        lines: Option<u32>,
    },
    /// `pane.agent_status_changed` — status transitions of one pane,
    /// optionally restricted to one status.
    PaneAgentStatusChanged {
        pane_id: String,
        agent_status: Option<AgentStatus>,
    },
    /// `pane.scroll_changed` — scroll metrics changes of one pane.
    PaneScrollChanged { pane_id: String },
}

impl Subscription {
    /// `pane.output_matched` subscription with the schema-required fields.
    pub fn pane_output_matched(
        pane_id: impl Into<String>,
        source: ReadSource,
        match_: OutputMatch,
    ) -> Self {
        Subscription::PaneOutputMatched {
            pane_id: pane_id.into(),
            source,
            match_,
            strip_ansi: None,
            lines: None,
        }
    }

    /// `pane.agent_status_changed` for one pane (any status).
    pub fn pane_agent_status_changed(pane_id: impl Into<String>) -> Self {
        Subscription::PaneAgentStatusChanged {
            pane_id: pane_id.into(),
            agent_status: None,
        }
    }

    /// `pane.scroll_changed` for one pane.
    pub fn pane_scroll_changed(pane_id: impl Into<String>) -> Self {
        Subscription::PaneScrollChanged {
            pane_id: pane_id.into(),
        }
    }
}

impl Serialize for Subscription {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let fields = match self {
            Subscription::Named(_) => 1,
            Subscription::PaneOutputMatched { .. } => 6,
            Subscription::PaneAgentStatusChanged { .. } => 3,
            Subscription::PaneScrollChanged { .. } => 2,
        };
        let mut map = serializer.serialize_map(Some(fields))?;
        match self {
            Subscription::Named(name) => {
                map.serialize_entry("type", name)?;
            }
            Subscription::PaneOutputMatched {
                pane_id,
                source,
                match_,
                strip_ansi,
                lines,
            } => {
                map.serialize_entry("type", "pane.output_matched")?;
                map.serialize_entry("pane_id", pane_id)?;
                map.serialize_entry("source", source)?;
                map.serialize_entry("match", match_)?;
                if let Some(strip) = strip_ansi {
                    map.serialize_entry("strip_ansi", strip)?;
                }
                if let Some(lines) = lines {
                    map.serialize_entry("lines", lines)?;
                }
            }
            Subscription::PaneAgentStatusChanged {
                pane_id,
                agent_status,
            } => {
                map.serialize_entry("type", "pane.agent_status_changed")?;
                map.serialize_entry("pane_id", pane_id)?;
                if let Some(status) = agent_status {
                    map.serialize_entry("agent_status", status)?;
                }
            }
            Subscription::PaneScrollChanged { pane_id } => {
                map.serialize_entry("type", "pane.scroll_changed")?;
                map.serialize_entry("pane_id", pane_id)?;
            }
        }
        map.end()
    }
}

/// The relay's topology subscription set — the Go client's
/// `topologySubscriptions` (20 lifecycle events) plus two gated optional
/// entries: `workspace.reordered` (older Herdr builds reject the whole
/// `events.subscribe` when the name is present) and `pane.output_changed`
/// (only present on builds whose schema exposes the subscription variant —
/// 0.9.1 lists the `pane_output_changed` event payload but ships no
/// `pane.output_changed` subscription, so the capability consult keeps it
/// off the wire there).
pub fn topology_subscriptions(
    include_workspace_reordered: bool,
    include_pane_output_changed: bool,
) -> Vec<Subscription> {
    const NAMES: &[&str] = &[
        "pane.created",
        "pane.closed",
        "pane.updated",
        "pane.moved",
        "pane.exited",
        "pane.agent_detected",
        "tab.created",
        "tab.closed",
        "tab.renamed",
        "tab.moved",
        "workspace.created",
        "workspace.updated",
        "workspace.metadata_updated",
        "workspace.closed",
        "workspace.renamed",
        "workspace.moved",
        "workspace.focused",
        "worktree.created",
        "worktree.opened",
        "worktree.removed",
    ];
    let mut subs: Vec<Subscription> = NAMES.iter().map(|n| Subscription::Named(n)).collect();
    if include_workspace_reordered {
        subs.push(Subscription::Named("workspace.reordered"));
    }
    if include_pane_output_changed {
        subs.push(Subscription::Named("pane.output_changed"));
    }
    subs
}

/// Run the `events.subscribe` handshake on a fresh connection.
///
/// On success returns the connection plus the [`wire::LineReader`] that read
/// the handshake — its buffer may already hold event lines flushed in the
/// same socket write, and the event stream must keep reading from it so
/// none are lost. The deadline covers dial + write + handshake read; the
/// returned stream has no deadline.
#[instrument(skip_all, fields(transport = %transport.describe()))]
pub(crate) async fn subscribe_on(
    transport: &dyn Transport,
    subscriptions: &[Subscription],
    request_id: &str,
    timeout: Duration,
    max_line_bytes: usize,
) -> Result<(BoxIo, wire::LineReader), SubscribeError> {
    let deadline = Instant::now() + timeout;
    let mut conn = tokio::time::timeout_at(deadline, transport.dial())
        .await
        .map_err(|_| {
            SubscribeError::transport(HerdrError::not_started(io::Error::new(
                io::ErrorKind::TimedOut,
                "herdr events dial timed out",
            )))
        })?
        .map_err(|e| SubscribeError::transport(HerdrError::not_started(e)))?;

    #[derive(Serialize)]
    struct Params<'a> {
        subscriptions: &'a [Subscription],
    }
    let payload = wire::encode_request(request_id, "events.subscribe", &Params { subscriptions })
        .map_err(SubscribeError::transport)?;
    wire::write_request(&mut conn, &payload, deadline)
        .await
        .map_err(SubscribeError::transport)?;
    let mut lines = wire::LineReader::new(max_line_bytes);
    let line = lines
        .next(&mut conn, deadline)
        .await
        .map_err(|e| SubscribeError::transport(HerdrError::dispatched_io(e)))?;
    let response = wire::decode_response(&line).map_err(SubscribeError::transport)?;

    if let Some(error) = response.error {
        let pre_dispatch = wire::is_pre_dispatch_refusal(&response.id, &error.code, &error.message);
        return Err(SubscribeError::refused(
            error.code,
            error.message,
            pre_dispatch,
        ));
    }
    if response.id != request_id {
        return Err(SubscribeError::transport(HerdrError::dispatched_msg(
            format!(
                "herdr events subscription response id mismatch: got {:?}, want {request_id:?}",
                response.id
            ),
        )));
    }
    let result_type = response
        .result
        .as_ref()
        .and_then(|r| serde_json::from_str::<Value>(r.get()).ok())
        .and_then(|v| v.get("type").and_then(Value::as_str).map(str::to_owned));
    match result_type.as_deref() {
        Some("subscription_started") => Ok((conn, lines)),
        other => Err(SubscribeError::transport(HerdrError::dispatched_msg(
            format!("herdr events subscription returned {other:?}"),
        ))),
    }
}

/// A live `events.subscribe` connection. Reading is driven by a background
/// task that drains the socket eagerly into a bounded queue: the consumer
/// never has to keep up with socket speed, and a consumer that stops entirely
/// surfaces `Lagged` (→ resync) instead of silently dropping events.
///
/// Implements [`futures_core::Stream`] (`Item = Result<Event,
/// EventStreamError>`) plus an inherent [`next_event`](Self::next_event).
/// Dropping the stream aborts the reader and closes the socket.
pub struct EventStream {
    rx: mpsc::Receiver<Result<Event, EventStreamError>>,
    reader: JoinHandle<()>,
    /// Terminal error captured by `drain` for the next poll.
    pending_terminal: Option<EventStreamError>,
    done: bool,
}

impl EventStream {
    /// Spawn the reader task on a connection whose handshake was consumed by
    /// `lines` — its buffer may already contain event lines.
    pub(crate) fn spawn(conn: BoxIo, lines: wire::LineReader, queue_capacity: usize) -> Self {
        let (tx, rx) = mpsc::channel(queue_capacity.max(1));
        let reader = tokio::spawn(read_loop(conn, lines, tx));
        EventStream {
            rx,
            reader,
            pending_terminal: None,
            done: false,
        }
    }

    /// Next event, or `None` once the stream has ended. Terminal errors are
    /// delivered once as `Err(..)` before the stream returns `None`.
    pub async fn next_event(&mut self) -> Option<Result<Event, EventStreamError>> {
        if let Some(err) = self.pending_terminal.take() {
            self.done = true;
            return Some(Err(err));
        }
        if self.done {
            return None;
        }
        match self.rx.recv().await {
            Some(Ok(event)) => Some(Ok(event)),
            Some(Err(err)) => {
                self.done = true;
                Some(Err(err))
            }
            None => {
                self.done = true;
                None
            }
        }
    }

    /// Pull every event already buffered without blocking — used by
    /// `bootstrap` to surface the events that arrived while the
    /// `session.snapshot` request was in flight (the Go client's
    /// `EventStream.drain`). A terminal marker found mid-drain is stored and
    /// returned by the next `next_event`/`poll_next`.
    pub fn drain(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(Ok(event)) => events.push(event),
                Ok(Err(err)) => {
                    self.pending_terminal = Some(err);
                    self.done = true;
                    break;
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    self.done = true;
                    break;
                }
            }
        }
        events
    }

    /// `true` once the stream has delivered its terminal item.
    pub fn is_done(&self) -> bool {
        self.done
    }
}

impl std::fmt::Debug for EventStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventStream")
            .field("done", &self.done)
            .field("pending_terminal", &self.pending_terminal)
            .finish()
    }
}

impl Stream for EventStream {
    type Item = Result<Event, EventStreamError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if let Some(err) = this.pending_terminal.take() {
            this.done = true;
            return Poll::Ready(Some(Err(err)));
        }
        if this.done {
            return Poll::Ready(None);
        }
        match Pin::new(&mut this.rx).poll_recv(cx) {
            Poll::Ready(Some(Ok(event))) => Poll::Ready(Some(Ok(event))),
            Poll::Ready(Some(Err(err))) => {
                this.done = true;
                Poll::Ready(Some(Err(err)))
            }
            Poll::Ready(None) => {
                this.done = true;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for EventStream {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

/// Decode one stream line into an event or a terminal error. Error envelopes
/// mid-stream (`{"id":…,"error":{…}}`) are how Herdr reports `events_lost`.
fn decode_stream_line(line: &[u8]) -> Result<Event, EventStreamError> {
    let value: Value =
        serde_json::from_slice(line).map_err(|e| EventStreamError::Decode(format!("{e}")))?;
    if let Some(error) = value.get("error") {
        let code = error
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        return Err(if code == "events_lost" {
            EventStreamError::EventsLost(message)
        } else {
            EventStreamError::Terminated { code, message }
        });
    }
    if value.get("event").and_then(Value::as_str).is_none() {
        return Err(EventStreamError::Decode(
            "stream line has no `event` field".into(),
        ));
    }
    serde_json::from_value(value).map_err(|e| EventStreamError::Decode(format!("{e}")))
}

/// Reader task: drains the socket as fast as the server writes into the
/// bounded queue. A full queue means the consumer has stopped — push one
/// terminal `Lagged` (waiting for the slot the consumer frees) and exit.
async fn read_loop(
    mut conn: BoxIo,
    mut lines: wire::LineReader,
    tx: mpsc::Sender<Result<Event, EventStreamError>>,
) {
    loop {
        // No deadline: a live subscription is idle-tolerant — silence is
        // healthy, a closed socket is the signal.
        let line = match lines.next(&mut conn, Instant::now() + FAR_FUTURE).await {
            Ok(line) => line,
            Err(e) => {
                let _ = tx.send(Err(EventStreamError::Closed(Arc::new(e)))).await;
                return;
            }
        };
        match decode_stream_line(&line) {
            Ok(event) => {
                if tx.try_send(Ok(event)).is_err() {
                    // Queue full: the consumer is gone or stalled past the
                    // bound. Deliver Lagged as the terminal item — waiting for
                    // a freed slot is fine because any consumer still reading
                    // will drain one and let this through.
                    let _ = tx.send(Err(EventStreamError::Lagged)).await;
                    return;
                }
            }
            Err(err) => {
                let _ = tx.send(Err(err)).await;
                return;
            }
        }
    }
}

/// A deadline far enough out to mean "none" without overflowing `Instant`.
pub(crate) const FAR_FUTURE: Duration = Duration::from_secs(365 * 24 * 3600);

/// `bootstrap()` result: the authoritative snapshot plus the live stream and
/// the events that arrived between `subscription_started` and the snapshot
/// response.
#[derive(Debug)]
pub struct Bootstrap {
    pub snapshot: SessionSnapshot,
    pub stream: EventStream,
    /// Events received while the snapshot was in flight. Snapshots and events
    /// share no sequence boundary — treat these as invalidation signals and
    /// re-read the affected entities; never replay them onto the snapshot.
    pub gap_events: Vec<Event>,
}

/// Exponential backoff for the resync loop — a pure state machine so tests
/// drive it under virtual time; the sleep happens in the supervisor task.
#[derive(Debug, Clone)]
pub struct Backoff {
    min: Duration,
    max: Duration,
    attempt: u32,
}

impl Backoff {
    /// Default resync cadence: 250ms doubling to a 10s cap.
    pub const fn new() -> Self {
        Backoff {
            min: Duration::from_millis(250),
            max: Duration::from_secs(10),
            attempt: 0,
        }
    }

    pub fn with_min_max(min: Duration, max: Duration) -> Self {
        Backoff {
            min,
            max: max.max(min),
            attempt: 0,
        }
    }

    /// Delay before the next attempt; doubles each call until `max`.
    pub fn next_delay(&mut self) -> Duration {
        let shift = self.attempt.min(20);
        let delay = self.min.saturating_mul(1u32 << shift).min(self.max);
        self.attempt = self.attempt.saturating_add(1);
        delay
    }

    /// Call after a successful bootstrap — the next failure starts from `min`
    /// again.
    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Backoff::new()
    }
}

/// What the supervisor emits. Consumers replace their projection on
/// [`Synced`](SupervisorSignal::Synced) and treat every
/// [`Invalidated`](SupervisorSignal::Invalidated) as a hint to re-read — never
/// as a payload to apply unconditionally (doc 08 / socket-api.mdx:
/// "Snapshots and events have no shared sequence boundary").
#[derive(Debug)]
pub enum SupervisorSignal {
    /// A fresh `session.snapshot` after (re)subscription — replace local state.
    Synced(Box<SessionSnapshot>),
    /// One event — an invalidation signal. `gap` is `true` for events that
    /// arrived between `subscription_started` and the snapshot (extra care:
    /// they may predate or postdate it).
    Invalidated { event: Event, gap: bool },
    /// The stream/bootstrap failed; the supervisor will retry after `delay`.
    /// Informational — no consumer action needed beyond marking state stale.
    Reconnecting {
        attempt: u32,
        delay: Duration,
        cause: String,
    },
}

/// The documented `events_lost` recovery loop as a stream transformer:
/// resubscribe → `subscription_started` → `session.snapshot` → forward events
/// as invalidation signals; on any stream end, back off and repeat. The task
/// runs until the returned stream is dropped.
pub struct EventSupervisor {
    subscriptions: Vec<Subscription>,
    /// When `true`, the bootstrap uses the topology subscription set with the
    /// `workspace.reordered` capability fallback (ignores `subscriptions`).
    topology_fallback: bool,
    backoff: Backoff,
    /// Cap on the internal signal queue before the supervisor applies
    /// backpressure to the socket reader (which then hits `Lagged`).
    signal_queue: usize,
}

impl EventSupervisor {
    /// Run the loop for an explicit subscription set.
    pub fn new(subscriptions: Vec<Subscription>) -> Self {
        EventSupervisor {
            subscriptions,
            topology_fallback: false,
            backoff: Backoff::new(),
            signal_queue: 512,
        }
    }

    /// Run the loop over the topology subscription set (the
    /// `workspace.reordered` fallback is handled inside `bootstrap`).
    pub fn topology() -> Self {
        EventSupervisor {
            subscriptions: Vec::new(),
            topology_fallback: true,
            backoff: Backoff::new(),
            signal_queue: 512,
        }
    }

    pub fn backoff(mut self, backoff: Backoff) -> Self {
        self.backoff = backoff;
        self
    }

    pub fn signal_queue(mut self, capacity: usize) -> Self {
        self.signal_queue = capacity;
        self
    }

    /// Spawn the loop. Returns a `Stream` of [`SupervisorSignal`] that lives
    /// until dropped; the supervisor task is aborted on drop.
    pub(crate) fn run(self, client: crate::Client) -> SupervisorStream {
        let (tx, rx) = mpsc::channel(self.signal_queue.max(1));
        let subscriptions = self.subscriptions;
        let topology_fallback = self.topology_fallback;
        let mut backoff = self.backoff;
        let task = tokio::spawn(async move {
            'resync: loop {
                let boot = client
                    .bootstrap_with(&subscriptions, topology_fallback)
                    .await;
                match boot {
                    Ok(mut boot) => {
                        backoff.reset();
                        if tx
                            .send(SupervisorSignal::Synced(Box::new(boot.snapshot)))
                            .await
                            .is_err()
                        {
                            return;
                        }
                        for event in std::mem::take(&mut boot.gap_events) {
                            if tx
                                .send(SupervisorSignal::Invalidated { event, gap: true })
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                        loop {
                            match boot.stream.next_event().await {
                                Some(Ok(event)) => {
                                    if tx
                                        .send(SupervisorSignal::Invalidated { event, gap: false })
                                        .await
                                        .is_err()
                                    {
                                        return;
                                    }
                                }
                                Some(Err(err)) => {
                                    let delay = backoff.next_delay();
                                    let cause = err.to_string();
                                    let _ = tx.try_send(SupervisorSignal::Reconnecting {
                                        attempt: backoff.attempt(),
                                        delay,
                                        cause,
                                    });
                                    tokio::time::sleep(delay).await;
                                    continue 'resync;
                                }
                                None => {
                                    // Clean close: the peer dropped the
                                    // subscription — same recovery path.
                                    let delay = backoff.next_delay();
                                    let _ = tx.try_send(SupervisorSignal::Reconnecting {
                                        attempt: backoff.attempt(),
                                        delay,
                                        cause: "herdr closed the event stream".to_string(),
                                    });
                                    tokio::time::sleep(delay).await;
                                    continue 'resync;
                                }
                            }
                        }
                    }
                    Err(err) => {
                        let delay = backoff.next_delay();
                        let _ = tx.try_send(SupervisorSignal::Reconnecting {
                            attempt: backoff.attempt(),
                            delay,
                            cause: err.to_string(),
                        });
                        tokio::time::sleep(delay).await;
                    }
                }
            }
        });
        SupervisorStream { rx, task }
    }
}

/// Stream half of the supervisor — yields [`SupervisorSignal`] until dropped.
pub struct SupervisorStream {
    rx: mpsc::Receiver<SupervisorSignal>,
    task: JoinHandle<()>,
}

impl SupervisorStream {
    /// Inherent async accessor equivalent to `Stream::next` — avoids a
    /// `futures-util` import for consumers.
    pub async fn next_signal(&mut self) -> Option<SupervisorSignal> {
        self.rx.recv().await
    }
}

impl Stream for SupervisorStream {
    type Item = SupervisorSignal;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.get_mut().rx).poll_recv(cx)
    }
}

impl Drop for SupervisorStream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn canonicalizes_legacy_names() {
        assert_eq!(canonical_event_name("pane_updated"), "pane.updated");
        assert_eq!(
            canonical_event_name("workspace_metadata_updated"),
            "workspace.metadata_updated"
        );
        assert_eq!(canonical_event_name("layout_updated"), "layout.updated");
        // Dotted-only and unknown names pass through.
        assert_eq!(
            canonical_event_name("pane.output_matched"),
            "pane.output_matched"
        );
        assert_eq!(canonical_event_name("something.new"), "something.new");
    }

    #[test]
    fn wire_name_maps_back() {
        assert_eq!(wire_event_name("pane.updated"), "pane_updated");
        assert_eq!(
            wire_event_name("pane.output_matched"),
            "pane.output_matched"
        );
        assert_eq!(wire_event_name("pane_updated"), "pane_updated");
    }

    #[test]
    fn event_deserialize_canonicalizes() {
        let event: Event = serde_json::from_value(json!({
            "event": "pane_updated",
            "data": {"pane_id": "wE:pE"}
        }))
        .unwrap();
        assert_eq!(event.name, "pane.updated");
        assert_eq!(event.data["pane_id"], "wE:pE");
        assert!(event.is_pane());
    }

    #[test]
    fn event_deserialize_rejects_missing_name() {
        // Missing `event` field — serde's own missing-field error fires
        // before the custom check.
        assert!(serde_json::from_value::<Event>(json!({"data": {}})).is_err());
        // Present but empty — the custom check.
        let err = serde_json::from_value::<Event>(json!({"event": "", "data": {}})).unwrap_err();
        assert!(err.to_string().contains("event kind"));
    }

    #[test]
    fn decode_stream_line_events_lost() {
        let line =
            br#"{"id":"lerdr-events","error":{"code":"events_lost","message":"history overrun"}}"#;
        let err = decode_stream_line(line).unwrap_err();
        assert!(matches!(err, EventStreamError::EventsLost(_)));
        assert!(err.history_lost());
    }

    #[test]
    fn decode_stream_line_other_error() {
        let line = br#"{"id":"lerdr-events","error":{"code":"boom","message":"x"}}"#;
        let err = decode_stream_line(line).unwrap_err();
        assert!(matches!(
            err,
            EventStreamError::Terminated { ref code, .. } if code == "boom"
        ));
        assert!(!err.history_lost());
    }

    #[test]
    fn subscription_serializes_global() {
        let v = serde_json::to_value(Subscription::Named("pane.updated")).unwrap();
        assert_eq!(v, json!({"type": "pane.updated"}));
    }

    #[test]
    fn subscription_serializes_per_pane() {
        let v = serde_json::to_value(Subscription::pane_output_matched(
            "wE:pE",
            ReadSource::RecentUnwrapped,
            OutputMatch::substring("hello"),
        ))
        .unwrap();
        assert_eq!(v["type"], "pane.output_matched");
        assert_eq!(v["pane_id"], "wE:pE");
        assert_eq!(v["source"], "recent_unwrapped");
        assert_eq!(v["match"], json!({"type": "substring", "value": "hello"}));
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let mut b = Backoff::with_min_max(Duration::from_millis(100), Duration::from_millis(500));
        assert_eq!(b.next_delay(), Duration::from_millis(100));
        assert_eq!(b.next_delay(), Duration::from_millis(200));
        assert_eq!(b.next_delay(), Duration::from_millis(400));
        assert_eq!(b.next_delay(), Duration::from_millis(500));
        assert_eq!(b.next_delay(), Duration::from_millis(500));
        b.reset();
        assert_eq!(b.next_delay(), Duration::from_millis(100));
    }

    #[test]
    fn topology_subscriptions_reordered_gate() {
        let with = topology_subscriptions(true, false);
        let without = topology_subscriptions(false, false);
        assert_eq!(with.len(), without.len() + 1);
        assert!(with
            .iter()
            .any(|s| matches!(s, Subscription::Named("workspace.reordered"))));
        assert!(!without
            .iter()
            .any(|s| matches!(s, Subscription::Named("workspace.reordered"))));
    }

    /// `pane.output_changed` rides the same bounded-set handshake as
    /// `workspace.reordered` — gated independently, absent by default.
    #[test]
    fn topology_subscriptions_output_changed_gate() {
        let with = topology_subscriptions(false, true);
        let without = topology_subscriptions(false, false);
        assert_eq!(with.len(), without.len() + 1);
        assert!(with
            .iter()
            .any(|s| matches!(s, Subscription::Named("pane.output_changed"))));
        assert!(!without
            .iter()
            .any(|s| matches!(s, Subscription::Named("pane.output_changed"))));
        // Both optionals together extend the 20-name base by two.
        assert_eq!(topology_subscriptions(true, true).len(), without.len() + 2);
    }

    #[tokio::test]
    async fn event_stream_delivers_events_then_close() {
        let (client, mut server) = tokio::net::UnixStream::pair().unwrap();
        let conn: BoxIo = Box::new(client);
        let mut stream = EventStream::spawn(conn, wire::LineReader::new(MAX_TEST_LINE), 16);
        // Two events in ONE socket flush — the regression case for dropping
        // bytes after the first newline.
        server
            .write_all(
                br#"{"event":"pane_updated","data":{"pane_id":"wE:pE"}}
{"event":"workspace_created","data":{"workspace_id":"wX"}}
"#,
            )
            .await
            .unwrap();
        server.shutdown().await.unwrap();

        let e = next_event_t(&mut stream).await.unwrap().unwrap();
        assert_eq!(e.name, "pane.updated");
        let e = next_event_t(&mut stream).await.unwrap().unwrap();
        assert_eq!(e.name, "workspace.created");
        match next_event_t(&mut stream).await {
            Some(Err(EventStreamError::Closed(_))) | None => {}
            other => panic!("expected Closed or None, got {other:?}"),
        }
        drop(server);
    }

    #[tokio::test]
    async fn event_stream_lagged_on_full_queue() {
        let (client, mut server) = tokio::net::UnixStream::pair().unwrap();
        let conn: BoxIo = Box::new(client);
        // Capacity 1: once the reader fills the slot and the next event won't
        // fit, it commits to pushing a terminal `Lagged`.
        let mut stream = EventStream::spawn(conn, wire::LineReader::new(MAX_TEST_LINE), 1);
        for i in 0..8u8 {
            server
                .write_all(
                    format!("{{\"event\":\"pane_updated\",\"data\":{{\"i\":{i}}}}}\n").as_bytes(),
                )
                .await
                .unwrap();
        }
        // Let the reader fill the queue and overflow — without consuming
        // concurrently, overflow is guaranteed.
        tokio::time::sleep(Duration::from_millis(50)).await;
        server.shutdown().await.unwrap();

        let first = next_event_t(&mut stream).await.expect("one buffered event");
        assert!(first.is_ok());
        let second = next_event_t(&mut stream).await.expect("terminal item");
        assert!(
            matches!(second, Err(EventStreamError::Lagged)),
            "expected Lagged, got {second:?}"
        );
        assert!(next_event_t(&mut stream).await.is_none());
    }

    /// Bounded `next_event` — a broken stream fails the test instead of
    /// hanging the suite.
    async fn next_event_t(stream: &mut EventStream) -> Option<Result<Event, EventStreamError>> {
        tokio::time::timeout(Duration::from_secs(10), stream.next_event())
            .await
            .expect("event stream wait timed out")
    }

    const MAX_TEST_LINE: usize = 1024 * 1024;
}
