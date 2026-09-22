//! Prompt credential revocation — `hub.DisconnectCredential`
//! (`internal/transport/ws.go:612-634`): after `revoke_device`/
//! `reset_devices` answers the requester, EVERY live session bound to a
//! destroyed credential gets the `CloseGoingAway`/"device credential
//! revoked" close — not just the requester. Peers are swept through the
//! server's credential→session index; the lazy `authorize` fence stays
//! only as fallback (`docs/10-spec-gaps.md`).
//!
//! These tests run the real `/ws` endpoint over TCP: a peer that never
//! sends another frame must still see the close — the bounded read IS
//! the promptness proof (a lazy fence would leave it connected forever).

mod support;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use lerdr_core::protocol::ENCRYPTED_WEBSOCKET_SUBPROTOCOL;
use lerdr_e2ee::handshake::{AuthKind, AuthSelector, CLIENT_FINISH_JSON, SECRET_BYTES};
use lerdr_e2ee::{Codec, Session};
use lerdr_relay::auth::{Credential, Role};
use lerdr_relay::store::MemoryAuthStore;
use lerdr_relay::Relay;
use support::*;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// A connected, handshaken WS client with its live e2ee session.
struct WsClient {
    sink: futures_util::stream::SplitSink<WsStream, Message>,
    stream: futures_util::stream::SplitStream<WsStream>,
    session: Session,
}

impl WsClient {
    async fn send_json(&mut self, plaintext: &[u8]) {
        let frame = self.session.seal(plaintext).expect("seal outbound");
        self.sink
            .send(Message::Text(String::from_utf8(frame).unwrap().into()))
            .await
            .expect("frame write");
    }

    async fn read_json(&mut self) -> serde_json::Value {
        let raw = match self.stream.next().await {
            Some(Ok(Message::Text(t))) => t.as_str().as_bytes().to_vec(),
            other => panic!("expected text frame, got {other:?}"),
        };
        let plaintext = self.session.open(&raw).expect("open frame");
        serde_json::from_slice(&plaintext).expect("server sends JSON")
    }

    /// Read until a message of type `want` arrives (skipping anything
    /// else), deadline-bounded so a missing reply fails instead of
    /// hanging.
    async fn read_until_type(&mut self, want: &str) -> serde_json::Value {
        let step = async {
            for _ in 0..16 {
                let value = self.read_json().await;
                if value["type"] == want {
                    return value;
                }
            }
            panic!("never saw message type {want}");
        };
        tokio::time::timeout(Duration::from_secs(5), step)
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {want}"))
    }

    /// The server's close frame — `(code, reason)`. Trailing data frames
    /// are skipped; the deadline is what makes "promptly" testable.
    async fn read_close(&mut self) -> (u16, String) {
        let step = async {
            loop {
                match self.stream.next().await {
                    Some(Ok(Message::Close(Some(frame)))) => {
                        return (u16::from(frame.code), frame.reason.to_string());
                    }
                    Some(Ok(_)) => continue,
                    other => panic!("expected close frame, got {other:?}"),
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(5), step)
            .await
            .expect("timed out waiting for the close frame")
    }
}

/// The full encrypted handshake over a real socket — hello, server hello,
/// finish, server finish.
async fn ws_connect(
    addr: SocketAddr,
    selector: &AuthSelector,
    secret: &[u8; SECRET_BYTES],
) -> WsClient {
    let url = format!("ws://{addr}/ws");
    let mut request = url.into_client_request().expect("request");
    request.headers_mut().append(
        "Sec-WebSocket-Protocol",
        ENCRYPTED_WEBSOCKET_SUBPROTOCOL.parse().unwrap(),
    );
    let (socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("upgrade succeeds");
    let (mut sink, mut stream) = socket.split();

    let hello = client_hello(selector, secret);
    sink.send(Message::Text(
        String::from_utf8(hello.bytes.clone()).unwrap().into(),
    ))
    .await
    .expect("hello write");
    let raw_hello = match stream.next().await {
        Some(Ok(Message::Text(t))) => t.as_str().as_bytes().to_vec(),
        other => panic!("expected text server hello, got {other:?}"),
    };
    let mut session = client_session(&hello, selector, secret, &raw_hello, Codec::Json);
    let finish_frame = session.seal(CLIENT_FINISH_JSON).unwrap();
    sink.send(Message::Text(
        String::from_utf8(finish_frame).unwrap().into(),
    ))
    .await
    .expect("finish write");
    let raw_finish = match stream.next().await {
        Some(Ok(Message::Text(t))) => t.as_str().as_bytes().to_vec(),
        other => panic!("expected text server finish, got {other:?}"),
    };
    lerdr_e2ee::handshake::parse_server_finish(&session.open(&raw_finish).expect("open finish"))
        .expect("server finish parses");
    WsClient {
        sink,
        stream,
        session,
    }
}

async fn bound_relay(store: Arc<MemoryAuthStore>) -> (SocketAddr, Relay) {
    let relay = Relay::new(store).with_session_config(test_config());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serving = relay.clone();
    tokio::spawn(async move { serving.serve(listener).await });
    (addr, relay)
}

/// Registration rides `on_connect` just past the handshake — poll until
/// the index reports `n` live sessions so the sweep cannot miss one.
async fn wait_for_clients(relay: &Relay, n: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while relay.connected_clients() != n {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("clients registered");
}

/// Seed a second controller credential; returns `(selector, raw secret)`.
fn seed_controller(
    store: &MemoryAuthStore,
    device: &str,
    cred: &str,
) -> (AuthSelector, [u8; SECRET_BYTES]) {
    let secret = [0xCD; SECRET_BYTES];
    store.add_credential(
        Credential {
            device_id: device.to_owned(),
            credential_id: cred.to_owned(),
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
        AuthSelector::new(AuthKind::Credential, cred, 1, "en"),
        secret,
    )
}

/// `DisconnectCredential` — the requester's `command_result`/`action_receipt`
/// land first, then the deferred GoingAway; the peer on the same credential
/// gets the identical close without lifting a finger.
#[tokio::test]
async fn revoke_device_disconnects_peer_sessions_promptly() {
    let store = Arc::new(MemoryAuthStore::new());
    let (selector, secret) = seed_credential(&store);
    // A second controller keeps `ErrLastController` out of the way.
    seed_controller(&store, "device-2", "cred-2");
    let (addr, relay) = bound_relay(Arc::clone(&store)).await;

    // Two sessions, ONE credential — a second app instance holding the
    // same pairing material.
    let mut a = ws_connect(addr, &selector, &secret).await;
    let mut b = ws_connect(addr, &selector, &secret).await;
    wait_for_clients(&relay, 2).await;

    a.send_json(
        br#"{"type":"revoke_device","protocol":3,"request_id":"req-a","action_id":"act-a","device_id":"device-1"}"#,
    )
    .await;

    // The requester's answer lands before any close —
    // `sendAuditedCommandResult` precedes the `time.AfterFunc` sweep.
    let result = a.read_until_type("command_result").await;
    assert_eq!(result["request_id"], "req-a");
    assert_eq!(result["ok"], true);
    assert_eq!(result["phase"], "completed");
    assert_eq!(result["data"]["device"]["device_id"], "device-1");
    assert_eq!(result["data"]["device"]["revoked"], true);
    let receipt = a.read_until_type("action_receipt").await;
    assert_eq!(receipt["receipt"]["phase"], "confirmed");

    // Then the deferred close — `conn.Close(CloseGoingAway,
    // "device credential revoked")` — for the requester…
    let (code, reason) = a.read_close().await;
    assert_eq!(code, 1001);
    assert_eq!(reason, "device credential revoked");

    // …and promptly for the peer. B never sent a frame; a lazy
    // `authorize` fence would hold it open forever.
    let (code, reason) = b.read_close().await;
    assert_eq!(code, 1001);
    assert_eq!(reason, "device credential revoked");

    relay.shutdown().cancel();
}

/// `disconnectCredentials` over the pre-reset `activeDeviceCredentials`
/// list — sessions on OTHER credentials die too, at their pre-reset
/// versions.
#[tokio::test]
async fn reset_devices_disconnects_sessions_on_every_credential() {
    let store = Arc::new(MemoryAuthStore::new());
    let (sel_a, sec_a) = seed_credential(&store);
    let (sel_c, sec_c) = seed_controller(&store, "device-2", "cred-2");
    let (addr, relay) = bound_relay(Arc::clone(&store)).await;

    let mut a = ws_connect(addr, &sel_a, &sec_a).await; // cred-1
    let mut c = ws_connect(addr, &sel_c, &sec_c).await; // cred-2
    wait_for_clients(&relay, 2).await;

    a.send_json(
        br#"{"type":"reset_devices","protocol":3,"request_id":"req-r","action_id":"act-r"}"#,
    )
    .await;

    let result = a.read_until_type("command_result").await;
    assert_eq!(result["ok"], true);
    assert_eq!(result["phase"], "completed");
    let receipt = a.read_until_type("action_receipt").await;
    assert_eq!(receipt["receipt"]["phase"], "confirmed");

    // Requester and the other credential's session alike.
    let (code, reason) = a.read_close().await;
    assert_eq!(code, 1001);
    assert_eq!(reason, "device credential revoked");
    let (code, reason) = c.read_close().await;
    assert_eq!(code, 1001);
    assert_eq!(reason, "device credential revoked");

    relay.shutdown().cancel();
}

/// Revoking a DIFFERENT credential disconnects its sessions — two of them
/// here — while the caller's own session stays up.
#[tokio::test]
async fn revoke_other_credential_kills_its_sessions_and_keeps_caller() {
    let store = Arc::new(MemoryAuthStore::new());
    let (sel_a, sec_a) = seed_credential(&store);
    let (sel_b, sec_b) = seed_controller(&store, "device-2", "cred-2");
    let (addr, relay) = bound_relay(Arc::clone(&store)).await;

    let mut a = ws_connect(addr, &sel_a, &sec_a).await;
    let mut b = ws_connect(addr, &sel_b, &sec_b).await;
    let mut c = ws_connect(addr, &sel_b, &sec_b).await;
    wait_for_clients(&relay, 3).await;

    a.send_json(
        br#"{"type":"revoke_device","protocol":3,"request_id":"req-o","device_id":"device-2"}"#,
    )
    .await;
    let result = a.read_until_type("command_result").await;
    assert_eq!(result["ok"], true);
    assert_eq!(result["data"]["device"]["device_id"], "device-2");
    assert_eq!(result["data"]["device"]["revoked"], true);
    let receipt = a.read_until_type("action_receipt").await;
    assert_eq!(receipt["receipt"]["phase"], "confirmed");

    // Both sessions bound to cred-2 are swept.
    let (code, reason) = b.read_close().await;
    assert_eq!(code, 1001);
    assert_eq!(reason, "device credential revoked");
    let (code, reason) = c.read_close().await;
    assert_eq!(code, 1001);
    assert_eq!(reason, "device credential revoked");

    // The caller's credential is untouched — its session keeps serving.
    a.send_json(br#"{"type":"device_list","protocol":3,"request_id":"req-l"}"#)
        .await;
    let list = a.read_until_type("command_result").await;
    assert_eq!(list["ok"], true);
    assert_eq!(list["data"]["devices"].as_array().unwrap().len(), 1);

    relay.shutdown().cancel();
}
