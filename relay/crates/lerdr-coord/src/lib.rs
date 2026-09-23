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
pub mod conversation;
mod fingerprint;
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

/// The relay's release version — the one honest answer every surface
/// (`push_config.version`, `update_status`, `version` subcommand,
/// `support-state.json`) shares. Precedence:
///
/// 1. `LERDR_VERSION` — stamped by the release pipeline at build time (the
///    oracle's `main.version` ldflags slot; manifests key on it).
/// 2. `CARGO_PKG_VERSION` — a real crate version once release PRs bump the
///    workspace.
/// 3. `0.0.0-dev` — the workspace ships `version = "0.0.0"` placeholders
///    today; reporting it bare would claim a `0.0.0` *release* that never
///    existed, so dev builds get a semver-valid pre-release marker instead.
pub fn release_version() -> &'static str {
    const PKG: &str = env!("CARGO_PKG_VERSION");
    if let Some(stamped) = option_env!("LERDR_VERSION").filter(|v| !v.is_empty()) {
        return stamped;
    }
    if PKG == "0.0.0" {
        "0.0.0-dev"
    } else {
        PKG
    }
}
