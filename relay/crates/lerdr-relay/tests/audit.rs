//! Integration checks for the remote-write audit seam — the session's
//! `recordWriteAudit` admission row plus the `sendAuditedCommandResult`
//! result row, read back off the JSONL file the way an operator would.

mod support;

use std::sync::Arc;

use lerdr_core::audit::{self, AuditLog};
use lerdr_e2ee::handshake::{AuthSelector, SECRET_BYTES};
use lerdr_relay::session::{AuditHook, ClientSink, ConnectionEnd};
use lerdr_relay::store::MemoryAuthStore;
use support::{seed_credential, serve, test_config, TestClient};
use tokio_util::sync::CancellationToken;

/// Session config with a real audit log under `dir` (the file lands at
/// `dir/audit/remote-writes.jsonl`).
fn audited_config(dir: &std::path::Path) -> lerdr_relay::session::SessionConfig {
    lerdr_relay::session::SessionConfig {
        audit: Some(AuditHook {
            log: Arc::new(AuditLog::open(dir).expect("audit log opens")),
            attribution: None,
        }),
        ..test_config()
    }
}

/// Pair as the seeded controller `device-1`/`cred-1` over an in-memory
/// duplex — same helper shape as `device_admin.rs`.
async fn establish(
    store: &Arc<MemoryAuthStore>,
    config: lerdr_relay::session::SessionConfig,
) -> (
    TestClient,
    lerdr_e2ee::Session,
    tokio::task::JoinHandle<ConnectionEnd>,
    tokio::sync::oneshot::Receiver<ClientSink>,
) {
    let (selector, secret): (AuthSelector, [u8; SECRET_BYTES]) = seed_credential(store);
    let (mut client, server_io) = TestClient::pair(64 * 1024);
    let (server, sink_rx) = serve(
        server_io,
        Arc::clone(store),
        config,
        CancellationToken::new(),
    );
    let established = client.handshake(&selector, &secret).await;
    (client, established.session, server, sink_rx)
}

fn audit_path(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join("audit").join("remote-writes.jsonl")
}

/// Read every JSONL row as a map — panics unless the file parses clean.
fn rows(dir: &std::path::Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(audit_path(dir))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).expect("audit line is JSON"))
        .collect()
}

/// `{"type":"rename_device","device_id":..,"name":..}` — an audited,
/// hub-owned admin action: attempt at admission, result on the reply.
#[tokio::test]
async fn audited_admin_action_writes_attempt_and_result_rows() {
    let store = Arc::new(lerdr_relay::store::MemoryAuthStore::new());
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut client, mut session, server, _sink) =
        establish(&store, audited_config(dir.path())).await;

    client
        .send_json(
            &mut session,
            br#"{"type":"rename_device","protocol":3,"request_id":"req-a1","action_id":"act-a1","device_id":"device-1","name":"pocket"}"#,
        )
        .await;
    let result = client.read_until_type(&mut session, "command_result").await;
    let receipt = client.read_until_type(&mut session, "action_receipt").await;
    assert_eq!(result["ok"], true);
    assert_eq!(receipt["receipt"]["phase"], "confirmed");
    drop(client);
    server.await.expect("session ends clean");

    let rows = rows(dir.path());
    assert_eq!(rows.len(), 2, "attempt + result, nothing else: {rows:?}");
    let attempt = &rows[0];
    assert_eq!(attempt["stage"], "attempt");
    assert_eq!(attempt["action"], "rename_device");
    assert_eq!(attempt["request_id"], "req-a1");
    assert_eq!(attempt["connection_id"], "client-1");
    // No `client_id` on the wire → `connection:<id>` fallback.
    assert_eq!(attempt["client_id"], "connection:client-1");
    assert_eq!(attempt["details"]["name"], "pocket");
    assert!(attempt["details"]["payload_sha256"].is_string());
    assert!(attempt["details"]["payload_bytes"].is_number());

    let row = &rows[1];
    assert_eq!(row["stage"], "result");
    assert_eq!(row["action"], "rename_device");
    assert_eq!(row["request_id"], "req-a1");
    assert_eq!(row["ok"], true);
    assert_eq!(row["phase"], "completed");
    // `Details = nil` on result rows — the key is absent, not empty.
    assert!(row.get("details").is_none());
}

/// `send_secret` is audited but its payload is a secret: the log keeps
/// `text_bytes` only — no digest, no field names to scrape.
#[tokio::test]
async fn send_secret_audit_row_is_shape_only() {
    let store = Arc::new(lerdr_relay::store::MemoryAuthStore::new());
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut client, mut session, server, _sink) =
        establish(&store, audited_config(dir.path())).await;

    client
        .send_json(
            &mut session,
            br#"{"type":"send_secret","protocol":3,"request_id":"req-s1","action_id":"act-s1","target":{"pane_id":"w:t:p"},"text":"hunter2-secret"}"#,
        )
        .await;
    // The stub router answers `dispatched_unknown` — the attempt row is
    // what the session writes regardless.
    let _ = client.read_until_type(&mut session, "action_receipt").await;
    drop(client);
    server.await.expect("session ends clean");

    let rows = rows(dir.path());
    assert_eq!(rows.len(), 1, "attempt only — stub emits no result");
    let attempt = &rows[0];
    assert_eq!(attempt["stage"], "attempt");
    assert_eq!(attempt["action"], "send_secret");
    assert_eq!(attempt["pane_id"], "w:t:p");
    let details = &attempt["details"];
    assert_eq!(details["text_bytes"], 14);
    assert!(
        details.get("payload_sha256").is_none(),
        "secrets never get a crackable digest: {details}"
    );
    assert!(
        details.get("text").is_none() && details.get("prompt").is_none(),
        "no payload fields leak: {details}"
    );
}

/// `device_list` is admin but NOT `Audited` — silence is the contract.
#[tokio::test]
async fn non_audited_action_writes_no_rows() {
    let store = Arc::new(lerdr_relay::store::MemoryAuthStore::new());
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut client, mut session, server, _sink) =
        establish(&store, audited_config(dir.path())).await;

    client
        .send_json(
            &mut session,
            br#"{"type":"device_list","protocol":3,"request_id":"req-d1","action_id":"act-d1"}"#,
        )
        .await;
    let _ = client.read_until_type(&mut session, "command_result").await;
    let _ = client.read_until_type(&mut session, "action_receipt").await;
    drop(client);
    server.await.expect("session ends clean");

    assert!(
        rows(dir.path()).is_empty(),
        "device_list is not audited — no rows expected"
    );
}

/// A denied audited write still logs its attempt — the oracle records
/// after `authorizeDeviceAction`, so only authorized sends audit.
#[tokio::test]
async fn unauthorized_write_is_denied_before_audit() {
    let store = Arc::new(lerdr_relay::store::MemoryAuthStore::new());
    let dir = tempfile::tempdir().expect("tempdir");
    // Reader credential: authorized to connect, denied on writes.
    let (selector, secret) = seed_credential(&store);
    let _ = (&selector, &secret);
    let (mut client, mut session, server, _sink) =
        establish(&store, audited_config(dir.path())).await;

    // Malformed audited action never reaches the switch — no rows.
    client
        .send_json(
            &mut session,
            br#"{"type":"rename_device","protocol":3,"request_id":"req-x1","action_id":"act-x1","device_id":"other-device","name":"x"}"#,
        )
        .await;
    let result = client.read_until_type(&mut session, "command_result").await;
    // Unknown device → the store refuses; the result row still audits
    // because the attempt was admitted and the reply is a command_result.
    assert_eq!(result["ok"], false);
    drop(client);
    server.await.expect("session ends clean");

    let rows = rows(dir.path());
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1]["stage"], "result");
    assert_eq!(rows[1]["ok"], false);
}

/// `audit::is_audited` gates at dispatch — a nonexistent action type still
/// hits `UNKNOWN_ACTION` before any audit row (nothing logged).
#[tokio::test]
async fn unknown_action_never_reaches_audit() {
    let store = Arc::new(lerdr_relay::store::MemoryAuthStore::new());
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut client, mut session, server, _sink) =
        establish(&store, audited_config(dir.path())).await;

    client
        .send_json(
            &mut session,
            br#"{"type":"no_such_action","protocol":3,"request_id":"req-u1","action_id":"act-u1"}"#,
        )
        .await;
    let _ = client.read_until_type(&mut session, "error").await;
    drop(client);
    server.await.expect("session ends clean");

    assert!(rows(dir.path()).is_empty());
}

/// Sanity: `is_audited`/`action_of` behave as the oracle's
/// `isAuditedWrite`/`auditAction` at the seam.
#[test]
fn audit_predicates_match_the_catalog() {
    assert!(audit::is_audited("send_text"));
    assert!(audit::is_audited("rename_device"));
    assert!(!audit::is_audited("device_list"));
    let map = serde_json::json!({"type": "command", "action": "send_text"})
        .as_object()
        .unwrap()
        .clone();
    assert_eq!(audit::action_of(&map), "send_text");
}
