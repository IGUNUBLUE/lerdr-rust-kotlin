//! `herdr-e2ee-v2` — the Lerdr wire E2EE stack (`docs/03-protocol.md` §1).
//!
//! Byte-parity oracle: `internal/transport/e2ee.go` in `IGUNUBLUE/lerdr`
//! (Go reference) and the golden vectors under `fixtures/crypto/`. The wire
//! format is frozen; every byte-level deviation is a bug.
//!
//! Layers:
//! - [`handshake`]: client/server hello codecs, `\x00`-joined binding,
//!   transcript proofs (HMAC-SHA256), ECDH + HKDF session-key derivation.
//! - [`Session`]: per-direction AES-256-GCM channels with monotonic BE64
//!   sequences, `c2s`/`s2c` direction-bound AAD.
//! - [`Codec`]: the two wire frame envelopes (JSON text frames, 10-byte-header
//!   binary frames).

mod codec;
mod error;
mod json;
mod session;

pub mod handshake;

pub use codec::Codec;
pub use error::{E2eeError, ErrorClass};
pub use session::{aad, frame_nonce, Direction, Session, MAX_SEQUENCE};

/// Negotiated E2EE version carried by hellos and frame envelopes.
pub const VERSION: u8 = 2;
