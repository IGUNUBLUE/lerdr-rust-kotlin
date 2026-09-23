//! Session-actor tests over the duplex transport: the dispatch gates
//! (unknown action, incompatible protocol, invalid request, malformed
//! plaintext), send-buffer eviction, writer-timeout eviction, and
//! structured shutdown.

mod support;

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use lerdr_relay::frame::{FrameRead, FrameWrite, ReadError};
use lerdr_relay::session::{ConnectionEnd, EvictReason, OutboundPush, SessionConfig};
use lerdr_relay::store::MemoryAuthStore;
use support::*;
use tokio_util::sync::CancellationToken;

/// Establish a credential session; returns the client, the server join
/// handle, and the registered sink receiver.
async fn establish(
    store: Arc<MemoryAuthStore>,
    config: SessionConfig,
    parent: CancellationToken,
) -> (
    TestClient,
    lerdr_e2ee::Session,
    tokio::task::JoinHandle<ConnectionEnd>,
    tokio::sync::oneshot::Receiver<lerdr_relay::session::ClientSink>,
) {
    let (selector, secret) = seed_credential(&store);
    let (mut client, server_io) = TestClient::pair(64 * 1024);
    let (server, sink_rx) = serve(server_io, store, config, parent);
    let established = client.handshake(&selector, &secret).await;
    (client, established.session, server, sink_rx)
}

#[tokio::test]
async fn routed_action_gets_dispatched_unknown_receipt() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, _sink_rx) =
        establish(store, test_config(), CancellationToken::new()).await;

    // `get_activity` reaches the stub router (device-admin actions are
    // intercepted in the session layer — covered by device_admin.rs).
    client
        .send_json(
            &mut session,
            br#"{"type":"get_activity","protocol":3,"request_id":"req-1","action_id":"act-9"}"#,
        )
        .await;
    let reply = client.read_until_type(&mut session, "action_receipt").await;
    assert_eq!(reply["request_id"], "req-1");
    assert_eq!(reply["receipt"]["action_id"], "act-9");
    assert_eq!(reply["receipt"]["phase"], "dispatched_unknown");
    assert!(reply["receipt"]["error"].is_null());

    drop(client);
    server.await.expect("server joins");
}

#[tokio::test]
async fn unknown_action_gets_error_envelope() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, _sink_rx) =
        establish(store, test_config(), CancellationToken::new()).await;

    client
        .send_json(
            &mut session,
            br#"{"type":"nonsense_action","protocol":3,"request_id":"req-2"}"#,
        )
        .await;
    let reply = client.read_until_type(&mut session, "error").await;
    assert_eq!(reply["request_id"], "req-2");
    assert_eq!(reply["error"]["code"], "unknown_action");
    assert_eq!(reply["error"]["args"]["operation"], "nonsense_action");

    drop(client);
    server.await.expect("server joins");
}

#[tokio::test]
async fn incompatible_protocol_gets_failed_before_dispatch_receipt() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, _sink_rx) =
        establish(store, test_config(), CancellationToken::new()).await;

    // `send_text` is protocol-gated; version 2 must bounce.
    client
        .send_json(
            &mut session,
            br#"{"type":"send_text","protocol":2,"request_id":"req-3","action_id":"act-3","text":"hi"}"#,
        )
        .await;
    let reply = client.read_until_type(&mut session, "action_receipt").await;
    assert_eq!(reply["request_id"], "req-3");
    assert_eq!(reply["receipt"]["action_id"], "act-3");
    assert_eq!(reply["receipt"]["phase"], "failed_before_dispatch");
    assert_eq!(reply["receipt"]["error"]["code"], "incompatible_protocol");
    assert_eq!(reply["receipt"]["error"]["args"]["received"], "2");
    assert_eq!(reply["receipt"]["error"]["args"]["required"], "3");

    drop(client);
    server.await.expect("server joins");
}

#[tokio::test]
async fn undecodable_object_gets_invalid_request() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, _sink_rx) =
        establish(store, test_config(), CancellationToken::new()).await;

    // A JSON object with no `type` fails typed decode → invalid_request.
    client
        .send_json(&mut session, br#"{"hello":"world","request_id":"req-4"}"#)
        .await;
    let reply = client.read_until_type(&mut session, "error").await;
    assert_eq!(reply["error"]["code"], "invalid_request");

    drop(client);
    server.await.expect("server joins");
}

#[tokio::test]
async fn non_object_plaintext_evicts() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, _sink_rx) =
        establish(store, test_config(), CancellationToken::new()).await;

    client.send_json(&mut session, b"[1,2,3]").await;
    let end = server.await.expect("server joins");
    assert!(matches!(
        end,
        ConnectionEnd::Evicted(EvictReason::MalformedMessage)
    ));
    // Eviction uses the normal close handshake — the peer sees code 1000.
    let err = client.reader.read_frame().await.expect_err("closed");
    assert!(matches!(
        err,
        ReadError::Closed {
            code: Some(1000),
            ..
        }
    ));
}

#[tokio::test]
async fn undecryptable_frame_evicts() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, _session, server, _sink_rx) =
        establish(store, test_config(), CancellationToken::new()).await;

    // Garbage ciphertext — the AEAD open fails, the client is evicted.
    client
        .writer
        .write_frame(b"bm90LWludmFsaWQ") // base64-looking junk
        .await
        .expect("write");
    let end = server.await.expect("server joins");
    assert!(matches!(
        end,
        ConnectionEnd::Evicted(EvictReason::DecryptFailed)
    ));
}

#[tokio::test]
async fn send_buffer_overflow_evicts_lagging_client() {
    // Buffer admits 2 items; the snapshot pushes 3 non-replaceable
    // `push_config` envelopes — the third is rejected and the client is
    // evicted rather than silently dropping queued data.
    let store = Arc::new(MemoryAuthStore::new());
    let config = SessionConfig {
        send_buffer_items: 2,
        snapshot: vec![
            lerdr_relay::session::default_snapshot()
                .into_iter()
                .next()
                .unwrap(),
            lerdr_relay::session::default_snapshot()
                .into_iter()
                .next()
                .unwrap(),
            lerdr_relay::session::default_snapshot()
                .into_iter()
                .next()
                .unwrap(),
        ],
        ..SessionConfig::default()
    };
    let (selector, secret) = seed_credential(&store);
    let (mut client, server_io) = TestClient::pair(64 * 1024);
    let (server, _sink_rx) = serve(server_io, store, config, CancellationToken::new());
    let _established = client.handshake(&selector, &secret).await;

    let end = server.await.expect("server joins");
    assert!(matches!(
        end,
        ConnectionEnd::Evicted(EvictReason::SendBufferFull(
            lerdr_core::sendbuffer::RejectReason::ItemLimit
        ))
    ));
}

#[tokio::test]
async fn producer_pushes_reach_the_client() {
    // The registry path: `ClientSink::try_push` → actor buffer → sealed →
    // socket. Producers use the sink captured at on_connect.
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, sink_rx) =
        establish(store, test_config(), CancellationToken::new()).await;
    let sink = sink_rx.await.expect("on_connect registered the sink");

    sink.try_send(&lerdr_relay::session::default_snapshot()[0])
        .expect("sink push");
    let reply = client.read_until_type(&mut session, "push_config").await;
    assert_eq!(reply["protocol"], 3);

    drop(client);
    server.await.expect("server joins");
}

#[tokio::test(start_paused = true)]
async fn stalled_writer_times_out_and_drops_transport() {
    // A peer that stops reading stalls the socket write; the per-write
    // deadline ends the session as a transport failure (no close frame).
    let store = Arc::new(MemoryAuthStore::new());
    let config = SessionConfig {
        send_timeout: Duration::from_secs(2),
        snapshot: Vec::new(),
        ..SessionConfig::default()
    };
    let (selector, secret) = seed_credential(&store);
    // A tiny duplex buffer: the first oversized frame jams the writer.
    let (mut client, server_io) = TestClient::pair(1024);
    let (server, sink_rx) = serve(
        server_io,
        Arc::clone(&store),
        config,
        CancellationToken::new(),
    );
    let _established = client.handshake(&selector, &secret).await;
    let sink = sink_rx.await.expect("sink registered");

    // 8 KiB plaintext — bigger than the 1 KiB duplex buffer — so the
    // writer blocks mid-frame with the peer not reading.
    sink.try_push(OutboundPush {
        data: vec![b'x'; 8 * 1024],
        kind: "agents".to_owned(),
        replaceable: false,
    })
    .expect("push");
    tokio::time::advance(Duration::from_secs(3)).await;
    let end = server.await.expect("server joins");
    assert!(matches!(end, ConnectionEnd::TransportFailed));
}

// ── Phase-5 capability negotiation (docs/13 §0) ────────────────────────────

/// `client_caps` announces the client's set and is absorbed silently —
/// the app's contract with old relays is `unknown_action` noise, with new
/// relays it is no reply at all.
#[tokio::test]
async fn client_caps_is_absorbed_without_reply() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, _sink_rx) =
        establish(store, test_config(), CancellationToken::new()).await;

    client
        .send_json(
            &mut session,
            br#"{"type":"client_caps","protocol":3,"request_id":"caps-1","capabilities":["focus"],"preferred_inner_codec":"binary-v1"}"#,
        )
        .await;
    client
        .send_json(
            &mut session,
            br#"{"type":"nonsense_action","protocol":3,"request_id":"req-2"}"#,
        )
        .await;
    // The very next frame must be the nonsense action's answer — any
    // reply to caps-1 would arrive first.
    let reply = client.read_json(&mut session).await;
    assert_eq!(reply["type"], "error");
    assert_eq!(reply["request_id"], "req-2");

    drop(client);
    server.await.expect("server joins");
}

/// A capability-gated action before any announcement: nothing is live.
#[tokio::test]
async fn gated_action_before_client_caps_is_capability_unsupported() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, _sink_rx) =
        establish(store, test_config(), CancellationToken::new()).await;

    client
        .send_json(
            &mut session,
            br#"{"type":"focus_workspace","protocol":3,"request_id":"req-1","target":{"workspace_id":"wE"}}"#,
        )
        .await;
    let reply = client.read_until_type(&mut session, "error").await;
    assert_eq!(reply["request_id"], "req-1");
    assert_eq!(reply["error"]["code"], "capability_unsupported");
    assert_eq!(reply["error"]["args"]["operation"], "focus_workspace");
    assert_eq!(reply["error"]["args"]["capability"], "focus");

    drop(client);
    server.await.expect("server joins");
}

/// Announced, but without `focus`: the intersection stays empty.
#[tokio::test]
async fn client_caps_without_focus_keeps_gate_closed() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, _sink_rx) =
        establish(store, test_config(), CancellationToken::new()).await;

    client
        .send_json(
            &mut session,
            br#"{"type":"client_caps","protocol":3,"capabilities":["workspace_management"]}"#,
        )
        .await;
    client
        .send_json(
            &mut session,
            br#"{"type":"focus_pane","protocol":3,"request_id":"req-1","pane_id":"wE:p1","target":{"pane_id":"wE:p1"}}"#,
        )
        .await;
    let reply = client.read_until_type(&mut session, "error").await;
    assert_eq!(reply["error"]["code"], "capability_unsupported");
    assert_eq!(reply["error"]["args"]["capability"], "focus");

    drop(client);
    server.await.expect("server joins");
}

/// `focus` on both lists routes the action to the router — the stub's
/// `dispatched_unknown` receipt proves it passed every gate.
#[tokio::test]
async fn client_caps_with_focus_routes_gated_actions() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, _sink_rx) =
        establish(store, test_config(), CancellationToken::new()).await;

    client
        .send_json(
            &mut session,
            br#"{"type":"client_caps","protocol":3,"capabilities":["focus"]}"#,
        )
        .await;
    for action in ["focus_workspace", "focus_tab", "focus_agent", "focus_pane"] {
        let target = match action {
            "focus_workspace" => r#"{"workspace_id":"wE"}"#,
            "focus_tab" => r#"{"pane_id":"wE:p1","tab_id":"wE:p1:t2"}"#,
            "focus_agent" => r#"{"agent_session_id":"wE:a3"}"#,
            _ => r#"{"pane_id":"wE:p1"}"#,
        };
        let frame = format!(
            r#"{{"type":"{action}","protocol":3,"request_id":"req-{action}","action_id":"a-{action}","target":{target}}}"#
        );
        client.send_json(&mut session, frame.as_bytes()).await;
        let reply = client.read_until_type(&mut session, "action_receipt").await;
        assert_eq!(reply["request_id"], format!("req-{action}"), "{action}");
        assert_eq!(reply["receipt"]["phase"], "dispatched_unknown", "{action}");
    }

    drop(client);
    server.await.expect("server joins");
}

/// The client's own `caps_update` re-announces — a mid-session capability
/// gain on the client side opens the gate just like `client_caps`.
#[tokio::test]
async fn inbound_caps_update_reannounces_client_set() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, _sink_rx) =
        establish(store, test_config(), CancellationToken::new()).await;

    client
        .send_json(
            &mut session,
            br#"{"type":"client_caps","protocol":3,"capabilities":[]}"#,
        )
        .await;
    client
        .send_json(
            &mut session,
            br#"{"type":"focus_workspace","protocol":3,"request_id":"req-deny","target":{"workspace_id":"wE"}}"#,
        )
        .await;
    let denied = client.read_until_type(&mut session, "error").await;
    assert_eq!(denied["error"]["code"], "capability_unsupported");

    // The flip: the client announces `focus` mid-session.
    client
        .send_json(
            &mut session,
            br#"{"type":"caps_update","capabilities":["focus"]}"#,
        )
        .await;
    client
        .send_json(
            &mut session,
            br#"{"type":"focus_workspace","protocol":3,"request_id":"req-ok","action_id":"a-1","target":{"workspace_id":"wE"}}"#,
        )
        .await;
    let reply = client.read_until_type(&mut session, "action_receipt").await;
    assert_eq!(reply["request_id"], "req-ok");
    assert_eq!(reply["receipt"]["phase"], "dispatched_unknown");

    drop(client);
    server.await.expect("server joins");
}

/// A server-side `caps_update` that drops `focus` retracts the live
/// capability on this session — the gate follows the advertised list the
/// client was actually sent (the actor observes the frame at enqueue).
#[tokio::test]
async fn server_caps_update_retracts_live_capability() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, sink_rx) =
        establish(store, test_config(), CancellationToken::new()).await;
    let sink = sink_rx.await.expect("sink registered");

    client
        .send_json(
            &mut session,
            br#"{"type":"client_caps","protocol":3,"capabilities":["focus"]}"#,
        )
        .await;
    client
        .send_json(
            &mut session,
            br#"{"type":"focus_workspace","protocol":3,"request_id":"req-ok","target":{"workspace_id":"wE"}}"#,
        )
        .await;
    let reply = client.read_until_type(&mut session, "action_receipt").await;
    assert_eq!(reply["receipt"]["phase"], "dispatched_unknown");

    // Mid-session retraction — lerdr-coord pushes this when Herdr
    // evidence refutes the whole focus family.
    sink.try_send(&lerdr_core::protocol::Outbound::CapsUpdate(
        lerdr_core::protocol::CapsUpdateMessage {
            r#type: "caps_update".to_owned(),
            capabilities: Some(lerdr_core::json::MaybeNull::Value(vec![
                "workspace_management".to_owned(),
            ])),
        },
    ))
    .expect("caps_update push");
    // Reading the frame proves the actor already observed it — the
    // observe step runs before the buffer push.
    let update = client.read_until_type(&mut session, "caps_update").await;
    assert_eq!(
        update["capabilities"],
        serde_json::json!(["workspace_management"])
    );

    client
        .send_json(
            &mut session,
            br#"{"type":"focus_workspace","protocol":3,"request_id":"req-deny","target":{"workspace_id":"wE"}}"#,
        )
        .await;
    let denied = client.read_until_type(&mut session, "error").await;
    assert_eq!(denied["request_id"], "req-deny");
    assert_eq!(denied["error"]["code"], "capability_unsupported");

    drop(client);
    server.await.expect("server joins");
}

/// `client_caps` is protocol-ungated by design — the negotiation frame
/// must reach a session regardless of the client's declared version.
#[tokio::test]
async fn client_caps_is_protocol_ungated() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, _sink_rx) =
        establish(store, test_config(), CancellationToken::new()).await;

    // No `protocol` field at all — a read-classified action passes the
    // compatibility check unconditionally.
    client
        .send_json(
            &mut session,
            br#"{"type":"client_caps","request_id":"caps-1","capabilities":["focus"]}"#,
        )
        .await;
    client
        .send_json(
            &mut session,
            br#"{"type":"focus_workspace","protocol":3,"request_id":"req-ok","target":{"workspace_id":"wE"}}"#,
        )
        .await;
    // First frame is the focus receipt — the caps frame announced
    // `focus` (gate open) and answered nothing itself.
    let reply = client.read_json(&mut session).await;
    assert_eq!(reply["type"], "action_receipt");
    assert_eq!(reply["request_id"], "req-ok");
    assert_eq!(reply["receipt"]["phase"], "dispatched_unknown");

    drop(client);
    server.await.expect("server joins");
}

/// Role gate still applies after negotiation: a reader with `focus`
/// announced is denied at authorization, not capability.
#[tokio::test]
async fn reader_role_denied_after_capability_gate() {
    let store = Arc::new(MemoryAuthStore::new());
    store.add_credential(
        lerdr_relay::auth::Credential {
            device_id: "device-reader".to_owned(),
            credential_id: "cred-reader".to_owned(),
            name: "reader".to_owned(),
            role: lerdr_relay::auth::Role::Reader,
            locale: "en".to_owned(),
            paired_at_ms: 1,
            last_seen_at_ms: 0,
            version: 1,
            revoked: false,
        },
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([0xCD; 32]),
    );
    let selector = lerdr_e2ee::handshake::AuthSelector::new(
        lerdr_e2ee::handshake::AuthKind::Credential,
        "cred-reader",
        1,
        "en",
    );
    let secret = [0xCD; 32];
    let (mut client, server_io) = TestClient::pair(64 * 1024);
    let (server, _sink_rx) = serve(server_io, store, test_config(), CancellationToken::new());
    let established = client.handshake(&selector, &secret).await;
    let mut session = established.session;

    client
        .send_json(
            &mut session,
            br#"{"type":"client_caps","protocol":3,"capabilities":["focus"]}"#,
        )
        .await;
    client
        .send_json(
            &mut session,
            br#"{"type":"focus_workspace","protocol":3,"request_id":"req-1","target":{"workspace_id":"wE"}}"#,
        )
        .await;
    let reply = client.read_until_type(&mut session, "error").await;
    assert_eq!(reply["error"]["code"], "reader_denied");
    assert_eq!(reply["error"]["args"]["operation"], "focus_workspace");

    drop(client);
    server.await.expect("server joins");
}

#[tokio::test]
async fn shutdown_cancellation_ends_session_with_going_away() {
    let store = Arc::new(MemoryAuthStore::new());
    let parent = CancellationToken::new();
    let (mut client, _session, server, _sink_rx) =
        establish(store, test_config(), parent.clone()).await;

    parent.cancel();
    let end = server.await.expect("server joins");
    assert!(matches!(end, ConnectionEnd::Shutdown));
    // The peer sees a graceful going-away close (1001).
    let err = client.reader.read_frame().await.expect_err("closed");
    assert!(matches!(
        err,
        ReadError::Closed {
            code: Some(1001),
            ..
        }
    ));
}

#[tokio::test]
async fn peer_close_reports_code_and_reason() {
    let store = Arc::new(MemoryAuthStore::new());
    let (selector, secret) = seed_credential(&store);
    let (mut client, server_io) = TestClient::pair(64 * 1024);
    let (server, _sink_rx) = serve(
        server_io,
        Arc::clone(&store),
        test_config(),
        CancellationToken::new(),
    );
    let _established = client.handshake(&selector, &secret).await;

    // The client initiates a graceful close with code 1000.
    client
        .writer
        .close(lerdr_relay::frame::CloseStatus::Normal, "bye")
        .await;
    let end = server.await.expect("server joins");
    assert!(matches!(
        end,
        ConnectionEnd::PeerClosed {
            code: Some(1000),
            ..
        }
    ));
}
