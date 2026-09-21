//! `crypto.frames.{json,binary}` — seal/open conformance.
//!
//! Per vector: the intermediate `nonce`/`aad` are asserted too, then
//! `seal(plaintext)` must equal `sealed_frame_b64` byte-for-byte and the
//! receiving peer must open it back. `seq = 2^53-1` vectors additionally
//! assert that the *next* seal fails with sequence exhaustion.

use lerdr_e2ee::handshake::SessionKeys;
use lerdr_e2ee::{aad, frame_nonce, Codec, Direction, E2eeError, Session, MAX_SEQUENCE};
use lerdr_fixture::{self as fixture, Suite};
use serde_json::Value;

type Check = Result<(), Box<dyn std::error::Error>>;

#[test]
fn frames_json() {
    run_suite("crypto.frames.json", Codec::Json);
}

#[test]
fn frames_binary() {
    run_suite("crypto.frames.binary", Codec::Binary);
}

fn run_suite(suite_name: &str, codec: Codec) {
    let suite = Suite::load("crypto", suite_name).expect("load suite");
    assert_eq!(suite.format_version, 1);
    for vector in &suite.vectors {
        let name = fixture::vector_name(vector);
        check_vector(vector, codec).unwrap_or_else(|e| panic!("{suite_name}#{name}: {e}"));
    }
}

fn check_vector(v: &Value, codec: Codec) -> Check {
    let direction =
        Direction::from_label(fixture::str_field(v, "direction")?).ok_or("bad direction")?;
    let seq = fixture::u64_field(v, "seq")?;
    let keys = SessionKeys {
        c2s: fixture::b64_field(v, "session_key_c2s_b64")?
            .try_into()
            .map_err(|_| "c2s key must be 32 bytes")?,
        s2c: fixture::b64_field(v, "session_key_s2c_b64")?
            .try_into()
            .map_err(|_| "s2c key must be 32 bytes")?,
    };
    let plaintext = fixture::b64_field(v, "plaintext_b64")?;
    let expected_frame = fixture::b64_field(v, "sealed_frame_b64")?;

    // Intermediate material is pinned too — nonce/AAD derivation must match
    // before GCM even runs.
    let nonce = fixture::b64_field(v, "nonce_b64")?;
    if frame_nonce(seq)[..] != nonce[..] {
        return Err(format!("nonce mismatch at seq {seq}").into());
    }
    let aad_bytes = fixture::b64_field(v, "aad_b64")?;
    if aad(direction, seq)[..] != aad_bytes[..] {
        return Err(format!("aad mismatch at seq {seq}").into());
    }

    // Seal: the sending peer with its send counter preset to `seq`.
    let mut sender = Session::sender(&keys, direction, codec)?;
    sender.set_send_sequence(seq);
    let sealed = sender.seal(&plaintext)?;
    if sealed != expected_frame {
        return Err(format!(
            "sealed frame mismatch:\n  actual   {}\n  expected {}",
            hex::encode(&sealed),
            hex::encode(&expected_frame)
        )
        .into());
    }

    // Open: the receiving peer with its receive counter preset to `seq`.
    let mut receiver = Session::receiver(&keys, direction, codec)?;
    receiver.set_receive_sequence(seq);
    let opened = receiver.open(&expected_frame)?;
    if opened != plaintext {
        return Err("opened plaintext mismatch".into());
    }

    // At the sequence ceiling the next seal must fail.
    if seq == MAX_SEQUENCE {
        if !matches!(sender.seal(b"x"), Err(E2eeError::SequenceExhausted)) {
            return Err("seal past 2^53-1 must fail".into());
        }
        // And a frame claiming the now-out-of-range next seq must be rejected
        // by the receiver even though it is exactly what it "expects" next.
        let mut tail = Session::receiver(&keys, direction, codec)?;
        tail.set_receive_sequence(seq + 1);
        // Re-point the envelope at seq 2^53 (over the ceiling).
        let forged = match codec {
            Codec::Binary => Codec::Binary.encode(MAX_SEQUENCE + 1, b"irrelevant-but-long-enough"),
            Codec::Json => Codec::Json.encode(MAX_SEQUENCE + 1, b"irrelevant-but-long-enough"),
        };
        let err = tail.open(&forged).unwrap_err();
        if err.class() != lerdr_e2ee::ErrorClass::Seq {
            return Err(format!("seq ceiling open: {err}").into());
        }
    }
    Ok(())
}
