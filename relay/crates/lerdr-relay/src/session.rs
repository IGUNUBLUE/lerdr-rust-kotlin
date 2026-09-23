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

use lerdr_core::audit;
use lerdr_core::json::{MaybeNull, RawJson};
use lerdr_core::protocol::{
    action_receipt_response, compatible, decode_failure_response, error_codes, error_response,
    incompatible_response, ActionClass, ActionMetadata, ActionReceipt, ActionReceiptPhase,
    ApiError, CommandResultMessage, HerdrStatus, Inbound, Outbound, PushConfig, RequestScope,
    CAPABILITIES, VERSION,
};
use lerdr_core::sendbuffer::{is_replaceable, PushResult, RejectReason, SendBuffer};
use lerdr_e2ee::Session;
use serde::Serialize;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info_span, warn, Instrument};

use crate::auth::{
    AuthError, AuthenticatedIdentity, BootstrapRearm, Credential, DeviceAuthStore,
    IssuedInvitation, Role,
};
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

/// `time.AfterFunc(250*time.Millisecond, …)` — the grace between answering a
/// `revoke_device`/`reset_devices` and closing the revoked connection, so
/// the `command_result` reaches the wire first (`server.go:816, 846`).
const REVOKED_DISCONNECT_DELAY: Duration = Duration::from_millis(250);

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
    /// When set, supersedes `snapshot`: evaluated per connection so
    /// late-joining clients get current state (lerdr-coord wires the
    /// topology projection in here).
    pub snapshot_fn: Option<SnapshotFn>,
    /// The bootstrap re-arm for `reset_devices` — the oracle's
    /// `ResetWithBootstrap([]byte(cfg.Token), s.hostname, …)` inputs: the
    /// relay key keeps the printed setup link pairing after the wipe.
    /// `None` on a tokenless relay resets to a pristine, unpaired store
    /// (the next `arm_invitation`/`ensure_pairing` — SIGUSR1 or restart —
    /// mints a fresh one).
    pub reset_bootstrap: Option<BootstrapRearm>,
    /// `s.disconnectCredentials` reach — after a successful
    /// `revoke_device`/`reset_devices` answers (and the 250 ms deferral
    /// elapses), the actor hands the affected `(credential_id,
    /// through_version)` fences here so the server can close every OTHER
    /// live session bound to them (`ws.go:612`). The [`Relay`] wires its
    /// own registry in per connection, overwriting whatever is set here;
    /// `None` — a bare `serve_connection` with no registry — leaves peers
    /// to the lazy `authorize` fence.
    ///
    /// [`Relay`]: crate::server::Relay
    pub disconnect_credentials: Option<DisconnectCredentials>,
    /// `s.auditLog` — the secret-safe remote-write audit the oracle opens
    /// with `audit.Open(cfg.CacheDir)` (`server.go:576`). When set, every
    /// `Audited` action writes an `attempt` row at admission and a
    /// `result` row per `command_result` — including the hub-owned
    /// device-admin replies the router never sees. `None` disables the
    /// log (`s.auditLog == nil` → `recordWriteAudit` no-ops).
    pub audit: Option<AuditHook>,
}

/// The write-audit hook — `Arc`-wrapped so `SessionConfig` stays
/// `Clone + Debug`. `attribution` is the `d.state.Agent(paneID)` lookup;
/// lerdr-coord supplies the topology projection, a bare relay passes
/// `None` and records go out attribution-free.
#[derive(Clone)]
pub struct AuditHook {
    /// The shared append-only log (process-wide — one file).
    pub log: Arc<audit::AuditLog>,
    /// `d.state.Agent(paneID)` — pane → agent/project/session/host.
    pub attribution: Option<Arc<AttributionFn>>,
}

/// Pane-attribution lookup for audit records — `d.state.Agent(paneID)`
/// projected onto our topology snapshot. Missing panes yield the empty
/// attribution.
pub type AttributionFn = dyn Fn(&str) -> audit::Attribution + Send + Sync;

impl std::fmt::Debug for AuditHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AuditHook(..)")
    }
}

impl AuditHook {
    /// The `d.state.Agent(paneID)` read — empty attribution when no lookup
    /// is wired or the pane is unknown.
    fn attribution(&self, pane_id: &str) -> audit::Attribution {
        self.attribution
            .as_ref()
            .map(|lookup| lookup(pane_id))
            .unwrap_or_default()
    }

    /// `recordWriteAudit` — one append, warn-and-continue on failure
    /// (`server.go:3084`: a failed audit write never fails the request).
    fn append(&self, record: audit::Record) {
        if let Err(error) = self.log.append(record) {
            warn!(%error, "remote write audit append failed");
        }
    }
}

/// Per-connection snapshot builder — wraps `Arc<dyn Fn>` so
/// `SessionConfig` stays `Clone + Debug`.
#[derive(Clone)]
pub struct SnapshotFn(pub std::sync::Arc<dyn Fn() -> Vec<Outbound> + Send + Sync>);

impl std::fmt::Debug for SnapshotFn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SnapshotFn(..)")
    }
}

impl SnapshotFn {
    pub fn build(&self) -> Vec<Outbound> {
        (self.0)()
    }
}

/// The peer-disconnect callable — `(requester_client_id, fences)` where
/// each fence is `(credential_id, through_version)`.
pub type DisconnectFn = dyn Fn(&str, &[(String, u64)]) + Send + Sync;

/// The peer-disconnect hook — `s.disconnectCredentials`. Arguments are the
/// requester's `client_id` (skipped — the requester keeps its own deferred
/// self-close) and the `(credential_id, through_version)` fences, one per
/// destroyed credential: the tombstone's post-bump `credential.Version`
/// for `revoke_device`, the pre-reset versions for `reset_devices`
/// (`server.go:817`, `server.go:846-848`).
#[derive(Clone)]
pub struct DisconnectCredentials(pub std::sync::Arc<DisconnectFn>);

impl std::fmt::Debug for DisconnectCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DisconnectCredentials(..)")
    }
}

impl DisconnectCredentials {
    /// Fire the sweep — `hub.DisconnectCredential` per pair.
    pub fn disconnect(&self, requester: &str, pairs: &[(String, u64)]) {
        (self.0)(requester, pairs)
    }
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
            snapshot_fn: None,
            reset_bootstrap: None,
            disconnect_credentials: None,
            audit: None,
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
        version: lerdr_core::release_version().to_owned(),
        release_version: lerdr_core::release_version().to_owned(),
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
    /// `revoke_device`/`reset_devices` destroyed this connection's own
    /// credential — `DisconnectCredential`, deferred past the response.
    CredentialRevoked,
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

/// What the registry records per session — the `onConnect(client)`
/// payload. Beyond the push endpoint it carries the committed identity
/// (the credential→session index key) and the session's kill switch, so
/// `DisconnectCredential` can close every peer bound to a destroyed
/// credential — and the `blocked` fence can refuse a registration that
/// lands behind the sweep.
pub struct ClientRegistration {
    /// `Hub.Send` reach.
    pub sink: ClientSink,
    /// The committed handshake identity — `client.identity` in Go.
    pub identity: AuthenticatedIdentity,
    signal: Signal,
}

impl ClientRegistration {
    /// `conn.Close(CloseGoingAway, "device credential revoked")` — the
    /// close a `DisconnectCredential` peer sees (`ws.go:630`).
    pub fn close_credential_revoked(&self) {
        self.signal.fire(
            CloseMode::Graceful(CloseStatus::GoingAway),
            "device credential revoked",
            EndKind::Evicted(EvictReason::CredentialRevoked),
        );
    }

    /// `conn.CloseNow()` — a registration that arrives at or below the
    /// `blocked` fence drops without a close frame (`ws.go:212-217`).
    pub fn close_now(&self) {
        self.signal.fire(
            CloseMode::Now,
            "",
            EndKind::Evicted(EvictReason::CredentialRevoked),
        );
    }
}

/// `SetOnConnect` — fires once the session is registered, handing the
/// server's registry this client's [`ClientRegistration`].
pub type OnConnect = Box<dyn FnOnce(ClientRegistration) + Send>;

/// `serve_connection` — one client from upgrade to close: handshake, then
/// the session actor + pumps, all joined before return. Transport-agnostic;
/// `server.rs` feeds it [`WsIo`](crate::ws::WsIo), tests feed it duplexes.
///
/// `on_connect` receives this client's [`ClientRegistration`] right after
/// the handshake commits — the registry stores it for producers and the
/// credential→session index; it is dropped (and the session torn down)
/// when this returns.
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
    // The registry gets the push endpoint, the credential binding the
    // `DisconnectCredential` sweep matches on, and the kill switch.
    if let Some(hook) = on_connect {
        hook(ClientRegistration {
            sink: ClientSink {
                tx: outbound_tx.clone(),
            },
            identity: identity.clone(),
            signal: signal.clone(),
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
        revoked_at: None,
        pending_disconnects: Vec::new(),
        self_disconnect: false,
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
    /// `DisconnectCredential` deferred past the response flush
    /// (`time.AfterFunc(250ms)` in the oracle) — armed by a successful
    /// `revoke_device`/`reset_devices`; peers bound to the destroyed
    /// credentials are swept through `config.disconnect_credentials` and
    /// this session closes too when `self_disconnect` is set.
    revoked_at: Option<Instant>,
    /// `(credential_id, through_version)` fences awaiting the deferred
    /// sweep — `disconnectCredentials`' argument list.
    pending_disconnects: Vec<(String, u64)>,
    /// This connection's own credential is among `pending_disconnects`.
    self_disconnect: bool,
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
        let snapshot = match &self.config.snapshot_fn {
            Some(f) => f.build(),
            None => self.config.snapshot.clone(),
        };
        for message in snapshot {
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
                // `time.AfterFunc(250ms, disconnectCredentials)` — the
                // requester gets its answer, then the doors: peers bound
                // to the destroyed credentials are swept through the
                // registry, this session closes when its own credential
                // was among them.
                _ = async {
                    match self.revoked_at {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => self.disconnect_revoked(),
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
        self.dispatch(inbound, &raw_map)
    }

    /// The `Hub.SetHandler` prologue: catalog lookup, protocol gate,
    /// `server_session_id` fence, authorization — then the router.
    fn dispatch(
        &mut self,
        inbound: Inbound,
        raw_map: &serde_json::Map<String, serde_json::Value>,
    ) -> Step {
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
        // `validateExactPaneTarget` (server.go:676) — after the
        // `server_session_id` fence and authorization, before
        // `recordWriteAudit`: a rejected target writes no audit row.
        if let Some(error) = self.router.validate_pane_target(&inbound) {
            return self.enqueue(Outbound::Error(error_response(&inbound.request_id, error)));
        }
        // `recordWriteAudit(client, msg, nil)` — audited writes log an
        // `attempt` row at admission, before the action switch
        // (`server.go:683-685`). The raw map carries fields `Inbound`
        // drops; `send_secret` degrades to shape-only inside
        // `write_details`.
        let audit_ctx = match &self.config.audit {
            Some(hook) if audit::is_audited(scope.action.operation) => {
                let ctx = audit::RequestContext::from_message(raw_map, &self.client_id);
                let attribution = hook.attribution(&ctx.pane_id);
                hook.append(audit::attempt_record(&ctx, raw_map, attribution));
                Some((ctx, hook.clone()))
            }
            _ => None,
        };
        // Device administration is hub-owned in the oracle — the
        // `s.deviceAuth.*` arms of the action switch (`server.go:757-853`)
        // — so it resolves straight out of the auth store here; the router
        // never sees it.
        if let Some(reply) = self.device_admin(&scope, &inbound) {
            // `sendAuditedCommandResult` — admin `command_result`s audit
            // too, even though they never reach the router.
            if let Some((ctx, hook)) = &audit_ctx {
                for message in &reply {
                    if let Outbound::CommandResult(result) = message {
                        let attribution = hook.attribution(&ctx.pane_id);
                        hook.append(audit::result_record(ctx, result, attribution));
                    }
                }
            }
            for message in reply {
                if self.enqueue(message) == STOP {
                    return STOP;
                }
            }
            return CONTINUE;
        }
        let ctx = ClientContext {
            client_id: &self.client_id,
            identity: &self.identity,
            transport: self.transport,
        };
        let reply = self.router.route(&ctx, &scope, &inbound);
        // Synchronously-emitted `command_result`s audit here; lerdr-coord's
        // spawned handlers append their own result rows per frame.
        if let Some((ctx, hook)) = &audit_ctx {
            for message in &reply.outbound {
                if let Outbound::CommandResult(result) = message {
                    let attribution = hook.attribution(&ctx.pane_id);
                    hook.append(audit::result_record(ctx, result, attribution));
                }
            }
        }
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

    /// The device-admin actions — the `s.deviceAuth.*` arms of the oracle's
    /// action switch (`server.go:757-853`) — answered straight out of the
    /// auth store. `None` for every other action, which the router sees.
    ///
    /// Success answers `command_result` first (the correlated request
    /// resolves on its payload) then the terminal `action_receipt` at
    /// `confirmed` — lerdr-coord's "result message first, receipt last"
    /// ordering. A store refusal answers the `failed` `command_result`
    /// alone, exactly the oracle's reply shape.
    fn device_admin(&mut self, scope: &RequestScope, inbound: &Inbound) -> Option<Vec<Outbound>> {
        let outcome = match scope.action.operation {
            "device_list" => self.admin_list(),
            "create_device_invitation" => self.admin_create_invitation(inbound),
            "rename_device" => self.admin_rename(inbound),
            "revoke_device" => self.admin_revoke(inbound),
            "reset_devices" => self.admin_reset(),
            _ => return None,
        };
        let mut outbound = Vec::with_capacity(2);
        match outcome.result {
            Ok(data) => {
                outbound.push(command_result(
                    &inbound.request_id,
                    scope.action.operation,
                    true,
                    "",
                    data,
                ));
                outbound.push(Outbound::ActionReceipt(action_receipt_response(
                    &inbound.request_id,
                    ActionReceipt {
                        action_id: scope.action_id.clone(),
                        phase: ActionReceiptPhase::from(ActionReceiptPhase::CONFIRMED),
                        error: None,
                    },
                )));
                // `time.AfterFunc(250ms, disconnectCredentials)` — the
                // response frames above flush first; at the deadline the
                // peers bound to the destroyed credentials are swept and
                // this session closes when its own credential was among
                // them. Back-to-back destructive actions merge into the
                // earliest pending deadline.
                if outcome.self_disconnect || !outcome.disconnects.is_empty() {
                    self.pending_disconnects.extend(outcome.disconnects);
                    self.self_disconnect |= outcome.self_disconnect;
                    let at = Instant::now() + REVOKED_DISCONNECT_DELAY;
                    self.revoked_at = Some(self.revoked_at.map_or(at, |armed| armed.min(at)));
                }
            }
            Err(error) => {
                outbound.push(command_result(
                    &inbound.request_id,
                    scope.action.operation,
                    false,
                    &error,
                    None,
                ));
            }
        }
        Some(outbound)
    }

    /// `device_list` — `activeDeviceCredentials` (tombstones filtered,
    /// `current` marked on the caller's own credential) plus `device_id` and
    /// `role` from the session identity — `client.Identity()` in the oracle,
    /// which the `authorizeDeviceAction` refresh never reaches (it mutates a
    /// copy).
    fn admin_list(&self) -> AdminOutcome {
        let credentials = match self.auth.list_devices() {
            Ok(credentials) => credentials,
            Err(error) => return AdminOutcome::failed(error.to_string()),
        };
        let role = self.identity.role;
        let devices = credentials
            .iter()
            .filter(|c| !c.revoked)
            .map(|c| device_wire(c, c.credential_id == self.identity.credential_id))
            .collect();
        AdminOutcome::ok(DeviceListData {
            current_device_id: self.identity.device_id.clone(),
            devices,
            role,
        })
    }

    /// `create_device_invitation` — `CreateInvitation(inbound.Name,
    /// Role(inbound.Role), identity.Locale)`: name and role arrive raw from
    /// the wire for the store's `validateMetadata`; the locale is the
    /// caller's, not the message's.
    fn admin_create_invitation(&self, inbound: &Inbound) -> AdminOutcome {
        match self
            .auth
            .create_invitation(&inbound.name, &inbound.role, &self.identity.locale)
        {
            Ok(invitation) => AdminOutcome::ok(InvitationData {
                invitation: invitation_wire(&invitation),
            }),
            Err(error) => AdminOutcome::failed(error.to_string()),
        }
    }

    /// `rename_device` — `deviceCredentialID` resolves the wire `device_id`
    /// then `RenameCredential` validates and persists the name.
    fn admin_rename(&self, inbound: &Inbound) -> AdminOutcome {
        match self.credential_id_for(&inbound.device_id) {
            Err(error) => AdminOutcome::failed(error.to_string()),
            Ok(None) => AdminOutcome::failed("Device credential was not found"),
            Ok(Some(credential_id)) => {
                match self.auth.rename_device(&credential_id, &inbound.name) {
                    Ok(credential) => AdminOutcome::ok(DeviceData {
                        device: device_wire(&credential, false),
                    }),
                    Err(error) => AdminOutcome::failed(error.to_string()),
                }
            }
        }
    }

    /// `revoke_device` — same resolution; on success the oracle drops
    /// every session holding the revoked credential, deferred past the
    /// response (`time.AfterFunc(250ms, DisconnectCredential)`). The
    /// tombstone's post-bump version is the sweep's `through_version`
    /// fence (`ws.go:617-624`).
    fn admin_revoke(&self, inbound: &Inbound) -> AdminOutcome {
        match self.credential_id_for(&inbound.device_id) {
            Err(error) => AdminOutcome::failed(error.to_string()),
            Ok(None) => AdminOutcome::failed("Device credential was not found"),
            Ok(Some(credential_id)) => match self.auth.revoke_device(&credential_id) {
                Ok(credential) => {
                    let self_disconnect = credential.credential_id == self.identity.credential_id;
                    let mut outcome = AdminOutcome::ok(DeviceData {
                        device: device_wire(&credential, false),
                    });
                    outcome.disconnects =
                        vec![(credential.credential_id.clone(), credential.version)];
                    if self_disconnect {
                        outcome = outcome.disconnecting();
                    }
                    outcome
                }
                Err(error) => AdminOutcome::failed(error.to_string()),
            },
        }
    }

    /// `reset_devices` — `ResetWithBootstrap(token, hostname, locale)`:
    /// every credential and the invitation die in one swap. The
    /// disconnect set is `activeDeviceCredentials` captured BEFORE the
    /// wipe, each at its pre-reset version (`server.go:830-848`). This
    /// connection's own credential is among them, so success always ends
    /// the session (deferred — the answer must reach the wire first).
    fn admin_reset(&self) -> AdminOutcome {
        let active = match self.auth.list_devices() {
            Ok(credentials) => credentials,
            Err(error) => return AdminOutcome::failed(error.to_string()),
        };
        match self
            .auth
            .reset_devices(self.config.reset_bootstrap.as_ref(), &self.identity.locale)
        {
            Ok(()) => {
                let mut outcome = AdminOutcome::ok_empty().disconnecting();
                outcome.disconnects = active
                    .into_iter()
                    .filter(|c| !c.revoked)
                    .map(|c| (c.credential_id, c.version))
                    .collect();
                outcome
            }
            Err(error) => AdminOutcome::failed(error.to_string()),
        }
    }

    /// `deviceCredentialID` — a `device_id` resolves to a `credential_id`
    /// over every record, tombstones included; blank finds nothing.
    fn credential_id_for(&self, device_id: &str) -> Result<Option<String>, AuthError> {
        if device_id.trim().is_empty() {
            return Ok(None);
        }
        Ok(self
            .auth
            .list_devices()?
            .into_iter()
            .find(|c| c.device_id == device_id)
            .map(|c| c.credential_id))
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

    /// The deferred `disconnectCredentials` sweep (`ws.go:612-634`): every
    /// OTHER session bound to a destroyed credential gets the
    /// `GoingAway`/"device credential revoked" close through the registry
    /// hook; this session follows when its own credential was among them
    /// (the oracle's sweep includes the requester — the deferral is what
    /// protects its response). A peer-only sweep leaves this session
    /// running.
    fn disconnect_revoked(&mut self) -> Step {
        self.revoked_at = None;
        let pairs = std::mem::take(&mut self.pending_disconnects);
        if let Some(hook) = &self.config.disconnect_credentials {
            hook.disconnect(&self.client_id, &pairs);
        }
        if !self.self_disconnect {
            return CONTINUE;
        }
        self.self_disconnect = false;
        self.signal.fire(
            CloseMode::Graceful(CloseStatus::GoingAway),
            "device credential revoked",
            EndKind::Evicted(EvictReason::CredentialRevoked),
        );
        STOP
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

// ---------------------------------------------------------------------------
// Device administration — the local `s.deviceAuth.*` answers.
// ---------------------------------------------------------------------------

/// One device-admin answer: the `command_result` payload (or the refusal
/// text), the credential→version fences `disconnectCredentials` sweeps at
/// the deferred deadline, and whether success destroyed this connection's
/// own credential — `DisconnectCredential`'s deferred close.
struct AdminOutcome {
    result: Result<Option<RawJson>, String>,
    /// `(credential_id, through_version)` — one per credential the action
    /// destroyed; the peer sweep disconnects sessions at or below.
    disconnects: Vec<(String, u64)>,
    self_disconnect: bool,
}

impl AdminOutcome {
    fn ok(data: impl Serialize) -> Self {
        Self {
            result: Ok(Some(raw_json(&data))),
            disconnects: Vec::new(),
            self_disconnect: false,
        }
    }

    fn ok_empty() -> Self {
        Self {
            result: Ok(None),
            disconnects: Vec::new(),
            self_disconnect: false,
        }
    }

    fn failed(error: impl Into<String>) -> Self {
        Self {
            result: Err(error.into()),
            disconnects: Vec::new(),
            self_disconnect: false,
        }
    }

    /// The action wiped this session's credential — answer, then close.
    fn disconnecting(mut self) -> Self {
        self.self_disconnect = true;
        self
    }
}

/// `commandResultMessage` (`server.go:2977`) — the flat map with `error`/
/// `pane_id` always present and `data` only when the command produced one.
/// `phase` is `completed`/`failed` (fixture `command-result-ok` pins it).
fn command_result(
    request_id: &str,
    action: &str,
    ok: bool,
    error: &str,
    data: Option<RawJson>,
) -> Outbound {
    Outbound::CommandResult(CommandResultMessage {
        action: Some(action.to_owned()),
        data: data.map(MaybeNull::Value),
        error: Some(error.to_owned()),
        ok: Some(ok),
        pane_id: Some(String::new()),
        phase: Some(if ok { "completed" } else { "failed" }.to_owned()),
        request_id: Some(request_id.to_owned()),
        r#type: "command_result".to_owned(),
    })
}

fn raw_json(data: &impl Serialize) -> RawJson {
    let text = serde_json::to_string(data).expect("wire DTO serialization cannot fail");
    RawJson(serde_json::value::RawValue::from_string(text).expect("DTO is valid JSON"))
}

/// `deviceauth.Credential` — wire form, Go struct order. `paired_at`/
/// `last_seen_at` are `time.Time` values (RFC3339; `omitempty` is a no-op on
/// a struct, so an unset one emits the zero time); `current` is
/// `,omitempty` — only the caller's own row in `device_list` carries it.
#[derive(Serialize)]
struct DeviceWire {
    device_id: String,
    credential_id: String,
    name: String,
    role: Role,
    locale: String,
    paired_at: String,
    last_seen_at: String,
    version: u64,
    revoked: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    current: bool,
}

fn device_wire(credential: &Credential, current: bool) -> DeviceWire {
    DeviceWire {
        device_id: credential.device_id.clone(),
        credential_id: credential.credential_id.clone(),
        name: credential.name.clone(),
        role: credential.role,
        locale: credential.locale.clone(),
        paired_at: wire_time(credential.paired_at_ms),
        last_seen_at: wire_time(credential.last_seen_at_ms),
        version: credential.version,
        revoked: credential.revoked,
        current,
    }
}

/// `deviceauth.Invitation` — wire form, Go struct order. `secret` is the
/// QR/link material (`invitation_secret` in the fixture families) — the one
/// place it is allowed on the wire.
#[derive(Serialize)]
struct InvitationWire {
    invitation_id: String,
    version: u64,
    secret: String,
    expires_at: String,
    name: String,
    role: Role,
    locale: String,
}

fn invitation_wire(invitation: &IssuedInvitation) -> InvitationWire {
    InvitationWire {
        invitation_id: invitation.invitation_id.clone(),
        version: invitation.version,
        secret: invitation.secret.clone(),
        expires_at: wire_time(invitation.expires_at_ms),
        name: invitation.name.clone(),
        role: invitation.role,
        locale: invitation.locale.clone(),
    }
}

/// `map[string]any{"devices":…,"current_device_id":…,"role":…}` — Go emits
/// map keys sorted, so the fields declare in that order.
#[derive(Serialize)]
struct DeviceListData {
    current_device_id: String,
    devices: Vec<DeviceWire>,
    role: Role,
}

#[derive(Serialize)]
struct DeviceData {
    device: DeviceWire,
}

#[derive(Serialize)]
struct InvitationData {
    invitation: InvitationWire,
}

/// Unix milliseconds → RFC3339Nano (`time.Time.MarshalJSON`): the fraction
/// prints only when nonzero, trailing zeros trimmed. `0` — the unset
/// sentinel — renders Go's zero time (`0001-01-01T00:00:00Z`).
fn wire_time(ms: i64) -> String {
    if ms == 0 {
        return "0001-01-01T00:00:00Z".to_owned();
    }
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    // civil_from_days (Hinnant): days since 1970-01-01 → civil y/m/d.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    if mo <= 2 {
        y += 1;
    }
    let fraction = match millis {
        0 => String::new(),
        n if n % 100 == 0 => format!(".{}", n / 100),
        n if n % 10 == 0 => format!(".{:02}", n / 10),
        n => format!(".{n:03}"),
    };
    format!(
        "{y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}{fraction}Z",
        tod / 3600,
        tod % 3600 / 60,
        tod % 60
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
