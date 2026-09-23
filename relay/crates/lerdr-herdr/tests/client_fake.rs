//! Client behavior against the fake Herdr server: dispatch taxonomy, fresh
//! connection per request, semaphore bound, singleflight, typed wrappers.

mod support;

use std::time::Duration;

use serde_json::json;
use support::{snapshot_result, Action, FakeHerdr};

use lerdr_herdr::{Client, ClientConfig, DispatchPhase, ReadFormat, ReadSource};

fn client_for(server: &FakeHerdr) -> Client {
    Client::unix(server.sock_path.clone())
}

#[tokio::test]
async fn unary_call_round_trip() {
    let server = FakeHerdr::start(Action::Reply(
        json!({"type": "pong", "version": "0.9.1", "protocol": 22}),
    ))
    .await;
    let pong = client_for(&server).ping().await.unwrap();
    assert_eq!(pong.version, "0.9.1");
    assert_eq!(pong.protocol, 22);

    let reqs = server.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].method, "ping");
    assert!(reqs[0].id.starts_with("lerdr-api-"));
}

#[tokio::test]
async fn fresh_connection_per_request() {
    let server = FakeHerdr::start(Action::Reply(
        json!({"type": "pong", "version": "x", "protocol": 22}),
    ))
    .await;
    let client = client_for(&server);
    client.ping().await.unwrap();
    client.ping().await.unwrap();
    client.ping().await.unwrap();
    assert_eq!(server.accept_count(), 3, "each request must dial fresh");
}

#[tokio::test]
async fn dial_failure_is_not_started() {
    let dir = tempfile::tempdir().unwrap();
    let client = Client::unix(dir.path().join("missing.sock"));
    let err = client.ping().await.unwrap_err();
    assert_eq!(err.phase(), DispatchPhase::NotStarted);
    assert!(err.is_safe_to_retry());
}

#[tokio::test]
async fn structured_refusal_is_refused() {
    let server = FakeHerdr::start(Action::Refuse("pane_not_found", "no such pane")).await;
    let client = client_for(&server);
    let err = client
        .pane_read("wE:pE", ReadSource::Visible, 10, ReadFormat::Text)
        .await
        .unwrap_err();
    assert_eq!(err.phase(), DispatchPhase::Refused);
    assert_eq!(err.refusal_code(), Some("pane_not_found"));
    assert!(!err.may_have_applied());
}

#[tokio::test]
async fn hangup_after_write_is_dispatched_unknown() {
    let server = FakeHerdr::start(Action::HangUp).await;
    let client = client_for(&server);
    let err = client.ping().await.unwrap_err();
    assert_eq!(err.phase(), DispatchPhase::DispatchedUnknown);
    assert!(err.may_have_applied());
}

#[tokio::test]
async fn foreign_response_id_is_dispatched_unknown() {
    let server = FakeHerdr::start(Action::ReplyForeignId(
        "someone-else",
        json!({"type": "pong"}),
    ))
    .await;
    let err = client_for(&server).ping().await.unwrap_err();
    assert_eq!(err.phase(), DispatchPhase::DispatchedUnknown);
}

#[tokio::test]
async fn truncated_response_is_dispatched_unknown() {
    let server = FakeHerdr::start(Action::Truncated).await;
    let err = client_for(&server).ping().await.unwrap_err();
    assert_eq!(err.phase(), DispatchPhase::DispatchedUnknown);
}

#[tokio::test]
async fn result_type_mismatch_is_dispatched_unknown() {
    let server = FakeHerdr::start(Action::Reply(json!({"type": "pane_read"}))).await;
    let err = client_for(&server).ping().await.unwrap_err();
    assert_eq!(err.phase(), DispatchPhase::DispatchedUnknown);
}

#[tokio::test]
async fn session_snapshot_decodes() {
    let server = FakeHerdr::start(Action::Reply(snapshot_result())).await;
    let snap = client_for(&server).session_snapshot().await.unwrap();
    assert_eq!(snap.protocol, 22);
    assert_eq!(snap.workspaces.len(), 1);
    assert_eq!(snap.workspaces[0].workspace_id, "wE");
    assert_eq!(snap.focused_pane_id.as_deref(), Some("wE:pE"));
}

#[tokio::test]
async fn pane_read_round_trip_and_params() {
    let server = FakeHerdr::start(Action::Reply(json!({
        "type": "pane_read",
        "read": {
            "pane_id": "wE:pE", "workspace_id": "wE", "tab_id": "wE:t1",
            "source": "visible", "format": "text",
            "text": "hello", "revision": 7, "truncated": false
        }
    })))
    .await;
    let client = client_for(&server);
    let read = client
        .pane_read("wE:pE", ReadSource::Visible, 50, ReadFormat::Text)
        .await
        .unwrap();
    assert_eq!(read.text, "hello");
    assert_eq!(read.revision, 7);

    let reqs = server.requests();
    assert_eq!(reqs[0].method, "pane.read");
    assert_eq!(reqs[0].params["pane_id"], "wE:pE");
    assert_eq!(reqs[0].params["source"], "visible");
    assert_eq!(reqs[0].params["format"], "text");
    // Go client semantics: strip_ansi = (format != ansi).
    assert_eq!(reqs[0].params["strip_ansi"], true);
}

#[tokio::test]
async fn pane_read_retries_non_definitive_once() {
    // First connection hangs up post-dispatch; second answers. One retry.
    let server = FakeHerdr::start(Action::Reply(json!({
        "type": "pane_read",
        "read": {"pane_id": "wE:pE", "workspace_id": "wE", "tab_id": "wE:t1",
                 "source": "visible", "format": "text", "text": "ok",
                 "revision": 1, "truncated": false}
    })))
    .await;
    server.push(Action::HangUp);
    let client = client_for(&server);
    let read = client
        .pane_read("wE:pE", ReadSource::Visible, 10, ReadFormat::Text)
        .await
        .unwrap();
    assert_eq!(read.text, "ok");
    assert_eq!(server.accept_count(), 2, "exactly one retry");
}

#[tokio::test]
async fn pane_read_never_retries_refused() {
    let server = FakeHerdr::start(Action::Refuse("pane_not_found", "nope")).await;
    let client = client_for(&server);
    let err = client
        .pane_read("wE:pE", ReadSource::Visible, 10, ReadFormat::Text)
        .await
        .unwrap_err();
    assert_eq!(err.phase(), DispatchPhase::Refused);
    assert_eq!(server.accept_count(), 1, "Refused must not be retried");
}

/// `stale_content` is the one refusal worth retrying: the fenced revision
/// raced an in-flight write (Herdr's seqlock contract), so the retry lands
/// on the settled buffer. One retry — a persistent stale read errors out.
#[tokio::test]
async fn pane_read_retries_stale_content_once() {
    let server = FakeHerdr::start(Action::Reply(json!({
        "type": "pane_read",
        "read": {"pane_id": "wE:pE", "workspace_id": "wE", "tab_id": "wE:t1",
                 "source": "visible", "format": "text", "text": "settled",
                 "revision": 8, "truncated": false}
    })))
    .await;
    server.push(Action::Refuse("stale_content", "revision mismatch"));
    let client = client_for(&server);
    let read = client
        .pane_read("wE:pE", ReadSource::Visible, 10, ReadFormat::Text)
        .await
        .unwrap();
    assert_eq!(read.text, "settled");
    assert_eq!(read.revision, 8);
    assert_eq!(server.accept_count(), 2, "one stale_content retry");

    // A second stale refusal surfaces the error — no unbounded spinning.
    let server = FakeHerdr::start(Action::Refuse("stale_content", "still racing")).await;
    let err = client_for(&server)
        .pane_read("wE:pE", ReadSource::Visible, 10, ReadFormat::Text)
        .await
        .unwrap_err();
    assert_eq!(err.refusal_code(), Some("stale_content"));
    assert_eq!(server.accept_count(), 2, "first + one retry, then give up");
}

#[tokio::test]
async fn singleflight_dedupes_identical_reads() {
    // Slow scripted reply: the leader's request stays in flight long enough
    // for every follower to join — then exactly one dial serves all callers.
    let server = FakeHerdr::start(Action::Custom(|conn, req| {
        Box::pin(async move {
            use tokio::io::AsyncWriteExt;
            let mut conn = conn;
            tokio::time::sleep(Duration::from_millis(120)).await;
            let _ = conn
                .write_all(
                    json!({"id": req.id, "result": {
                        "type": "pane_read",
                        "read": {"pane_id": "wE:pE", "workspace_id": "wE",
                                 "tab_id": "wE:t1", "source": "visible",
                                 "format": "text", "text": "shared",
                                 "revision": 1, "truncated": false}
                    }})
                    .to_string()
                    .as_bytes(),
                )
                .await;
            let _ = conn.write_all(b"\n").await;
        })
    }))
    .await;
    let client = client_for(&server);

    let mut handles = Vec::new();
    for _ in 0..6 {
        let client = client.clone();
        handles.push(tokio::spawn(async move {
            client
                .pane_read("wE:pE", ReadSource::Visible, 10, ReadFormat::Text)
                .await
        }));
    }
    for h in handles {
        let res = tokio::time::timeout(Duration::from_secs(30), h)
            .await
            .expect("deduped pane.read hung")
            .unwrap();
        assert_eq!(res.unwrap().text, "shared");
    }
    assert_eq!(
        server.accept_count(),
        1,
        "6 identical in-flight reads must dedupe to one dial"
    );
}

#[tokio::test(start_paused = true)]
async fn request_timeout_after_dispatch_is_unknown() {
    // Server accepts and reads the request but never replies. Virtual time
    // fast-forwards the request deadline — bytes went out, so the failure is
    // DispatchedUnknown, not NotStarted.
    let server = FakeHerdr::start(Action::Custom(|conn, _req| {
        Box::pin(async move {
            let _conn = conn; // hold open, never respond
            tokio::time::sleep(Duration::from_secs(600)).await;
        })
    }))
    .await;
    let config = ClientConfig {
        request_timeout: Duration::from_secs(2),
        ..ClientConfig::default()
    };
    let client = Client::unix_with(server.sock_path.clone(), config);
    // command.invoke: mutation, bypasses singleflight.
    let err = client
        .command_invoke(lerdr_herdr::CommandInvokeParams {
            command_id: "x".into(),
            pane_id: None,
            tab_id: None,
            workspace_id: None,
            selection: None,
        })
        .await
        .unwrap_err();
    assert_eq!(err.phase(), DispatchPhase::DispatchedUnknown);
    assert_eq!(server.accept_count(), 1);
}

#[tokio::test]
async fn mutations_are_not_singleflighted() {
    // Same reply to every connection; two identical mutating calls must both
    // reach the server.
    let server = FakeHerdr::start(Action::Reply(json!({"type": "notification_show",
                                                     "shown": true,
                                                     "reason": "shown"})))
    .await;
    let client = client_for(&server);
    let params = lerdr_herdr::NotificationShowParams {
        title: "hello".into(),
        body: None,
        position: None,
        sound: None,
    };
    let (a, b) = tokio::join!(
        client.notification_show(params.clone()),
        client.notification_show(params)
    );
    a.unwrap();
    b.unwrap();
    assert_eq!(server.accept_count(), 2, "mutations must not dedupe");
}

#[tokio::test]
async fn semaphore_bounds_concurrent_dials() {
    // Slow scripted replies so requests overlap; the server's own concurrency
    // counter is the ground truth for the client's bound.
    let server = FakeHerdr::start(Action::Custom(|conn, req| {
        Box::pin(async move {
            use tokio::io::AsyncWriteExt;
            let mut conn = conn;
            tokio::time::sleep(Duration::from_millis(80)).await;
            let _ = conn
                .write_all(
                    json!({"id": req.id, "result": {"type": "pong", "version": "x", "protocol": 22}})
                        .to_string()
                        .as_bytes(),
                )
                .await;
            let _ = conn.write_all(b"\n").await;
        })
    }))
    .await;

    let config = ClientConfig {
        max_in_flight: 3,
        ..ClientConfig::default()
    };
    let client = Client::unix_with(server.sock_path.clone(), config);
    // `command.invoke` is a mutation — never singleflighted, so all 9 calls
    // really dial.
    let mut handles = Vec::new();
    for i in 0..9 {
        let client = client.clone();
        handles.push(tokio::spawn(async move {
            client
                .command_invoke(lerdr_herdr::CommandInvokeParams {
                    command_id: format!("cmd-{i}"),
                    pane_id: None,
                    tab_id: None,
                    workspace_id: None,
                    selection: None,
                })
                .await
        }));
    }
    for h in handles {
        tokio::time::timeout(Duration::from_secs(30), h)
            .await
            .expect("command.invoke hung")
            .unwrap()
            .unwrap();
    }
    assert_eq!(server.accept_count(), 9);
    assert!(
        server.max_concurrent() <= 3,
        "dial semaphore exceeded: {} > 3",
        server.max_concurrent()
    );
}
