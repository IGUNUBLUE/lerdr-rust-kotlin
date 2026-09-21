//! In-process handshake tests — the full `herdr-e2ee-v2` exchange over the
//! duplex transport: credential + invitation auth, commit ordering, proof
//! failure, unknown selector, and the 10-second timeout under paused time.

mod support;

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use lerdr_e2ee::handshake::{AuthKind, AuthSelector, SECRET_BYTES};
use lerdr_relay::frame::{FrameRead, ReadError};
use lerdr_relay::session::ConnectionEnd;
use lerdr_relay::store::MemoryAuthStore;
use support::*;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn credential_handshake_commits_identity() {
    let store = Arc::new(MemoryAuthStore::new());
    let (selector, secret) = seed_credential(&store);
    let (mut client, server_io) = TestClient::pair(64 * 1024);
    let (server, _sink_rx) = serve(
        server_io,
        Arc::clone(&store),
        test_config(),
        CancellationToken::new(),
    );

    let established = client.handshake(&selector, &secret).await;
    assert_eq!(established.finish.device_id, "device-1");
    assert_eq!(established.finish.credential_id, "cred-1");
    assert_eq!(established.finish.role, "controller");
    assert_eq!(established.finish.credential_version, 1);
    // Credential handshakes never carry a fresh secret.
    assert!(established.finish.credential_secret.is_none());
    // Liveness refreshed (seeded with last_seen_at_ms = 0).
    assert!(store.credentials()[0].last_seen_at_ms > 0);

    drop(client);
    let end = server.await.expect("server task joins");
    assert!(matches!(
        end,
        ConnectionEnd::PeerClosed { .. } | ConnectionEnd::TransportFailed
    ));
}

#[tokio::test]
async fn invitation_handshake_redeems_and_issues_credential() {
    let store = Arc::new(MemoryAuthStore::new());
    let (selector, secret) = seed_invitation(&store);
    let (mut client, server_io) = TestClient::pair(64 * 1024);
    let (server, _sink_rx) = serve(
        server_io,
        Arc::clone(&store),
        test_config(),
        CancellationToken::new(),
    );

    let established = client.handshake(&selector, &secret).await;
    // Redemption issued a fresh credential whose secret rides the finish.
    assert!(!established.finish.device_id.is_empty());
    assert!(!established.finish.credential_id.is_empty());
    assert_eq!(established.finish.credential_version, 1);
    let issued_secret = established
        .finish
        .credential_secret
        .expect("invitation issues a credential secret");
    let issued = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&issued_secret)
        .expect("credential_secret is b64");
    assert_eq!(issued.len(), SECRET_BYTES);
    // The store minted exactly that credential.
    let credentials = store.credentials();
    assert_eq!(credentials.len(), 1);
    assert_eq!(
        credentials[0].credential_id,
        established.finish.credential_id
    );

    drop(client);
    let end = server.await.expect("server task joins");
    assert!(matches!(
        end,
        ConnectionEnd::PeerClosed { .. } | ConnectionEnd::TransportFailed
    ));
}

#[tokio::test]
async fn invitation_not_redeemed_until_client_finish() {
    // Commit ordering: `complete(true)` fires only after the sealed client
    // finish opens — a client that stalls after the server hello must not
    // consume the invitation.
    let store = Arc::new(MemoryAuthStore::new());
    let (selector, secret) = seed_invitation(&store);
    let (mut client, server_io) = TestClient::pair(64 * 1024);
    let (server, _sink_rx) = serve(
        server_io,
        Arc::clone(&store),
        test_config(),
        CancellationToken::new(),
    );

    let hello = client_hello(&selector, &secret);
    client.send_hello(&hello).await;
    let mut session = client.advance_to_finish(&hello, &selector, &secret).await;

    // Server hello is out; the invitation must still be pending.
    assert!(store.credentials().is_empty(), "nothing committed yet");
    assert!(store.invitation().is_some());

    // Completing the exchange mints the credential exactly once.
    client.finish(&mut session).await;
    assert_eq!(store.credentials().len(), 1);

    drop(client);
    server.await.expect("server task joins");
}

#[tokio::test]
async fn bad_proof_rejects_with_4401_and_records_attempt() {
    let store = Arc::new(MemoryAuthStore::new());
    let (selector, secret) = seed_invitation(&store);
    let (mut client, server_io) = TestClient::pair(64 * 1024);
    let (server, _sink_rx) = serve(
        server_io,
        Arc::clone(&store),
        test_config(),
        CancellationToken::new(),
    );

    let hello = client_hello_bad_proof(&selector, &secret);
    client.send_hello(&hello).await;

    // Permanent refusal → graceful 4401 close, no server hello.
    let err = client
        .reader
        .read_frame()
        .await
        .expect_err("server closes after proof failure");
    match err {
        ReadError::Closed { code, .. } => {
            assert_eq!(code, Some(lerdr_relay::frame::UNAUTHORIZED_CLOSE_CODE))
        }
        other => panic!("expected close frame, got {other}"),
    }
    // The failed attempt was recorded against the invitation.
    assert_eq!(store.invitation().unwrap().failed_attempts, 1);
    assert!(store.credentials().is_empty());

    let end = server.await.expect("server task joins");
    assert!(matches!(end, ConnectionEnd::HandshakeFailed(e) if e.is_rejected()));
}

#[tokio::test]
async fn unknown_selector_rejects_with_4401() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, server_io) = TestClient::pair(64 * 1024);
    let (server, _sink_rx) = serve(
        server_io,
        Arc::clone(&store),
        test_config(),
        CancellationToken::new(),
    );

    let selector = AuthSelector::new(AuthKind::Credential, "nobody", 1, "en");
    let hello = client_hello(&selector, &[0x11; SECRET_BYTES]);
    client.send_hello(&hello).await;

    let err = client
        .reader
        .read_frame()
        .await
        .expect_err("unknown selector is refused");
    assert!(matches!(
        err,
        ReadError::Closed {
            code: Some(lerdr_relay::frame::UNAUTHORIZED_CLOSE_CODE),
            ..
        }
    ));
    let end = server.await.expect("server task joins");
    assert!(matches!(end, ConnectionEnd::HandshakeFailed(e) if e.is_rejected()));
}

#[tokio::test(start_paused = true)]
async fn handshake_times_out_at_ten_seconds() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, server_io) = TestClient::pair(64 * 1024);
    let (server, _sink_rx) = serve(
        server_io,
        Arc::clone(&store),
        test_config(),
        CancellationToken::new(),
    );

    // The client never speaks; virtual time drives the deadline.
    tokio::time::advance(Duration::from_secs(10)).await;
    let end = server.await.expect("server task joins");
    assert!(matches!(
        end,
        ConnectionEnd::HandshakeFailed(lerdr_relay::handshake::HandshakeError::Timeout)
    ));
    // A non-rejected failure drops the transport without a close frame —
    // the peer sees bare EOF.
    let err = client.reader.read_frame().await.expect_err("EOF");
    assert!(matches!(err, ReadError::Closed { code: None, .. }));
}
