//! Real-socket test: the axum `/ws` endpoint over TCP with a tungstenite
//! client — subprotocol gate, the full encrypted handshake, one routed
//! action, `/healthz`, and graceful shutdown.

mod support;

use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use lerdr_core::protocol::ENCRYPTED_WEBSOCKET_SUBPROTOCOL;
use lerdr_e2ee::handshake::CLIENT_FINISH_JSON;
use lerdr_e2ee::Codec;
use lerdr_relay::store::MemoryAuthStore;
use lerdr_relay::Relay;
use support::*;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

async fn bound_relay(store: Arc<MemoryAuthStore>) -> (std::net::SocketAddr, Relay) {
    let relay = Relay::new(store);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serving = relay.clone();
    tokio::spawn(async move { serving.serve(listener).await });
    (addr, relay)
}

/// ws endpoint refuses a bare upgrade — no `herdr-e2ee-v2` subprotocol.
#[tokio::test]
async fn ws_requires_e2ee_subprotocol() {
    let store = Arc::new(MemoryAuthStore::new());
    let (addr, _relay) = bound_relay(store).await;

    let url = format!("ws://{addr}/ws");
    let request = url.into_client_request().expect("request");
    let err = tokio_tungstenite::connect_async(request)
        .await
        .expect_err("no subprotocol offered → refused");
    match err {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status(), 400);
        }
        other => panic!("expected HTTP refusal, got {other}"),
    }
}

/// A `token` query param is refused outright — auth lives in the handshake.
#[tokio::test]
async fn ws_refuses_token_auth() {
    let store = Arc::new(MemoryAuthStore::new());
    let (addr, _relay) = bound_relay(store).await;

    let url = format!("ws://{addr}/ws?token=deadbeef");
    let mut request = url.into_client_request().expect("request");
    request.headers_mut().append(
        "Sec-WebSocket-Protocol",
        ENCRYPTED_WEBSOCKET_SUBPROTOCOL.parse().unwrap(),
    );
    let err = tokio_tungstenite::connect_async(request)
        .await
        .expect_err("token param is refused");
    match err {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status(), 400);
        }
        other => panic!("expected HTTP refusal, got {other}"),
    }
}

/// Full credential handshake + one routed action over a real socket.
#[tokio::test]
async fn ws_end_to_end_handshake_and_receipt() {
    let store = Arc::new(MemoryAuthStore::new());
    let (selector, secret) = seed_credential(&store);
    let (addr, _relay) = bound_relay(Arc::clone(&store)).await;

    let url = format!("ws://{addr}/ws");
    let mut request = url.into_client_request().expect("request");
    request.headers_mut().append(
        "Sec-WebSocket-Protocol",
        ENCRYPTED_WEBSOCKET_SUBPROTOCOL.parse().unwrap(),
    );
    let (socket, response) = tokio_tungstenite::connect_async(request)
        .await
        .expect("upgrade succeeds");
    // The negotiated subprotocol is echoed back.
    assert_eq!(
        response
            .headers()
            .get("Sec-WebSocket-Protocol")
            .and_then(|v| v.to_str().ok()),
        Some(ENCRYPTED_WEBSOCKET_SUBPROTOCOL)
    );
    let (mut sink, mut stream) = socket.split();

    // Client hello — plaintext text frame.
    let hello = client_hello(&selector, &secret);
    sink.send(Message::Text(
        String::from_utf8(hello.bytes.clone()).unwrap().into(),
    ))
    .await
    .expect("hello write");

    // Server hello — plaintext text frame.
    let raw_hello = match stream.next().await {
        Some(Ok(Message::Text(t))) => t.as_str().as_bytes().to_vec(),
        other => panic!("expected text server hello, got {other:?}"),
    };
    let mut session = client_session(&hello, &selector, &secret, &raw_hello, Codec::Json);

    // Client finish — first sealed frame, still a text frame on the wire.
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
    let finish = lerdr_e2ee::handshake::parse_server_finish(
        &session.open(&raw_finish).expect("open finish"),
    )
    .expect("server finish parses");
    assert_eq!(finish.device_id, "device-1");

    // One routed action → dispatched_unknown receipt (the snapshot's
    // push_config may arrive first — read through it). `get_activity`
    // reaches the router; `device_list` would be session-intercepted.
    let action = session
        .seal(br#"{"type":"get_activity","protocol":3,"request_id":"req-ws"}"#)
        .unwrap();
    sink.send(Message::Text(String::from_utf8(action).unwrap().into()))
        .await
        .expect("action write");
    for _ in 0..16 {
        let raw = match stream.next().await {
            Some(Ok(Message::Text(t))) => t.as_str().as_bytes().to_vec(),
            other => panic!("expected text frame, got {other:?}"),
        };
        let value: serde_json::Value =
            serde_json::from_slice(&session.open(&raw).unwrap()).unwrap();
        if value["type"] == "action_receipt" {
            assert_eq!(value["request_id"], "req-ws");
            assert_eq!(value["receipt"]["phase"], "dispatched_unknown");
            return;
        }
    }
    panic!("never saw the action_receipt");
}

/// `/healthz` answers 200 without any auth dance.
#[tokio::test]
async fn healthz_answers() {
    let store = Arc::new(MemoryAuthStore::new());
    let (addr, _relay) = bound_relay(store).await;

    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf);
    assert!(text.starts_with("HTTP/1.1 200"), "{text}");
    assert!(text.contains("ok"), "{text}");
}
