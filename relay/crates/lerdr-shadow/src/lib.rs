//! `lerdr-shadow` — the Phase-3 shadow-diff parity harness.
//!
//! Two binaries share this library:
//!
//! - `lerdr-shadow` — a scripted `herdr-e2ee-v2` client (`run`) that plays a
//!   JSON scenario against one relay and records every inbound plaintext as a
//!   JSONL *trace*, plus the `diff` subcommand that normalizes two traces and
//!   reports semantic differences.
//! - `lerdr-fake-herdr` — a minimal Herdr socket-API endpoint backed by a
//!   static scenario file, so both relays see identical `session.snapshot`,
//!   `*.list`, `pane.read`, `pane.send_input`, `worktree.list`, and
//!   `events.subscribe` answers.
//!
//! The Python driver under `tools/shadow/` orchestrates the processes; this
//! crate owns the protocol surface.

pub mod client;
pub mod compare;
pub mod fake;
pub mod normalize;
pub mod scenario;
pub mod trace;

/// Harness error type — one `thiserror` over io/serde/ws/e2ee failures.
#[derive(Debug, thiserror::Error)]
pub enum ShadowError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("e2ee: {0}")]
    E2ee(#[from] lerdr_e2ee::E2eeError),
    #[error("websocket: {0}")]
    Ws(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("{0}")]
    Msg(String),
}

impl ShadowError {
    pub fn msg(text: impl Into<String>) -> Self {
        Self::Msg(text.into())
    }
}

pub type Result<T> = std::result::Result<T, ShadowError>;
