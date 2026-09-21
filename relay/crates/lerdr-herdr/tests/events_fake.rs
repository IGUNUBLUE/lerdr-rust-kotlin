//! Event path against the fake server: handshake, stream parsing, terminal
//! errors, bootstrap gap draining, and the supervisor resync loop.

mod support;

use std::time::Duration;

use serde_json::json;
use support::{snapshot_result, subscription_started_line, Action, FakeHerdr};

use lerdr_herdr::{
    Backoff, Client, EventStreamError, EventSupervisor, Subscription, SupervisorSignal,
};

fn client_for(server: &FakeHerdr) -> Client {
    Client::unix(server.sock_path.clone())
}

/// `stream.next_event()` bounded — a broken stream fails the test instead of
/// hanging the suite.
async fn next_event(
    stream: &mut lerdr_herdr::EventStream,
) -> Option<Result<lerdr_herdr::Event, EventStreamError>> {
    tokio::time::timeout(Duration::from_secs(10), stream.next_event())
        .await
        .expect("event stream wait timed out")
}

/// `signals.next_signal()` bounded — same reason.
async fn next_signal(signals: &mut lerdr_herdr::SupervisorStream) -> Option<SupervisorSignal> {
    tokio::time::timeout(Duration::from_secs(10), signals.next_signal())
        .await
        .expect("supervisor signal wait timed out")
}

/// Server action: subscription handshake, then a scripted event line, then
/// hold the socket open.
fn subscribe_then(lines: Vec<Vec<u8>>) -> Action {
    let mut stream = vec![subscription_started_line()];
    stream.extend(lines);
    Action::Stream(stream)
}

#[tokio::test]
async fn subscribe_handshake_and_events() {
    let server = FakeHerdr::start(subscribe_then(vec![
        br#"{"event":"pane_updated","data":{"pane_id":"wE:pE"}}"#.to_vec(),
        br#"{"event":"workspace_created","data":{"workspace_id":"wX"}}"#.to_vec(),
    ]))
    .await;
    let client = client_for(&server);
    let mut stream = client
        .subscribe_events(&[Subscription::Named("pane.updated")])
        .await
        .unwrap();

    let e = next_event(&mut stream).await.unwrap().unwrap();
    assert_eq!(e.name, "pane.updated");
    let e = next_event(&mut stream).await.unwrap().unwrap();
    assert_eq!(e.name, "workspace.created");

    let reqs = server.requests();
    assert_eq!(reqs[0].method, "events.subscribe");
    assert_eq!(reqs[0].id, "lerdr-events");
    assert_eq!(
        reqs[0].params["subscriptions"],
        json!([{"type": "pane.updated"}])
    );
}

#[tokio::test]
async fn subscribe_refused_unknown_variant() {
    // Older herdr rejects `workspace.reordered` with a pre-dispatch refusal —
    // the fallback must resubscribe without it.
    let server = FakeHerdr::start(Action::Stream(vec![subscription_started_line()])).await;
    server.push(Action::Custom(|conn, _req| {
        Box::pin(async move {
            use tokio::io::AsyncWriteExt;
            let mut conn = conn;
            let _ = conn
                .write_all(
                    br#"{"id":"","error":{"code":"invalid_request","message":"invalid request: unknown variant `workspace.reordered`"}}
"#
                    .as_slice(),
                )
                .await;
        })
    }));
    let client = client_for(&server);
    let stream = client.subscribe_topology().await.unwrap();
    assert_eq!(
        server.accept_count(),
        2,
        "fallback resubscribe must dial again"
    );
    assert_eq!(client.workspace_reordered_supported(), Some(false));
    drop(stream);
}

#[tokio::test]
async fn subscribe_reordered_supported_when_accepted() {
    let server = FakeHerdr::start(Action::Stream(vec![subscription_started_line()])).await;
    let client = client_for(&server);
    let _stream = client.subscribe_topology().await.unwrap();
    assert_eq!(server.accept_count(), 1);
    assert_eq!(client.workspace_reordered_supported(), Some(true));
}

#[tokio::test]
async fn subscribe_rejected_non_capability_error() {
    let server = FakeHerdr::start(Action::Custom(|conn, _req| {
        Box::pin(async move {
            use tokio::io::AsyncWriteExt;
            let mut conn = conn;
            let _ = conn
                .write_all(
                    br#"{"id":"lerdr-events","error":{"code":"permission_denied","message":"nope"}}
"#
                    .as_slice(),
                )
                .await;
        })
    }))
    .await;
    let client = client_for(&server);
    let err = client
        .subscribe_events(&[Subscription::Named("pane.updated")])
        .await
        .unwrap_err();
    assert_eq!(err.code.as_deref(), Some("permission_denied"));
    assert!(!err.is_workspace_reordered_rejected());
}

#[tokio::test]
async fn events_lost_terminates_stream() {
    let server = FakeHerdr::start(subscribe_then(vec![
        br#"{"event":"pane_updated","data":{}}"#.to_vec(),
        br#"{"id":"lerdr-events","error":{"code":"events_lost","message":"overrun"}}"#.to_vec(),
    ]))
    .await;
    let client = client_for(&server);
    let mut stream = client
        .subscribe_events(&[Subscription::Named("pane.updated")])
        .await
        .unwrap();
    assert!(next_event(&mut stream).await.unwrap().is_ok());
    let err = next_event(&mut stream).await.unwrap().unwrap_err();
    assert!(matches!(err, EventStreamError::EventsLost(_)));
    assert!(err.history_lost());
    assert!(next_event(&mut stream).await.is_none());
}

#[tokio::test]
async fn bootstrap_returns_snapshot_and_gap_events() {
    // Conn 1 (subscribe): handshake + an event written immediately. Conn 2
    // (snapshot): delayed reply so the event reaches the stream's queue
    // while the snapshot is in flight — exactly the gap the drain captures.
    let server = FakeHerdr::start(Action::Reply(snapshot_result())).await;
    server.push(subscribe_then(vec![
        br#"{"event":"pane_updated","data":{"pane_id":"wE:pE"}}"#.to_vec(),
    ]));
    server.push(Action::Custom(|conn, req| {
        Box::pin(async move {
            use tokio::io::AsyncWriteExt;
            let mut conn = conn;
            tokio::time::sleep(Duration::from_millis(100)).await;
            let _ = conn
                .write_all(
                    json!({"id": req.id, "result": snapshot_result()})
                        .to_string()
                        .as_bytes(),
                )
                .await;
            let _ = conn.write_all(b"\n").await;
        })
    }));
    let client = client_for(&server);
    let boot = client
        .bootstrap(&[Subscription::Named("pane.updated")])
        .await
        .unwrap();
    assert_eq!(boot.snapshot.version, "0.9.1");
    assert_eq!(boot.gap_events.len(), 1);
    assert_eq!(boot.gap_events[0].name, "pane.updated");
    // The stream is still live for subsequent events.
    assert!(!boot.stream.is_done());
}

#[tokio::test]
async fn supervisor_resyncs_after_close() {
    // Method-routed handler: subscribe conns get handshake + one event +
    // server-side close (ending the stream → resync); everything else gets
    // the snapshot reply.
    let server = FakeHerdr::start(Action::Custom(|conn, req| {
        Box::pin(async move {
            use tokio::io::AsyncWriteExt;
            let mut conn = conn;
            if req.method == "events.subscribe" {
                let _ = conn.write_all(&subscription_started_line()).await;
                let _ = conn.write_all(b"\n").await;
                let _ = conn
                    .write_all(
                        br#"{"event":"pane_updated","data":{"pane_id":"wE:pE"}}
"#
                        .as_slice(),
                    )
                    .await;
                // Close — the client's stream ends and the supervisor resyncs.
            } else {
                let _ = conn
                    .write_all(
                        json!({"id": req.id, "result": snapshot_result()})
                            .to_string()
                            .as_bytes(),
                    )
                    .await;
                let _ = conn.write_all(b"\n").await;
            }
        })
    }))
    .await;
    let client = client_for(&server);
    let mut signals = client.supervise_events(
        EventSupervisor::new(vec![Subscription::Named("pane.updated")]).backoff(
            Backoff::with_min_max(Duration::from_millis(5), Duration::from_millis(20)),
        ),
    );

    // Each cycle: Synced → Invalidated → (close) → Reconnecting → Synced.
    let mut synced = 0usize;
    let mut reconnected = 0usize;
    let mut invalidated = 0usize;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while synced < 2 || reconnected < 1 {
        assert!(
            std::time::Instant::now() < deadline,
            "supervisor never resynced (synced={synced}, reconnected={reconnected})"
        );
        match next_signal(&mut signals).await {
            Some(SupervisorSignal::Synced(_)) => synced += 1,
            Some(SupervisorSignal::Reconnecting { .. }) => reconnected += 1,
            Some(SupervisorSignal::Invalidated { .. }) => invalidated += 1,
            None => panic!("supervisor stream ended"),
        }
    }
    assert!(invalidated >= 1, "expected at least one event invalidation");
    drop(signals);
}

#[tokio::test]
async fn supervisor_reports_reconnecting_on_failure() {
    // Server that refuses every subscribe — the supervisor emits
    // Reconnecting signals instead of dying.
    let server = FakeHerdr::start(Action::Refuse("unknown_method", "no events")).await;
    let client = client_for(&server);
    let mut signals = client.supervise_events(
        EventSupervisor::new(vec![Subscription::Named("pane.updated")]).backoff(
            Backoff::with_min_max(Duration::from_millis(5), Duration::from_millis(20)),
        ),
    );
    match next_signal(&mut signals).await.unwrap() {
        SupervisorSignal::Reconnecting { attempt, delay, .. } => {
            assert_eq!(attempt, 1);
            assert_eq!(delay, Duration::from_millis(5));
        }
        other => panic!("expected Reconnecting, got {other:?}"),
    }
    drop(signals);
}
