//! `crypto.handshake.{credential,invitation}` — full handshake stepping.
//!
//! Per vector: selector binding, client proof, transcript, ECDH shared
//! secret, key salt, HKDF session keys, server proof, byte-exact hello JSON,
//! then the two finish frames sealed/opened at seq 0.

use lerdr_e2ee::handshake::{
    client_proof, derive_session_keys, ecdh_shared, encode_client_hello, encode_server_hello,
    key_salt, parse_client_finish, parse_client_hello, parse_server_finish, parse_server_hello,
    server_proof, transcript, verify_client_proof, AuthKind, AuthSelector, ServerHandshake,
};
use lerdr_e2ee::{Codec, E2eeError};
use lerdr_fixture::{self as fixture, Suite};
use p256::elliptic_curve::sec1::ToSec1Point;
use p256::{PublicKey, SecretKey};
use serde_json::Value;

type Check = Result<(), Box<dyn std::error::Error>>;

#[test]
fn handshake_credential() {
    run_suite("crypto.handshake.credential");
}

#[test]
fn handshake_invitation() {
    run_suite("crypto.handshake.invitation");
}

fn run_suite(suite_name: &str) {
    let suite = Suite::load("crypto", suite_name).expect("load suite");
    assert_eq!(suite.format_version, 1);
    for vector in &suite.vectors {
        let name = fixture::vector_name(vector);
        check_vector(vector).unwrap_or_else(|e| panic!("{suite_name}#{name}: {e}"));
    }
}

fn fixed<const N: usize>(
    bytes: Vec<u8>,
    what: &str,
) -> Result<[u8; N], Box<dyn std::error::Error>> {
    bytes
        .try_into()
        .map_err(|v: Vec<u8>| format!("{what}: {} bytes, want {N}", v.len()).into())
}

fn expect_eq(actual: &[u8], expected: &[u8], what: &str) -> Check {
    if actual != expected {
        return Err(format!(
            "{what}:\n  actual   {}\n  expected {}",
            hex::encode(actual),
            hex::encode(expected)
        )
        .into());
    }
    Ok(())
}

fn check_vector(v: &Value) -> Check {
    let kind = match fixture::str_field(v, "auth_kind")? {
        "credential" => AuthKind::Credential,
        "invitation" => AuthKind::Invitation,
        other => return Err(format!("auth_kind {other:?}").into()),
    };
    let selector = AuthSelector::new(
        kind,
        fixture::str_field(v, "auth_id")?,
        fixture::u64_field(v, "auth_version")?,
        fixture::str_field(v, "locale")?,
    );
    let secret: [u8; 32] = fixed(fixture::b64_field(v, "secret_b64")?, "secret")?;
    let client_nonce: [u8; 32] = fixed(fixture::b64_field(v, "client_nonce_b64")?, "client_nonce")?;
    let server_nonce: [u8; 32] = fixed(fixture::b64_field(v, "server_nonce_b64")?, "server_nonce")?;
    let client_private =
        SecretKey::from_slice(&fixture::b64_field(v, "client_ephemeral_priv_b64")?)
            .map_err(|e| format!("client_ephemeral_priv: {e}"))?;
    let server_private =
        SecretKey::from_slice(&fixture::b64_field(v, "server_ephemeral_priv_b64")?)
            .map_err(|e| format!("server_ephemeral_priv: {e}"))?;

    // Ephemeral public keys: uncompressed 65-byte SEC1 points.
    let client_public_bytes: [u8; 65] = client_private
        .public_key()
        .to_sec1_point(false)
        .as_bytes()
        .try_into()
        .unwrap();
    let server_public_bytes: [u8; 65] = server_private
        .public_key()
        .to_sec1_point(false)
        .as_bytes()
        .try_into()
        .unwrap();
    expect_eq(
        &client_public_bytes[..],
        &fixture::b64_field(v, "client_ephemeral_pub_b64")?,
        "client_ephemeral_pub",
    )?;
    expect_eq(
        &server_public_bytes[..],
        &fixture::b64_field(v, "server_ephemeral_pub_b64")?,
        "server_ephemeral_pub",
    )?;

    // Binding and client proof.
    let binding = selector.binding();
    expect_eq(&binding, &fixture::hex_field(v, "binding_hex")?, "binding")?;
    let proof_client = client_proof(&secret, &binding, &client_nonce, &client_public_bytes);
    expect_eq(
        &proof_client[..],
        &fixture::b64_field(v, "proof_client_b64")?,
        "proof_client",
    )?;

    // Transcript, shared secret (both ECDH directions must agree), salt, keys.
    let transcript = transcript(
        &binding,
        &client_nonce,
        &client_public_bytes,
        &server_nonce,
        &server_public_bytes,
    );
    expect_eq(
        &transcript,
        &fixture::hex_field(v, "transcript_hex")?,
        "transcript",
    )?;
    let client_public = PublicKey::from_sec1_bytes(&client_public_bytes).expect("client public");
    let server_public = PublicKey::from_sec1_bytes(&server_public_bytes).expect("server public");
    let shared = ecdh_shared(&server_private, &client_public);
    expect_eq(
        &shared[..],
        &fixture::b64_field(v, "shared_secret_b64")?,
        "shared_secret",
    )?;
    let shared_reverse = ecdh_shared(&client_private, &server_public);
    expect_eq(&shared_reverse[..], &shared[..], "shared_secret (reverse)")?;

    let key_salt = key_salt(&secret, &transcript);
    expect_eq(
        &key_salt[..],
        &fixture::b64_field(v, "key_salt_b64")?,
        "key_salt",
    )?;
    let keys = derive_session_keys(&shared, &key_salt);
    expect_eq(
        &keys.c2s[..],
        &fixture::b64_field(v, "session_key_c2s_b64")?,
        "session_key_c2s",
    )?;
    expect_eq(
        &keys.s2c[..],
        &fixture::b64_field(v, "session_key_s2c_b64")?,
        "session_key_s2c",
    )?;
    let proof_server = server_proof(&secret, &transcript);
    expect_eq(
        &proof_server[..],
        &fixture::b64_field(v, "proof_server_b64")?,
        "proof_server",
    )?;

    // Byte-exact hello JSON, then parse back through the real validator.
    let client_hello_json = fixture::str_field(v, "client_hello_json")?;
    let encoded_client_hello = encode_client_hello(
        &selector,
        &client_nonce,
        &client_public_bytes,
        &proof_client,
    );
    expect_eq(
        &encoded_client_hello,
        client_hello_json.as_bytes(),
        "client_hello_json",
    )?;
    let parsed = parse_client_hello(client_hello_json.as_bytes())
        .map_err(|e| format!("parse_client_hello: {e}"))?;
    if parsed.selector != selector {
        return Err(format!("selector {:?} != {:?}", parsed.selector, selector).into());
    }
    if parsed.nonce != client_nonce || parsed.public_bytes != client_public_bytes {
        return Err("parsed hello material differs".into());
    }
    if !verify_client_proof(&secret, &parsed) {
        return Err("verify_client_proof rejected fixture proof".into());
    }

    let server_hello_json = fixture::str_field(v, "server_hello_json")?;
    let encoded_server_hello =
        encode_server_hello(&server_nonce, &server_public_bytes, &proof_server);
    expect_eq(
        &encoded_server_hello,
        server_hello_json.as_bytes(),
        "server_hello_json",
    )?;
    let parsed_server = parse_server_hello(server_hello_json.as_bytes())
        .map_err(|e| format!("parse_server_hello: {e}"))?;
    if parsed_server.nonce != server_nonce || parsed_server.public_bytes != server_public_bytes {
        return Err("parsed server hello material differs".into());
    }
    expect_eq(
        &parsed_server.proof[..],
        &proof_server[..],
        "server hello proof",
    )?;

    // The state-machine path must land on the same material, and a wrong
    // secret must be refused at begin().
    let handshake = ServerHandshake::begin(client_hello_json.as_bytes(), &secret)
        .map_err(|e| format!("ServerHandshake::begin: {e}"))?;
    expect_eq(handshake.binding(), &binding, "handshake binding")?;
    match ServerHandshake::begin(client_hello_json.as_bytes(), &[0xAB; 32]) {
        Err(E2eeError::ClientProofFailed) => {}
        Err(e) => return Err(format!("wrong secret: unexpected error {e}").into()),
        Ok(_) => return Err("wrong secret authenticated".into()),
    }
    let (hello_out, established) = handshake
        .respond(&secret, &server_private, server_nonce)
        .map_err(|e| format!("respond: {e}"))?;
    expect_eq(
        &hello_out,
        server_hello_json.as_bytes(),
        "handshake server_hello",
    )?;
    expect_eq(
        established.transcript(),
        &transcript,
        "handshake transcript",
    )?;
    expect_eq(
        &established.shared_secret()[..],
        &shared[..],
        "handshake shared",
    )?;
    expect_eq(
        &established.session_keys().c2s[..],
        &keys.c2s[..],
        "handshake c2s key",
    )?;

    // Finish frames: c2s client finish then s2c server finish, both seq 0,
    // JSON codec (the WS handshake rides text frames).
    let auth_version = fixture::u64_field(v, "auth_version")?;
    let mut client = established.client_session(Codec::Json);
    let mut server = established.server_session(Codec::Json);
    for frame in fixture::array_field(v, "finish_frames")? {
        let direction = fixture::str_field(frame, "direction")?;
        let seq = fixture::u64_field(frame, "sequence")?;
        let plaintext = fixture::b64_field(frame, "plaintext_b64")?;
        let expected = fixture::b64_field(frame, "frame_b64")?;
        if seq != 0 {
            return Err(format!("finish frame seq {seq}").into());
        }
        match direction {
            "c2s" => {
                let sealed = client.seal(&plaintext).map_err(|e| e.to_string())?;
                expect_eq(&sealed, &expected, "client finish frame")?;
                let opened = server.open(&sealed).map_err(|e| e.to_string())?;
                expect_eq(&opened, &plaintext, "client finish plaintext")?;
                parse_client_finish(&opened).map_err(|e| e.to_string())?;
            }
            "s2c" => {
                let sealed = server.seal(&plaintext).map_err(|e| e.to_string())?;
                expect_eq(&sealed, &expected, "server finish frame")?;
                let opened = client.open(&sealed).map_err(|e| e.to_string())?;
                expect_eq(&opened, &plaintext, "server finish plaintext")?;
                let finish = parse_server_finish(&opened).map_err(|e| e.to_string())?;
                match kind {
                    // For credential auth the issued version is the credential's
                    // own; for invitation auth the server issues version 1.
                    AuthKind::Credential if finish.credential_version != auth_version => {
                        return Err("credential_version mismatch".into())
                    }
                    AuthKind::Credential if finish.credential_secret.is_some() => {
                        return Err("credential auth must not carry credential_secret".into())
                    }
                    AuthKind::Invitation if finish.credential_secret.is_none() => {
                        return Err("invitation finish must carry credential_secret".into())
                    }
                    _ => {}
                }
            }
            other => return Err(format!("finish direction {other:?}").into()),
        }
    }
    Ok(())
}
