//! The axum server — `Hub` + `HandleWebSocket` in the Go oracle.
//!
//! Routes: `GET /ws` (upgraded, `herdr-e2ee-v2` subprotocol mandatory) and
//! `GET /healthz`. No web assets — the product is Android-only
//! (`docs/02-architecture.md`).
//!
//! Lifecycle: every connection's session runs under a child
//! [`CancellationToken`]; [`Relay::shutdown`] cascades to all of them, the
//! writer pumps emit `GoingAway` closes, and the [`TaskTracker`] join bounds
//! teardown. New handshakes are refused once shutdown starts (the axum
//! listener stops accepting; in-flight upgrades complete or die with it).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use lerdr_core::protocol::{Outbound, ENCRYPTED_WEBSOCKET_SUBPROTOCOL};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{debug, info, info_span, warn, Instrument};

use crate::auth::DeviceAuthStore;
use crate::handshake::OsKeySource;
use crate::router::{ActionRouter, StubRouter};
use crate::session::{serve_connection, ClientSink, ConnectionEnd, OnConnect, SessionConfig};
use crate::ws::WsIo;

/// `wsMaxReadBytes` — the largest WS message the relay reads (21 MiB).
const WS_MAX_READ_BYTES: usize = 21 * 1024 * 1024;

/// A router factory — one router per connection (routers may hold
/// per-client state; [`StubRouter`] doesn't, but the seam allows it).
pub type RouterFactory = Arc<dyn Fn() -> Box<dyn ActionRouter> + Send + Sync>;

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
    shutdown: CancellationToken,
    /// `hub.clients` — live push endpoints by `client-N`.
    clients: Mutex<HashMap<String, ClientSink>>,
    next_client_id: AtomicU64,
    tracker: TaskTracker,
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
                shutdown: CancellationToken::new(),
                clients: Mutex::new(HashMap::new()),
                next_client_id: AtomicU64::new(0),
                tracker: TaskTracker::new(),
            }),
        }
    }

    /// Override per-session tunables (tests shrink the buffers).
    pub fn with_session_config(self, config: SessionConfig) -> Self {
        // `Arc::get_mut` fails once the router is shared — rebuild instead.
        let shared = Arc::new(Shared {
            auth: self.shared.auth.clone(),
            make_router: self.shared.make_router.clone(),
            config,
            shutdown: self.shared.shutdown.clone(),
            clients: Mutex::new(HashMap::new()),
            next_client_id: AtomicU64::new(0),
            tracker: TaskTracker::new(),
        });
        Self { shared }
    }

    /// The token that stops every session and the accept loop.
    pub fn shutdown(&self) -> CancellationToken {
        self.shared.shutdown.clone()
    }

    /// `hub.clients` — live push endpoints, keyed by `client-N`.
    pub fn client_sink(&self, client_id: &str) -> Option<ClientSink> {
        self.shared
            .clients
            .lock()
            .expect("clients poisoned")
            .get(client_id)
            .cloned()
    }

    /// `Metrics.ConnectedClients`.
    pub fn connected_clients(&self) -> usize {
        self.shared.clients.lock().expect("clients poisoned").len()
    }

    /// `hub.broadcast` — push a frame to every live client. Sinks that
    /// have fallen behind are skipped; the session's send-buffer contract
    /// already evicts them.
    pub fn broadcast(&self, message: &Outbound) {
        let clients = self.shared.clients.lock().expect("clients poisoned");
        for sink in clients.values() {
            let _ = sink.try_send(message);
        }
    }

    /// The axum router — `Router::new().route("/ws", …).route("/healthz", …)`.
    /// Exposed so tests can compose it into their own servers.
    pub fn router(&self) -> Router {
        Router::new()
            .route("/ws", get(ws_upgrade))
            .route("/healthz", get(healthz))
            .with_state(self.shared.clone())
    }

    /// Bind and serve until [`shutdown`](Self::shutdown) fires. Connection
    /// tasks drain first (GoingAway closes), then the tracker joins them.
    pub async fn serve(&self, listener: TcpListener) -> std::io::Result<()> {
        let shutdown = self.shared.shutdown.clone();
        let tracker = self.shared.tracker.clone();
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

/// `GET /healthz` — liveness probe.
async fn healthz() -> &'static str {
    "ok\n"
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
    let config = shared.config.clone();
    let shared_for_task = Arc::clone(&shared);
    let shared_for_register = Arc::clone(&shared);
    let registered_id = client_id.clone();
    shared.tracker.spawn(async move {
        let register: OnConnect = Box::new(move |sink| {
            shared_for_register
                .clients
                .lock()
                .expect("clients poisoned")
                .insert(registered_id.clone(), sink);
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
            .clients
            .lock()
            .expect("clients poisoned")
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
