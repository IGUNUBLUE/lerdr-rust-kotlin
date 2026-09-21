//! `paneFingerprint` parity — `internal/app/server.go`.
//!
//! ```text
//! hex( sha256(utf8(content)) [0..8] )  →  16 lowercase hex chars
//! ```
//!
//! Scope: content bytes only — line budget and format are not bound (doc 10).

use sha2::{Digest, Sha256};

/// The wire fingerprint for one pane frame's content.
pub fn content_fingerprint(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    hex::encode(&digest[..8])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oracle_vectors() {
        assert_eq!(content_fingerprint(""), "e3b0c44298fc1c14");
        assert_eq!(content_fingerprint("a\nb\n"), "911169ddaaf146af");
        assert_eq!(content_fingerprint("hello world\n"), "a948904f2f0f479b");
    }
}
