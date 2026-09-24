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
pub mod framezstd;
pub mod json;
pub mod lease;
pub mod protocol;
pub mod sendbuffer;
pub mod uploadbinary;

/// The relay's release version — the one honest answer every surface
/// (`push_config.version`, `update_status`, `version` subcommand,
/// `support-state.json`) shares. Lives in `-core` so the session layer
/// (`lerdr-relay`) can report it without a dependency cycle. Precedence:
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
