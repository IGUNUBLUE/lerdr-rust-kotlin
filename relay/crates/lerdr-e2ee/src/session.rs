//! The sealed session: per-direction AES-256-GCM channels with strictly
//! ordered, direction-bound frames (`e2eeSession` in `e2ee.go`).
//!
//! ```text
//! nonce = 0x00000000 ‖ BE64(sequence)                    (12 bytes)
//! AAD   = "herdr-e2ee-v2 " ‖ direction ‖ 0x00 ‖ BE64(sequence)  (26 bytes)
//! cipher = AES-256-GCM(direction key, nonce).seal(plaintext, AAD)
//! ```
//!
//! Sequences run `0..=2^53-1` per direction and must arrive strictly in order:
//! a frame below the receiver's next expected sequence is a replay, anything
//! else out of order is a sequence error.

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{AeadCore, Aes256Gcm, KeyInit, Nonce};

use crate::codec::Codec;
use crate::error::E2eeError;
use crate::handshake::SessionKeys;

/// Go `maxE2EESequence` — sequence ceiling on seal and open.
pub const MAX_SEQUENCE: u64 = (1 << 53) - 1;

const NONCE_BYTES: usize = 12;
const AAD_BYTES: usize = "herdr-e2ee-v2 ".len() + 3 + 1 + 8;
const AAD_PREFIX: &[u8] = b"herdr-e2ee-v2 ";
const KEY_BYTES: usize = 32;
type GcmNonce = Nonce<<Aes256Gcm as AeadCore>::NonceSize>;

fn gcm_nonce(sequence: u64) -> GcmNonce {
    GcmNonce::try_from(&frame_nonce(sequence)[..]).expect("nonce is 12 bytes")
}

/// Wire direction of a frame (`e2eeClientDirection`/`e2eeServerDirection`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Client → server.
    C2S,
    /// Server → client.
    S2C,
}

impl Direction {
    /// AAD label and HKDF info suffix.
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::C2S => "c2s",
            Direction::S2C => "s2c",
        }
    }

    /// The opposite direction (the peer's view of the same channel).
    pub fn peer(self) -> Self {
        match self {
            Direction::C2S => Direction::S2C,
            Direction::S2C => Direction::C2S,
        }
    }

    /// Parse the fixture `direction` label (`"c2s"` | `"s2c"`).
    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "c2s" => Some(Direction::C2S),
            "s2c" => Some(Direction::S2C),
            _ => None,
        }
    }
}

impl std::fmt::Display for Direction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `e2eeFrameNonce`: four zero bytes ‖ big-endian u64 sequence.
pub fn frame_nonce(sequence: u64) -> [u8; NONCE_BYTES] {
    let mut nonce = [0u8; NONCE_BYTES];
    nonce[4..].copy_from_slice(&sequence.to_be_bytes());
    nonce
}

/// `e2eeAAD`: `"herdr-e2ee-v2 " ‖ direction ‖ 0x00 ‖ BE64(sequence)`.
pub fn aad(direction: Direction, sequence: u64) -> [u8; AAD_BYTES] {
    let mut aad = [0u8; AAD_BYTES];
    aad[..AAD_PREFIX.len()].copy_from_slice(AAD_PREFIX);
    aad[AAD_PREFIX.len()..AAD_PREFIX.len() + 3].copy_from_slice(direction.as_str().as_bytes());
    // aad[AAD_PREFIX.len() + 3] is already 0x00.
    aad[AAD_PREFIX.len() + 4..].copy_from_slice(&sequence.to_be_bytes());
    aad
}

/// A bidirectional sealed session (`e2eeSession`).
///
/// `send`/`receive` hold per-direction keys; `seal` and `open` advance their
/// own counters independently. Construct via [`Session::server`] /
/// [`Session::client`] / [`Session::receiver`], or [`Session::new`] for full
/// control.
pub struct Session {
    send: Aes256Gcm,
    receive: Aes256Gcm,
    codec: Codec,
    send_direction: Direction,
    receive_direction: Direction,
    send_sequence: u64,
    receive_sequence: u64,
}

impl Session {
    /// `newE2EESession` — explicit keys, directions, codec, zeroed sequences.
    pub fn new(
        send_key: &[u8],
        receive_key: &[u8],
        send_direction: Direction,
        receive_direction: Direction,
        codec: Codec,
    ) -> Result<Self, E2eeError> {
        if send_key.len() != KEY_BYTES || receive_key.len() != KEY_BYTES {
            return Err(E2eeError::InvalidKeyMaterial);
        }
        Ok(Self {
            send: Aes256Gcm::new_from_slice(send_key).map_err(|_| E2eeError::InvalidKeyMaterial)?,
            receive: Aes256Gcm::new_from_slice(receive_key)
                .map_err(|_| E2eeError::InvalidKeyMaterial)?,
            codec,
            send_direction,
            receive_direction,
            send_sequence: 0,
            receive_sequence: 0,
        })
    }

    /// The relay's view: sends `s2c` (s2c key), receives `c2s` (c2s key).
    pub fn server(keys: &SessionKeys, codec: Codec) -> Result<Self, E2eeError> {
        Self::new(&keys.s2c, &keys.c2s, Direction::S2C, Direction::C2S, codec)
    }

    /// The client's view: sends `c2s` (c2s key), receives `s2c` (s2c key).
    pub fn client(keys: &SessionKeys, codec: Codec) -> Result<Self, E2eeError> {
        Self::new(&keys.c2s, &keys.s2c, Direction::C2S, Direction::S2C, codec)
    }

    /// The receiving peer for frames travelling in `direction` — `c2s` yields
    /// the server-side session, `s2c` the client-side one (mirrors the fixture
    /// generator's `openFixtureFrame`).
    pub fn receiver(
        keys: &SessionKeys,
        direction: Direction,
        codec: Codec,
    ) -> Result<Self, E2eeError> {
        match direction {
            Direction::C2S => Self::server(keys, codec),
            Direction::S2C => Self::client(keys, codec),
        }
    }

    /// The sender peer for frames travelling in `direction`.
    pub fn sender(
        keys: &SessionKeys,
        direction: Direction,
        codec: Codec,
    ) -> Result<Self, E2eeError> {
        match direction {
            Direction::C2S => Self::client(keys, codec),
            Direction::S2C => Self::server(keys, codec),
        }
    }

    pub fn codec(&self) -> Codec {
        self.codec
    }

    pub fn set_codec(&mut self, codec: Codec) {
        self.codec = codec;
    }

    pub fn send_sequence(&self) -> u64 {
        self.send_sequence
    }

    pub fn receive_sequence(&self) -> u64 {
        self.receive_sequence
    }

    /// Preset the send counter — fixture replay and stream resume both need
    /// it (the Go exporter presets `sendSequence` the same way).
    pub fn set_send_sequence(&mut self, sequence: u64) {
        self.send_sequence = sequence;
    }

    /// Preset the receive counter; used to replay mid-stream frames.
    pub fn set_receive_sequence(&mut self, sequence: u64) {
        self.receive_sequence = sequence;
    }

    /// `seal`: encrypt `plaintext` under the next send sequence and wrap it in
    /// the negotiated codec. Fails once `send_sequence` exceeds
    /// [`MAX_SEQUENCE`] — the last legal frame is `seq = 2^53-1`.
    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, E2eeError> {
        if self.send_sequence > MAX_SEQUENCE {
            return Err(E2eeError::SequenceExhausted);
        }
        let sequence = self.send_sequence;
        let nonce = gcm_nonce(sequence);
        let aad = aad(self.send_direction, sequence);
        let ciphertext = self
            .send
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| E2eeError::Authentication)?;
        let frame = self.codec.encode(sequence, &ciphertext);
        self.send_sequence += 1;
        Ok(frame)
    }

    /// `open`: decode the envelope, enforce the sequence, then verify the GCM
    /// tag under the receive direction's AAD. Advances `receive_sequence` only
    /// on success.
    pub fn open(&mut self, raw_frame: &[u8]) -> Result<Vec<u8>, E2eeError> {
        let (sequence, ciphertext) = self.codec.decode(raw_frame)?;
        if sequence > MAX_SEQUENCE || sequence != self.receive_sequence {
            return Err(E2eeError::InvalidSequence {
                expected: self.receive_sequence,
                received: sequence,
            });
        }
        let nonce = gcm_nonce(sequence);
        let aad = aad(self.receive_direction, sequence);
        let plaintext = self
            .receive
            .decrypt(
                &nonce,
                Payload {
                    msg: &ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| E2eeError::Authentication)?;
        self.receive_sequence += 1;
        Ok(plaintext)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> SessionKeys {
        SessionKeys {
            c2s: [0x11; 32],
            s2c: [0x22; 32],
        }
    }

    #[test]
    fn nonce_and_aad_layout() {
        assert_eq!(frame_nonce(0), [0u8; 12]);
        assert_eq!(
            frame_nonce(0x0102030405060708),
            [0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8]
        );
        assert_eq!(&aad(Direction::C2S, 0)[..18], b"herdr-e2ee-v2 c2s\x00");
        assert_eq!(
            &aad(Direction::S2C, 0x0102030405060708)[..],
            b"herdr-e2ee-v2 s2c\x00\x01\x02\x03\x04\x05\x06\x07\x08"
        );
    }

    #[test]
    fn roundtrip_both_codecs() {
        for codec in [Codec::Json, Codec::Binary] {
            let mut client = Session::client(&keys(), codec).unwrap();
            let mut server = Session::server(&keys(), codec).unwrap();
            for seq in 0..4u64 {
                let pt = format!("msg {seq}").into_bytes();
                let frame = client.seal(&pt).unwrap();
                assert_eq!(server.open(&frame).unwrap(), pt);
                let frame = server.seal(&pt).unwrap();
                assert_eq!(client.open(&frame).unwrap(), pt);
            }
        }
    }

    #[test]
    fn out_of_order_open_fails_without_consuming() {
        let keys = keys();
        let mut client = Session::client(&keys, Codec::Binary).unwrap();
        let mut server = Session::server(&keys, Codec::Binary).unwrap();
        let f0 = client.seal(b"a").unwrap();
        let f1 = client.seal(b"b").unwrap();
        let f2 = client.seal(b"c").unwrap();
        // Deliver f0 twice: first ok, second is a replay.
        assert_eq!(server.open(&f0).unwrap(), b"a");
        let err = server.open(&f0).unwrap_err();
        assert_eq!(err.class(), crate::ErrorClass::Replay);
        // Seq 2 while expecting 1 is a gap (seq class), and the failed opens
        // consumed nothing: f1 still opens cleanly.
        let err = server.open(&f2).unwrap_err();
        assert_eq!(err.class(), crate::ErrorClass::Seq);
        assert_eq!(server.open(&f1).unwrap(), b"b");
        assert_eq!(server.open(&f2).unwrap(), b"c");
    }

    #[test]
    fn send_exhaustion() {
        let keys = keys();
        let mut client = Session::client(&keys, Codec::Binary).unwrap();
        client.set_send_sequence(MAX_SEQUENCE);
        client.seal(b"last").unwrap();
        assert!(matches!(
            client.seal(b"over"),
            Err(E2eeError::SequenceExhausted)
        ));
    }

    proptest::proptest! {
        #[test]
        fn open_inverts_seal(plaintext in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..512)) {
            let keys = keys();
            let mut client = Session::client(&keys, Codec::Json).unwrap();
            let mut server = Session::server(&keys, Codec::Json).unwrap();
            let frame = client.seal(&plaintext).unwrap();
            proptest::prop_assert_eq!(server.open(&frame).unwrap(), plaintext);
        }
    }
}
