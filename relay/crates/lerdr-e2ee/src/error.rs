//! Error taxonomy for the e2ee layer.
//!
//! `Display` strings reproduce the Go reference's `errors.New` text verbatim —
//! `crypto.failures` vectors pin them via `go_error`. [`E2eeError::class`]
//! maps each variant onto the suite's `expected_error` vocabulary
//! (`format` / `replay` / `seq` / `auth`).

/// Failure classes of the `crypto.failures` suite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// The envelope could not be decoded or is not an e2ee frame.
    Format,
    /// A well-formed frame below the receiver's next expected sequence.
    Replay,
    /// Sequence gap or a sequence past the 2^53-1 ceiling.
    Seq,
    /// GCM tag verification (or handshake proof) failed.
    Auth,
}

impl ErrorClass {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorClass::Format => "format",
            ErrorClass::Replay => "replay",
            ErrorClass::Seq => "seq",
            ErrorClass::Auth => "auth",
        }
    }
}

impl std::fmt::Display for ErrorClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Errors of the e2ee handshake and session layer.
///
/// Frame-layer messages match `internal/transport/e2ee.go` verbatim.
#[derive(Debug, thiserror::Error)]
pub enum E2eeError {
    // --- frame envelope decode ---
    #[error("invalid encrypted frame")]
    InvalidFrame,
    #[error("unsupported encrypted frame")]
    UnsupportedFrame,
    #[error("invalid encrypted frame ciphertext")]
    InvalidCiphertext,

    // --- sequence / authentication ---
    /// `sequence != expected` or `sequence > MAX_SEQUENCE`. The fixture splits
    /// this Go error into `replay` (received < expected) and `seq` (received
    /// >= expected); see [`E2eeError::class`].
    #[error("invalid encrypted frame sequence")]
    InvalidSequence { expected: u64, received: u64 },
    #[error("encrypted send sequence exhausted")]
    SequenceExhausted,
    #[error("encrypted frame authentication failed")]
    Authentication,

    // --- client hello (server side) ---
    #[error("invalid client hello")]
    InvalidClientHello,
    #[error("unsupported client hello")]
    UnsupportedClientHello,
    #[error("invalid client authentication selector")]
    InvalidAuthSelector,
    #[error("invalid client nonce")]
    InvalidClientNonce,
    #[error("invalid client public key")]
    InvalidClientPublicKey,
    #[error("invalid client proof")]
    InvalidClientProof,

    // --- server hello / finishes (client side + tests) ---
    #[error("invalid server hello")]
    InvalidServerHello,
    #[error("unsupported server hello")]
    UnsupportedServerHello,
    #[error("invalid server nonce")]
    InvalidServerNonce,
    #[error("invalid server public key")]
    InvalidServerPublicKey,
    #[error("invalid server proof")]
    InvalidServerProof,
    #[error("invalid client finish")]
    InvalidClientFinish,
    #[error("invalid server finish")]
    InvalidServerFinish,

    // --- proof / key material ---
    #[error("client proof did not authenticate")]
    ClientProofFailed,
    #[error("invalid key material")]
    InvalidKeyMaterial,
    #[error("derive shared secret")]
    EcdhFailed,
}

impl E2eeError {
    /// The `crypto.failures` error class.
    ///
    /// The suite only exercises frame-layer errors; handshake structural
    /// failures classify as `format` and proof failure as `auth`.
    pub fn class(&self) -> ErrorClass {
        use E2eeError::*;
        match self {
            InvalidFrame | UnsupportedFrame | InvalidCiphertext => ErrorClass::Format,
            InvalidSequence { expected, received } if received < expected => ErrorClass::Replay,
            InvalidSequence { .. } | SequenceExhausted => ErrorClass::Seq,
            Authentication | ClientProofFailed => ErrorClass::Auth,
            InvalidClientHello
            | UnsupportedClientHello
            | InvalidAuthSelector
            | InvalidClientNonce
            | InvalidClientPublicKey
            | InvalidClientProof
            | InvalidServerHello
            | UnsupportedServerHello
            | InvalidServerNonce
            | InvalidServerPublicKey
            | InvalidServerProof
            | InvalidClientFinish
            | InvalidServerFinish
            | InvalidKeyMaterial
            | EcdhFailed => ErrorClass::Format,
        }
    }
}
