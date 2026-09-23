//! `lerdr-relay` — the Lerdr relay binary's first slice: the axum
//! `/ws` + `/health`/`/healthz`/`/readyz` server, the `herdr-e2ee-v2`
//! handshake driver, the device auth boundary, and the per-client
//! session actor.
//!
//! Layering (state-machine + driver split — sync cores, async edges):
//!
//! - [`frame`]: the `FrameConn` port — logical-frame duplex traits plus the
//!   in-process `duplex` transport tests run over.
//! - [`ws`]: `FrameIo` over an axum WebSocket (text frames only, per the Go
//!   `requireText` contract).
//! - [`auth`]: the `DeviceAuthStore` boundary and error taxonomy.
//! - [`store`]: `MemoryAuthStore` + `FileAuthStore` (`devices.json`).
//! - [`handshake`]: the 10-second four-frame driver around
//!   [`lerdr_e2ee::handshake::ServerHandshake`].
//! - [`session`]: `SessionActor`, `ClientSink`, bounded queues, eviction.
//! - [`router`]: the `ActionRouter` seam and `StubRouter`.
//! - [`server`]: `Relay` — axum wiring, subprotocol gate, lifecycle.
//!
//! Wire contract: `protocol v3` / `herdr-e2ee-v2`, frozen — golden vectors
//! under `fixtures/` are the oracle.

pub mod auth;
pub mod frame;
pub mod handshake;
pub mod router;
pub mod server;
pub mod session;
pub mod store;
pub mod ws;

pub use server::Relay;
