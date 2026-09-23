//! Async Rust client for Herdr's local socket API — NDJSON over a Unix socket
//! (Windows named pipe slotting in behind the [`Transport`] trait later).
//!
//! Wire rules, per `docs/08-herdr-boundary.md` and the Go client in the
//! Lerdr reference repo (`internal/herdr`):
//!
//! - **One request per connection.** No pooling, no reuse; Herdr closes the
//!   socket after each unary response. `events.subscribe` holds its
//!   connection open for the stream.
//! - **Every request carries a fresh id** (`lerdr-api-N`; `lerdr-events` for
//!   subscriptions) and the response must echo it.
//! - **Every failure preserves the dispatch boundary**: [`HerdrError`]
//!   distinguishes `NotStarted` (safe to retry), `Refused` (Herdr answered
//!   `error`, definitively not applied) and `DispatchedUnknown` (may have
//!   applied — surfaces must not silently claim failure).
//! - **Reads are singleflighted**; waits and mutations are not.
//! - **Event recovery is a resync, not a replay**: resubscribe →
//!   `subscription_started` → `session.snapshot` → treat events as
//!   invalidation signals ([`Client::supervise_events`] runs that loop).
//!
//! ```no_run
//! use lerdr_herdr::{Client, ReadSource, ReadFormat};
//!
//! # async fn example() -> Result<(), lerdr_herdr::HerdrError> {
//! let client = Client::from_env().expect("HERDR_SOCKET_PATH or default path");
//! let snapshot = client.session_snapshot().await?;
//! let read = client
//!     .pane_read("wA:tA:pA", ReadSource::Visible, 50, ReadFormat::Text)
//!     .await?;
//! # Ok(())
//! # }
//! ```

pub mod capabilities;
mod cli;
mod client;
mod error;
mod events;
pub mod schema;
mod singleflight;
mod transport;
mod types;
mod view;
mod wire;

pub use capabilities::{CapabilityReport, FeatureEvidence, FeatureState};
pub use client::{Client, ClientConfig};
pub use error::{
    BootstrapError, DispatchPhase, EventStreamError, HerdrError, SubscribeError,
    KNOWN_REFUSAL_CODES, TRANSIENT_REFUSAL_CODES,
};
pub use events::{
    canonical_event_name, topology_subscriptions, wire_event_name, Backoff, Bootstrap, Event,
    EventStream, EventSupervisor, Subscription, SupervisorSignal, SupervisorStream,
    EVENTS_REQUEST_ID,
};
pub use schema::{SchemaError, SchemaRegistry, SchemaSource};
pub use transport::{default_socket_path, BoxIo, Io, Transport, UnixTransport};
pub use types::*;
pub use view::{
    assert_agent_view, lerdr_agent_view, ViewAssertOutcome, AGENT_VIEW_LABEL, AGENT_VIEW_SOURCE,
};
pub use wire::{DEFAULT_REQUEST_TIMEOUT, MAX_LINE_BYTES};
