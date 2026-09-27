//! Fixture conformance for the `crypto.*` suites (`fixtures/crypto/`).
//!
//! Every vector in every suite is replayed: handshake transcripts and proofs,
//! frame seal/open in both codecs and both directions, and the failure
//! taxonomy.

mod failures;
mod frames;
mod handshake;
