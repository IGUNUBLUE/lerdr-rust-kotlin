//! Latency/throughput probes for the paths a user feels: the E2EE
//! handshake, per-frame seal/open at real payload sizes, the pane-delta
//! line diff, the per-tick content fingerprint, and outbound frame
//! serialization. Not criterion — plain Instant sampling, no new deps.
//!
//! Run optimized for meaningful numbers:
//!   cargo test -p lerdr-relay --test perf --release -- --nocapture
//!
//! The probes print min/median/p95/mean and assert nothing — they measure,
//! they don't gate. Timing varies by machine; treat output as a profile,
//! not a verdict.

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use lerdr_core::delta;
use lerdr_core::json::MaybeNull;
use lerdr_core::protocol::{Outbound, PaneDelta};
use lerdr_e2ee::handshake::SessionKeys;
use lerdr_e2ee::{Codec, Session};
use lerdr_relay::store::MemoryAuthStore;
use sha2::{Digest, Sha256};
use support::{seed_credential, serve, test_config, TestClient};
use tokio_util::sync::CancellationToken;

/// Run `iters` of `f`, collect per-iter durations, print the quartiles.
fn bench(name: &str, iters: usize, mut f: impl FnMut() -> Duration) {
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        samples.push(f());
    }
    samples.sort();
    let total: Duration = samples.iter().sum();
    let report = |label: &str, d: Duration| {
        if d.as_secs() >= 1 || d.as_millis() > 0 {
            println!("    {label:<6} {:>8.3} ms", d.as_secs_f64() * 1e3);
        } else {
            println!("    {label:<6} {:>8.3} us", d.as_secs_f64() * 1e6);
        }
    };
    println!("{name}  (n={iters})");
    report("min", samples[0]);
    report("median", samples[iters / 2]);
    report("p95", samples[iters * 95 / 100]);
    report("mean", total / iters as u32);
}

fn timed(f: impl FnOnce()) -> Duration {
    let t = Instant::now();
    f();
    t.elapsed()
}

fn pane(cols: usize, rows: usize) -> String {
    let mut s = String::with_capacity(cols * rows);
    for r in 0..rows {
        for _ in 0..cols.saturating_sub(1) {
            s.push(if r % 3 == 0 { 'x' } else { '.' });
        }
        s.push('\n');
    }
    s
}

fn churn(pane: &str, lines: &[usize]) -> String {
    let mut out: Vec<String> = pane.lines().map(str::to_owned).collect();
    for &i in lines {
        if i < out.len() {
            out[i] = format!("$ changed line {i} {}", "█".repeat(20));
        }
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

// --- E2EE session crypto ---------------------------------------------------

fn session_pair() -> (Session, Session) {
    let keys = SessionKeys {
        c2s: [0x11; 32],
        s2c: [0x22; 32],
    };
    (
        Session::client(&keys, Codec::Json).unwrap(),
        Session::server(&keys, Codec::Json).unwrap(),
    )
}

#[test]
fn perf_seal_open_payload_sizes() {
    for size in [1_024usize, 16_384, 262_144, 1_048_576] {
        let payload = vec![b'x'; size];
        let (mut tx, mut rx) = session_pair();
        bench(&format!("seal+open {size}B"), 500, || {
            timed(|| {
                let frame = tx.seal(&payload).unwrap();
                let plain = rx.open(&frame).unwrap();
                assert_eq!(plain.len(), size);
            })
        });
    }
}

// --- Pane delta path ---------------------------------------------------------

#[test]
fn perf_delta_build_pane_sizes() {
    for (cols, rows) in [(80usize, 24usize), (240, 120)] {
        let before = pane(cols, rows);
        // Realistic churn: one line (a redrawn prompt row) then heavy
        // churn (half the viewport — scroll/clear).
        let light = churn(&before, &[rows - 2]);
        let heavy = churn(&before, &(0..rows).step_by(2).collect::<Vec<_>>());
        for (label, after) in [("1 line", light), ("50%", heavy)] {
            let after = &after;
            bench(&format!("delta::build {cols}x{rows} {label}"), 300, || {
                timed(|| {
                    let segs = delta::build(&before, after);
                    std::hint::black_box(&segs);
                })
            });
        }
    }
}

#[test]
fn perf_content_fingerprint_per_tick() {
    for (cols, rows) in [(80usize, 24usize), (240, 120)] {
        let content = pane(cols, rows);
        // The watch tick cost: sha256 + hex — exactly content_fingerprint().
        bench(&format!("fingerprint {cols}x{rows}"), 2000, || {
            timed(|| {
                let d = Sha256::digest(content.as_bytes());
                std::hint::black_box(hex::encode(&d[..8]));
            })
        });
    }
}

#[test]
fn perf_outbound_serialize() {
    let before = pane(240, 120);
    let after = churn(&before, &[60]);
    let segments = delta::build(&before, &after);
    let frame = Outbound::PaneDelta(Box::new(PaneDelta {
        r#type: "pane_delta".into(),
        pane_id: Some("wE:p1".into()),
        base_fingerprint: Some("a948904f2f0f479b".into()),
        content_fingerprint: Some("deadbeefdeadbeef".into()),
        format: Some("text".into()),
        segments: Some(MaybeNull::Value(segments)),
        ..PaneDelta::default()
    }));
    bench("Outbound::encode PaneDelta", 2000, || {
        timed(|| {
            let bytes = frame.encode();
            std::hint::black_box(bytes);
        })
    });
}

// --- End-to-end: session actor ----------------------------------------------

#[tokio::test]
async fn perf_handshake_and_roundtrip() {
    let store = Arc::new(MemoryAuthStore::new());
    let parent = CancellationToken::new();

    // Handshake: fresh pair + serve + full e2ee-v2 exchange each iter.
    let mut samples = Vec::new();
    for _ in 0..60 {
        let (selector, secret) = seed_credential(&store);
        let (mut client, server_io) = TestClient::pair(64 * 1024);
        let (server, _sink_rx) = serve(server_io, store.clone(), test_config(), parent.clone());
        let t = Instant::now();
        let _est = client.handshake(&selector, &secret).await;
        samples.push(t.elapsed());
        server.abort();
    }
    samples.sort();
    println!("handshake e2ee-v2  (n=60)");
    println!("    min    {:>8.3} ms", samples[0].as_secs_f64() * 1e3);
    println!("    median {:>8.3} ms", samples[30].as_secs_f64() * 1e3);
    println!("    p95    {:>8.3} ms", samples[57].as_secs_f64() * 1e3);

    // Roundtrip: one established session, N unknown actions (dispatch →
    // error envelope → sealed reply) — the full per-action pipeline.
    let (selector, secret) = seed_credential(&store);
    let (mut client, server_io) = TestClient::pair(1024 * 1024);
    let (server, _sink_rx) = serve(server_io, store.clone(), test_config(), parent.clone());
    let mut session = client.handshake(&selector, &secret).await.session;

    let action = br#"{"action":"nonexistent_probe","request_id":"p1"}"#;
    let mut rtt = Vec::with_capacity(400);
    for _ in 0..400 {
        let t = Instant::now();
        client.send_json(&mut session, action).await;
        let reply = client.read_json(&mut session).await;
        rtt.push(t.elapsed());
        assert!(reply["type"].is_string());
    }
    server.abort();
    rtt.sort();
    println!("action roundtrip (sealed in→dispatch→sealed out)  (n=400)");
    println!("    min    {:>8.3} ms", rtt[0].as_secs_f64() * 1e3);
    println!("    median {:>8.3} ms", rtt[200].as_secs_f64() * 1e3);
    println!("    p95    {:>8.3} ms", rtt[380].as_secs_f64() * 1e3);

    parent.cancel();
}
