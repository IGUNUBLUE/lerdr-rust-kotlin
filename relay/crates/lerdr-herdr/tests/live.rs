//! Live integration test against a real Herdr socket. Env-gated — never runs
//! by default:
//!
//! ```sh
//! HERDR_LIVE=1 HERDR_SOCKET_PATH=~/.config/herdr/herdr.sock \
//!     cargo test -p lerdr-herdr --test live
//! ```
//!
//! Verifies the real wire contract: `session.snapshot` decodes the live
//! topology, and an `events.subscribe` handshake yields `subscription_started`
//! plus (best-effort) at least one real event when the host is active.

use std::time::Duration;

use lerdr_herdr::{Client, EventStreamError, Subscription};

fn live_client() -> Option<Client> {
    if std::env::var("HERDR_LIVE").ok().as_deref() != Some("1") {
        return None;
    }
    let path = std::env::var_os("HERDR_SOCKET_PATH")
        .map(std::path::PathBuf::from)
        .or_else(lerdr_herdr::default_socket_path)?;
    Some(Client::unix(path))
}

#[tokio::test]
async fn live_snapshot_and_events() {
    let Some(client) = live_client() else {
        eprintln!("HERDR_LIVE not set — skipping live test");
        return;
    };

    let snap = client.session_snapshot().await.expect("session.snapshot");
    assert!(snap.protocol >= 1, "protocol {}", snap.protocol);
    assert!(!snap.version.is_empty());
    eprintln!(
        "live snapshot: version={} protocol={} workspaces={} tabs={} panes={} agents={}",
        snap.version,
        snap.protocol,
        snap.workspaces.len(),
        snap.tabs.len(),
        snap.panes.len(),
        snap.agents.len()
    );

    // Subscribe to a broad but valid set; the handshake must complete and
    // the stream must stay alive (no terminal error within a short window).
    let mut stream = client
        .subscribe_events(&[
            Subscription::Named("pane.updated"),
            Subscription::Named("pane.focused"),
            Subscription::Named("workspace.focused"),
        ])
        .await
        .expect("events.subscribe handshake");

    // Drain for ~2s: any events are a bonus; the contract being asserted is
    // that the stream delivers parsed events or stays silent — never errors.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut seen = 0usize;
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(300), stream.next_event()).await {
            Ok(Some(Ok(event))) => {
                seen += 1;
                eprintln!("event: {} {:?}", event.name, event.data);
            }
            Ok(Some(Err(err))) => panic!("live stream terminated: {err}"),
            Ok(None) => break,
            Err(_) => {} // idle window — healthy silence
        }
    }
    eprintln!("live events seen: {seen}");
    drop(stream);
}

#[tokio::test]
async fn live_bootstrap() {
    let Some(client) = live_client() else {
        eprintln!("HERDR_LIVE not set — skipping live test");
        return;
    };
    let boot = client
        .bootstrap(&[
            Subscription::Named("pane.updated"),
            Subscription::Named("workspace.focused"),
        ])
        .await
        .expect("bootstrap");
    assert!(!boot.snapshot.version.is_empty());
    // The subscription stays live after the snapshot.
    assert!(!boot.stream.is_done());
    drop(boot.stream);
}

/// Topology subscription incl. the `workspace.reordered` capability probe —
/// against the real server this validates the fallback probe result is read
/// back coherently.
#[tokio::test]
async fn live_topology_subscribe() {
    let Some(client) = live_client() else {
        eprintln!("HERDR_LIVE not set — skipping live test");
        return;
    };
    let stream = client
        .subscribe_topology()
        .await
        .expect("topology subscribe");
    let supported = client.workspace_reordered_supported();
    eprintln!("workspace.reordered supported: {supported:?}");
    assert_eq!(
        supported,
        Some(true),
        "herdr 0.9.1 supports workspace.reordered"
    );
    drop(stream);
    // Sanity: EventStreamError is in scope for the API surface check.
    let _ = EventStreamError::Lagged;
}
