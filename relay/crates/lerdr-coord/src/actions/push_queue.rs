//! `queue.json` — the oracle's `durableQueue` file (`internal/push/queue.go`).
//!
//! The file is the delivery queue's durable half: `{"entries": …,
//! "delivered": …}` marshaled like Go's `json.MarshalIndent(q.state,
//! "", "  ")` plus a trailing newline at `0600`. Map keys are delivery
//! ids — `BTreeMap` keeps Go's sorted-key marshal deterministic.
//! `PushEvent.payload` is the Go `[]byte` base64 (`base64.StdEncoding`,
//! padded) — handled by the `go_bytes_b64` serde module on the field.
//!
//! Load semantics follow the repo's durable-file convention rather than
//! the oracle's hard failure (`newDurableQueue` errors `NewManager` on
//! a corrupt file): a file that doesn't decode whole is salvaged
//! member-wise, renamed aside for forensics (`quarantineIndex`-style,
//! `queue.invalid-<unix-nanos>.json`), and rebuilt from what survived.
//! A file the salvage can't parse at all yields an empty queue — the
//! rebuilt file is the artifact that matters.
//!
//! Persist runs through [`persist`] — the oracle's `persistLocked`
//! (`maxQueueEntries`/`maxQueueBytes` caps, then atomic tmp+rename).
//! The dirty/flush split lives in `super::push`: immediate mutations
//! (`enqueue`/`cancelKey`/`removeSubscriptions`/…) persist inline with
//! rollback like the oracle; drain-pass mutations (`finishInMemory`,
//! `rescheduleInMemory`, the recover/restore halves) mark
//! `state.queue_dirty` and land once per pass through `flush_queue`
//! (the oracle's `flush()` inside `finish`).

use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tracing::warn;

use lerdr_core::json::de_default;

use super::push::{atomic_write, DeliveredRecord, QueueEntry};

/// `maxQueueEntries` — entries + delivered rows cap a persisted queue.
pub(crate) const MAX_QUEUE_ENTRIES: usize = 1024;
/// `maxQueueBytes` — the marshaled file plus trailing newline cap.
pub(crate) const MAX_QUEUE_BYTES: usize = 4 * 1024 * 1024;
/// The file name under the push dir.
pub(crate) const QUEUE_FILENAME: &str = "queue.json";

/// `queueFile` — the on-disk envelope. Go emits `entries`/`delivered`
/// unconditionally (`nil` maps are `make`d on load), so both always
/// serialize.
#[derive(Serialize)]
struct QueueFile<'a> {
    entries: &'a BTreeMap<String, QueueEntry>,
    delivered: &'a BTreeMap<String, DeliveredRecord>,
}

/// The read half — `json.Unmarshal` tolerance: absent or `null` maps
/// decode empty like Go's `nil`-then-`make`.
#[derive(Deserialize)]
struct QueueFileRead {
    #[serde(default, deserialize_with = "de_default")]
    entries: BTreeMap<String, QueueEntry>,
    #[serde(default, deserialize_with = "de_default")]
    delivered: BTreeMap<String, DeliveredRecord>,
}

/// The loaded queue halves — `durableQueue.state` after `newDurableQueue`.
#[derive(Debug, Default)]
pub(crate) struct LoadedQueue {
    pub(crate) entries: BTreeMap<String, QueueEntry>,
    pub(crate) delivered: BTreeMap<String, DeliveredRecord>,
}

/// `newDurableQueue` — read `queue.json` under `dir`; absent is empty,
/// undecodable is salvaged + quarantined + rebuilt (see the module doc
/// for the deliberate delta from the oracle's fatal decode error).
pub(crate) fn load_queue(dir: &Path) -> io::Result<LoadedQueue> {
    let path = dir.join(QUEUE_FILENAME);
    let data = match std::fs::read(&path) {
        Ok(data) => data,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(LoadedQueue::default()),
        Err(e) => return Err(e),
    };
    match serde_json::from_slice::<QueueFileRead>(&data) {
        Ok(file) => Ok(LoadedQueue {
            entries: file.entries,
            delivered: file.delivered,
        }),
        Err(strict_error) => {
            // Member-wise salvage: every entry/record that still decodes
            // survives; the corrupt original is renamed aside and the
            // file rebuilt from the salvaged halves.
            let mut loaded = LoadedQueue::default();
            let mut dropped = 0usize;
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&data) {
                dropped += salvage_field(&value, "entries", &mut loaded.entries);
                dropped += salvage_field(&value, "delivered", &mut loaded.delivered);
            }
            warn!(
                error = %strict_error,
                dropped, "push queue file is corrupt; quarantining and rebuilding"
            );
            quarantine(&path)?;
            persist(dir, &loaded.entries, &loaded.delivered).map_err(|code| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("rebuild push queue: {code}"),
                )
            })?;
            Ok(loaded)
        }
    }
}

/// Decode the decodable members of `value[field]` into `target`;
/// returns how many were dropped. A missing/non-object field drops
/// nothing — it contributes zero members (the strict decode already
/// failed for the file to get here).
fn salvage_field<T: serde::de::DeserializeOwned>(
    value: &serde_json::Value,
    field: &str,
    target: &mut BTreeMap<String, T>,
) -> usize {
    let mut dropped = 0;
    let Some(object) = value.get(field).and_then(|v| v.as_object()) else {
        return dropped;
    };
    for (id, raw) in object {
        match serde_json::from_value::<T>(raw.clone()) {
            Ok(decoded) => {
                target.insert(id.clone(), decoded);
            }
            Err(_) => dropped += 1,
        }
    }
    dropped
}

/// `quarantineIndex`-style rename — move the undecodable file aside
/// for forensics: `queue.invalid-<unix-nanos>.json`, `-<n>` suffixes on
/// collision. Failure aborts the load (the caller surfaces it as a
/// `Push::new` error → in-memory fallback), matching `uploads`' rule
/// that an unquarantineable index aborts `NewManager`.
fn quarantine(path: &Path) -> io::Result<()> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    for attempt in 0..1000u32 {
        let name = if attempt == 0 {
            format!("queue.invalid-{stamp}.json")
        } else {
            format!("queue.invalid-{stamp}-{attempt}.json")
        };
        let target = dir.join(name);
        match std::fs::symlink_metadata(&target) {
            Ok(_) => continue,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                std::fs::rename(path, &target)?;
                return Ok(());
            }
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "push queue quarantine name exhausted",
    ))
}

/// `durableQueue.persistLocked` — `maxQueueEntries`/`maxQueueBytes`
/// caps, then `MarshalIndent` + `\n` through `atomic_write` at `0600`.
/// `push_queue_limit` is the oracle's own error string for the caps;
/// `push_persist_failed` matches the sibling files' I/O mapping.
pub(crate) fn persist(
    dir: &Path,
    entries: &BTreeMap<String, QueueEntry>,
    delivered: &BTreeMap<String, DeliveredRecord>,
) -> Result<(), &'static str> {
    if entries.len() + delivered.len() > MAX_QUEUE_ENTRIES {
        return Err("push_queue_limit");
    }
    let file = QueueFile { entries, delivered };
    let mut data = serde_json::to_string_pretty(&file).map_err(|_| "push_persist_failed")?;
    data.push('\n');
    // The oracle bounds `len(marshal) + 1` — the newline is already in.
    if data.len() > MAX_QUEUE_BYTES {
        return Err("push_queue_limit");
    }
    atomic_write(&dir.join(QUEUE_FILENAME), data.as_bytes(), 0o600)
        .map_err(|_| "push_persist_failed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::push::{
        PublishRequest, Push, PushEvent, PushEventKey, Subscription, SubscriptionKeys, Timestamp,
    };

    fn sub(device: &str, endpoint: &str) -> Subscription {
        Subscription {
            endpoint: endpoint.to_owned(),
            keys: SubscriptionKeys {
                p256dh: "key".to_owned(),
                auth: "auth".to_owned(),
            },
            device_id: device.to_owned(),
            locale: "en".to_owned(),
            platform: "other".to_owned(),
            user_agent: String::new(),
            notify_finished: false,
            client_id: "client".to_owned(),
        }
    }

    fn key(device: &str, event_id: &str) -> PushEventKey {
        PushEventKey {
            device_id: device.to_owned(),
            server_session_id: "primary".to_owned(),
            pane_id: "pane-1".to_owned(),
            terminal_id: "term-1".to_owned(),
            agent_session_id: "sess-1".to_owned(),
            generation: 0,
            event_id: event_id.to_owned(),
            interaction_revision: 1,
            category: "question".to_owned(),
        }
    }

    fn entry(id: &str, event_id: &str, due_at: Timestamp) -> QueueEntry {
        QueueEntry {
            id: id.to_owned(),
            event: PushEvent {
                key: key("device-1", event_id),
                payload: b"{\"v\":1}".to_vec(),
                created_at: due_at,
                expires_at: due_at.add_ns(60_000_000_000),
                retract: false,
            },
            subscription: sub("device-1", "https://fcm.googleapis.com/send/x"),
            due_at,
            attempts: 0,
        }
    }

    fn record(id: &str, event_id: &str, accepted_at: Timestamp) -> DeliveredRecord {
        DeliveredRecord {
            key: key("device-1", event_id),
            subscription: sub("device-1", "https://fcm.googleapis.com/send/x"),
            tag: format!("herdr-{id}"),
            accepted_at,
        }
    }

    fn publish(push: &Push, event_id: &str, created_at: Timestamp) {
        let result = push
            .publish(PublishRequest {
                key: key("device-1", event_id),
                preview: "question",
                created_at: Some(created_at),
                expires_at: Some(created_at.add_ns(300 * 1_000_000_000)),
            })
            .expect("publish");
        assert!(result.queued > 0, "publish queued nothing");
    }

    #[test]
    fn missing_file_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = load_queue(dir.path()).unwrap();
        assert!(loaded.entries.is_empty());
        assert!(loaded.delivered.is_empty());
        assert!(!dir.path().join(QUEUE_FILENAME).exists());
    }

    #[test]
    fn file_layout_matches_oracle_shape() {
        let dir = tempfile::tempdir().unwrap();
        let now = Timestamp::now();
        let mut entries = BTreeMap::new();
        entries.insert("id-a".to_owned(), entry("id-a", "evt-1", now));
        let mut retracted = entry("id-b", "evt-2", now);
        retracted.event.retract = true;
        entries.insert("id-b".to_owned(), retracted);
        let mut delivered = BTreeMap::new();
        delivered.insert("id-c".to_owned(), record("id-c", "evt-3", now));
        persist(dir.path(), &entries, &delivered).unwrap();

        let raw = std::fs::read_to_string(dir.path().join(QUEUE_FILENAME)).unwrap();
        assert!(raw.ends_with("}\n"), "MarshalIndent + newline: {raw:?}");
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        // `queueFile` — both maps always present.
        let entry_a = &value["entries"]["id-a"];
        assert_eq!(
            entry_a["event"]["key"]["event_id"],
            serde_json::json!("evt-1")
        );
        // Go `[]byte` → base64.StdEncoding string.
        assert_eq!(
            entry_a["event"]["payload"],
            serde_json::json!("eyJ2IjoxfQ==")
        );
        // `retract` omits when false, emits when true (Go `omitempty`).
        assert!(entry_a["event"].get("retract").is_none());
        assert_eq!(
            value["entries"]["id-b"]["event"]["retract"],
            serde_json::json!(true)
        );
        // The full Go Subscription shape rides inside each entry;
        // `omitempty` fields absent when empty.
        assert_eq!(
            entry_a["subscription"]["device_id"],
            serde_json::json!("device-1")
        );
        assert!(entry_a["subscription"].get("user_agent").is_none());
        // `deliveredRecord` — key/subscription/tag/accepted_at always emit.
        let record = &value["delivered"]["id-c"];
        assert_eq!(record["tag"], serde_json::json!("herdr-id-c"));
        assert!(record["accepted_at"].is_string());
        // Mode is the oracle's 0600.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(dir.path().join(QUEUE_FILENAME))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn roundtrips_entries_and_delivered() {
        let dir = tempfile::tempdir().unwrap();
        let now = Timestamp::now();
        let mut entries = BTreeMap::new();
        entries.insert("id-a".to_owned(), entry("id-a", "evt-1", now));
        let mut delivered = BTreeMap::new();
        delivered.insert("id-c".to_owned(), record("id-c", "evt-3", now));
        persist(dir.path(), &entries, &delivered).unwrap();
        let loaded = load_queue(dir.path()).unwrap();
        assert_eq!(loaded.entries, entries);
        assert_eq!(loaded.delivered, delivered);
    }

    #[test]
    fn corrupt_file_quarantines_and_rebuilds_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(QUEUE_FILENAME);
        std::fs::write(&path, b"{not json at all").unwrap();
        let loaded = load_queue(dir.path()).unwrap();
        assert!(loaded.entries.is_empty());
        assert!(loaded.delivered.is_empty());
        // The corrupt original moved aside; a clean file stands in
        // its place (`queue.json` exists again — rebuilt, not the
        // original bytes).
        let quarantined: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| {
                let name = e.unwrap().file_name().into_string().unwrap();
                name.starts_with("queue.invalid-").then_some(name)
            })
            .collect();
        assert_eq!(quarantined.len(), 1, "quarantined files: {quarantined:?}");
        let rebuilt = std::fs::read_to_string(dir.path().join(QUEUE_FILENAME)).unwrap();
        let value: serde_json::Value = serde_json::from_str(&rebuilt).unwrap();
        assert_eq!(value["entries"], serde_json::json!({}));
        assert_eq!(value["delivered"], serde_json::json!({}));
    }

    #[test]
    fn partial_corruption_salvages_decodable_entries() {
        let dir = tempfile::tempdir().unwrap();
        // Seed a real file, then splice in an undecodable member.
        let now = Timestamp::now();
        let mut entries = BTreeMap::new();
        entries.insert("id-a".to_owned(), entry("id-a", "evt-1", now));
        persist(dir.path(), &entries, &BTreeMap::new()).unwrap();
        let path = dir.path().join(QUEUE_FILENAME);
        let mut value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        value["entries"]["id-bad"] = serde_json::json!({
            "id": "id-bad",
            "event": {"key": 42},
            "due_at": "not-a-time",
            "attempts": -3,
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();

        let loaded = load_queue(dir.path()).unwrap();
        // The decodable entry survives; the corrupt member dropped.
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries["id-a"].event.key.event_id, "evt-1");
        // …and the rebuilt file strictly decodes now.
        let reloaded = load_queue(dir.path()).unwrap();
        assert_eq!(reloaded.entries, loaded.entries);
        assert!(std::fs::read_dir(dir.path()).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .into_string()
            .unwrap()
            .starts_with("queue.invalid-")));
    }

    #[test]
    fn persist_enforces_oracle_entry_cap() {
        let dir = tempfile::tempdir().unwrap();
        let now = Timestamp::now();
        let entries: BTreeMap<String, QueueEntry> = (0..MAX_QUEUE_ENTRIES + 1)
            .map(|i| {
                let id = format!("id-{i}");
                (id.clone(), entry(&id, "evt", now))
            })
            .collect();
        assert_eq!(
            persist(dir.path(), &entries, &BTreeMap::new()),
            Err("push_queue_limit")
        );
    }

    /// `Push::new` wires the load: a published entry lands in
    /// `queue.json`, and a second `Push::new` recovers it (delivery
    /// itself stays gated on `reconcile` — the drain tests cover that).
    #[test]
    fn push_new_persists_and_recovers_entries() {
        let dir = tempfile::tempdir().unwrap();
        let push = Push::new(dir.path()).unwrap();
        push.subscribe(sub("device-1", "https://fcm.googleapis.com/send/x"), &[])
            .unwrap();
        publish(&push, "evt-1", Timestamp::now());
        assert!(dir.path().join(QUEUE_FILENAME).exists());

        let recovered = Push::new(dir.path()).unwrap();
        assert_eq!(recovered.recovered_keys(), vec![key("device-1", "evt-1")]);
        // Recovered keys activate but hold delivery until reconcile —
        // `m.reconciled = len(recovered) == 0`.
        assert!(!recovered.is_reconciled());
        let recovered_again = Push::new(dir.path()).unwrap();
        // `Publish` on the fresh handle dedups by delivery id — the
        // recovered entry keeps its slot rather than doubling.
        assert_eq!(recovered_again.recovered_keys().len(), 1);
    }
}
