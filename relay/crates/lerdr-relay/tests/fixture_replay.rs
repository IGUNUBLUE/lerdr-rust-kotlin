//! Relay-boundary fixture replay — the golden vectors under `fixtures/`
//! driven through the exact code path the WS session uses.
//!
//! lerdr-core's `conformance.rs` already proves the *types* are byte-exact
//! against the Go oracle; this suite proves the *relay boundary* honors the
//! same contract end to end:
//!
//! - every `c2s` envelope decodes through [`Inbound::decode`] — the same
//!   parse → `decode_map` composition `Actor::handle_frame` runs on opened
//!   plaintext — and re-serializes to `decoded_json` byte-for-byte;
//! - every `s2c` envelope decodes through [`Outbound::decode`] and
//!   re-encodes byte-exactly (the reply path the writer pump seals);
//! - every `crypto.frames.*` sealed frame opens through
//!   [`Session::receiver`] — the call site the session actor runs per
//!   inbound frame — and every `crypto.failures` mutation is rejected with
//!   the expected error class, never a panic;
//! - the live session (real `herdr-e2ee-v2` handshake over the duplex
//!   harness) accepts all 72 canonical `c2s` wire shapes and answers each
//!   with a well-formed encrypted `action_receipt` envelope instead of
//!   evicting or hanging.
//!
//! Note: `protocol.envelope` carries no negative vectors today — protocol
//! negatives would appear here as vectors with an `expect_error`-style
//! field; the crypto layer owns the malformed-input coverage.

mod support;

use std::sync::Arc;
use std::time::Duration;

use lerdr_core::protocol::{Inbound, Outbound};
use lerdr_e2ee::handshake::SessionKeys;
use lerdr_e2ee::{Codec, Direction, Session};
use lerdr_fixture::{self as fixture, Suite};
use lerdr_relay::frame::{FrameRead, FrameWrite, ReadError};
use lerdr_relay::session::{ConnectionEnd, EvictReason};
use lerdr_relay::store::MemoryAuthStore;
use serde::Deserialize;
use support::*;
use tokio_util::sync::CancellationToken;

/// `protocol.envelope` vector shape (`{name, type, direction, json,
/// decoded_json?}` — see fixtures/README.md).
#[derive(Deserialize)]
struct EnvelopeVector {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    direction: String,
    json: String,
    decoded_json: Option<String>,
}

fn envelope_suite() -> Vec<EnvelopeVector> {
    let suite = Suite::load("protocol", "protocol.envelope").expect("suite loads");
    suite
        .vectors
        .iter()
        .map(|v| serde_json::from_value(v.clone()).expect("envelope vector schema"))
        .collect()
}

fn session_keys(vector: &serde_json::Value) -> SessionKeys {
    SessionKeys {
        c2s: fixture::b64_field(vector, "session_key_c2s_b64")
            .expect("session_key_c2s_b64")
            .try_into()
            .expect("c2s key is 32 bytes"),
        s2c: fixture::b64_field(vector, "session_key_s2c_b64")
            .expect("session_key_s2c_b64")
            .try_into()
            .expect("s2c key is 32 bytes"),
    }
}

// ---------------------------------------------------------------------------
// protocol.envelope — c2s vectors through the inbound decoder.
// ---------------------------------------------------------------------------

#[test]
fn inbound_envelope_fixtures_decode_and_round_trip() {
    let mut count = 0usize;
    for vector in envelope_suite() {
        match vector.direction.as_str() {
            "c2s" => {}
            "s2c" => continue,
            other => panic!("{}: unknown direction {other}", vector.name),
        }
        let name = vector.name.as_str();

        // The same decode the session actor runs on opened plaintext:
        // JSON object parse -> decode_map (which owns the command-envelope
        // rewrite and the dropped-field semantics).
        let decoded = Inbound::decode(vector.json.as_bytes()).unwrap_or_else(|err| {
            panic!("{name}: canonical wire bytes rejected by Inbound::decode: {err}")
        });
        assert_eq!(
            decoded.r#type, vector.kind,
            "{name}: decoded type differs from fixture type"
        );

        // Re-serialization must reproduce `decoded_json` byte-exactly — the
        // canonical post-decode bytes the Go oracle emitted.
        let canonical = vector
            .decoded_json
            .as_deref()
            .expect("c2s vectors carry decoded_json");
        assert_eq!(
            decoded.encode(),
            canonical.as_bytes(),
            "{name}: Inbound::encode differs from decoded_json"
        );

        // `decoded_json` is itself legal wire input: decoding it must be a
        // fixed point (the typed view is stable under re-decode).
        let recoded = Inbound::decode(canonical.as_bytes())
            .unwrap_or_else(|err| panic!("{name}: decoded_json failed to re-decode: {err}"));
        assert_eq!(
            recoded.encode(),
            canonical.as_bytes(),
            "{name}: decode->encode is not idempotent on decoded_json"
        );
        count += 1;
    }
    assert_eq!(count, 72, "c2s fixture vector count changed");
}

// ---------------------------------------------------------------------------
// protocol.envelope — s2c vectors through the outbound decoder.
// ---------------------------------------------------------------------------

#[test]
fn outbound_envelope_fixtures_decode_and_round_trip() {
    let mut count = 0usize;
    for vector in envelope_suite() {
        match vector.direction.as_str() {
            "s2c" => {}
            "c2s" => continue,
            other => panic!("{}: unknown direction {other}", vector.name),
        }
        let name = vector.name.as_str();

        let decoded = Outbound::decode(vector.json.as_bytes()).unwrap_or_else(|err| {
            panic!("{name}: canonical wire bytes rejected by Outbound::decode: {err}")
        });
        // Every fixture message type is a modeled variant — `Unknown` would
        // still round-trip byte-exact, but the contract is full coverage.
        assert!(
            !matches!(decoded, Outbound::Unknown(_)),
            "{name}: type {:?} decoded as Outbound::Unknown",
            vector.kind
        );
        assert_eq!(
            decoded.encode(),
            vector.json.as_bytes(),
            "{name}: Outbound::encode differs from canonical wire bytes"
        );
        count += 1;
    }
    assert_eq!(count, 51, "s2c fixture vector count changed");
}

// ---------------------------------------------------------------------------
// crypto.frames.* — canonical sealed frames through the receive path.
// ---------------------------------------------------------------------------

#[test]
fn crypto_frame_fixtures_open_through_session_receive_path() {
    for (suite_name, codec) in [
        ("crypto.frames.json", Codec::Json),
        ("crypto.frames.binary", Codec::Binary),
    ] {
        let suite = Suite::load("crypto", suite_name).expect("suite loads");
        for vector in &suite.vectors {
            let name = fixture::vector_name(vector);
            let direction =
                Direction::from_label(fixture::str_field(vector, "direction").expect("direction"))
                    .unwrap_or_else(|| panic!("{suite_name}#{name}: bad direction label"));
            let keys = session_keys(vector);
            let frame = fixture::b64_field(vector, "sealed_frame_b64").expect("sealed_frame_b64");
            let expected = fixture::b64_field(vector, "plaintext_b64").expect("plaintext_b64");

            // `Session::receiver` is the same open path the actor runs
            // (`session.open` on the c2s direction, and the client's view
            // of s2c).
            let mut receiver =
                Session::receiver(&keys, direction, codec).expect("receiver session");
            receiver.set_receive_sequence(fixture::u64_field(vector, "seq").expect("seq"));
            let plaintext = receiver.open(&frame).unwrap_or_else(|err| {
                panic!("{suite_name}#{name}: canonical frame rejected: {err}")
            });
            assert_eq!(
                plaintext, expected,
                "{suite_name}#{name}: opened plaintext differs"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// crypto.failures — every mutation rejected with its expected error class.
// ---------------------------------------------------------------------------

#[test]
fn crypto_failure_fixtures_reject_with_expected_error_class() {
    let suite = Suite::load("crypto", "crypto.failures").expect("suite loads");
    assert_eq!(suite.vectors.len(), 17, "crypto.failures vector count");
    for vector in &suite.vectors {
        let name = fixture::vector_name(vector);
        let direction =
            Direction::from_label(fixture::str_field(vector, "direction").expect("direction"))
                .unwrap_or_else(|| panic!("crypto.failures#{name}: bad direction label"));
        let codec = Codec::from_label(fixture::str_field(vector, "codec").expect("codec"))
            .unwrap_or_else(|| panic!("crypto.failures#{name}: bad codec label"));
        let keys = session_keys(vector);
        let frame = fixture::b64_field(vector, "frame_b64").expect("frame_b64");
        let expected = fixture::str_field(vector, "expected_error").expect("expected_error");

        // Reaching `open` at all — rather than panicking on the malformed
        // input — is half the assertion; the error class is the other half.
        let mut receiver = Session::receiver(&keys, direction, codec).expect("receiver session");
        receiver.set_receive_sequence(
            fixture::u64_field(vector, "receiver_next_sequence").expect("receiver_next_sequence"),
        );
        match receiver.open(&frame) {
            Ok(plaintext) => panic!(
                "crypto.failures#{name}: mutated frame opened to {} plaintext bytes",
                plaintext.len()
            ),
            Err(err) => assert_eq!(
                err.class().as_str(),
                expected,
                "crypto.failures#{name}: wrong error class ({err})"
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Live session boundary — canonical c2s wire shapes over sealed frames.
// ---------------------------------------------------------------------------

/// Read one reply frame with a deadline, open it, and decode it as a typed
/// [`Outbound`] envelope — the same pipeline a real client's receive loop
/// runs.
async fn read_reply(client: &mut TestClient, session: &mut Session, name: &str) -> Outbound {
    let raw = match tokio::time::timeout(Duration::from_secs(5), client.reader.read_frame()).await {
        Ok(Ok(raw)) => raw,
        Ok(Err(err)) => panic!("{name}: reply frame read failed: {err}"),
        Err(_) => panic!("{name}: timed out waiting for a reply frame"),
    };
    let plaintext = session
        .open(&raw)
        .unwrap_or_else(|err| panic!("{name}: reply frame failed to open: {err}"));
    Outbound::decode(&plaintext)
        .unwrap_or_else(|err| panic!("{name}: reply is not a decodable outbound envelope: {err}"))
}

/// Every canonical `c2s` fixture is driven through a real established
/// session (handshake → sealed frame → actor → sealed reply). The session
/// must answer each with an `action_receipt` echoing the request id —
/// proving the wire shape passed decode, the catalog/protocol/fence/authz
/// gates, and routing — and it must still be alive after all 72.
#[tokio::test]
async fn session_replays_every_inbound_envelope_fixture() {
    let vectors = envelope_suite();

    let store = Arc::new(MemoryAuthStore::new());
    let (selector, secret) = seed_credential(&store);
    let (mut client, server_io) = TestClient::pair(64 * 1024);
    let (server, _sink_rx) = serve(server_io, store, test_config(), CancellationToken::new());
    let mut session = client.handshake(&selector, &secret).await.session;

    let mut replayed = 0usize;
    for vector in vectors.iter().filter(|v| v.direction == "c2s") {
        let name = vector.name.as_str();
        // Pair replies by the decoded request id — the same view of the
        // fixture the actor will compute.
        let inbound = Inbound::decode(vector.json.as_bytes())
            .unwrap_or_else(|err| panic!("{name}: fixture must decode: {err}"));

        // The fixture's canonical wire bytes, verbatim, under the e2ee seal.
        client.send_json(&mut session, vector.json.as_bytes()).await;

        match read_reply(&mut client, &mut session, name).await {
            Outbound::ActionReceipt(reply) => {
                assert_eq!(reply.r#type, "action_receipt", "{name}: reply type");
                assert_eq!(
                    reply.request_id.as_deref().unwrap_or_default(),
                    inbound.request_id,
                    "{name}: receipt must echo request_id"
                );
                let receipt = reply
                    .receipt
                    .expect("action_receipt envelope carries a receipt");
                assert!(
                    !receipt.phase.as_str().is_empty(),
                    "{name}: receipt phase must be set"
                );
            }
            other => panic!("{name}: expected action_receipt, got {other:?}"),
        }
        replayed += 1;
    }
    assert_eq!(
        replayed, 72,
        "every c2s fixture crossed the session boundary"
    );

    // The session survived the whole sweep — a final action still receipts.
    client
        .send_json(
            &mut session,
            br#"{"type":"device_list","protocol":3,"request_id":"req-after-sweep"}"#,
        )
        .await;
    match read_reply(&mut client, &mut session, "liveness").await {
        Outbound::ActionReceipt(reply) => {
            assert_eq!(reply.request_id.as_deref(), Some("req-after-sweep"));
        }
        other => panic!("liveness check expected action_receipt, got {other:?}"),
    }

    drop(client);
    let end = server.await.expect("server task joins");
    assert!(
        matches!(
            end,
            ConnectionEnd::PeerClosed { .. } | ConnectionEnd::TransportFailed
        ),
        "session must not have been evicted during the sweep: {end:?}"
    );
}

/// The four representative actions the task pins by name — same boundary,
/// asserted individually so a regression report names the action.
#[tokio::test]
async fn session_receipts_representative_actions() {
    let vectors = envelope_suite();
    let store = Arc::new(MemoryAuthStore::new());
    let (selector, secret) = seed_credential(&store);
    let (mut client, server_io) = TestClient::pair(64 * 1024);
    let (server, _sink_rx) = serve(server_io, store, test_config(), CancellationToken::new());
    let mut session = client.handshake(&selector, &secret).await.session;

    for want in ["read_pane", "watch_pane", "send_text", "device_list"] {
        let vector = vectors
            .iter()
            .find(|v| v.direction == "c2s" && v.name == want)
            .unwrap_or_else(|| panic!("fixture {want} missing from protocol.envelope"));
        let inbound = Inbound::decode(vector.json.as_bytes()).expect("fixture decodes");

        client.send_json(&mut session, vector.json.as_bytes()).await;
        match read_reply(&mut client, &mut session, want).await {
            Outbound::ActionReceipt(reply) => {
                assert_eq!(
                    reply.request_id.as_deref().unwrap_or_default(),
                    inbound.request_id
                );
                let receipt = reply.receipt.expect("receipt payload");
                assert!(
                    !receipt.phase.as_str().is_empty(),
                    "{want}: receipt phase must be set"
                );
            }
            other => panic!("{want}: expected action_receipt, got {other:?}"),
        }
    }

    drop(client);
    server.await.expect("server task joins");
}

// ---------------------------------------------------------------------------
// Live session boundary — crypto.failures mutations evict, never panic.
// ---------------------------------------------------------------------------

/// Every `crypto.failures` mutation, written verbatim onto a live encrypted
/// session, must end the connection as `Evicted(DecryptFailed)` with a
/// graceful close — the relay's answer to a frame `open` rejects.
#[tokio::test]
async fn mutated_frames_evict_the_session() {
    let suite = Suite::load("crypto", "crypto.failures").expect("suite loads");
    for vector in &suite.vectors {
        let name = fixture::vector_name(vector).to_owned();
        let frame = fixture::b64_field(vector, "frame_b64").expect("frame_b64");

        let store = Arc::new(MemoryAuthStore::new());
        let (selector, secret) = seed_credential(&store);
        let (mut client, server_io) = TestClient::pair(64 * 1024);
        let (server, _sink_rx) = serve(server_io, store, test_config(), CancellationToken::new());
        // The negotiated keys differ from the fixture's — for these vectors
        // that is the point: the server must reject, not crash or hang.
        client.handshake(&selector, &secret).await;

        client
            .writer
            .write_frame(&frame)
            .await
            .expect("mutated frame write");

        let end = server.await.expect("server task joins");
        assert!(
            matches!(end, ConnectionEnd::Evicted(EvictReason::DecryptFailed)),
            "crypto.failures#{name}: mutated frame must evict with DecryptFailed, got {end:?}"
        );
        // Eviction is a graceful normal close (1000), not a transport rip.
        let err = client.reader.read_frame().await.expect_err("closed");
        assert!(
            matches!(
                err,
                ReadError::Closed {
                    code: Some(1000),
                    ..
                }
            ),
            "crypto.failures#{name}: peer must see close 1000, got {err}"
        );
    }
}
