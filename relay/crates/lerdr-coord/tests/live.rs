//! Live coord ↔ Herdr integration — gated on `LERDR_LIVE=1` plus a
//! reachable socket (`HERDR_SOCKET_PATH` or the default path).
//!
//! Covers the coordinator slice end to end without the WS handshake (that
//! leg lives in `lerdr-relay/tests/ws_server.rs`):
//!
//! 1. `TopologyActor` consumes the real event stream → first `Synced`
//!    arrives → topology goes non-stale with a non-empty `version`.
//! 2. `compose_snapshot` emits `push_config`+`herdr_status`+`workspaces`+
//!    `agents` that serialize to valid wire envelopes.
//! 3. `pane.read` through the same `Client` the router holds returns real
//!    pane content when a pane exists.
//!
//! ```bash
//! LERDR_LIVE=1 cargo test -p lerdr-coord --test live -- --nocapture
//! ```

use std::time::Duration;

use lerdr_coord::{compose_snapshot, TopologyActor};
use lerdr_core::protocol::Outbound;
use lerdr_herdr::{Client, ReadFormat, ReadSource};
use tokio_util::sync::CancellationToken;

fn live_client() -> Option<Client> {
    if std::env::var("LERDR_LIVE").ok().as_deref() != Some("1") {
        eprintln!("LERDR_LIVE=1 not set — live coord test skipped");
        return None;
    }
    let client =
        Client::from_env().or_else(|| lerdr_herdr::default_socket_path().map(Client::unix));
    if client.is_none() {
        eprintln!("no Herdr socket found — live coord test skipped");
    }
    client
}

/// Wait until the projection goes non-stale (first `Synced`) or time out.
async fn await_fresh(handle: &lerdr_coord::TopologyHandle, timeout: Duration) -> bool {
    let mut rx = handle.topology.clone();
    tokio::time::timeout(timeout, async {
        loop {
            if !rx.borrow().stale {
                return true;
            }
            if rx.changed().await.is_err() {
                return false;
            }
        }
    })
    .await
    .unwrap_or(false)
}

#[tokio::test]
async fn topology_actor_projects_live_snapshot() {
    let Some(client) = live_client() else { return };
    let cancel = CancellationToken::new();
    let handle = TopologyActor::spawn(client, cancel.clone());

    assert!(
        await_fresh(&handle, Duration::from_secs(15)).await,
        "topology never synced — is Herdr serving the event stream?"
    );
    let topo = handle.topology.borrow().clone();
    assert!(!topo.snapshot.version.is_empty(), "snapshot has no version");
    assert!(
        !topo.snapshot.workspaces.is_empty() || !topo.snapshot.panes.is_empty(),
        "snapshot carries no topology entities"
    );

    cancel.cancel();
}

#[tokio::test]
async fn snapshot_frames_serialize() {
    let Some(client) = live_client() else { return };
    let cancel = CancellationToken::new();
    let handle = TopologyActor::spawn(client, cancel.clone());
    assert!(await_fresh(&handle, Duration::from_secs(15)).await);

    let topo = handle.topology.borrow().clone();
    let frames = compose_snapshot(&topo);
    assert!(frames.len() >= 4, "snapshot too small: {}", frames.len());

    // push_config is first and advertises capabilities + herdr_status.
    let Outbound::PushConfig(config) = &frames[0] else {
        panic!("first frame is not push_config");
    };
    let caps = config.capabilities.value().expect("capabilities present");
    assert!(caps.iter().any(|c| c == "structured_questions"));
    assert_eq!(
        config.herdr_status.server_version, topo.snapshot.version,
        "herdr_status must reflect the live server version"
    );

    for frame in &frames {
        let bytes = frame.encode();
        let back = Outbound::decode(&bytes).expect("round-trip decode");
        let _ = back;
    }

    cancel.cancel();
}

#[tokio::test]
async fn pane_read_round_trips() {
    let Some(client) = live_client() else { return };
    let cancel = CancellationToken::new();
    let handle = TopologyActor::spawn(client.clone(), cancel.clone());
    assert!(await_fresh(&handle, Duration::from_secs(15)).await);

    let pane_id = {
        let topo = handle.topology.borrow().clone();
        topo.snapshot
            .panes
            .first()
            .map(|p| p.pane_id.clone())
            .or_else(|| topo.snapshot.agents.first().map(|a| a.pane_id.clone()))
    };
    let Some(pane_id) = pane_id else {
        eprintln!("no panes in the live session — read path not exercised");
        cancel.cancel();
        return;
    };

    let read = client
        .pane_read(&pane_id, ReadSource::RecentUnwrapped, 50, ReadFormat::Text)
        .await
        .expect("pane.read refused on a live pane");
    assert_eq!(read.pane_id, pane_id);
    assert!(!read.text.is_empty() || read.revision > 0);

    cancel.cancel();
}
