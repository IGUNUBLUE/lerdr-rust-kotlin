//! `lerdr-coord` — the coordinator layer between [`lerdr_relay`] sessions and
//! the Herdr socket.
//!
//! Topology (doc 02 actor model):
//!
//! ```text
//!            ┌─────────────────── Herdr unix socket ───────────────────┐
//!            │  events.subscribe (topology)   pane.read/send_input/…  │
//!            └───────▲──────────────────────────────────────▲─────────┘
//!                    │ SupervisorSignal                    │ calls
//!            ┌───────┴───────────┐                  ┌───────┴─────────┐
//!            │  TopologyActor    │                  │  per-action     │
//!            │  (one per relay)  │                  │  spawned tasks  │
//!            └───────┬───────────┘                  └───────▲─────────┘
//!                    │ watch::Sender<Topology>              │ ClientSink
//!                    │ broadcast<Event> (pane.* invalidations)
//!            ┌───────▼─────────────────────────────────────┴─────────┐
//!            │  HerdRouter — one per client session (ActionRouter)   │
//!            └──────────────────────────────────────────────────────┘
//! ```
//!
//! Contracts honored here:
//!
//! - **Invalidation semantics** (doc 08): snapshot replaces on
//!   [`SupervisorSignal::Synced`]; events are never applied as payloads —
//!   topology events trigger a fresh `session.snapshot`, pane events are
//!   forwarded to watch tasks as "re-read" signals.
//! - **Dispatch-boundary taxonomy**: `HerdrError` phases map onto
//!   `ActionReceipt.phase` — `NotStarted` → `failed_before_dispatch`,
//!   `DispatchedUnknown` → `dispatched_unknown`, `Refused` → `confirmed`
//!   carrying the refusal error.
//! - **Ack gate** (`pane_watch.go` parity): one unacked pane frame per
//!   (client, pane); `pane_applied` clears it; 4s timeout resets the gate so
//!   the next invalidation re-sends a full `ack_required` frame.
//! - **Fingerprint**: `content_fingerprint` = `hex(sha256(content)[0..8])`
//!   over content bytes only (doc 10, round-3 finding).

mod actions;
mod actor;
mod classify;
pub mod conversation;
mod fingerprint;
mod history;
mod router;
mod snapshot;
mod topology;
mod watches;

pub use actor::Invalidation;
pub use actor::{TopologyActor, TopologyHandle};
pub use fingerprint::content_fingerprint;
pub use router::{ClientSinkLookup, HerdRouter, HerdRouterFactory};
pub use snapshot::compose_snapshot;
pub use topology::Topology;
pub use watches::{pane_unchanged, WatchCtl, WatchSet, ACK_TIMEOUT, DEFAULT_LINES};

/// The relay's release version — re-exported from `lerdr-core`, which
/// owns the `LERDR_VERSION` → `CARGO_PKG_VERSION` → `0.0.0-dev` chain.
pub use lerdr_core::release_version;
