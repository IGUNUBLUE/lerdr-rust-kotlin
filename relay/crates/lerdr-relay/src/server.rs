//! The axum server — `Hub` + `HandleWebSocket` in the Go oracle.
//!
//! Routes: `GET /ws` (upgraded, `herdr-e2ee-v2` subprotocol mandatory) and
//! the probe endpoints `GET /health`, `GET /healthz`, `GET /readyz`
//! (`server.go:1258-1260`). No web assets — the product is Android-only
//! (`docs/02-architecture.md`), so `/healthz` omits the oracle's
//! `bundle_*` keys it only emits when a web handler is installed.
//!
//! Lifecycle: every connection's session runs under a child
//! [`CancellationToken`]; [`Relay::shutdown`] cascades to all of them, the
//! writer pumps emit `GoingAway` closes, and the [`TaskTracker`] join bounds
//! teardown. New handshakes are refused once shutdown starts (the axum
//! listener stops accepting; in-flight upgrades complete or die with it).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use lerdr_core::protocol::{
    InventoryStatusMessage, Outbound, ENCRYPTED_WEBSOCKET_SUBPROTOCOL, VERSION,
};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{debug, info, info_span, warn, Instrument};

use crate::auth::DeviceAuthStore;
use crate::handshake::OsKeySource;
use crate::router::{ActionRouter, StubRouter};
use crate::session::{
    serve_connection, ClientRegistration, ClientSink, ConnectionEnd, DisconnectCredentials,
    OnConnect, SessionConfig,
};
use crate::ws::WsIo;

/// `wsMaxReadBytes` — the largest WS message the relay reads (21 MiB).
const WS_MAX_READ_BYTES: usize = 21 * 1024 * 1024;

/// A router factory — one router per connection (routers may hold
/// per-client state; [`StubRouter`] doesn't, but the seam allows it).
pub type RouterFactory = Arc<dyn Fn() -> Box<dyn ActionRouter> + Send + Sync>;

/// `s.state.InventoryStatus` as a probe — one shared-cell read per
/// request. The coordinator wires in a closure over its topology
/// `watch::Receiver`, so health handlers see the live committed
/// inventory without calling into coordinator internals or touching
/// disk.
#[derive(Clone)]
pub struct InventoryProbeFn(pub Arc<dyn Fn() -> InventoryStatusMessage + Send + Sync>);

/// The health/readiness surface the probe handlers report —
/// `s.cfg.InstanceID` plus the `s.version`/`s.revision` binary stamps.
/// Installed once at construction via [`Relay::with_health`].
#[derive(Clone)]
pub struct HealthProbe {
    /// `cfg.InstanceID` — the `X-Herdr-Relay-Instance` header and the
    /// `instance` healthz key.
    pub instance_id: String,
    /// `s.version` — emitted as both `version` and `release_version`.
    pub version: String,
    /// `s.revision` — the build's commit stamp.
    pub revision: String,
    /// `s.state.InventoryStatus` — evaluated per request. `None` reports
    /// the oracle's zero-value map (`state: "starting"`).
    pub inventory: Option<InventoryProbeFn>,
}

impl Default for HealthProbe {
    fn default() -> Self {
        Self {
            instance_id: String::new(),
            version: lerdr_core::release_version().to_owned(),
            revision: option_env!("LERDR_REVISION").unwrap_or("dev").to_owned(),
            inventory: None,
        }
    }
}

/// The relay server. Cloneable handle — clones share the registry and the
/// shutdown token.
#[derive(Clone)]
pub struct Relay {
    shared: Arc<Shared>,
}

struct Shared {
    auth: Arc<dyn DeviceAuthStore>,
    make_router: RouterFactory,
    config: SessionConfig,
    health: HealthProbe,
    /// `s.ready` — set once `serve` starts accepting; never cleared, like
    /// the oracle's one-way latch under `s.mu`.
    serving: AtomicBool,
    shutdown: CancellationToken,
    /// `hub.clients` + `hub.blocked` under the `register`/`mu` pair — one
    /// mutex serializes registration against the `DisconnectCredential`
    /// sweep like the oracle's lock ordering does.
    registry: Mutex<Registry>,
    next_client_id: AtomicU64,
    tracker: TaskTracker,
}

/// The live-session registry — `hub.clients` plus the credential index.
#[derive(Default)]
struct Registry {
    /// `client-N` → push endpoint, credential binding, kill switch.
    clients: HashMap<String, ClientRegistration>,
    /// `credential_id → through_version` — `hub.blocked`: once a
    /// revocation sweep runs, sessions authenticating at or below the
    /// fenced version never register (the completion-to-registration
    /// race the deferred disconnect leaves open, `ws.go:211-217`).
    blocked: HashMap<String, u64>,
}

impl Relay {
    /// A relay authenticating against `auth`, stub-routing actions.
    pub fn new(auth: Arc<dyn DeviceAuthStore>) -> Self {
        Self::with_router_factory(auth, || Box::new(StubRouter::new()))
    }

    /// Supply the real routing factory (later slices).
    pub fn with_router_factory(
        auth: Arc<dyn DeviceAuthStore>,
        factory: impl Fn() -> Box<dyn ActionRouter> + Send + Sync + 'static,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                auth,
                make_router: Arc::new(factory),
                config: SessionConfig::default(),
                health: HealthProbe::default(),
                serving: AtomicBool::new(false),
                shutdown: CancellationToken::new(),
                registry: Mutex::new(Registry::default()),
                next_client_id: AtomicU64::new(0),
                tracker: TaskTracker::new(),
            }),
        }
    }

    /// `Arc::get_mut` fails once the router is shared — rebuild instead,
    /// carrying over the pieces a builder call must not reset (the
    /// shutdown token chains existing clones; `serving` stays off until
    /// the rebuilt relay actually serves).
    fn rebuild(&self, config: SessionConfig, health: HealthProbe) -> Self {
        Self {
            shared: Arc::new(Shared {
                auth: self.shared.auth.clone(),
                make_router: self.shared.make_router.clone(),
                config,
                health,
                serving: AtomicBool::new(false),
                shutdown: self.shared.shutdown.clone(),
                registry: Mutex::new(Registry::default()),
                next_client_id: AtomicU64::new(0),
                tracker: TaskTracker::new(),
            }),
        }
    }

    /// Override per-session tunables (tests shrink the buffers).
    pub fn with_session_config(self, config: SessionConfig) -> Self {
        self.rebuild(config, self.shared.health.clone())
    }

    /// Install the probe-endpoint surface — `InstanceID` and the live
    /// inventory probe the coordinator wires to its topology watch cell.
    pub fn with_health(self, health: HealthProbe) -> Self {
        self.rebuild(self.shared.config.clone(), health)
    }

    /// The token that stops every session and the accept loop.
    pub fn shutdown(&self) -> CancellationToken {
        self.shared.shutdown.clone()
    }

    /// `hub.clients` — live push endpoints, keyed by `client-N`.
    pub fn client_sink(&self, client_id: &str) -> Option<ClientSink> {
        self.shared
            .registry
            .lock()
            .expect("registry poisoned")
            .clients
            .get(client_id)
            .map(|registration| registration.sink.clone())
    }

    /// `Metrics.ConnectedClients`.
    pub fn connected_clients(&self) -> usize {
        self.shared
            .registry
            .lock()
            .expect("registry poisoned")
            .clients
            .len()
    }

    /// `hub.broadcast` — push a frame to every live client. Sinks that
    /// have fallen behind are skipped; the session's send-buffer contract
    /// already evicts them.
    pub fn broadcast(&self, message: &Outbound) {
        self.broadcast_except(message, "");
    }

    /// Broadcast to every live client except `exclude` — used when the
    /// requester already carries the frame in its own response (the
    /// oracle's `broadcastToAll` + per-client response ordering).
    pub fn broadcast_except(&self, message: &Outbound, exclude: &str) {
        let registry = self.shared.registry.lock().expect("registry poisoned");
        for (id, registration) in registry.clients.iter() {
            if id == exclude {
                continue;
            }
            let _ = registration.sink.try_send(message);
        }
    }

    /// The axum router — `mux.HandleFunc("GET /ws"/"/health"/"/healthz"/
    /// "/readyz")`. Exposed so tests can compose it into their own
    /// servers.
    pub fn router(&self) -> Router {
        Router::new()
            .route("/ws", get(ws_upgrade))
            .route("/health", get(health))
            .route("/healthz", get(healthz))
            .route("/readyz", get(readyz))
            .with_state(self.shared.clone())
    }

    /// Bind and serve until [`shutdown`](Self::shutdown) fires. Connection
    /// tasks drain first (GoingAway closes), then the tracker joins them.
    pub async fn serve(&self, listener: TcpListener) -> std::io::Result<()> {
        let shutdown = self.shared.shutdown.clone();
        let tracker = self.shared.tracker.clone();
        // `s.ready = true` — one-way latch once the bound listener is in
        // axum's hands (the oracle sets it right after Listen succeeds).
        self.shared.serving.store(true, Ordering::Relaxed);
        axum::serve(
            listener,
            self.router()
                .into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move { shutdown.cancelled().await })
        .await?;
        // The accept loop has stopped; sessions got the cascade. Wait out
        // the pumps' close handshakes.
        tracker.close();
        tracker.wait().await;
        Ok(())
    }
}

impl Shared {
    /// `s.disconnectCredentials` → `hub.DisconnectCredential` per pair
    /// (`ws.go:612-634`). For each `(credential_id, through_version)` the
    /// fence lands in `blocked` first — registrations at or below it are
    /// refused from now on — then every live session bound to the
    /// credential at or below `through_version` gets the
    /// `GoingAway`/"device credential revoked" close. The requester is
    /// skipped: its own deferred self-close stands.
    fn disconnect_credentials(&self, requester: &str, pairs: &[(String, u64)]) {
        let mut registry = self.registry.lock().expect("registry poisoned");
        for (credential_id, through_version) in pairs {
            if credential_id.is_empty() || *through_version == 0 {
                continue;
            }
            let fence = registry.blocked.entry(credential_id.clone()).or_insert(0);
            *fence = (*fence).max(*through_version);
            for (client_id, registration) in registry.clients.iter() {
                if client_id == requester {
                    continue;
                }
                let identity = &registration.identity;
                if identity.credential_id == *credential_id
                    && identity.credential_version <= *through_version
                {
                    registration.close_credential_revoked();
                }
            }
        }
    }
}

/// `X-Herdr-Relay-Instance` — the oracle's literal header name.
const INSTANCE_HEADER: header::HeaderName =
    header::HeaderName::from_static("x-herdr-relay-instance");

/// `handleHealth` — plain-text liveness plus the instance header.
async fn health(State(shared): State<Arc<Shared>>) -> Response {
    // A control-character-bearing instance id can't be a header value —
    // emit an empty one rather than fail the liveness probe over it.
    let instance = HeaderValue::from_str(&shared.health.instance_id)
        .unwrap_or_else(|_| HeaderValue::from_static(""));
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            ),
            (INSTANCE_HEADER, instance),
        ],
        "ok\n",
    )
        .into_response()
}

/// `handleHealthz` — full status JSON: readiness derives from the serve
/// latch plus the live inventory state (`ready`/`error` → `ready`/
/// `degraded`, anything else → `starting`). `bundle_*` keys are omitted:
/// the oracle emits them only when a web-bundle handler is installed and
/// this relay serves no web bundle.
async fn healthz(State(shared): State<Arc<Shared>>) -> Response {
    let inventory = inventory_map(&shared.health);
    let readiness = readiness_label(
        shared.serving.load(Ordering::Relaxed),
        inventory.get("state").and_then(serde_json::Value::as_str),
    );
    json_response(
        StatusCode::OK,
        &serde_json::json!({
            "status": "ok",
            "readiness": readiness,
            "inventory": inventory,
            "instance": shared.health.instance_id,
            "version": shared.health.version,
            "release_version": shared.health.version,
            "revision": shared.health.revision,
            "protocol": VERSION,
        }),
    )
}

/// `handleReadyz` — 200/`ready` only while serving AND inventory is
/// ready; otherwise 503/`unavailable`.
async fn readyz(State(shared): State<Arc<Shared>>) -> Response {
    let inventory = inventory_map(&shared.health);
    let ready = shared.serving.load(Ordering::Relaxed)
        && inventory.get("state").and_then(serde_json::Value::as_str) == Some("ready");
    let (status, code) = if ready {
        ("ready", StatusCode::OK)
    } else {
        ("unavailable", StatusCode::SERVICE_UNAVAILABLE)
    };
    json_response(
        code,
        &serde_json::json!({
            "status": status,
            "inventory": inventory,
        }),
    )
}

/// `s.ready` + `inventory.state` → the healthz `readiness` string.
fn readiness_label(serving: bool, inventory_state: Option<&str>) -> &'static str {
    if !serving {
        return "starting";
    }
    match inventory_state {
        Some("ready") => "ready",
        Some("error") => "degraded",
        _ => "starting",
    }
}

/// `s.state.InventoryStatus()` minus the `message` key — the oracle
/// strips the free-text detail from HTTP responses (`delete(inventory,
/// "message")`) but keeps every other key unconditionally.
fn inventory_map(health: &HealthProbe) -> serde_json::Map<String, serde_json::Value> {
    let mut map = match &health.inventory {
        Some(probe) => match serde_json::to_value((probe.0)()) {
            Ok(serde_json::Value::Object(map)) => map,
            _ => serde_json::Map::new(),
        },
        None => serde_json::Map::new(),
    };
    // `type` is the wire discriminator — not part of the oracle's map.
    map.remove("type");
    map.remove("message");
    // `inventoryStatusLocked` emits all six keys unconditionally; an
    // absent probe (or a field the projection left unset) still reports
    // the zero value rather than dropping the key.
    for (key, default) in [
        ("state", serde_json::json!("starting")),
        ("error_code", serde_json::json!("")),
        ("last_attempt_at", serde_json::json!(0)),
        ("last_success_at", serde_json::json!(0)),
        ("stale", serde_json::json!(false)),
    ] {
        map.entry(key.to_owned()).or_insert(default);
    }
    map
}

/// `json.NewEncoder(w).Encode(resp)` — compact object + trailing newline.
fn json_response(code: StatusCode, value: &serde_json::Value) -> Response {
    let mut body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    body.push(b'\n');
    (code, [(header::CONTENT_TYPE, "application/json")], body).into_response()
}

/// `HandleWebSocket` + `webSocketUpgradeAllowed`: the encrypted socket
/// requires the `herdr-e2ee-v2` subprotocol and forbids `Authorization` /
/// `?token` (token-mode auth lives inside the handshake, never in headers).
async fn ws_upgrade(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    uri: Uri,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let token_in_query = uri.query().is_some_and(|q| {
        q.split('&')
            .any(|pair| pair == "token" || pair.starts_with("token="))
    });
    if headers.contains_key(header::AUTHORIZATION) || token_in_query {
        return (
            StatusCode::BAD_REQUEST,
            "Encrypted WebSocket handshake required\n",
        )
            .into_response();
    }
    let offered = headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|list| {
            list.split(',')
                .any(|p| p.trim() == ENCRYPTED_WEBSOCKET_SUBPROTOCOL)
        });
    if !offered {
        return (
            StatusCode::BAD_REQUEST,
            "Encrypted WebSocket handshake required\n",
        )
            .into_response();
    }
    ws.protocols([ENCRYPTED_WEBSOCKET_SUBPROTOCOL])
        .max_message_size(WS_MAX_READ_BYTES)
        .on_upgrade(move |socket| handle_socket(shared, socket, addr))
        .into_response()
}

/// One upgraded socket → one tracked session task.
async fn handle_socket(shared: Arc<Shared>, socket: WebSocket, addr: SocketAddr) {
    if shared.shutdown.is_cancelled() {
        // `h.closing` — refuse connections arriving during shutdown.
        return;
    }
    let id = shared.next_client_id.fetch_add(1, Ordering::Relaxed) + 1;
    let client_id = format!("client-{id}");
    let parent = shared.shutdown.child_token();
    let span = info_span!("connection", %client_id, %addr);
    info!(parent: &span, "client connected");

    let auth = Arc::clone(&shared.auth);
    let make_router = Arc::clone(&shared.make_router);
    let mut config = shared.config.clone();
    let shared_for_task = Arc::clone(&shared);
    let shared_for_register = Arc::clone(&shared);
    // `s.disconnectCredentials` — the session actor reaches the registry
    // back through this hook when `revoke_device`/`reset_devices` lands;
    // the sweep runs on the actor's 250 ms deferral.
    let shared_for_disconnect = Arc::clone(&shared);
    config.disconnect_credentials =
        Some(DisconnectCredentials(Arc::new(move |requester, pairs| {
            shared_for_disconnect.disconnect_credentials(requester, pairs)
        })));
    let registered_id = client_id.clone();
    shared.tracker.spawn(async move {
        let register: OnConnect = Box::new(move |registration| {
            let mut registry = shared_for_register
                .registry
                .lock()
                .expect("registry poisoned");
            // `identity.CredentialVersion <= blockedVersion` — a session
            // whose handshake committed before the sweep but registers
            // after it is refused outright (`conn.CloseNow()`).
            let blocked = registry
                .blocked
                .get(&registration.identity.credential_id)
                .copied()
                .unwrap_or(0);
            if !registration.identity.credential_id.is_empty()
                && registration.identity.credential_version <= blocked
            {
                registration.close_now();
                return;
            }
            registry.clients.insert(registered_id.clone(), registration);
        });
        let end = serve_connection(
            WsIo::new(socket),
            &*auth,
            &mut OsKeySource,
            (make_router)(),
            client_id.clone(),
            config,
            parent,
            Some(register),
        )
        .instrument(span)
        .await;
        shared_for_task
            .registry
            .lock()
            .expect("registry poisoned")
            .clients
            .remove(&client_id);
        match &end {
            ConnectionEnd::HandshakeFailed(e) if e.peer_closed() => {
                debug!(%client_id, "peer left during handshake")
            }
            ConnectionEnd::HandshakeFailed(e) => {
                debug!(%client_id, error = %e, "encrypted handshake failed")
            }
            ConnectionEnd::Evicted(reason) => {
                warn!(%client_id, ?reason, "client evicted")
            }
            ConnectionEnd::PeerClosed { .. } => {
                debug!(%client_id, "client disconnected")
            }
            ConnectionEnd::TransportFailed => {
                debug!(%client_id, "transport failed")
            }
            ConnectionEnd::Shutdown => {
                debug!(%client_id, "session ended by shutdown")
            }
        }
    });
}
