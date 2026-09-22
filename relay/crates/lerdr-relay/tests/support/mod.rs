//! Test client harness — drives the `herdr-e2ee-v2` client side over any
//! `FrameIo`, so the same code exercises the duplex transport and a real
//! WebSocket.

#![allow(dead_code)]

use std::sync::Arc;

use base64::Engine;
use lerdr_e2ee::handshake::{
    client_proof, derive_session_keys, ecdh_shared, encode_client_hello, key_salt,
    parse_server_finish, parse_server_hello, server_proof, transcript, AuthKind, AuthSelector,
    ServerFinish, CLIENT_FINISH_JSON, NONCE_BYTES, PUBLIC_KEY_BYTES, SECRET_BYTES,
};
use lerdr_e2ee::{Codec, Session};
use lerdr_relay::auth::{Credential, Role};
use lerdr_relay::frame::duplex::{self, DuplexIo, DuplexReader, DuplexWriter};
use lerdr_relay::frame::{FrameIo, FrameRead, FrameWrite};
use lerdr_relay::session::{ConnectionEnd, SessionConfig};
use lerdr_relay::store::MemoryAuthStore;
use p256::elliptic_curve::sec1::ToSec1Point;
use p256::elliptic_curve::Generate;
use p256::SecretKey;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// The client's half of a handshake, before the server hello arrives.
pub struct ClientHello {
    pub bytes: Vec<u8>,
    pub private: SecretKey,
    pub nonce: [u8; NONCE_BYTES],
    pub public_bytes: [u8; PUBLIC_KEY_BYTES],
}

/// Build a valid `e2ee_client_hello` for `selector`/`secret` (fresh key +
/// nonce, real proof).
pub fn client_hello(selector: &AuthSelector, secret: &[u8; SECRET_BYTES]) -> ClientHello {
    let private = SecretKey::generate();
    let public_bytes: [u8; PUBLIC_KEY_BYTES] = private
        .public_key()
        .to_sec1_point(false)
        .as_bytes()
        .try_into()
        .expect("P-256 uncompressed point");
    let nonce: [u8; NONCE_BYTES] = <[u8; NONCE_BYTES]>::generate();
    let proof = client_proof(secret, &selector.binding(), &nonce, &public_bytes);
    ClientHello {
        bytes: encode_client_hello(selector, &nonce, &public_bytes, &proof),
        private,
        nonce,
        public_bytes,
    }
}

/// Same, but the proof is computed against a different secret — the
/// proof-failure path.
pub fn client_hello_bad_proof(selector: &AuthSelector, secret: &[u8; SECRET_BYTES]) -> ClientHello {
    let mut wrong = *secret;
    wrong[0] ^= 0xFF;
    client_hello_with_proof_secret(selector, secret, &wrong)
}

fn client_hello_with_proof_secret(
    selector: &AuthSelector,
    _secret: &[u8; SECRET_BYTES],
    proof_secret: &[u8; SECRET_BYTES],
) -> ClientHello {
    let private = SecretKey::generate();
    let public_bytes: [u8; PUBLIC_KEY_BYTES] = private
        .public_key()
        .to_sec1_point(false)
        .as_bytes()
        .try_into()
        .expect("P-256 uncompressed point");
    let nonce: [u8; NONCE_BYTES] = <[u8; NONCE_BYTES]>::generate();
    let proof = client_proof(proof_secret, &selector.binding(), &nonce, &public_bytes);
    ClientHello {
        bytes: encode_client_hello(selector, &nonce, &public_bytes, &proof),
        private,
        nonce,
        public_bytes,
    }
}

/// Derive the client session after the server hello: verify the server
/// proof, run ECDH, build `Session::client`. Panics on a bad proof — a test
/// client that can't authenticate has nothing to test.
pub fn client_session(
    hello: &ClientHello,
    selector: &AuthSelector,
    secret: &[u8; SECRET_BYTES],
    raw_server_hello: &[u8],
    codec: Codec,
) -> Session {
    let server = parse_server_hello(raw_server_hello).expect("server hello parses");
    let transcript = transcript(
        &selector.binding(),
        &hello.nonce,
        &hello.public_bytes,
        &server.nonce,
        &server.public_bytes,
    );
    assert_eq!(
        server.proof,
        server_proof(secret, &transcript),
        "server proof must verify"
    );
    let shared = ecdh_shared(&hello.private, &server.public_key);
    let keys = derive_session_keys(&shared, &key_salt(secret, &transcript));
    Session::client(&keys, codec).expect("session keys are 32 bytes")
}

/// A completed client-side handshake: the live session plus the
/// `e2ee_server_finish` identity the relay committed.
pub struct ClientEstablished {
    pub session: Session,
    pub finish: ServerFinish,
}

/// A duplex-connected test client.
pub struct TestClient {
    pub reader: DuplexReader,
    pub writer: DuplexWriter,
}

impl TestClient {
    /// Pair `capacity` bytes of in-memory transport, JSON codec.
    pub fn pair(capacity: usize) -> (Self, DuplexIo) {
        let (client, server) = duplex::pair(capacity, Codec::Json);
        let (reader, writer) = client.split();
        (Self { reader, writer }, server)
    }

    /// Send the client hello only (steps 1-2 of the exchange).
    pub async fn send_hello(&mut self, hello: &ClientHello) {
        self.writer
            .write_frame(&hello.bytes)
            .await
            .expect("hello write");
    }

    /// Read the server hello and return the established client session
    /// WITHOUT sending the client finish — for commit-ordering tests.
    pub async fn advance_to_finish(
        &mut self,
        hello: &ClientHello,
        selector: &AuthSelector,
        secret: &[u8; SECRET_BYTES],
    ) -> Session {
        let raw = self.reader.read_frame().await.expect("server hello");
        client_session(hello, selector, secret, &raw, Codec::Json)
    }

    /// Send the sealed client finish, read the sealed server finish.
    pub async fn finish(&mut self, session: &mut Session) -> ServerFinish {
        let frame = session
            .seal(CLIENT_FINISH_JSON)
            .expect("seal client finish");
        self.writer.write_frame(&frame).await.expect("finish write");
        let raw = self.reader.read_frame().await.expect("server finish");
        let plaintext = session.open(&raw).expect("open server finish");
        parse_server_finish(&plaintext).expect("server finish parses")
    }

    /// The whole exchange in one call.
    pub async fn handshake(
        &mut self,
        selector: &AuthSelector,
        secret: &[u8; SECRET_BYTES],
    ) -> ClientEstablished {
        let hello = client_hello(selector, secret);
        self.send_hello(&hello).await;
        let mut session = self.advance_to_finish(&hello, selector, secret).await;
        let finish = self.finish(&mut session).await;
        ClientEstablished { session, finish }
    }

    /// Send one inbound plaintext through the session.
    pub async fn send_json(&mut self, session: &mut Session, plaintext: &[u8]) {
        let frame = session.seal(plaintext).expect("seal outbound");
        self.writer.write_frame(&frame).await.expect("frame write");
    }

    /// Read one frame, open it, parse the JSON envelope.
    pub async fn read_json(&mut self, session: &mut Session) -> serde_json::Value {
        let raw = self.reader.read_frame().await.expect("frame read");
        let plaintext = session.open(&raw).expect("open frame");
        serde_json::from_slice(&plaintext).expect("server sends JSON")
    }

    /// Read until the first JSON message whose `type` equals `want`.
    /// Deadline-bounded so a missing reply fails instead of hanging.
    pub async fn read_until_type(
        &mut self,
        session: &mut Session,
        want: &str,
    ) -> serde_json::Value {
        let deadline = tokio::time::sleep(std::time::Duration::from_secs(5));
        tokio::pin!(deadline);
        for _ in 0..16 {
            let value = tokio::select! {
                () = &mut deadline => panic!("timed out waiting for message type {want}"),
                value = self.read_json(session) => value,
            };
            if value["type"] == want {
                return value;
            }
        }
        panic!("never saw message type {want}");
    }
}

/// Seed a controller credential; returns `(selector, raw secret)`.
pub fn seed_credential(store: &MemoryAuthStore) -> (AuthSelector, [u8; SECRET_BYTES]) {
    let secret = [0xAB; SECRET_BYTES];
    store.add_credential(
        Credential {
            device_id: "device-1".to_owned(),
            credential_id: "cred-1".to_owned(),
            name: "phone".to_owned(),
            role: Role::Controller,
            locale: "en".to_owned(),
            paired_at_ms: 1,
            last_seen_at_ms: 0,
            version: 1,
            revoked: false,
        },
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret),
    );
    (
        AuthSelector::new(AuthKind::Credential, "cred-1", 1, "en"),
        secret,
    )
}

/// Mint an invitation; returns `(selector, raw secret)`.
pub fn seed_invitation(store: &MemoryAuthStore) -> (AuthSelector, [u8; SECRET_BYTES]) {
    let invitation = store.create_invitation("phone", Role::Controller, "en");
    let secret_vec = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&invitation.secret)
        .expect("store writes b64 secrets");
    let secret: [u8; SECRET_BYTES] = secret_vec.try_into().expect("32 bytes");
    (
        AuthSelector::new(
            AuthKind::Invitation,
            invitation.invitation_id.clone(),
            1,
            "en",
        ),
        secret,
    )
}

/// Spawn `serve_connection` over `io` against `store`; returns the join
/// handle plus a oneshot delivering the [`ClientSink`] once the session
/// registers. (A blocking `std::sync::mpsc` recv would stall the
/// current-thread test runtime — the server task lives on it too.)
pub fn serve(
    io: DuplexIo,
    store: Arc<MemoryAuthStore>,
    config: SessionConfig,
    parent: CancellationToken,
) -> (
    JoinHandle<ConnectionEnd>,
    tokio::sync::oneshot::Receiver<lerdr_relay::session::ClientSink>,
) {
    let (sink_tx, sink_rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(async move {
        lerdr_relay::session::serve_connection(
            io,
            &*store,
            &mut lerdr_relay::handshake::OsKeySource,
            lerdr_relay::router::StubRouter::new(),
            "client-1".to_owned(),
            config,
            parent,
            Some(Box::new(move |registration| {
                let _ = sink_tx.send(registration.sink);
            })),
        )
        .await
    });
    (handle, sink_rx)
}

/// A session config tuned for tests: no snapshot noise unless requested.
pub fn test_config() -> SessionConfig {
    SessionConfig {
        snapshot: Vec::new(),
        ..SessionConfig::default()
    }
}
