//! Golden-vector conformance against `fixtures/` — the Go oracle is the
//! contract; every vector must pass byte-exact.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use lerdr_core::delta;
use lerdr_core::lease::{LeaseError, LeaseManager, PaneHost, TerminalSize};
use lerdr_core::protocol::{Inbound, Outbound};
use lerdr_core::sendbuffer::{is_replaceable, PushResult, RejectReason, SendBuffer};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../fixtures")
        .canonicalize()
        .expect("fixtures directory")
}

fn load_suite(path: &str) -> serde_json::Value {
    let file = fixtures_dir().join(path);
    serde_json::from_str(&std::fs::read_to_string(&file).expect("fixture readable"))
        .expect("fixture is JSON")
}

// ---------------------------------------------------------------------------
// protocol.envelope — c2s decode->decoded_json, s2c decode->encode round-trip.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct EnvelopeVector {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    direction: String,
    json: String,
    decoded_json: Option<String>,
}

#[test]
fn protocol_envelope_vectors() {
    let suite = load_suite("protocol/protocol.envelope.json");
    let vectors: Vec<EnvelopeVector> =
        serde_json::from_value(suite["vectors"].clone()).expect("vector schema");
    assert_eq!(vectors.len(), 123, "fixture vector count changed");

    let mut c2s = 0usize;
    let mut s2c = 0usize;
    for vector in &vectors {
        match vector.direction.as_str() {
            "c2s" => {
                c2s += 1;
                let decoded = Inbound::decode(vector.json.as_bytes()).unwrap_or_else(|err| {
                    panic!("{}: decode failed: {err}", vector.name);
                });
                assert_eq!(decoded.r#type, vector.kind, "{}: decoded type", vector.name);
                let encoded = decoded.encode();
                let expected = vector.decoded_json.as_deref().expect("decoded_json");
                assert_eq!(
                    String::from_utf8_lossy(&encoded),
                    expected,
                    "{}: decoded_json mismatch",
                    vector.name
                );
            }
            "s2c" => {
                s2c += 1;
                let decoded = Outbound::decode(vector.json.as_bytes())
                    .unwrap_or_else(|err| panic!("{}: decode failed: {err}", vector.name));
                let encoded = decoded.encode();
                assert_eq!(
                    String::from_utf8_lossy(&encoded),
                    vector.json,
                    "{}: s2c round-trip mismatch",
                    vector.name
                );
            }
            other => panic!("{}: unknown direction {other}", vector.name),
        }
    }
    assert_eq!((c2s, s2c), (72, 51));
}

// ---------------------------------------------------------------------------
// pane.delta — Build emits expected_segments; Apply yields expected_applied.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct DeltaVector {
    name: String,
    previous: String,
    current: String,
    expected_segments: Vec<delta::Segment>,
    expected_applied: String,
    efficient: bool,
}

#[test]
fn pane_delta_vectors() {
    let suite = load_suite("pane/pane.delta.json");
    let vectors: Vec<DeltaVector> =
        serde_json::from_value(suite["vectors"].clone()).expect("vector schema");
    assert_eq!(vectors.len(), 21);

    for vector in &vectors {
        let segments = delta::build(&vector.previous, &vector.current);
        assert_eq!(
            segments, vector.expected_segments,
            "{}: build segments",
            vector.name
        );
        let applied = delta::apply(&vector.previous, &segments)
            .unwrap_or_else(|| panic!("{}: apply failed", vector.name));
        assert_eq!(applied, vector.expected_applied, "{}: applied", vector.name);
        // Apply the FIXTURE's segments too — they're legal input for Apply.
        let applied = delta::apply(&vector.previous, &vector.expected_segments)
            .unwrap_or_else(|| panic!("{}: apply(fixture) failed", vector.name));
        assert_eq!(
            applied, vector.expected_applied,
            "{}: applied (fixture segments)",
            vector.name
        );
        assert_eq!(
            delta::efficient(&segments, &vector.current),
            vector.efficient,
            "{}: efficient",
            vector.name
        );
    }
}

// ---------------------------------------------------------------------------
// pane.sendbuffer — push/drain/pop op scripts.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct SendbufferVector {
    name: String,
    capacity_bytes: usize,
    capacity_items: usize,
    ops: Vec<SendbufferOp>,
    expected: SendbufferExpected,
    op_results: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(tag = "op")]
enum SendbufferOp {
    #[serde(rename = "push")]
    Push { msg_type: String, size: usize },
    #[serde(rename = "drain")]
    Drain { count: usize },
    #[serde(rename = "pop")]
    Pop,
}

#[derive(Deserialize)]
struct SendbufferExpected {
    evicted: Vec<Eviction>,
    pending_types: Vec<String>,
    bytes: usize,
}

#[derive(Deserialize)]
struct Eviction {
    op_index: usize,
    msg_type: String,
    reason: String,
}

/// A synthetic message of exactly `size` bytes carrying `kind` — the buffer
/// keys on serialized bytes; the envelope lets `pop` report the type back.
fn synthetic_message(kind: &str, size: usize) -> Vec<u8> {
    let overhead = format!("{{\"type\":\"{kind}\",\"p\":\"\"}}").len();
    let pad = size.saturating_sub(overhead);
    format!("{{\"type\":\"{kind}\",\"p\":\"{}\"}}", "x".repeat(pad)).into_bytes()
}

fn sniff_kind(data: &[u8]) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(data)
        .ok()
        .and_then(|v| v["type"].as_str().map(str::to_owned))
}

#[test]
fn pane_sendbuffer_vectors() {
    let suite = load_suite("pane/pane.sendbuffer.json");
    let vectors: Vec<SendbufferVector> =
        serde_json::from_value(suite["vectors"].clone()).expect("vector schema");
    assert_eq!(vectors.len(), 11);

    for vector in &vectors {
        let mut buffer = SendBuffer::with_capacity(vector.capacity_items, vector.capacity_bytes);
        let mut results: Vec<serde_json::Value> = Vec::new();
        let mut evicted: Vec<Eviction> = Vec::new();

        for (index, op) in vector.ops.iter().enumerate() {
            match op {
                SendbufferOp::Push { msg_type, size } => {
                    let data = synthetic_message(msg_type, *size);
                    assert_eq!(data.len(), *size, "synthetic size");
                    let result =
                        buffer.push_typed(data, msg_type.clone(), is_replaceable(msg_type));
                    let label = match result {
                        PushResult::Queued => "queued",
                        PushResult::Coalesced => "coalesced",
                        PushResult::Rejected(reason) => {
                            let reason = match reason {
                                RejectReason::ItemLimit => "item_limit",
                                RejectReason::ByteLimit => "byte_limit",
                                RejectReason::CoalesceByteLimit => "coalesce_byte_limit",
                                RejectReason::Closed => "closed",
                            };
                            evicted.push(Eviction {
                                op_index: index,
                                msg_type: msg_type.clone(),
                                reason: reason.to_owned(),
                            });
                            "rejected"
                        }
                    };
                    results.push(serde_json::Value::from(label));
                }
                SendbufferOp::Drain { count } => {
                    let mut drained = Vec::new();
                    for _ in 0..*count {
                        match buffer.pop() {
                            Some(data) => drained.push(sniff_kind(&data).expect("typed message")),
                            None => break,
                        }
                    }
                    results.push(serde_json::json!(drained));
                }
                SendbufferOp::Pop => match buffer.pop() {
                    Some(data) => results.push(serde_json::json!(sniff_kind(&data).unwrap())),
                    None => results.push(serde_json::Value::Null),
                },
            }
        }

        assert_eq!(results, vector.op_results, "{}: op_results", vector.name);
        let expected_evicted: Vec<(usize, &str, &str)> = vector
            .expected
            .evicted
            .iter()
            .map(|e| (e.op_index, e.msg_type.as_str(), e.reason.as_str()))
            .collect();
        let actual_evicted: Vec<(usize, &str, &str)> = evicted
            .iter()
            .map(|e| (e.op_index, e.msg_type.as_str(), e.reason.as_str()))
            .collect();
        assert_eq!(actual_evicted, expected_evicted, "{}: evicted", vector.name);
        assert_eq!(
            buffer.bytes(),
            vector.expected.bytes,
            "{}: bytes",
            vector.name
        );
        assert_eq!(
            buffer.len(),
            vector.expected.pending_types.len(),
            "{}: len",
            vector.name
        );
        let pending: Vec<String> = (0..vector.expected.pending_types.len())
            .filter_map(|_| buffer.pop())
            .map(|data| sniff_kind(&data).expect("typed"))
            .collect();
        assert_eq!(
            pending, vector.expected.pending_types,
            "{}: pending_types",
            vector.name
        );
    }
}

// ---------------------------------------------------------------------------
// pane.lease — arbitration op scripts against a virtual pane host.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct LeaseVector {
    name: String,
    initial_size: Size,
    ops: Vec<LeaseOp>,
    expected_effective: Size,
    expected_holders: Vec<String>,
}

#[derive(Deserialize, Clone, Copy)]
struct Size {
    cols: i64,
    rows: i64,
}

#[derive(Deserialize)]
#[serde(tag = "op")]
enum LeaseOp {
    #[serde(rename = "lease")]
    Lease {
        client: Option<String>,
        cols: i64,
        rows: i64,
        #[serde(default)]
        owner_gone: bool,
        #[serde(default)]
        expect_error: Option<String>,
    },
    #[serde(rename = "release")]
    Release { client: String },
    #[serde(rename = "release_client")]
    ReleaseClient { client: String },
    #[serde(rename = "local_resize")]
    LocalResize { cols: i64, rows: i64 },
    #[serde(rename = "advance_seconds")]
    Advance { n: u64 },
}

/// In-memory pane host — the `provider`+`stty` seam, keyed by pane id.
struct ScriptedHost {
    sizes: HashMap<String, TerminalSize>,
}

impl PaneHost for ScriptedHost {
    fn pane_size(&mut self, pane_id: &str) -> Result<TerminalSize, LeaseError> {
        self.sizes
            .get(pane_id)
            .copied()
            .ok_or(LeaseError::ProcessUnavailable)
    }

    fn set_size(&mut self, pane_id: &str, columns: i64, rows: i64) -> Result<(), LeaseError> {
        let size = self
            .sizes
            .get_mut(pane_id)
            .ok_or(LeaseError::ProcessUnavailable)?;
        size.columns = columns;
        if rows > 0 {
            size.rows = rows;
        }
        Ok(())
    }
}

#[test]
fn pane_lease_vectors() {
    let suite = load_suite("pane/pane.lease.json");
    let vectors: Vec<LeaseVector> =
        serde_json::from_value(suite["vectors"].clone()).expect("vector schema");
    assert_eq!(vectors.len(), 23);

    for vector in &vectors {
        let pane = "pane-1".to_owned();
        let host = ScriptedHost {
            sizes: HashMap::from([(
                pane.clone(),
                TerminalSize {
                    columns: vector.initial_size.cols,
                    rows: vector.initial_size.rows,
                },
            )]),
        };
        let mut manager = LeaseManager::new(host);

        for op in &vector.ops {
            match op {
                LeaseOp::Lease {
                    client,
                    cols,
                    rows,
                    owner_gone,
                    expect_error,
                } => {
                    let result = manager.acquire(
                        client.as_deref().unwrap_or(""),
                        &pane,
                        *cols,
                        *rows,
                        *owner_gone,
                    );
                    match expect_error {
                        Some(expected) => {
                            let err =
                                result.expect_err(&format!("{}: lease should fail", vector.name));
                            assert_eq!(
                                err.as_str(),
                                expected.as_str(),
                                "{}: error kind",
                                vector.name
                            );
                        }
                        None => {
                            result.unwrap_or_else(|err| {
                                panic!("{}: lease failed: {err}", vector.name)
                            });
                        }
                    }
                }
                LeaseOp::Release { client } => {
                    manager
                        .release(client, &pane)
                        .unwrap_or_else(|err| panic!("{}: release failed: {err}", vector.name));
                }
                LeaseOp::ReleaseClient { client } => {
                    manager.release_client(client).unwrap_or_else(|err| {
                        panic!("{}: release_client failed: {err}", vector.name)
                    });
                }
                LeaseOp::LocalResize { cols, rows } => {
                    // Out-of-band tty resize; the manager notices on the next
                    // acquire's size read.
                    manager.host_mut().sizes.insert(
                        pane.clone(),
                        TerminalSize {
                            columns: *cols,
                            rows: *rows,
                        },
                    );
                }
                LeaseOp::Advance { n } => {
                    manager.advance(Duration::from_secs(*n));
                    // Go sweeps on a 1s ticker; a single sweep at the end of
                    // the window reaches the same state for these scripts.
                    manager
                        .sweep_expired()
                        .unwrap_or_else(|err| panic!("{}: sweep failed: {err}", vector.name));
                }
            }
        }

        let effective = manager
            .host()
            .sizes
            .get(&pane)
            .copied()
            .expect("pane known to host");
        assert_eq!(
            (effective.columns, effective.rows),
            (
                vector.expected_effective.cols,
                vector.expected_effective.rows
            ),
            "{}: effective size",
            vector.name
        );
        assert_eq!(
            manager.holders(&pane),
            vector.expected_holders,
            "{}: holders",
            vector.name
        );
    }
}
