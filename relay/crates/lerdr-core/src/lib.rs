//! `lerdr-core` — the wire-semantics crate for the Lerdr relay.
//!
//! Pure, synchronous ports of the Go oracle's wire-facing decision logic:
//!
//! - [`protocol`]: inbound action envelopes + outbound message DTOs,
//!   byte-exact with `encoding/json` (frozen `protocol v3` /
//!   `herdr-e2ee-v2` contract).
//! - [`delta`]: the pane delta codec (`Build`/`Apply`/`Efficient`).
//! - [`sendbuffer`]: the per-client bounded outbound queue with tail
//!   coalescing.
//! - [`lease`]: pane-size lease arbitration with an injected clock.
//! - [`json`]: Go-compatible JSON formatting shared by all of the above.
//! - [`audit`]: the secret-safe remote-write audit log (`internal/audit`).
//!
//! Golden vectors under `fixtures/` are the oracle; conformance tests live
//! in `tests/`.

pub mod audit;
pub mod delta;
pub mod json;
pub mod lease;
pub mod protocol;
pub mod sendbuffer;
