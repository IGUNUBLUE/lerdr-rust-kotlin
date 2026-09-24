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

/// `client_caps` announces the client's set and gets the symmetric
/// reply: a `caps_update` carrying the server's advertised list — the
/// explicit negotiation ack. (An old relay answers `unknown_action`,
/// which the app ignores; this one declares its set back.)
#[tokio::test]
async fn client_caps_is_answered_with_server_caps_update() {
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
    // The very next frame must be the negotiation reply carrying the
    // server's advertised list.
    let reply = client.read_json(&mut session).await;
    assert_eq!(reply["type"], "caps_update");
    assert!(reply["capabilities"]
        .as_array()
        .expect("capabilities array")
        .iter()
        .any(|cap| cap == "focus"));
    let reply = client.read_until_type(&mut session, "error").await;
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
    // The caps frame announced `focus` (gate open) and drew the
    // `caps_update` negotiation reply — skip it for the focus receipt.
    let reply = client.read_until_type(&mut session, "action_receipt").await;
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

// ── Phase-5 Track-A families (docs/13 §1) ───────────────────────────────────

/// The six Track-A actions sit behind three independent capability gates.
/// Before any announcement every one of them is `capability_unsupported`,
/// and each reports the capability its family requires.
#[tokio::test]
async fn phase5_actions_gate_per_family_before_announcement() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, _sink_rx) =
        establish(store, test_config(), CancellationToken::new()).await;

    let cases: [(&str, &str); 6] = [
        ("pane_search", "pane_search"),
        ("pane_selection_read", "pane_search"),
        ("pane_link_resolve", "pane_links"),
        ("pane_link_activate", "pane_links"),
        ("layout_export", "layout"),
        ("layout_apply", "layout"),
    ];
    for (action, capability) in cases {
        let frame = format!(
            r#"{{"type":"{action}","protocol":3,"request_id":"req-{action}","target":{{"pane_id":"wE:p1"}}}}"#
        );
        client.send_json(&mut session, frame.as_bytes()).await;
        let reply = client.read_until_type(&mut session, "error").await;
        assert_eq!(reply["request_id"], format!("req-{action}"), "{action}");
        assert_eq!(reply["error"]["code"], "capability_unsupported", "{action}");
        assert_eq!(
            reply["error"]["args"]["capability"],
            serde_json::json!(capability),
            "{action}"
        );
        assert_eq!(
            reply["error"]["args"]["operation"],
            serde_json::json!(action),
            "{action}"
        );
    }

    drop(client);
    server.await.expect("server joins");
}

/// Announcing the three Track-A capabilities opens all six gates — the
/// stub router's `dispatched_unknown` receipt proves each action routed.
#[tokio::test]
async fn client_caps_with_phase5_caps_routes_all_track_a_actions() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, _sink_rx) =
        establish(store, test_config(), CancellationToken::new()).await;

    client
        .send_json(
            &mut session,
            br#"{"type":"client_caps","protocol":3,"capabilities":["pane_search","pane_links","layout"]}"#,
        )
        .await;
    for action in [
        "pane_search",
        "pane_selection_read",
        "pane_link_resolve",
        "pane_link_activate",
        "layout_export",
        "layout_apply",
    ] {
        let frame = format!(
            r#"{{"type":"{action}","protocol":3,"request_id":"req-{action}","action_id":"a-{action}","target":{{"pane_id":"wE:p1"}}}}"#
        );
        client.send_json(&mut session, frame.as_bytes()).await;
        let reply = client.read_until_type(&mut session, "action_receipt").await;
        assert_eq!(reply["request_id"], format!("req-{action}"), "{action}");
        assert_eq!(reply["receipt"]["phase"], "dispatched_unknown", "{action}");
    }

    drop(client);
    server.await.expect("server joins");
}

/// Families gate independently: `pane_links` alone opens only the link
/// pair — `pane_search` stays closed, `layout` stays closed.
#[tokio::test]
async fn phase5_families_gate_independently() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, _sink_rx) =
        establish(store, test_config(), CancellationToken::new()).await;

    client
        .send_json(
            &mut session,
            br#"{"type":"client_caps","protocol":3,"capabilities":["pane_links"]}"#,
        )
        .await;
    client
        .send_json(
            &mut session,
            br#"{"type":"pane_link_resolve","protocol":3,"request_id":"req-open","target":{"pane_id":"wE:p1"}}"#,
        )
        .await;
    let routed = client.read_until_type(&mut session, "action_receipt").await;
    assert_eq!(routed["receipt"]["phase"], "dispatched_unknown");

    for (action, capability) in [("pane_search", "pane_search"), ("layout_apply", "layout")] {
        let frame = format!(
            r#"{{"type":"{action}","protocol":3,"request_id":"req-{action}","target":{{"pane_id":"wE:p1"}}}}"#
        );
        client.send_json(&mut session, frame.as_bytes()).await;
        let denied = client.read_until_type(&mut session, "error").await;
        assert_eq!(
            denied["error"]["code"], "capability_unsupported",
            "{action}"
        );
        assert_eq!(
            denied["error"]["args"]["capability"],
            serde_json::json!(capability),
            "{action}"
        );
    }

    drop(client);
    server.await.expect("server joins");
}

// ── Phase-5 Track-B `convo_sub` (docs/13 §2.3) ───────────────────────────────

/// `subscribe_conversation`/`unsubscribe_conversation` ride the `convo_sub`
/// gate: denied before any announcement, each naming the capability it
/// requires.
#[tokio::test]
async fn convo_sub_actions_gate_before_announcement() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, _sink_rx) =
        establish(store, test_config(), CancellationToken::new()).await;

    for action in ["subscribe_conversation", "unsubscribe_conversation"] {
        let frame = format!(
            r#"{{"type":"{action}","protocol":3,"request_id":"req-{action}","target":{{"pane_id":"wE:p1"}}}}"#
        );
        client.send_json(&mut session, frame.as_bytes()).await;
        let reply = client.read_until_type(&mut session, "error").await;
        assert_eq!(reply["request_id"], format!("req-{action}"), "{action}");
        assert_eq!(reply["error"]["code"], "capability_unsupported", "{action}");
        assert_eq!(
            reply["error"]["args"]["capability"],
            serde_json::json!("convo_sub"),
            "{action}"
        );
        assert_eq!(
            reply["error"]["args"]["operation"],
            serde_json::json!(action),
            "{action}"
        );
    }

    drop(client);
    server.await.expect("server joins");
}

/// Announcing `convo_sub` opens both gates — the stub router's
/// `dispatched_unknown` receipts prove each action routed.
#[tokio::test]
async fn client_caps_with_convo_sub_routes_subscription_actions() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, _sink_rx) =
        establish(store, test_config(), CancellationToken::new()).await;

    client
        .send_json(
            &mut session,
            br#"{"type":"client_caps","protocol":3,"capabilities":["convo_sub"]}"#,
        )
        .await;
    for action in ["subscribe_conversation", "unsubscribe_conversation"] {
        let frame = format!(
            r#"{{"type":"{action}","protocol":3,"request_id":"req-{action}","action_id":"a-{action}","target":{{"pane_id":"wE:p1"}}}}"#
        );
        client.send_json(&mut session, frame.as_bytes()).await;
        let reply = client.read_until_type(&mut session, "action_receipt").await;
        assert_eq!(reply["request_id"], format!("req-{action}"), "{action}");
        assert_eq!(reply["receipt"]["phase"], "dispatched_unknown", "{action}");
    }

    drop(client);
    server.await.expect("server joins");
}

// ── Phase-5 Track-B `frame_zstd` (docs/13 §2.2) ───────────────────────────────

/// A watch-shaped `pane_content` — the kind a watch task pushes through
/// the sink — with a repetitive multi-KB body so zstd visibly wins.
fn pane_frame() -> lerdr_core::protocol::Outbound {
    lerdr_core::protocol::Outbound::PaneContent(Box::new(lerdr_core::protocol::PaneContent {
        r#type: "pane_content".to_owned(),
        pane_id: Some("wE:p1".to_owned()),
        content: Some("The quick brown fox jumps over the lazy dog. $ cargo test\n".repeat(64)),
        content_fingerprint: Some("0123456789abcdef".to_owned()),
        ack_required: Some(true),
        format: Some("text".to_owned()),
        ..Default::default()
    }))
}

/// Inflate a received `pane_content` frame back to its logical `content`
/// through the public decode path (`Outbound::decode` +
/// `PaneContent::decompress_payload`).
fn inflated_content(reply: &serde_json::Value) -> String {
    let bytes = serde_json::to_vec(reply).expect("frame re-serializes");
    let decoded = lerdr_core::protocol::Outbound::decode(&bytes).expect("typed decode");
    let lerdr_core::protocol::Outbound::PaneContent(mut message) = decoded else {
        panic!("expected pane_content");
    };
    message.decompress_payload().expect("payload inflates");
    message.content.expect("content restored")
}

/// Without the client announcing `frame_zstd`, a producer-pushed
/// `pane_content` rides the wire exactly as before — plaintext `content`,
/// no `encoding`/`payload`.
#[tokio::test]
async fn pane_content_stays_plaintext_without_frame_zstd() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, sink_rx) =
        establish(store, test_config(), CancellationToken::new()).await;
    let sink = sink_rx.await.expect("sink registered");

    sink.try_send(&pane_frame()).expect("sink push");
    let reply = client.read_until_type(&mut session, "pane_content").await;
    assert_eq!(reply["type"], "pane_content");
    assert!(
        reply["content"].as_str().is_some_and(|s| s.len() > 1024),
        "plaintext content rides the envelope"
    );
    assert!(reply.get("encoding").is_none());
    assert!(reply.get("payload").is_none());
    assert_eq!(reply["content_fingerprint"], "0123456789abcdef");

    drop(client);
    server.await.expect("server joins");
}

/// `frame_zstd` on both lists flips the encode path: the same pushed
/// `pane_content` arrives with the envelope intact and `content` folded
/// into `payload` — and the compressed form beats the raw content even
/// after base64.
#[tokio::test]
async fn frame_zstd_negotiation_compresses_pane_content() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, sink_rx) =
        establish(store, test_config(), CancellationToken::new()).await;
    let sink = sink_rx.await.expect("sink registered");

    client
        .send_json(
            &mut session,
            br#"{"type":"client_caps","protocol":3,"capabilities":["frame_zstd"]}"#,
        )
        .await;
    // The negotiation reply doubles as the barrier — reading it proves
    // the actor recorded the client set (and published the gate) before
    // the next sink push encodes.
    let reply = client.read_until_type(&mut session, "caps_update").await;
    assert!(reply["capabilities"]
        .as_array()
        .expect("capabilities array")
        .iter()
        .any(|cap| cap == "frame_zstd"));

    sink.try_send(&pane_frame()).expect("sink push");
    let reply = client.read_until_type(&mut session, "pane_content").await;
    assert_eq!(reply["type"], "pane_content");
    assert_eq!(reply["encoding"], "zstd");
    assert!(
        reply.get("content").is_none(),
        "content folded into payload"
    );
    // Envelope stays plaintext — routing/coalescing metadata untouched.
    assert_eq!(reply["pane_id"], "wE:p1");
    assert_eq!(reply["content_fingerprint"], "0123456789abcdef");
    assert_eq!(reply["ack_required"], true);
    let payload = reply["payload"].as_str().expect("base64 payload");
    let raw = "The quick brown fox jumps over the lazy dog. $ cargo test\n".repeat(64);
    assert!(
        payload.len() < raw.len(),
        "payload {} should beat raw content {}",
        payload.len(),
        raw.len()
    );
    assert_eq!(inflated_content(&reply), raw);

    // A second push stays compressed — the gate holds.
    sink.try_send(&pane_frame()).expect("sink push");
    let reply = client.read_until_type(&mut session, "pane_content").await;
    assert_eq!(reply["encoding"], "zstd");

    drop(client);
    server.await.expect("server joins");
}

/// The client retracts `frame_zstd` mid-session via its own `caps_update`
/// — the very next pushed frame encodes plaintext again.
#[tokio::test]
async fn client_caps_update_retraction_returns_plaintext() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, sink_rx) =
        establish(store, test_config(), CancellationToken::new()).await;
    let sink = sink_rx.await.expect("sink registered");

    client
        .send_json(
            &mut session,
            br#"{"type":"client_caps","protocol":3,"capabilities":["frame_zstd"]}"#,
        )
        .await;
    client.read_until_type(&mut session, "caps_update").await;
    sink.try_send(&pane_frame()).expect("sink push");
    let reply = client.read_until_type(&mut session, "pane_content").await;
    assert_eq!(reply["encoding"], "zstd");

    // Client-side retraction — absorbed silently, so the following
    // receipt is the barrier proving the actor processed it.
    client
        .send_json(&mut session, br#"{"type":"caps_update","capabilities":[]}"#)
        .await;
    client
        .send_json(
            &mut session,
            br#"{"type":"get_activity","protocol":3,"request_id":"req-barrier","action_id":"act-b"}"#,
        )
        .await;
    let barrier = client.read_until_type(&mut session, "action_receipt").await;
    assert_eq!(barrier["request_id"], "req-barrier");

    sink.try_send(&pane_frame()).expect("sink push");
    let reply = client.read_until_type(&mut session, "pane_content").await;
    assert!(reply.get("encoding").is_none());
    assert!(reply.get("payload").is_none());
    assert!(reply["content"].as_str().is_some_and(|s| s.len() > 1024));

    drop(client);
    server.await.expect("server joins");
}

/// A server-side `caps_update` that drops `frame_zstd` retracts the live
/// capability — the gate the sink encodes against follows the advertised
/// list the client was actually sent.
#[tokio::test]
async fn server_caps_update_retraction_returns_plaintext() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, sink_rx) =
        establish(store, test_config(), CancellationToken::new()).await;
    let sink = sink_rx.await.expect("sink registered");

    client
        .send_json(
            &mut session,
            br#"{"type":"client_caps","protocol":3,"capabilities":["frame_zstd"]}"#,
        )
        .await;
    client.read_until_type(&mut session, "caps_update").await;
    sink.try_send(&pane_frame()).expect("sink push");
    let reply = client.read_until_type(&mut session, "pane_content").await;
    assert_eq!(reply["encoding"], "zstd");

    // Server-side retraction — lerdr-coord pushes this when the advertised
    // set changes. Reading the frame proves `observe` already ran.
    sink.try_send(&lerdr_core::protocol::Outbound::CapsUpdate(
        lerdr_core::protocol::CapsUpdateMessage {
            r#type: "caps_update".to_owned(),
            capabilities: Some(lerdr_core::json::MaybeNull::Value(vec![
                "workspace_management".to_owned(),
            ])),
        },
    ))
    .expect("caps_update push");
    let update = client.read_until_type(&mut session, "caps_update").await;
    assert_eq!(
        update["capabilities"],
        serde_json::json!(["workspace_management"])
    );

    sink.try_send(&pane_frame()).expect("sink push");
    let reply = client.read_until_type(&mut session, "pane_content").await;
    assert!(reply.get("encoding").is_none());
    assert!(reply["content"].as_str().is_some_and(|s| s.len() > 1024));

    drop(client);
    server.await.expect("server joins");
}

/// Only `pane_content` transforms: `pane_delta` (spec: deltas already
/// compress well) and `pane_resync` (no payload member) ride plaintext
/// even while the gate is live.
#[tokio::test]
async fn non_content_pane_frames_never_compress() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, sink_rx) =
        establish(store, test_config(), CancellationToken::new()).await;
    let sink = sink_rx.await.expect("sink registered");

    client
        .send_json(
            &mut session,
            br#"{"type":"client_caps","protocol":3,"capabilities":["frame_zstd"]}"#,
        )
        .await;
    client.read_until_type(&mut session, "caps_update").await;

    sink.try_send(&lerdr_core::protocol::Outbound::PaneDelta(Box::new(
        lerdr_core::protocol::PaneDelta {
            r#type: "pane_delta".to_owned(),
            pane_id: Some("wE:p1".to_owned()),
            content_fingerprint: Some("deadbeefcafebabe".to_owned()),
            ..Default::default()
        },
    )))
    .expect("delta push");
    let reply = client.read_until_type(&mut session, "pane_delta").await;
    assert!(reply.get("encoding").is_none());
    assert!(reply.get("payload").is_none());

    sink.try_send(&lerdr_core::protocol::Outbound::PaneResync(
        lerdr_core::protocol::PaneResync {
            r#type: "pane_resync".to_owned(),
            pane_id: Some("wE:p1".to_owned()),
            ..Default::default()
        },
    ))
    .expect("resync push");
    let reply = client.read_until_type(&mut session, "pane_resync").await;
    assert!(reply.get("encoding").is_none());
    assert!(reply.get("payload").is_none());

    drop(client);
    server.await.expect("server joins");
}

// ── Phase-5 Track-B `upload_binary` (docs/13 §2.4) ────────────────────

/// A 32-char base64url opaque id — the shape `upload_begin` mints and
/// the `0x03` header carries verbatim.
const UPLOAD_ID: &str = "abcdefghijklmnopqrstuvwxyz123456";

/// A router that records binary chunks as the dispatch seam sees them
/// and answers with the same JSON `upload_chunk_result` the real
/// handler produces — `request_id:""`, `next_sequence` correlating.
/// JSON actions delegate to the stub so liveness barriers (an
/// `action_receipt`) work the same as in the other tests.
#[derive(Default)]
struct BinaryRecorder {
    chunks: Arc<std::sync::Mutex<Vec<lerdr_core::uploadbinary::BinaryChunk>>>,
    stub: lerdr_relay::router::StubRouter,
}

impl lerdr_relay::router::ActionRouter for BinaryRecorder {
    fn route(
        &mut self,
        ctx: &lerdr_relay::router::ClientContext<'_>,
        scope: &lerdr_core::protocol::RequestScope,
        message: &lerdr_core::protocol::Inbound,
    ) -> lerdr_relay::router::RouterReply {
        self.stub.route(ctx, scope, message)
    }

    fn route_binary_chunk(
        &mut self,
        _ctx: &lerdr_relay::router::ClientContext<'_>,
        chunk: lerdr_core::uploadbinary::BinaryChunk,
    ) -> lerdr_relay::router::RouterReply {
        let next_sequence = chunk.sequence + 1;
        self.chunks.lock().expect("chunks").push(chunk);
        let result = serde_json::value::RawValue::from_string(format!(
            r#"{{"file_index":0,"next_sequence":{next_sequence},"received_bytes":7}}"#
        ))
        .expect("result JSON");
        lerdr_relay::router::RouterReply::send(vec![
            lerdr_core::protocol::Outbound::UploadChunkResult(
                lerdr_core::protocol::UploadResultMessage {
                    r#type: "upload_chunk_result".to_owned(),
                    request_id: Some(String::new()),
                    result: Some(lerdr_core::json::MaybeNull::Value(
                        lerdr_core::json::RawJson(result),
                    )),
                    ..Default::default()
                },
            ),
        ])
    }
}

/// `establish` against a recording router; the session `client-1` id
/// matches `serve_with`'s label.
async fn establish_binary(
    store: Arc<MemoryAuthStore>,
    router: BinaryRecorder,
) -> (
    TestClient,
    lerdr_e2ee::Session,
    tokio::task::JoinHandle<ConnectionEnd>,
    tokio::sync::oneshot::Receiver<lerdr_relay::session::ClientSink>,
) {
    let (selector, secret) = seed_credential(&store);
    let (mut client, server_io) = TestClient::pair(64 * 1024);
    let (server, sink_rx) = serve_with(
        server_io,
        store,
        router,
        test_config(),
        CancellationToken::new(),
    );
    let established = client.handshake(&selector, &secret).await;
    (client, established.session, server, sink_rx)
}

/// A `0x03` frame from a client that never announced `upload_binary`
/// answers `capability_unsupported` — the gated-action reply shape with
/// an empty correlation — and the session survives (a mid-flight
/// `caps_update` retraction must not become a kill race).
#[tokio::test]
async fn binary_chunk_without_capability_is_capability_unsupported() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, _sink_rx) =
        establish_binary(store, BinaryRecorder::default()).await;

    let frame = lerdr_core::uploadbinary::encode_chunk(UPLOAD_ID, 0, b"raw").expect("frame");
    client.send_json(&mut session, &frame).await;
    let reply = client.read_until_type(&mut session, "error").await;
    // The carrier has no request id — the member is omitted, matching
    // the oracle's `request_id,omitempty` envelope.
    assert!(reply.get("request_id").is_none() || reply["request_id"].is_null());
    assert_eq!(reply["error"]["code"], "capability_unsupported");
    assert_eq!(reply["error"]["args"]["operation"], "upload_chunk");
    assert_eq!(reply["error"]["args"]["capability"], "upload_binary");

    // The connection is alive — a follow-up action routes normally.
    client
        .send_json(
            &mut session,
            br#"{"type":"get_activity","protocol":3,"request_id":"req-barrier"}"#,
        )
        .await;
    let barrier = client.read_until_type(&mut session, "action_receipt").await;
    assert_eq!(barrier["request_id"], "req-barrier");

    drop(client);
    server.await.expect("server joins");
}

/// Negotiated `upload_binary`: a well-formed `0x03` frame reaches the
/// router with the verbatim id, the BE64 sequence, and the raw bytes;
/// the ack rides back as JSON `upload_chunk_result` with an empty
/// `request_id`.
#[tokio::test]
async fn binary_chunk_routes_when_negotiated() {
    let recorder = BinaryRecorder::default();
    let seen = recorder.chunks.clone();
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, _sink_rx) = establish_binary(store, recorder).await;

    client
        .send_json(
            &mut session,
            br#"{"type":"client_caps","protocol":3,"capabilities":["upload_binary"]}"#,
        )
        .await;
    // The negotiation reply is the barrier: the actor recorded the
    // client set before the next inbound is dispatched.
    let reply = client.read_until_type(&mut session, "caps_update").await;
    assert!(reply["capabilities"]
        .as_array()
        .expect("capabilities array")
        .iter()
        .any(|cap| cap == "upload_binary"));

    let payload = b"raw bytes \x00\x01\xff";
    let frame = lerdr_core::uploadbinary::encode_chunk(UPLOAD_ID, 0, payload).expect("frame");
    client.send_json(&mut session, &frame).await;
    let reply = client
        .read_until_type(&mut session, "upload_chunk_result")
        .await;
    assert_eq!(reply["request_id"], "");
    assert_eq!(reply["result"]["next_sequence"], 1);

    let seen = seen.lock().expect("chunks").clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].upload_id, UPLOAD_ID);
    assert_eq!(seen[0].sequence, 0);
    assert_eq!(seen[0].data, payload);

    drop(client);
    server.await.expect("server joins");
}

/// A negotiated client that authors a malformed `0x03` — truncated
/// header or an id outside the base64url alphabet — committed a
/// protocol violation inside the authenticated channel: the connection
/// closes exactly like non-JSON plaintext does.
#[tokio::test]
async fn malformed_binary_chunk_evicts() {
    let recorder = BinaryRecorder::default();
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, _sink_rx) = establish_binary(store, recorder).await;

    client
        .send_json(
            &mut session,
            br#"{"type":"client_caps","protocol":3,"capabilities":["upload_binary"]}"#,
        )
        .await;
    client.read_until_type(&mut session, "caps_update").await;

    // The bare type byte — far short of the 41-byte header.
    client.send_json(&mut session, &[0x03]).await;
    let end = server.await.expect("server joins");
    assert!(matches!(
        end,
        ConnectionEnd::Evicted(EvictReason::MalformedMessage)
    ));
}

/// `chunk_encoding:"binary"` lands on `upload_begin_result` payloads
/// only while the capability is negotiated — the encode-time gate
/// follows the same announce/retract cadence as `frame_zstd`.
#[tokio::test]
async fn chunk_encoding_stamp_follows_negotiation() {
    let begin_result = || {
        lerdr_core::protocol::Outbound::UploadBeginResult(
            lerdr_core::protocol::UploadResultMessage {
                r#type: "upload_begin_result".to_owned(),
                request_id: Some("r1".to_owned()),
                result: Some(lerdr_core::json::MaybeNull::Value(
                    lerdr_core::json::RawJson(
                        serde_json::value::RawValue::from_string(
                            r#"{"upload_id":"u","chunk_bytes":262144,"limits":{}}"#.to_owned(),
                        )
                        .expect("result JSON"),
                    ),
                )),
                ..Default::default()
            },
        )
    };
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, sink_rx) =
        establish_binary(store, BinaryRecorder::default()).await;
    let sink = sink_rx.await.expect("sink registered");

    // No announcement — the result rides through untouched.
    sink.try_send(&begin_result()).expect("begin push");
    let reply = client
        .read_until_type(&mut session, "upload_begin_result")
        .await;
    assert!(reply["result"].get("chunk_encoding").is_none());

    // Negotiate — the next begin result reports the binary carrier.
    client
        .send_json(
            &mut session,
            br#"{"type":"client_caps","protocol":3,"capabilities":["upload_binary"]}"#,
        )
        .await;
    client.read_until_type(&mut session, "caps_update").await;
    sink.try_send(&begin_result()).expect("begin push");
    let reply = client
        .read_until_type(&mut session, "upload_begin_result")
        .await;
    assert_eq!(reply["result"]["chunk_encoding"], "binary");
    assert_eq!(reply["result"]["upload_id"], "u");

    // Retract mid-session — the stamp disappears again.
    client
        .send_json(&mut session, br#"{"type":"caps_update","capabilities":[]}"#)
        .await;
    client
        .send_json(
            &mut session,
            br#"{"type":"get_activity","protocol":3,"request_id":"req-barrier"}"#,
        )
        .await;
    client.read_until_type(&mut session, "action_receipt").await;
    sink.try_send(&begin_result()).expect("begin push");
    let reply = client
        .read_until_type(&mut session, "upload_begin_result")
        .await;
    assert!(reply["result"].get("chunk_encoding").is_none());

    drop(client);
    server.await.expect("server joins");
}

/// Error `upload_begin_result`s never carry the marker — a failed begin
/// stages nothing the client could stream into.
#[tokio::test]
async fn chunk_encoding_never_stamps_error_results() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, sink_rx) =
        establish_binary(store, BinaryRecorder::default()).await;
    let sink = sink_rx.await.expect("sink registered");

    client
        .send_json(
            &mut session,
            br#"{"type":"client_caps","protocol":3,"capabilities":["upload_binary"]}"#,
        )
        .await;
    client.read_until_type(&mut session, "caps_update").await;

    sink.try_send(&lerdr_core::protocol::Outbound::UploadBeginResult(
        lerdr_core::protocol::UploadResultMessage {
            r#type: "upload_begin_result".to_owned(),
            request_id: Some("r9".to_owned()),
            error: Some(lerdr_core::protocol::ApiError::new(
                "attachment_upload_busy",
                std::collections::BTreeMap::new(),
            )),
            ..Default::default()
        },
    ))
    .expect("error begin push");
    let reply = client
        .read_until_type(&mut session, "upload_begin_result")
        .await;
    assert_eq!(reply["error"]["code"], "attachment_upload_busy");
    assert!(reply.get("result").is_none() || reply["result"].is_null());

    drop(client);
    server.await.expect("server joins");
}
