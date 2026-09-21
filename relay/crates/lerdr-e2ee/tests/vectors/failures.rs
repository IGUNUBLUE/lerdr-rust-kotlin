//! `crypto.failures` — every mutation must produce the expected error class
//! **and** the exact Go error string (`go_error`), proving both taxonomy and
//! the error-ordering contract (envelope → sequence → GCM).

use lerdr_e2ee::handshake::SessionKeys;
use lerdr_e2ee::{Codec, Direction, ErrorClass, Session};
use lerdr_fixture::{self as fixture, Suite};
use serde_json::Value;

type Check = Result<(), Box<dyn std::error::Error>>;

#[test]
fn failures() {
    let suite = Suite::load("crypto", "crypto.failures").expect("load suite");
    assert_eq!(suite.format_version, 1);
    for vector in &suite.vectors {
        let name = fixture::vector_name(vector);
        check_vector(vector).unwrap_or_else(|e| panic!("crypto.failures#{name}: {e}"));
    }
}

fn check_vector(v: &Value) -> Check {
    let direction =
        Direction::from_label(fixture::str_field(v, "direction")?).ok_or("bad direction")?;
    let codec = Codec::from_label(fixture::str_field(v, "codec")?).ok_or("bad codec")?;
    let keys = SessionKeys {
        c2s: fixture::b64_field(v, "session_key_c2s_b64")?
            .try_into()
            .map_err(|_| "c2s key must be 32 bytes")?,
        s2c: fixture::b64_field(v, "session_key_s2c_b64")?
            .try_into()
            .map_err(|_| "s2c key must be 32 bytes")?,
    };
    let frame = fixture::b64_field(v, "frame_b64")?;
    let expected_class = match fixture::str_field(v, "expected_error")? {
        "format" => ErrorClass::Format,
        "replay" => ErrorClass::Replay,
        "seq" => ErrorClass::Seq,
        "auth" => ErrorClass::Auth,
        other => return Err(format!("expected_error {other:?}").into()),
    };
    let go_error = fixture::str_field(v, "go_error")?;

    let mut receiver = Session::receiver(&keys, direction, codec)?;
    receiver.set_receive_sequence(fixture::u64_field(v, "receiver_next_sequence")?);

    match receiver.open(&frame) {
        Ok(plaintext) => Err(format!(
            "mutated frame opened to {} plaintext bytes",
            plaintext.len()
        )
        .into()),
        Err(err) => {
            if err.class() != expected_class {
                return Err(format!(
                    "error class {} ({err}) != expected {expected_class}",
                    err.class()
                )
                .into());
            }
            if err.to_string() != go_error {
                return Err(format!("error text {err:?} != go_error {go_error:?}").into());
            }
            Ok(())
        }
    }
}
