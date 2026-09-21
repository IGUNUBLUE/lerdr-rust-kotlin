//! `SessionActor` — one per authenticated client — plus the reader/writer
//! pumps and the connection supervisor that ties them together.
//!
//! Topology (mirrors `Hub.Serve`/`readPump`/`writePump`):
//!
//! ```text
//! socket ──► reader task ──► bounded mpsc ──► ACTOR ──► SendBuffer
//!                                              │            │ pop → seal
//! producers ──► bounded mpsc (ClientSink) ──►──┘            ▼
//!                                            sealed mpsc ──► writer task ──► socket
//! ```
//!
//! Every queue is bounded. Overflow anywhere on the send path is lag, and
//! lag evicts the client — queued data is never silently dropped
//! (`sendbuffer.go`, `Hub.Send`). The actor owns the [`Session`]: sealing
//! runs inside the actor so the writer pump never touches key state.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use lerdr_core::json::MaybeNull;
use lerdr_core::protocol::{
    compatible, decode_failure_response, error_codes, error_response, incompatible_response,
    ActionClass, ActionMetadata, ApiError, CommandResultMessage, HerdrStatus, Inbound, Outbound,
    PushConfig, RequestScope, CAPABILITIES, VERSION,
};
use lerdr_core::sendbuffer::{is_replaceable, PushResult, RejectReason, SendBuffer};
use lerdr_e2ee::Session;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info_span, warn, Instrument};

use crate::auth::{AuthenticatedIdentity, DeviceAuthStore, Role};
use crate::frame::{CloseStatus, FrameIo, FrameRead, FrameWrite, ReadError};
use crate::handshake::{self, KeySource};
use crate::router::{ActionRouter, ClientContext};

/// `wsSendTimeout` — bound on a single socket write.
pub const SEND_TIMEOUT: Duration = Duration::from_secs(5);
/// `wsCloseTimeout` — bound on the graceful close handshake.
pub const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);
/// `orderedIngressCapacity` — per-client inbound queue (Go's is hub-wide;
/// the actor-per-client layout makes it per-connection).
pub const INBOUND_CAPACITY: usize = 128;
/// Producer channel depth — bounded like everything else; a full producer
/// queue is lag, and lag evicts.
pub const OUTBOUND_CAPACITY: usize = 64;

/// Push actions bound to the caller's own device — a reader is as entitled
/// as a controller (`authorizeAuthenticatedIdentity`).
const DEVICE_BOUND_PUSH: &[&str] = &[
    "push_open_ref",
    "push_policy_get",
    "push_policy_set",
    "push_snooze",
    "push_subscribe",
    "push_test_device",
    "push_unsubscribe",
    "push_viewed_pane",
];

/// Tunables for one connection. Defaults match `internal/transport` —
/// 64 messages / 4 MiB send buffer, 5 s writes, 1 s close handshake.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Per-client encrypted-frame inbox capacity.
    pub inbound_capacity: usize,
    /// Producer→actor channel capacity.
    pub outbound_capacity: usize,
    /// SendBuffer item bound (`clientOutboundMaxItems`).
    pub send_buffer_items: usize,
    /// SendBuffer byte bound (`clientOutboundMaxBytes`).
    pub send_buffer_bytes: usize,
    /// Per-write socket timeout.
    pub send_timeout: Duration,
    /// Graceful-close timeout.
    pub close_timeout: Duration,
    /// Messages pushed right after the session starts — the
    /// `sendConnectionSnapshot` seam. Defaults to a minimal `push_config`.
    pub snapshot: Vec<Outbound>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            inbound_capacity: INBOUND_CAPACITY,
            outbound_capacity: OUTBOUND_CAPACITY,
            send_buffer_items: lerdr_core::sendbuffer::DEFAULT_MAX_ITEMS,
            send_buffer_bytes: lerdr_core::sendbuffer::DEFAULT_MAX_BYTES,
            send_timeout: SEND_TIMEOUT,
            close_timeout: CLOSE_TIMEOUT,
            snapshot: default_snapshot(),
        }
    }
}

/// The minimal post-handshake snapshot: `push_config` with the static
/// capability list and an empty Herdr status (the real snapshot composes
/// inventory/workspaces/activity once those subsystems exist).
pub fn default_snapshot() -> Vec<Outbound> {
    vec![Outbound::PushConfig(Box::new(PushConfig {
        r#type: "push_config".to_owned(),
        protocol: VERSION,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        release_version: env!("CARGO_PKG_VERSION").to_owned(),
        capabilities: MaybeNull::Value(CAPABILITIES.iter().map(|s| s.to_string()).collect()),
        herdr_status: HerdrStatus::default(),
        ..PushConfig::default()
    }))]
}

/// One unit of outbound work — `(data, kind, replaceable)`, the
/// `pushTyped`/`encodeMessage` triple.
#[derive(Debug)]
pub struct OutboundPush {
    pub data: Vec<u8>,
    pub kind: String,
    pub replaceable: bool,
}

impl OutboundPush {
    /// Classify an [`Outbound`] envelope exactly as `encodeMessage` does:
    /// serialize, sniff `type`, `replaceable` iff the type is in the
    /// coalescing set.
    pub fn of(message: &Outbound) -> Self {
        let data = message.encode();
        let kind = sniff_message_type(&data).unwrap_or_default();
        let replaceable = is_replaceable(&kind);
        Self {
            data,
            kind,
            replaceable,
        }
    }
}

/// `messageType(data)` — sniff the envelope `type` without a typed decode.
fn sniff_message_type(data: &[u8]) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Envelope<'a> {
        #[serde(rename = "type", borrow)]
        kind: std::borrow::Cow<'a, str>,
    }
    serde_json::from_slice::<Envelope>(data)
        .ok()
        .map(|e| e.kind.into_owned())
}

/// Why a push onto a client queue was refused.
#[derive(Debug, thiserror::Error)]
pub enum LagError {
    /// The queue is at capacity — the client is lagging.
    #[error("client queue full")]
    Full,
    /// The session is gone.
    #[error("client queue closed")]
    Closed,
}

/// The handle other actors hold to push frames to a connected client —
/// `Hub.Send`/`Hub.Broadcast`'s per-client endpoint. Cloneable and bounded:
/// [`try_push`] is the Go `push` (failure = lag → the caller disconnects the
/// client); [`push`] awaits capacity for producers that prefer backpressure.
///
/// [`try_push`]: ClientSink::try_push
/// [`push`]: ClientSink::push
#[derive(Clone)]
pub struct ClientSink {
    tx: mpsc::Sender<OutboundPush>,
}

impl ClientSink {
    /// Non-blocking push — `Err(LagError::Full)` means lag.
    pub fn try_push(&self, push: OutboundPush) -> Result<(), LagError> {
        self.tx.try_send(push).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => LagError::Full,
            mpsc::error::TrySendError::Closed(_) => LagError::Closed,
        })
    }

    /// Non-blocking push of a typed envelope (`Hub.Send(client, message)`).
    pub fn try_send(&self, message: &Outbound) -> Result<(), LagError> {
        self.try_push(OutboundPush::of(message))
    }

    /// Backpressure push — waits for capacity, `Closed` if the client left.
    pub async fn push(&self, push: OutboundPush) -> Result<(), LagError> {
        self.tx.send(push).await.map_err(|_| LagError::Closed)
    }
}

/// How the transport should go away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CloseMode {
    /// No closing handshake (`conn.CloseNow()`).
    Now,
    /// Send a close frame bounded by `close_timeout` (`conn.Close`).
    Graceful(CloseStatus),
}

/// Why the connection ended, minus the handshake branch — recorded by
/// whichever task detects it first.
#[derive(Debug)]
enum EndKind {
    /// The peer closed cleanly.
    PeerClosed { code: Option<u16>, reason: String },
    /// Lag or a protocol violation evicted the client.
    Evicted(EvictReason),
    /// Transport failure mid-session (read or write side).
    TransportFailed,
    /// Cancellation — the relay is shutting down.
    Shutdown,
}

/// What the supervisor reports and tests assert on.
#[derive(Debug)]
pub enum ConnectionEnd {
    /// The E2EE handshake never completed.
    HandshakeFailed(handshake::HandshakeError),
    /// The peer closed cleanly.
    PeerClosed { code: Option<u16>, reason: String },
    /// Lag or a protocol violation evicted the client.
    Evicted(EvictReason),
    /// Transport failure mid-session (read or write side).
    TransportFailed,
    /// The relay is shutting down.
    Shutdown,
}

/// The eviction taxonomy — `send buffer full`/`write failed`/`encrypted
/// frame rejected` in the Go logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvictReason {
    /// `SendBuffer` push rejected (items/bytes) — lagging client.
    SendBufferFull(RejectReason),
    /// The sealed-frame channel is full — the writer is behind.
    WriterLag,
    /// The inbound inbox is full AND the busy response could not be queued.
    InboundLag,
    /// `session.open` rejected an encrypted frame (`encrypted frame
    /// rejected` → close in Go).
    DecryptFailed,
    /// `session.seal` failed while draining the buffer.
    SealFailed,
    /// Opened plaintext was not a JSON object — Go closes the connection
    /// for this on the encrypted socket.
    MalformedMessage,
}

struct Directive {
    mode: CloseMode,
    /// Close-frame reason — always a static string, never client data.
    reason: &'static str,
    end: EndKind,
}

/// The kill switch shared by the three tasks. First-fire wins: whoever
/// detects the end states the cause; cancellation always propagates.
#[derive(Clone)]
struct Signal {
    token: CancellationToken,
    directive: Arc<OnceLock<Directive>>,
}

impl Signal {
    fn new() -> Self {
        Self {
            token: CancellationToken::new(),
            directive: Arc::new(OnceLock::new()),
        }
    }

    fn fire(&self, mode: CloseMode, reason: &'static str, end: EndKind) {
        let _ = self.directive.set(Directive { mode, reason, end });
        self.token.cancel();
    }

    fn directive(&self) -> Option<&Directive> {
        self.directive.get()
    }
}

/// `SetOnConnect` — fires once the session is registered, handing the
/// server's registry the push endpoint for this client (`Hub.Send` reach).
pub type OnConnect = Box<dyn FnOnce(ClientSink) + Send>;

/// `serve_connection` — one client from upgrade to close: handshake, then
/// the session actor + pumps, all joined before return. Transport-agnostic;
/// `server.rs` feeds it [`WsIo`](crate::ws::WsIo), tests feed it duplexes.
///
/// `on_connect` receives this client's [`ClientSink`] right after the
/// handshake commits — the registry stores it for producers; the sink is
/// dropped (and the session torn down) when this returns.
#[allow(clippy::too_many_arguments)]
pub async fn serve_connection<I, A, R, K>(
    io: I,
    auth: &A,
    keys: &mut K,
    router: R,
    client_id: String,
    config: SessionConfig,
    parent: CancellationToken,
    on_connect: Option<OnConnect>,
) -> ConnectionEnd
where
    I: FrameIo,
    I::Reader: 'static,
    I::Writer: 'static,
    A: DeviceAuthStore + ?Sized,
    R: ActionRouter,
    K: KeySource + ?Sized,
{
    let transport = io.transport();
    let codec = io.codec();
    let (mut reader, mut writer) = io.split();

    let handshake_span = info_span!("handshake", %client_id, transport);
    let outcome = handshake::run(&mut reader, &mut writer, auth, keys, codec)
        .instrument(handshake_span)
        .await;
    let success = match outcome {
        Ok(success) => success,
        Err(err) => {
            if err.is_rejected() {
                // Permanent refusal gets the graceful 4401 so the phone stops
                // retrying dead material (`conn.Close(CloseUnauthorized, …)`).
                writer
                    .close(CloseStatus::Unauthorized, "device authentication rejected")
                    .await;
            } else {
                writer.close_now();
            }
            return ConnectionEnd::HandshakeFailed(err);
        }
    };

    serve_session(
        reader,
        writer,
        success.session,
        success.identity,
        auth,
        router,
        client_id,
        transport,
        config,
        parent,
        on_connect,
    )
    .await
}

/// Run one established session end to end: reader pump + writer pump +
/// actor under `parent` cancellation; every task joins before return.
#[allow(clippy::too_many_arguments)]
async fn serve_session<Rd, Wr, A, R>(
    reader: Rd,
    writer: Wr,
    session: Session,
    identity: AuthenticatedIdentity,
    auth: &A,
    router: R,
    client_id: String,
    transport: &'static str,
    config: SessionConfig,
    parent: CancellationToken,
    on_connect: Option<OnConnect>,
) -> ConnectionEnd
where
    Rd: FrameRead + 'static,
    Wr: FrameWrite + 'static,
    A: DeviceAuthStore + ?Sized,
    R: ActionRouter,
{
    let signal = Signal::new();
    let (inbound_tx, inbound_rx) = mpsc::channel(config.inbound_capacity);
    let (outbound_tx, outbound_rx) = mpsc::channel(config.outbound_capacity);
    // Sealed frames ride a channel sized to one full buffer.
    let (sealed_tx, sealed_rx) = mpsc::channel(config.send_buffer_items);

    // Registration — `onConnect(client)` — before the first frame moves.
    if let Some(hook) = on_connect {
        hook(ClientSink {
            tx: outbound_tx.clone(),
        });
    }

    let mut tasks = JoinSet::new();
    tasks.spawn(
        reader_pump(reader, inbound_tx, outbound_tx.clone(), signal.clone())
            .instrument(info_span!("reader_pump", %client_id, transport)),
    );
    tasks.spawn(
        writer_pump(
            writer,
            sealed_rx,
            signal.clone(),
            config.send_timeout,
            config.close_timeout,
        )
        .instrument(info_span!("writer_pump", %client_id, transport)),
    );

    let actor_span = info_span!(
        "session_actor",
        %client_id,
        transport,
        device_id = %identity.device_id
    );
    let mut actor = Actor {
        session,
        identity,
        auth,
        router,
        buffer: SendBuffer::with_capacity(config.send_buffer_items, config.send_buffer_bytes),
        client_id,
        transport,
        sealed_tx,
        signal: signal.clone(),
        config: &config,
    };

    // The actor runs inline — it IS the supervisor's payload. Producers
    // reach `outbound_rx` through their ClientSink clones.
    actor
        .run(inbound_rx, outbound_rx, parent)
        .instrument(actor_span)
        .await;

    // Whatever ended us already fired the signal; cancel is idempotent.
    signal.token.cancel();
    while let Some(joined) = tasks.join_next().await {
        if let Err(err) = joined {
            warn!(error = %err, "session task failed");
        }
    }

    match signal.directive().map(|d| &d.end) {
        Some(EndKind::PeerClosed { code, reason }) => ConnectionEnd::PeerClosed {
            code: *code,
            reason: reason.clone(),
        },
        Some(EndKind::Evicted(reason)) => ConnectionEnd::Evicted(*reason),
        Some(EndKind::Shutdown) => ConnectionEnd::Shutdown,
        Some(EndKind::TransportFailed) | None => ConnectionEnd::TransportFailed,
    }
}

/// The actor body: owns the sealed session, the send buffer, the router —
/// everything the Go `ClientConn` mutexes protected. Synchronous decisions;
/// the pumps own I/O.
struct Actor<'a, A: DeviceAuthStore + ?Sized, R: ActionRouter> {
    session: Session,
    identity: AuthenticatedIdentity,
    auth: &'a A,
    router: R,
    buffer: SendBuffer,
    client_id: String,
    transport: &'static str,
    sealed_tx: mpsc::Sender<Vec<u8>>,
    signal: Signal,
    config: &'a SessionConfig,
}

/// Loop control — `false` stops the actor.
type Step = bool;
const CONTINUE: Step = true;
const STOP: Step = false;

impl<A: DeviceAuthStore + ?Sized, R: ActionRouter> Actor<'_, A, R> {
    async fn run(
        &mut self,
        mut inbound_rx: mpsc::Receiver<Vec<u8>>,
        mut outbound_rx: mpsc::Receiver<OutboundPush>,
        parent: CancellationToken,
    ) {
        // Post-handshake snapshot — `onConnect(client)` in the Go hub.
        for message in self.config.snapshot.clone() {
            if self.enqueue(message) == STOP {
                return;
            }
        }
        if !self.drain() {
            return;
        }
        let mut outbound_open = true;
        loop {
            let step = tokio::select! {
                biased;
                _ = parent.cancelled() => {
                    self.signal.fire(
                        CloseMode::Graceful(CloseStatus::GoingAway),
                        "server shutting down",
                        EndKind::Shutdown,
                    );
                    STOP
                }
                _ = self.signal.token.cancelled() => STOP,
                raw = inbound_rx.recv() => match raw {
                    None => {
                        // The reader exited without firing — abnormal.
                        self.signal
                            .fire(CloseMode::Now, "", EndKind::TransportFailed);
                        STOP
                    }
                    Some(raw) => self.handle_frame(&raw),
                },
                push = outbound_rx.recv(), if outbound_open => match push {
                    None => { outbound_open = false; CONTINUE }
                    Some(push) => self.handle_push(push),
                },
            };
            if step == CONTINUE {
                if !self.drain() {
                    break;
                }
            } else {
                break;
            }
        }
    }

    /// One inbound ciphertext frame: open → object check → typed decode →
    /// gates → route. Each enqueue failure can end the session.
    fn handle_frame(&mut self, raw: &[u8]) -> Step {
        let plaintext = match self.session.open(raw) {
            Ok(plaintext) => plaintext,
            Err(err) => {
                debug!(error = %err, "encrypted frame rejected, evicting");
                return self.evict(EvictReason::DecryptFailed);
            }
        };
        // Go `decodeWebSocketMessage`: a decrypted payload that is not a JSON
        // object closes an encrypted connection outright.
        let raw_map: serde_json::Map<String, serde_json::Value> =
            match serde_json::from_slice(&plaintext) {
                Ok(map) => map,
                Err(_) => return self.evict(EvictReason::MalformedMessage),
            };
        let inbound = match Inbound::decode_map(&raw_map) {
            Ok(inbound) => inbound,
            Err(err) => {
                debug!(error = %err, "inbound decode failed");
                return self.enqueue(Outbound::Error(decode_failure_response(&raw_map)));
            }
        };
        self.dispatch(inbound)
    }

    /// The `Hub.SetHandler` prologue: catalog lookup, protocol gate,
    /// `server_session_id` fence, authorization — then the router.
    fn dispatch(&mut self, inbound: Inbound) -> Step {
        let Some(scope) = RequestScope::for_message(&inbound) else {
            return self.enqueue(Outbound::Error(error_response(
                &inbound.request_id,
                ApiError::new(
                    error_codes::UNKNOWN_ACTION,
                    BTreeMap::from([(
                        "operation".to_owned(),
                        serde_json::Value::from(inbound.r#type.clone()),
                    )]),
                ),
            )));
        };
        if !compatible(&inbound) {
            return self.enqueue(Outbound::ActionReceipt(incompatible_response(&inbound)));
        }
        // `server_session_id` fence: target and field must agree, and only
        // "primary" is a real destination.
        let mut requested = scope.server_session_id.clone();
        if let Some(target) = &scope.target {
            if !requested.is_empty()
                && !target.server_session_id.is_empty()
                && requested != target.server_session_id
            {
                return self.field_error(&inbound, "server_session_id");
            }
            if requested.is_empty() {
                requested = target.server_session_id.clone();
            }
        }
        if !requested.is_empty() && requested != "primary" {
            return self.field_error(&inbound, "server_session_id");
        }
        if let Some(error) = self.authorize(&scope.action, &inbound.device_id) {
            return self.enqueue(Outbound::Error(error_response(&inbound.request_id, error)));
        }
        let ctx = ClientContext {
            client_id: &self.client_id,
            identity: &self.identity,
            transport: self.transport,
        };
        let reply = self.router.route(&ctx, &scope, &inbound);
        for message in reply.outbound {
            if self.enqueue(message) == STOP {
                return STOP;
            }
        }
        CONTINUE
    }

    fn field_error(&mut self, inbound: &Inbound, field: &'static str) -> Step {
        self.enqueue(Outbound::Error(error_response(
            &inbound.request_id,
            ApiError::new(
                error_codes::INVALID_REQUEST,
                BTreeMap::from([("field".to_owned(), serde_json::Value::from(field))]),
            ),
        )))
    }

    /// `authorizeDeviceAction` + `authorizeAuthenticatedIdentity`: the
    /// credential must still be current (rotated/revoked credentials die
    /// mid-session), then role gates apply.
    fn authorize(&self, action: &ActionMetadata, device_id: &str) -> Option<ApiError> {
        let current = self.auth.authorize(
            &self.identity.credential_id,
            self.identity.credential_version,
        );
        let role = match current {
            Some(credential) if credential.device_id == self.identity.device_id => credential.role,
            _ => {
                return Some(ApiError::new(
                    error_codes::READER_DENIED,
                    BTreeMap::from([
                        (
                            "operation".to_owned(),
                            serde_json::Value::from(action.operation),
                        ),
                        (
                            "reason".to_owned(),
                            serde_json::Value::from("credential_revoked"),
                        ),
                    ]),
                ));
            }
        };
        if DEVICE_BOUND_PUSH.contains(&action.operation) {
            // Device-bound push operations act on the caller's own
            // subscription — any authenticated device may use them.
            return if self.identity.device_id.is_empty() {
                Some(reader_denied(action.operation))
            } else {
                None
            };
        }
        if action.class == ActionClass::ReadOnly || role == Role::Controller {
            return None;
        }
        if action.operation == "revoke_device"
            && !device_id.is_empty()
            && device_id == self.identity.device_id
        {
            return None;
        }
        Some(reader_denied(action.operation))
    }

    /// `Hub.Send` → `push` → evict-on-reject.
    fn enqueue(&mut self, message: Outbound) -> Step {
        let push = OutboundPush::of(&message);
        self.handle_push(push)
    }

    /// One buffer push (both the response path and the `ClientSink` inbox).
    fn handle_push(&mut self, push: OutboundPush) -> Step {
        match self
            .buffer
            .push_typed(push.data, push.kind, push.replaceable)
        {
            PushResult::Rejected(reason) => {
                warn!(?reason, "send buffer rejected push, evicting client");
                self.evict(EvictReason::SendBufferFull(reason))
            }
            PushResult::Queued | PushResult::Coalesced => CONTINUE,
        }
    }

    /// Drain the plaintext buffer into sealed frames toward the writer pump.
    /// A full sealed channel means the writer is behind — lag evicts.
    fn drain(&mut self) -> Step {
        while let Some(plaintext) = self.buffer.pop() {
            let frame = match self.session.seal(&plaintext) {
                Ok(frame) => frame,
                Err(err) => {
                    warn!(error = %err, "encrypt failed, evicting");
                    return self.evict(EvictReason::SealFailed);
                }
            };
            if self.sealed_tx.try_send(frame).is_err() {
                warn!("sealed channel blocked, evicting lagging client");
                return self.evict(EvictReason::WriterLag);
            }
        }
        CONTINUE
    }

    /// Lag/violation exit: Go ends evicted connections with a normal close
    /// (`removeClient` → `Close(CloseNormal)`).
    fn evict(&mut self, reason: EvictReason) -> Step {
        warn!(?reason, client_id = %self.client_id, "evicting client");
        self.signal.fire(
            CloseMode::Graceful(CloseStatus::Normal),
            "",
            EndKind::Evicted(reason),
        );
        STOP
    }
}

fn reader_denied(operation: &str) -> ApiError {
    ApiError::new(
        error_codes::READER_DENIED,
        BTreeMap::from([("operation".to_owned(), serde_json::Value::from(operation))]),
    )
}

/// The `submitMessage` busy response — the queue-overflow reply Go sends
/// when ordered ingress rejects a message. The request_id/action fields
/// Go echoes are inside the still-encrypted frame, so they stay empty here.
fn busy_response() -> OutboundPush {
    OutboundPush::of(&Outbound::CommandResult(CommandResultMessage {
        ok: Some(false),
        phase: Some("not_started".to_owned()),
        error: Some("Relay is busy; command was not sent".to_owned()),
        r#type: "command_result".to_owned(),
        ..CommandResultMessage::default()
    }))
}

/// `readPump` — frames off the socket into the actor's bounded inbox.
/// Exiting fires the kill signal with the observed cause.
async fn reader_pump<Rd: FrameRead>(
    mut reader: Rd,
    inbound_tx: mpsc::Sender<Vec<u8>>,
    outbound_tx: mpsc::Sender<OutboundPush>,
    signal: Signal,
) {
    loop {
        let frame = tokio::select! {
            biased;
            _ = signal.token.cancelled() => return,
            frame = reader.read_frame() => frame,
        };
        match frame {
            Ok(raw) => match inbound_tx.try_send(raw) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    // Inbox full: Go submits a busy `command_result` to the
                    // client's buffer; if even that can't queue, the client
                    // is lagging and gets evicted.
                    if outbound_tx.try_send(busy_response()).is_err() {
                        signal.fire(
                            CloseMode::Graceful(CloseStatus::Normal),
                            "",
                            EndKind::Evicted(EvictReason::InboundLag),
                        );
                        return;
                    }
                }
                Err(mpsc::error::TrySendError::Closed(_)) => return,
            },
            Err(ReadError::Closed { code, reason }) => {
                signal.fire(
                    CloseMode::Graceful(CloseStatus::Normal),
                    "",
                    EndKind::PeerClosed { code, reason },
                );
                return;
            }
            Err(err) => {
                debug!(error = %err, "read error, dropping connection");
                signal.fire(CloseMode::Now, "", EndKind::TransportFailed);
                return;
            }
        }
    }
}

/// `writePump` — sealed frames onto the socket, bounded per write, and the
/// closing handshake on the way out.
async fn writer_pump<Wr: FrameWrite>(
    mut writer: Wr,
    mut sealed_rx: mpsc::Receiver<Vec<u8>>,
    signal: Signal,
    send_timeout: Duration,
    close_timeout: Duration,
) {
    loop {
        let frame = tokio::select! {
            biased;
            _ = signal.token.cancelled() => break,
            frame = sealed_rx.recv() => frame,
        };
        let Some(frame) = frame else { break };
        let wrote = tokio::time::timeout(send_timeout, writer.write_frame(&frame)).await;
        match wrote {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                debug!(error = %err, "write failed, evicting");
                signal.fire(CloseMode::Now, "", EndKind::TransportFailed);
                break;
            }
            Err(_) => {
                debug!("write timed out, evicting");
                signal.fire(CloseMode::Now, "", EndKind::TransportFailed);
                break;
            }
        }
    }
    // Apply the recorded close. Graceful close is bounded; on timeout the
    // transport dies when the halves drop anyway.
    match signal.directive() {
        Some(directive) => match directive.mode {
            CloseMode::Now => writer.close_now(),
            CloseMode::Graceful(status) => {
                let _ = tokio::time::timeout(close_timeout, writer.close(status, directive.reason))
                    .await;
            }
        },
        None => writer.close_now(),
    }
}
