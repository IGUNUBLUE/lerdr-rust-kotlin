//! Device-administration actions — the `s.deviceAuth.*` arms of the
//! oracle's action switch (`internal/app/server.go:757-853`) served by the
//! relay's own auth store: `device_list`, `rename_device`, `revoke_device`,
//! `create_device_invitation`, `reset_devices`.
//!
//! Success replies arrive as `command_result` (the oracle's whole reply)
//! followed by a terminal `action_receipt` at `confirmed`; store refusals
//! answer the `failed` `command_result` alone. `revoke_device` on the
//! caller's own credential and `reset_devices` close the connection ~250 ms
//! later (`time.AfterFunc` + `DisconnectCredential`).

mod support;

use std::sync::Arc;

use base64::Engine;
use lerdr_e2ee::handshake::{AuthKind, AuthSelector, SECRET_BYTES};
use lerdr_relay::auth::{BootstrapRearm, Credential, DeviceAuthStore, Role};
use lerdr_relay::frame::{FrameRead, ReadError};
use lerdr_relay::session::{ConnectionEnd, EvictReason, SessionConfig};
use lerdr_relay::store::{
    FileAuthStore, Invitation, MemoryAuthStore, BOOTSTRAP_INVITATION_ID, INVITATION_LIFETIME_MS,
};
use support::*;
use tokio_util::sync::CancellationToken;

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
}

/// A stored credential seed — the caller picks ids, role, and revocation.
fn credential(device: &str, cred: &str, role: Role) -> Credential {
    Credential {
        device_id: device.to_owned(),
        credential_id: cred.to_owned(),
        name: "phone".to_owned(),
        role,
        locale: "en".to_owned(),
        paired_at_ms: 1,
        last_seen_at_ms: 0,
        version: 1,
        revoked: false,
    }
}

/// Seed a second credential; returns `(selector, raw secret)` for it.
fn seed_extra(
    store: &MemoryAuthStore,
    device: &str,
    cred: &str,
    role: Role,
) -> (AuthSelector, [u8; SECRET_BYTES]) {
    let secret = [0xCD; SECRET_BYTES];
    store.add_credential(credential(device, cred, role), b64().encode(secret));
    (
        AuthSelector::new(AuthKind::Credential, cred, 1, "en"),
        secret,
    )
}

/// Establish a credential session over the duplex harness; returns the
/// client, live session, server join handle, and the sink oneshot.
async fn establish_as(
    store: &Arc<MemoryAuthStore>,
    selector: &AuthSelector,
    secret: &[u8; SECRET_BYTES],
    config: SessionConfig,
) -> (
    TestClient,
    lerdr_e2ee::Session,
    tokio::task::JoinHandle<ConnectionEnd>,
    tokio::sync::oneshot::Receiver<lerdr_relay::session::ClientSink>,
) {
    let (mut client, server_io) = TestClient::pair(64 * 1024);
    let (server, sink_rx) = serve(
        server_io,
        Arc::clone(store),
        config,
        CancellationToken::new(),
    );
    let established = client.handshake(selector, secret).await;
    (client, established.session, server, sink_rx)
}

/// The default path: pair as the seeded controller `device-1`/`cred-1`.
async fn establish(
    store: &Arc<MemoryAuthStore>,
    config: SessionConfig,
) -> (
    TestClient,
    lerdr_e2ee::Session,
    tokio::task::JoinHandle<ConnectionEnd>,
    tokio::sync::oneshot::Receiver<lerdr_relay::session::ClientSink>,
) {
    let (selector, secret) = seed_credential(store);
    establish_as(store, &selector, &secret, config).await
}

/// Send an action envelope, read the `command_result` then the terminal
/// `action_receipt` — the two-message success order the actor emits.
async fn action_roundtrip(
    client: &mut TestClient,
    session: &mut lerdr_e2ee::Session,
    request: &[u8],
) -> (serde_json::Value, serde_json::Value) {
    client.send_json(session, request).await;
    let result = client.read_until_type(session, "command_result").await;
    let receipt = client.read_until_type(session, "action_receipt").await;
    (result, receipt)
}

#[tokio::test]
async fn device_list_returns_devices_then_confirmed_receipt() {
    // Pinned clock: the handshake's `complete_credential` refreshes
    // `last_seen_at` to now, so a fixed `now` makes the RFC3339 assertion
    // exact (and exercises the zero-fraction path).
    let store = Arc::new(MemoryAuthStore::new().with_clock(|| 1_700_000_000_000));
    let (mut client, mut session, server, _sink_rx) = establish(&store, test_config()).await;

    let (result, receipt) = action_roundtrip(
        &mut client,
        &mut session,
        br#"{"type":"device_list","protocol":3,"request_id":"req-1","action_id":"act-1"}"#,
    )
    .await;

    // commandResultMessage: flat fields, `data` carrying the device map.
    assert_eq!(result["type"], "command_result");
    assert_eq!(result["request_id"], "req-1");
    assert_eq!(result["action"], "device_list");
    assert_eq!(result["ok"], true);
    assert_eq!(result["phase"], "completed");
    assert_eq!(result["error"], "");
    assert_eq!(result["pane_id"], "");
    let data = &result["data"];
    assert_eq!(data["current_device_id"], "device-1");
    assert_eq!(data["role"], "controller");
    let devices = data["devices"].as_array().expect("devices array");
    assert_eq!(devices.len(), 1);
    let device = &devices[0];
    assert_eq!(device["device_id"], "device-1");
    assert_eq!(device["credential_id"], "cred-1");
    assert_eq!(device["name"], "phone");
    assert_eq!(device["role"], "controller");
    assert_eq!(device["locale"], "en");
    // time.Time wire form: RFC3339 — millisecond fraction on `paired_at`,
    // no fraction on the whole-second `last_seen_at` (RFC3339Nano trims).
    assert_eq!(device["paired_at"], "1970-01-01T00:00:00.001Z");
    assert_eq!(device["last_seen_at"], "2023-11-14T22:13:20Z");
    assert_eq!(device["version"], 1);
    assert_eq!(device["revoked"], false);
    // `current` only on the caller's own credential.
    assert_eq!(device["current"], true);

    assert_eq!(receipt["request_id"], "req-1");
    assert_eq!(receipt["receipt"]["action_id"], "act-1");
    assert_eq!(receipt["receipt"]["phase"], "confirmed");
    assert!(receipt["receipt"]["error"].is_null());

    drop(client);
    server.await.expect("server joins");
}

#[tokio::test]
async fn device_list_filters_revoked_tombstones() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, _sink_rx) = establish(&store, test_config()).await;

    // Enroll a second device, then revoke it — the list must hide the
    // tombstone like `activeDeviceCredentials`.
    store.add_credential(
        credential("device-2", "cred-2", Role::Reader),
        b64().encode([2u8; SECRET_BYTES]),
    );
    DeviceAuthStore::revoke_device(&*store, "cred-2").expect("revoke reader");

    let (result, _) = action_roundtrip(
        &mut client,
        &mut session,
        br#"{"type":"device_list","protocol":3,"request_id":"req-2"}"#,
    )
    .await;
    let devices = result["data"]["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0]["device_id"], "device-1");
    assert!(devices[0].get("current").is_some());
    // The revoked row stays in `ListCredentials` for id resolution.
    assert_eq!(store.credentials().len(), 2);

    drop(client);
    server.await.expect("server joins");
}

#[tokio::test]
async fn rename_device_persists_and_is_visible_in_list() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, _sink_rx) = establish(&store, test_config()).await;

    let (result, receipt) = action_roundtrip(
        &mut client,
        &mut session,
        br#"{"type":"rename_device","protocol":3,"request_id":"req-3","action_id":"act-3","device_id":"device-1","name":"  Work phone  "}"#,
    )
    .await;
    assert_eq!(result["ok"], true);
    assert_eq!(result["phase"], "completed");
    // RenameCredential trims and returns the updated record.
    assert_eq!(result["data"]["device"]["name"], "Work phone");
    assert_eq!(receipt["receipt"]["phase"], "confirmed");

    let (list, _) = action_roundtrip(
        &mut client,
        &mut session,
        br#"{"type":"device_list","protocol":3,"request_id":"req-4"}"#,
    )
    .await;
    assert_eq!(list["data"]["devices"][0]["name"], "Work phone");

    drop(client);
    server.await.expect("server joins");
}

#[tokio::test]
async fn rename_device_unknown_device_fails() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, _sink_rx) = establish(&store, test_config()).await;

    client
        .send_json(
            &mut session,
            br#"{"type":"rename_device","protocol":3,"request_id":"req-5","device_id":"no-such","name":"x"}"#,
        )
        .await;
    let result = client.read_until_type(&mut session, "command_result").await;
    assert_eq!(result["request_id"], "req-5");
    assert_eq!(result["ok"], false);
    assert_eq!(result["phase"], "failed");
    // The oracle's `deviceCredentialID` miss string, verbatim.
    assert_eq!(result["error"], "Device credential was not found");
    assert!(result["data"].is_null());

    drop(client);
    server.await.expect("server joins");
}

#[tokio::test]
async fn rename_device_rejects_blank_name() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, _sink_rx) = establish(&store, test_config()).await;

    client
        .send_json(
            &mut session,
            br#"{"type":"rename_device","protocol":3,"request_id":"req-6","device_id":"device-1","name":"   "}"#,
        )
        .await;
    let result = client.read_until_type(&mut session, "command_result").await;
    assert_eq!(result["ok"], false);
    assert_eq!(result["error"], "invalid device name");

    drop(client);
    server.await.expect("server joins");
}

#[tokio::test]
async fn revoke_device_self_answers_then_disconnects() {
    // A second controller keeps `ErrLastController` out of the way so the
    // self-revoke actually lands.
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, _sink_rx) = establish(&store, test_config()).await;
    seed_extra(&store, "device-2", "cred-2", Role::Controller);

    let (result, receipt) = action_roundtrip(
        &mut client,
        &mut session,
        br#"{"type":"revoke_device","protocol":3,"request_id":"req-7","action_id":"act-7","device_id":"device-1"}"#,
    )
    .await;
    assert_eq!(result["ok"], true);
    assert_eq!(result["data"]["device"]["device_id"], "device-1");
    assert_eq!(result["data"]["device"]["revoked"], true);
    // Revocation bumps the version — the tombstone reports 2.
    assert_eq!(result["data"]["device"]["version"], 2);
    assert_eq!(receipt["receipt"]["phase"], "confirmed");

    // The revoked credential stops authorizing immediately.
    assert!(store.authorize("cred-1", 1).is_none());

    // `time.AfterFunc(250ms, DisconnectCredential)` — the answer lands
    // first, then the graceful going-away close.
    let err = client.reader.read_frame().await.expect_err("closed");
    assert!(
        matches!(
            err,
            ReadError::Closed {
                code: Some(1001),
                ..
            }
        ),
        "expected GoingAway close, got {err:?}"
    );
    let end = server.await.expect("server joins");
    assert!(matches!(
        end,
        ConnectionEnd::Evicted(EvictReason::CredentialRevoked)
    ));
}

#[tokio::test]
async fn revoke_last_controller_is_refused() {
    // device-1 is the only controller — the oracle refuses to orphan the hub.
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, _sink_rx) = establish(&store, test_config()).await;

    client
        .send_json(
            &mut session,
            br#"{"type":"revoke_device","protocol":3,"request_id":"req-8","device_id":"device-1"}"#,
        )
        .await;
    let result = client.read_until_type(&mut session, "command_result").await;
    assert_eq!(result["ok"], false);
    assert_eq!(result["phase"], "failed");
    assert_eq!(result["error"], "cannot revoke the last controller");

    // A refused revoke does not touch the session.
    let (list, _) = action_roundtrip(
        &mut client,
        &mut session,
        br#"{"type":"device_list","protocol":3,"request_id":"req-9"}"#,
    )
    .await;
    assert_eq!(list["data"]["devices"].as_array().unwrap().len(), 1);

    drop(client);
    server.await.expect("server joins");
}

#[tokio::test]
async fn revoke_other_device_keeps_caller_session() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, _sink_rx) = establish(&store, test_config()).await;
    seed_extra(&store, "device-2", "cred-2", Role::Controller);

    let (result, _) = action_roundtrip(
        &mut client,
        &mut session,
        br#"{"type":"revoke_device","protocol":3,"request_id":"req-10","device_id":"device-2"}"#,
    )
    .await;
    assert_eq!(result["ok"], true);
    assert_eq!(result["data"]["device"]["device_id"], "device-2");
    assert_eq!(result["data"]["device"]["revoked"], true);

    // cred-2 is fenced; the caller's own credential still authorizes.
    assert!(store.authorize("cred-2", 1).is_none());
    assert!(store.authorize("cred-1", 1).is_some());

    // The caller stays connected — the disconnect targets only the revoked
    // credential's own connections.
    let (list, _) = action_roundtrip(
        &mut client,
        &mut session,
        br#"{"type":"device_list","protocol":3,"request_id":"req-11"}"#,
    )
    .await;
    assert_eq!(list["data"]["devices"].as_array().unwrap().len(), 1);

    drop(client);
    server.await.expect("server joins");
}

#[tokio::test]
async fn create_device_invitation_returns_redeemable_secret() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, _sink_rx) = establish(&store, test_config()).await;

    let (result, receipt) = action_roundtrip(
        &mut client,
        &mut session,
        br#"{"type":"create_device_invitation","protocol":3,"request_id":"req-12","action_id":"act-12","name":"Kitchen tablet","role":"reader"}"#,
    )
    .await;
    assert_eq!(result["ok"], true);
    assert_eq!(receipt["receipt"]["phase"], "confirmed");

    // The invitation payload carries everything the QR/link redemption
    // needs — `setup=<secret>&invite=<id>&invite_version=<v>`.
    let invitation = &result["data"]["invitation"];
    let invitation_id = invitation["invitation_id"].as_str().unwrap();
    assert_eq!(invitation_id.len(), 24, "18 bytes b64url");
    assert_eq!(invitation["version"], 1);
    let secret_b64 = invitation["secret"].as_str().unwrap();
    assert_eq!(secret_b64.len(), 43, "32 bytes b64url");
    assert_eq!(invitation["name"], "Kitchen tablet");
    assert_eq!(invitation["role"], "reader");
    // `identity.Locale` of the caller — not the request's locale field.
    assert_eq!(invitation["locale"], "en");
    assert!(
        invitation["expires_at"].as_str().unwrap().ends_with('Z'),
        "RFC3339 expiry"
    );

    // The secret redeems: a second client completes the invitation
    // handshake and enrols as a reader.
    let secret_bytes: [u8; SECRET_BYTES] = b64()
        .decode(secret_b64)
        .unwrap()
        .try_into()
        .expect("32-byte secret");
    let selector = AuthSelector::new(AuthKind::Invitation, invitation_id, 1, "en");
    let (mut second, server_io) = TestClient::pair(64 * 1024);
    let (second_server, _sink2) = serve(
        server_io,
        Arc::clone(&store),
        test_config(),
        CancellationToken::new(),
    );
    let established = second.handshake(&selector, &secret_bytes).await;
    assert_eq!(established.finish.role, "reader");
    assert!(!established.finish.device_id.is_empty());
    assert!(established.finish.credential_secret.is_some());
    assert_eq!(store.credentials().len(), 2);

    drop(client);
    drop(second);
    server.await.expect("server joins");
    second_server.await.expect("second server joins");
}

#[tokio::test]
async fn create_device_invitation_rejects_invalid_role() {
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, _sink_rx) = establish(&store, test_config()).await;

    client
        .send_json(
            &mut session,
            br#"{"type":"create_device_invitation","protocol":3,"request_id":"req-13","name":"x","role":"superuser"}"#,
        )
        .await;
    let result = client.read_until_type(&mut session, "command_result").await;
    assert_eq!(result["ok"], false);
    assert_eq!(result["error"], "invalid device role");

    drop(client);
    server.await.expect("server joins");
}

#[tokio::test]
async fn reset_devices_wipes_rearms_and_disconnects() {
    let store = Arc::new(MemoryAuthStore::new());
    let rearm_secret = [0x42; SECRET_BYTES];
    let config = SessionConfig {
        reset_bootstrap: Some(BootstrapRearm {
            secret: rearm_secret,
            name: "workstation".to_owned(),
        }),
        ..test_config()
    };
    let (mut client, mut session, server, _sink_rx) = establish(&store, config).await;

    let (result, receipt) = action_roundtrip(
        &mut client,
        &mut session,
        br#"{"type":"reset_devices","protocol":3,"request_id":"req-14","action_id":"act-14"}"#,
    )
    .await;
    assert_eq!(result["ok"], true);
    assert_eq!(result["phase"], "completed");
    // Reset success carries no data (the oracle's `nil` payload).
    assert!(result["data"].is_null());
    assert_eq!(receipt["receipt"]["phase"], "confirmed");

    // The caller's credential died with the store — deferred close lands.
    let err = client.reader.read_frame().await.expect_err("closed");
    assert!(matches!(
        err,
        ReadError::Closed {
            code: Some(1001),
            ..
        }
    ));
    let end = server.await.expect("server joins");
    assert!(matches!(
        end,
        ConnectionEnd::Evicted(EvictReason::CredentialRevoked)
    ));

    // `ResetWithBootstrap`: credentials wiped, a fresh `bootstrap` record
    // armed with the configured relay key.
    assert!(store.credentials().is_empty());
    let invitation = store.invitation().expect("bootstrap re-armed");
    assert_eq!(invitation.invitation_id, BOOTSTRAP_INVITATION_ID);
    assert_eq!(invitation.version, 1);
    assert_eq!(invitation.role, Role::Controller);
    assert_eq!(invitation.name, "workstation");

    // The re-armed bootstrap invitation pairs a fresh device.
    let selector = AuthSelector::new(AuthKind::Invitation, BOOTSTRAP_INVITATION_ID, 1, "en");
    let (mut client2, server_io2) = TestClient::pair(64 * 1024);
    let (server2, _sink2) = serve(
        server_io2,
        Arc::clone(&store),
        test_config(),
        CancellationToken::new(),
    );
    let established = client2.handshake(&selector, &rearm_secret).await;
    assert_eq!(established.finish.role, "controller");
    assert_eq!(store.credentials().len(), 1);

    drop(client2);
    server2.await.expect("second server joins");
}

#[tokio::test]
async fn reset_devices_without_rearm_leaves_empty_store() {
    // Tokenless relay: no bootstrap re-arm — the wipe stands alone and the
    // next `ensure_pairing`/SIGUSR1 mints a fresh invitation.
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, _sink_rx) = establish(&store, test_config()).await;

    let (result, _) = action_roundtrip(
        &mut client,
        &mut session,
        br#"{"type":"reset_devices","protocol":3,"request_id":"req-15"}"#,
    )
    .await;
    assert_eq!(result["ok"], true);

    let end = server.await.expect("server joins");
    assert!(matches!(
        end,
        ConnectionEnd::Evicted(EvictReason::CredentialRevoked)
    ));
    assert!(store.credentials().is_empty());
    assert!(store.invitation().is_none());
}

#[tokio::test]
async fn reader_lists_but_cannot_mutate_except_self_revoke() {
    let store = Arc::new(MemoryAuthStore::new());
    // cred-1 is the controller; the session pairs as reader cred-2.
    seed_credential(&store);
    let (reader_selector, reader_secret) = seed_extra(&store, "device-2", "cred-2", Role::Reader);
    let (mut client, mut session, server, _sink_rx) =
        establish_as(&store, &reader_selector, &reader_secret, test_config()).await;

    // device_list is a read action — a reader is welcome to it.
    let (result, _) = action_roundtrip(
        &mut client,
        &mut session,
        br#"{"type":"device_list","protocol":3,"request_id":"req-16"}"#,
    )
    .await;
    assert_eq!(result["ok"], true);
    assert_eq!(result["data"]["role"], "reader");
    assert_eq!(result["data"]["devices"].as_array().unwrap().len(), 2);
    // `current` marks the caller's credential, not the controller's.
    let current = result["data"]["devices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["current"] == true)
        .expect("one current row");
    assert_eq!(current["device_id"], "device-2");

    // Mutating another device: reader_denied (the authorize gate's
    // `error` envelope, not a command_result — the action never ran).
    client
        .send_json(
            &mut session,
            br#"{"type":"rename_device","protocol":3,"request_id":"req-17","device_id":"device-1","name":"x"}"#,
        )
        .await;
    let reply = client.read_until_type(&mut session, "error").await;
    assert_eq!(reply["error"]["code"], "reader_denied");
    assert_eq!(reply["error"]["args"]["operation"], "rename_device");

    // revoke_device against someone else's device is denied too.
    client
        .send_json(
            &mut session,
            br#"{"type":"revoke_device","protocol":3,"request_id":"req-18","device_id":"device-1"}"#,
        )
        .await;
    let reply = client.read_until_type(&mut session, "error").await;
    assert_eq!(reply["error"]["code"], "reader_denied");

    // Self-revoke is the carve-out (`authorizeAuthenticatedIdentity`) —
    // a reader may retire its own credential.
    let (result, _) = action_roundtrip(
        &mut client,
        &mut session,
        br#"{"type":"revoke_device","protocol":3,"request_id":"req-19","device_id":"device-2"}"#,
    )
    .await;
    assert_eq!(result["ok"], true);
    assert_eq!(result["data"]["device"]["revoked"], true);
    let end = server.await.expect("server joins");
    assert!(matches!(
        end,
        ConnectionEnd::Evicted(EvictReason::CredentialRevoked)
    ));
}

#[tokio::test]
async fn revoked_credential_fails_authorization_on_next_action() {
    // Revocation lands between actions — the per-action `authorize`
    // re-check fences the session with `credential_revoked`.
    let store = Arc::new(MemoryAuthStore::new());
    let (mut client, mut session, server, _sink_rx) = establish(&store, test_config()).await;
    seed_extra(&store, "device-2", "cred-2", Role::Controller);

    // Simulate another session revoking this credential out from under us.
    DeviceAuthStore::revoke_device(&*store, "cred-1").expect("revoke cred-1");

    client
        .send_json(
            &mut session,
            br#"{"type":"device_list","protocol":3,"request_id":"req-20"}"#,
        )
        .await;
    let reply = client.read_until_type(&mut session, "error").await;
    assert_eq!(reply["error"]["code"], "reader_denied");
    assert_eq!(reply["error"]["args"]["reason"], "credential_revoked");

    drop(client);
    server.await.expect("server joins");
}

// ---------------------------------------------------------------------------
// Store-level semantics — the `internal/deviceauth` port, exercised directly.
// ---------------------------------------------------------------------------

#[test]
fn store_rename_validates_and_persists() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileAuthStore::open(dir.path()).expect("open store");
    store
        .add_credential(
            credential("device-1", "cred-1", Role::Controller),
            b64().encode([7u8; 32]),
        )
        .unwrap();

    let renamed = store
        .rename_device("cred-1", "  Kitchen  ")
        .expect("rename");
    assert_eq!(renamed.name, "Kitchen");
    assert!(matches!(
        store.rename_device("cred-1", ""),
        Err(lerdr_relay::auth::AuthError::InvalidName)
    ));
    assert!(matches!(
        store.rename_device("nope", "x"),
        Err(lerdr_relay::auth::AuthError::NotFound)
    ));

    // Persisted through `transact` — a reopen sees the new name.
    let reopened = FileAuthStore::open(dir.path()).expect("reopen");
    assert_eq!(reopened.credentials()[0].name, "Kitchen");
}

#[test]
fn store_revoke_tombstones_and_fences() {
    let store = MemoryAuthStore::new();
    store.add_credential(
        credential("d1", "c1", Role::Controller),
        b64().encode([1u8; 32]),
    );
    store.add_credential(
        credential("d2", "c2", Role::Reader),
        b64().encode([2u8; 32]),
    );

    let revoked = store.revoke_device("c2").expect("revoke reader");
    assert!(revoked.revoked);
    assert_eq!(revoked.version, 2);
    // The tombstone drops its secret and stops authorizing at any version.
    assert!(store.authorize("c2", 1).is_none());
    assert!(store.authorize("c2", 2).is_none());
    // The record stays listed — `ListCredentials` keeps tombstones.
    assert_eq!(store.credentials().len(), 2);

    // Idempotent: revoking again returns the tombstone unchanged.
    let again = store.revoke_device("c2").expect("revoke again");
    assert_eq!(again.version, 2);

    assert!(matches!(
        store.revoke_device("missing"),
        Err(lerdr_relay::auth::AuthError::NotFound)
    ));
    // The last active controller cannot be revoked.
    assert!(matches!(
        store.revoke_device("c1"),
        Err(lerdr_relay::auth::AuthError::LastController)
    ));
}

#[tokio::test]
async fn store_revoke_drops_pending_invitation() {
    let store = MemoryAuthStore::new();
    // A standing controller keeps `ErrLastController` out of the way — the
    // invitation mints controller credentials.
    seed_credential(&store);
    let (selector, _secret) = seed_invitation(&store);
    // Redeem the invitation — the minted credential is pending on it until
    // its first credential handshake.
    let outcome = store.complete(&selector, true).await.expect("redeem");
    let cred = outcome.identity.credential_id.clone();
    assert_eq!(store.invitation().unwrap().pending_credential_id, cred);

    // Revoking the pending credential drops the invitation outright.
    DeviceAuthStore::revoke_device(&store, &cred).expect("revoke pending");
    assert!(store.invitation().is_none());
}

#[test]
fn store_create_invitation_mints_and_validates() {
    let store = MemoryAuthStore::new().with_clock(|| 1_000_000);
    let invitation =
        DeviceAuthStore::create_invitation(&store, "Kitchen", "reader", "en").expect("mint");
    assert_eq!(invitation.invitation_id.len(), 24);
    assert_eq!(invitation.secret.len(), 43);
    assert_eq!(invitation.version, 1);
    assert_eq!(invitation.expires_at_ms, 1_000_000 + INVITATION_LIFETIME_MS);
    assert_eq!(invitation.role, Role::Reader);

    // Error order is name → role → locale (`validateMetadata`).
    assert!(matches!(
        DeviceAuthStore::create_invitation(&store, "", "reader", "en"),
        Err(lerdr_relay::auth::AuthError::InvalidName)
    ));
    assert!(matches!(
        DeviceAuthStore::create_invitation(&store, "n", "owner", "en"),
        Err(lerdr_relay::auth::AuthError::InvalidRole)
    ));
    assert!(matches!(
        DeviceAuthStore::create_invitation(&store, "n", "reader", "en US"),
        Err(lerdr_relay::auth::AuthError::InvalidLocale)
    ));

    // A second mint replaces the invitation slot entirely.
    let second =
        DeviceAuthStore::create_invitation(&store, "Other", "controller", "en").expect("mint2");
    assert_ne!(invitation.invitation_id, second.invitation_id);
    assert_eq!(
        store.invitation().unwrap().invitation_id,
        second.invitation_id
    );
}

#[test]
fn store_reset_wipes_and_rearms_bootstrap() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileAuthStore::open(dir.path()).expect("open store");
    store
        .add_credential(
            credential("d1", "c1", Role::Controller),
            b64().encode([9u8; 32]),
        )
        .unwrap();
    store
        .set_invitation(Invitation {
            invitation_id: "old".to_owned(),
            version: 3,
            secret: b64().encode([5u8; 32]),
            expires_at_ms: 1,
            name: "old".to_owned(),
            role: Role::Controller,
            locale: "en".to_owned(),
            failed_attempts: 0,
            next_attempt_at_ms: 0,
            pending_credential_id: String::new(),
        })
        .unwrap();

    let rearm = BootstrapRearm {
        secret: [0x42; SECRET_BYTES],
        name: "workstation".to_owned(),
    };
    store.reset_devices(Some(&rearm), "en").expect("reset");
    assert!(store.credentials().is_empty());
    let invitation = store.invitation().expect("bootstrap record");
    assert_eq!(invitation.invitation_id, BOOTSTRAP_INVITATION_ID);
    assert_eq!(invitation.secret, b64().encode([0x42; SECRET_BYTES]));
    assert_eq!(invitation.name, "workstation");
    assert_eq!(invitation.locale, "en");

    // The swap persisted — a reopened store is identically empty + armed.
    let reopened = FileAuthStore::open(dir.path()).expect("reopen");
    assert!(reopened.credentials().is_empty());
    assert_eq!(
        reopened.invitation().unwrap().invitation_id,
        BOOTSTRAP_INVITATION_ID
    );

    // Wipe-only: no rearm → no invitation.
    let plain = MemoryAuthStore::new();
    plain.add_credential(
        credential("d", "c", Role::Controller),
        b64().encode([1u8; 32]),
    );
    DeviceAuthStore::reset_devices(&plain, None, "en").expect("wipe-only reset");
    assert!(plain.credentials().is_empty());
    assert!(plain.invitation().is_none());
}
