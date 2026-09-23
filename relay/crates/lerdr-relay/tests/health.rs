//! The HTTP probe endpoints — `server.go`'s `handleHealth`,
//! `handleHealthz`, `handleReadyz`: the JSON shapes, the serving +
//! inventory readiness transition, and the `X-Herdr-Relay-Instance`
//! header. The inventory probe is a cell the test flips, standing in for
//! the coordinator's topology watch.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use lerdr_core::protocol::{InventoryStatusMessage, VERSION};
use lerdr_relay::server::{HealthProbe, InventoryProbeFn};
use lerdr_relay::store::MemoryAuthStore;
use lerdr_relay::Relay;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// A shared inventory cell — the HealthProbe closure reads it per
/// request, so flipping it mid-test exercises the live-projection seam.
fn inventory_cell(state: &str) -> (Arc<Mutex<InventoryStatusMessage>>, InventoryProbeFn) {
    let cell = Arc::new(Mutex::new(InventoryStatusMessage {
        state: Some(state.to_owned()),
        error_code: Some(String::new()),
        message: Some("free-text detail the HTTP surface must drop".to_owned()),
        last_attempt_at: Some(1_700_000_000),
        last_success_at: Some(1_699_999_000),
        stale: Some(false),
        r#type: "inventory_status".to_owned(),
    }));
    let probe = {
        let cell = Arc::clone(&cell);
        InventoryProbeFn(Arc::new(move || cell.lock().unwrap().clone()))
    };
    (cell, probe)
}

fn set_state(cell: &Arc<Mutex<InventoryStatusMessage>>, state: &str) {
    cell.lock().unwrap().state = Some(state.to_owned());
}

/// A `GET` over a raw stream → `(status, headers, body)`.
async fn http_get(addr: SocketAddr, path: &str) -> (u16, HashMap<String, String>, String) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").expect("HTTP head/body split");
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("HTTP status code");
    let headers = head
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    (status, headers, body.to_owned())
}

fn health_json(body: &str) -> serde_json::Value {
    serde_json::from_str(body.trim_end()).expect("health body is JSON")
}

async fn serve_with_health(health: HealthProbe) -> (SocketAddr, Relay) {
    let relay = Relay::new(Arc::new(MemoryAuthStore::new())).with_health(health);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serving = relay.clone();
    tokio::spawn(async move { serving.serve(listener).await });
    (addr, relay)
}

/// `handleHealth` — `ok\n` plaintext plus the instance header.
#[tokio::test]
async fn health_reports_ok_and_instance() {
    let (_cell, probe) = inventory_cell("ready");
    let (addr, _relay) = serve_with_health(HealthProbe {
        instance_id: "test-instance-7".to_owned(),
        inventory: Some(probe),
        ..HealthProbe::default()
    })
    .await;

    let (status, headers, body) = http_get(addr, "/health").await;
    assert_eq!(status, 200);
    assert_eq!(body, "ok\n");
    assert_eq!(
        headers.get("content-type").map(String::as_str),
        Some("text/plain; charset=utf-8")
    );
    assert_eq!(
        headers.get("x-herdr-relay-instance").map(String::as_str),
        Some("test-instance-7")
    );
}

/// `handleHealthz` — the complete JSON shape: status, readiness,
/// inventory (minus `message`/`type`), instance, version,
/// release_version, revision, protocol.
#[tokio::test]
async fn healthz_reports_full_shape() {
    let (_cell, probe) = inventory_cell("ready");
    let (addr, _relay) = serve_with_health(HealthProbe {
        instance_id: "shape-instance".to_owned(),
        version: "9.9.9".to_owned(),
        revision: "abc123".to_owned(),
        inventory: Some(probe),
    })
    .await;

    let (status, _headers, body) = http_get(addr, "/healthz").await;
    assert_eq!(status, 200);
    let health = health_json(&body);
    assert_eq!(health["status"], "ok");
    assert_eq!(health["readiness"], "ready");
    assert_eq!(health["instance"], "shape-instance");
    assert_eq!(health["version"], "9.9.9");
    assert_eq!(health["release_version"], "9.9.9");
    assert_eq!(health["revision"], "abc123");
    assert_eq!(health["protocol"], VERSION);

    let inventory = &health["inventory"];
    assert_eq!(inventory["state"], "ready");
    assert_eq!(inventory["error_code"], "");
    assert_eq!(inventory["last_attempt_at"], 1_700_000_000);
    assert_eq!(inventory["last_success_at"], 1_699_999_000);
    assert_eq!(inventory["stale"], false);
    // `delete(inventory, "message")` — and `type` is the wire
    // discriminator, never part of the oracle's map.
    assert!(inventory.get("message").is_none(), "{inventory}");
    assert!(inventory.get("type").is_none(), "{inventory}");
}

/// `handleReadyz` — 503 while inventory is starting, 200 once the probe
/// reports ready, 503 again on error. The serving latch is already set
/// (`serve` is running), so only the live inventory projection moves it.
#[tokio::test]
async fn readyz_follows_live_inventory() {
    let (cell, probe) = inventory_cell("starting");
    let (addr, _relay) = serve_with_health(HealthProbe {
        inventory: Some(probe),
        ..HealthProbe::default()
    })
    .await;

    let (status, _headers, body) = http_get(addr, "/readyz").await;
    assert_eq!(status, 503);
    let reply = health_json(&body);
    assert_eq!(reply["status"], "unavailable");
    assert_eq!(reply["inventory"]["state"], "starting");
    assert!(reply["inventory"].get("message").is_none());

    set_state(&cell, "ready");
    let (status, _headers, body) = http_get(addr, "/readyz").await;
    assert_eq!(status, 200);
    assert_eq!(health_json(&body)["status"], "ready");
    let (status, _headers, body) = http_get(addr, "/healthz").await;
    assert_eq!(status, 200);
    assert_eq!(health_json(&body)["readiness"], "ready");

    set_state(&cell, "error");
    let (status, _headers, body) = http_get(addr, "/readyz").await;
    assert_eq!(status, 503);
    assert_eq!(health_json(&body)["status"], "unavailable");
    // An inventory error degrades healthz readiness rather than
    // unready-ing it.
    let (_status, _headers, body) = http_get(addr, "/healthz").await;
    assert_eq!(health_json(&body)["readiness"], "degraded");
}

/// Before `serve` runs, `s.ready` is false: even a ready inventory
/// reports `starting`/`unavailable` (the oracle's `s.ready` leg).
#[tokio::test]
async fn readyz_unavailable_before_serving() {
    let (_cell, probe) = inventory_cell("ready");
    let relay = Relay::new(Arc::new(MemoryAuthStore::new())).with_health(HealthProbe {
        inventory: Some(probe),
        ..HealthProbe::default()
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // Drive the router directly — `relay.serve` never runs, so the
    // serving latch stays false.
    let app = relay.router();
    tokio::spawn(async move { axum::serve(listener, app.into_make_service()).await });

    let (status, _headers, body) = http_get(addr, "/readyz").await;
    assert_eq!(status, 503);
    assert_eq!(health_json(&body)["status"], "unavailable");
    let (_status, _headers, body) = http_get(addr, "/healthz").await;
    assert_eq!(health_json(&body)["readiness"], "starting");
}
