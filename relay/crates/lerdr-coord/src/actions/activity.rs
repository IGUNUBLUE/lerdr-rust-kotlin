//! Relay-side activity journal — the `internal/activity` port.
//!
//! The oracle records every routed mutation (`recordActivity`: action,
//! status, summary, pane, request — success and failure) plus agent-state
//! transitions into a bounded journal; `get_activity` answers
//! `activity_history` with the newest entries in append order and
//! `clear_activities` drains it. Committed entries also fan out live as
//! `{"type":"activity","activity":…}` and a clear republishes an empty
//! history — [`Journal::subscribe`] is that `d.broadcast` half, the seam
//! the orchestrator wires to per-session sinks.
//!
//! The Go journal is a durable JSONL file guarded by tombstones; the
//! durable substrate is deliberately out of scope here — [`Journal`] is
//! the same ring-buffer contract (bounds, ordering, id/timestamp
//! stamping, live-feed events) over an in-memory `Mutex<VecDeque>`.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use lerdr_core::json::MaybeNull;
use lerdr_core::protocol::{
    ActivityEntry, ActivityHistoryMessage, ActivityMessage, Inbound, Outbound,
};
use tokio::sync::broadcast;

use super::{ActionContext, Outcome};

// journal.go bounds.
/// `maxItems` — entries retained, newest winning eviction.
const MAX_ITEMS: usize = 500;
/// `maxBytes` — serialized-bytes bound, evicting oldest first.
const MAX_BYTES: usize = 2 * 1024 * 1024;
/// `maxExtractChars` — `NormalizeEntry`'s rune cap on `extract`.
const MAX_EXTRACT_CHARS: usize = 100_000;
/// `get_activity`'s clamp — `intValue(message["limit"], 500)`.
const HISTORY_LIMIT: i64 = 500;

/// `entryIDSequence` — process-global like the oracle's package atomic,
/// so ids keep climbing across journals and clears.
static ENTRY_ID_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// What callers hand [`Journal::record`] — `activity.Entry` minus `id`,
/// which the journal stamps, and minus `timestamp`, which it stamps too
/// unless `timestamp` carries the `transition_at` override
/// `RecordTransitionActivity` applies.
#[allow(dead_code)] // consumed as action modules wire their recordActivity sites
#[derive(Debug, Clone, Default)]
pub(crate) struct NewEntry {
    /// `Entry.Kind` — the action name (`send_text`, `agent_stop`, …), the
    /// action-adjacent kinds (`input`, `approval`, `question`, `upload`),
    /// or the transition kinds (`working`, `blocked`, `finished`).
    pub kind: String,
    /// `Entry.Status` — `sent`/`completed`/`failed`/`approved`/… — the
    /// oracle's per-call-site strings, not a closed enum.
    pub status: String,
    /// `Entry.Summary` — the human sentence the feed shows.
    pub summary: String,
    /// `Entry.Host` — the pane's host for routed actions
    /// (`agentState.Host`), the relay's own hostname for transitions.
    pub host: String,
    /// `Entry.PaneID` — empty for pane-less actions (workspace/worktree).
    pub pane_id: String,
    /// `Entry.Agent` — the pane's agent name at record time.
    pub agent: String,
    /// `Entry.Project` — the pane's project label.
    pub project: String,
    /// `Entry.RequestID` — the action's `request_id` (empty for
    /// transition records, which have no originating request).
    pub request_id: String,
    /// `Entry.Extract` — the long-form payload (prompt text, agent
    /// response); `NormalizeEntry` truncates at `maxExtractChars` runes.
    pub extract: String,
    /// `Entry.Session` — the pane's agent session id.
    pub session: String,
    /// `Entry.Details` — `{"action": kind}` for routed actions
    /// ([`NewEntry::action`] fills it), transition metadata
    /// (`event_id`/`transition`/`attention_kind`/`transition_at`) for
    /// `RecordTransitionActivity`.
    pub details: Option<BTreeMap<String, serde_json::Value>>,
    /// `RecordTransitionActivity`'s `details["transition_at"]` override —
    /// stamp the state-change time instead of the append time.
    pub timestamp: Option<i64>,
}

#[allow(dead_code)]
impl NewEntry {
    /// `recordActivity`'s shape — `details.action` mirrors `kind`, the
    /// pane attribution rides `with_attribution`.
    pub(crate) fn action(
        kind: &str,
        status: &str,
        summary: impl Into<String>,
        pane_id: &str,
        request_id: &str,
    ) -> Self {
        Self {
            kind: kind.to_owned(),
            status: status.to_owned(),
            summary: summary.into(),
            pane_id: pane_id.to_owned(),
            request_id: request_id.to_owned(),
            details: Some(BTreeMap::from([(
                "action".to_owned(),
                serde_json::Value::String(kind.to_owned()),
            )])),
            ..Self::default()
        }
    }

    /// `recordActivityWithExtract`'s payload (`Extract` in Go).
    pub(crate) fn with_extract(mut self, extract: impl Into<String>) -> Self {
        self.extract = extract.into();
        self
    }

    /// The pane attribution `recordActivityWithExtract` reads out of
    /// `d.state.Agent(paneID)` — agent/project/host/session.
    pub(crate) fn with_attribution(
        mut self,
        agent: &str,
        project: &str,
        host: &str,
        session: &str,
    ) -> Self {
        self.agent = agent.to_owned();
        self.project = project.to_owned();
        self.host = host.to_owned();
        self.session = session.to_owned();
        self
    }
}

/// Live-feed event — the `d.broadcast` half of the journal: every commit
/// fans `{"type":"activity","activity":…}` out to all sessions, a clear
/// fans `{"type":"activity_history","activities":[]}`.
#[allow(dead_code)] // consumed once the orchestrator wires per-session fanout
#[derive(Debug, Clone)]
pub(crate) enum JournalEvent {
    /// A committed entry — broadcast `activity`. Boxed so the event stays
    /// small next to the payload-less `Cleared`.
    Recorded(Box<ActivityEntry>),
    /// The journal drained — broadcast `activity_history` with `[]`.
    Cleared,
}

#[allow(dead_code)]
impl JournalEvent {
    /// The wire frame the oracle broadcasts for this event.
    pub(crate) fn into_outbound(self) -> Outbound {
        match self {
            Self::Recorded(entry) => Outbound::Activity(ActivityMessage {
                activity: Some(MaybeNull::Value(*entry)),
                r#type: "activity".to_owned(),
            }),
            // `[]any{}` — an explicit empty array, never `null`.
            Self::Cleared => Outbound::ActivityHistory(ActivityHistoryMessage {
                activities: Some(MaybeNull::Value(Vec::new())),
                r#type: "activity_history".to_owned(),
            }),
        }
    }
}

/// Shared activity journal — one per relay (the oracle's
/// `activity.Journal` ring). Handlers call [`Journal::record`] as they
/// complete so the feed reflects what actually happened.
#[derive(Clone)]
pub(crate) struct Journal {
    inner: Arc<Mutex<Inner>>,
    /// `d.broadcast` — every mutation emits a [`JournalEvent`] for the
    /// fanout task (no receivers is a silent no-op, like a nil hub).
    events: broadcast::Sender<JournalEvent>,
}

#[derive(Default)]
struct Inner {
    /// Rows newest-at-the-back; each carries its serialized `encodeEntry`
    /// length (payload + newline) so `retainWithinLimits` stays O(1).
    entries: VecDeque<(usize, ActivityEntry)>,
    bytes: usize,
}

impl Default for Journal {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            // The oracle's worker mailbox is 64 deep; the fanout buffer
            // only bounds a lagging subscriber's backlog.
            events: broadcast::channel(64).0,
        }
    }
}

impl Journal {
    /// Record one completed/failed action — the oracle's
    /// `recordActivity`/`Commit`: normalize, stamp `id` + `timestamp`,
    /// append, evict oldest past the ring bounds, return the committed
    /// entry so the caller can wire the `activity` push.
    #[allow(dead_code)] // consumed as action modules wire their recordActivity sites
    pub(crate) fn record(&self, entry: NewEntry) -> ActivityEntry {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let entry = ActivityEntry {
            // `NewEntry` — `{unixnano}-{sequence}-{paneID}`.
            id: format!(
                "{}-{}-{}",
                now.as_nanos(),
                ENTRY_ID_SEQUENCE.fetch_add(1, Ordering::Relaxed) + 1,
                entry.pane_id
            ),
            timestamp: entry.timestamp.unwrap_or(now.as_millis() as i64),
            kind: entry.kind,
            status: entry.status,
            summary: entry.summary,
            host: entry.host,
            pane_id: entry.pane_id,
            agent: entry.agent,
            project: entry.project,
            request_id: entry.request_id,
            extract: truncate_chars(&entry.extract, MAX_EXTRACT_CHARS),
            session: entry.session,
            details: entry.details,
        };
        let size = encoded_len(&entry);
        {
            let mut inner = self.inner.lock().expect("activity journal poisoned");
            inner.bytes += size;
            inner.entries.push_back((size, entry.clone()));
            // `retainWithinLimits` — the newest entries win both bounds.
            // The byte bound always keeps at least the newest entry; a
            // single entry exceeding `maxBytes` is unreachable here
            // (extract, the only unbounded field, is truncated far below
            // it), where the oracle would reject the append outright.
            while inner.entries.len() > MAX_ITEMS {
                if let Some((size, _)) = inner.entries.pop_front() {
                    inner.bytes -= size;
                }
            }
            while inner.bytes > MAX_BYTES && inner.entries.len() > 1 {
                if let Some((size, _)) = inner.entries.pop_front() {
                    inner.bytes -= size;
                }
            }
        }
        if self.events.receiver_count() > 0 {
            let _ = self
                .events
                .send(JournalEvent::Recorded(Box::new(entry.clone())));
        }
        entry
    }

    /// `Recent` — the newest `limit` entries, oldest first. A `limit` of
    /// 0 or above the retained count answers everything.
    pub(crate) fn recent(&self, limit: usize) -> Vec<ActivityEntry> {
        let inner = self.inner.lock().expect("activity journal poisoned");
        let len = inner.entries.len();
        let limit = if limit == 0 || limit > len {
            len
        } else {
            limit
        };
        inner
            .entries
            .iter()
            .skip(len - limit)
            .map(|(_, entry)| entry.clone())
            .collect()
    }

    /// `Clear` — drains the journal; the [`JournalEvent::Cleared`]
    /// broadcast is the oracle's empty-history republish.
    pub(crate) fn clear(&self) {
        {
            let mut inner = self.inner.lock().expect("activity journal poisoned");
            inner.entries.clear();
            inner.bytes = 0;
        }
        let _ = self.events.send(JournalEvent::Cleared);
    }

    /// `d.broadcast` — subscribe to journal events for fanout to every
    /// session's client sink.
    #[allow(dead_code)] // consumed once the orchestrator wires per-session fanout
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<JournalEvent> {
        self.events.subscribe()
    }
}

/// `NormalizeEntry` — `extract` is capped at `maxExtractChars` **runes**.
fn truncate_chars(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_owned();
    }
    value.chars().take(max).collect()
}

/// `encodeEntry` — the serialized line length (payload + `\n`) the byte
/// bound charges against.
fn encoded_len(entry: &ActivityEntry) -> usize {
    lerdr_core::json::to_vec(entry)
        .map(|v| v.len() + 1)
        .unwrap_or(0)
}

/// `get_activity` — `{"type":"activity_history","activities":[…]}`.
/// `limit` rides the raw-field seam (`messageInt(msg["limit"], 500)`):
/// integral numbers only, clamped to [1, 500]. The oracle answers in
/// journal order — oldest → newest — never newest-first.
pub(crate) async fn get_activity(
    ctx: ActionContext,
    _request_id: &str,
    _action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    let limit = message.raw_int("limit").unwrap_or(HISTORY_LIMIT);
    let limit = if (1..=HISTORY_LIMIT).contains(&limit) {
        limit as usize
    } else {
        HISTORY_LIMIT as usize
    };
    let entries = ctx.activities.recent(limit);
    vec![Outbound::ActivityHistory(ActivityHistoryMessage {
        // The oracle's `recentActivities` maps an empty view to
        // `"activities":null`, not `[]` (`append(nil)` stays nil).
        activities: Some(if entries.is_empty() {
            MaybeNull::Null
        } else {
            MaybeNull::Value(entries)
        }),
        r#type: "activity_history".to_owned(),
    })]
}

/// `HandleClearActivities` — drains the journal, then `command_result`
/// (`completed`) + receipt. The empty-history republish reaches all
/// sessions through [`Journal::subscribe`]; the oracle's failure branches
/// ("Activity storage is unavailable", "could not be cleared") are
/// I/O-only and unreachable for the in-memory ring.
pub(crate) async fn clear_activities(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    ctx.activities.clear();
    Outcome::completed("", None).frames(request_id, "clear_activities", action_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::Acks;
    use crate::topology::Topology;
    use crate::TopologyActor;
    use tokio_util::sync::CancellationToken;

    fn entry(summary: &str) -> NewEntry {
        NewEntry::action("send_text", "sent", summary, "pane-1", "req-1")
    }

    /// The `get_activity`/`clear_activities` handlers need an
    /// `ActionContext`; nothing they touch dials Herdr, so a dead socket
    /// and a pre-cancelled actor token keep the fixture quiet.
    fn test_context(journal: Journal) -> ActionContext {
        let client = lerdr_herdr::Client::unix("/nonexistent-lerdr-activity-test.sock");
        let cancel = CancellationToken::new();
        cancel.cancel();
        ActionContext {
            client: client.clone(),
            topology: Arc::new(Topology::default()),
            handle: TopologyActor::spawn(client.clone(), cancel),
            leases: crate::actions::leases::Leases::new(client),
            acks: Acks::default(),
            profiles: crate::actions::profiles::Resolver::with_config_home(
                tempfile::tempdir().expect("tempdir").keep(),
            ),
            questions: crate::actions::questions::Questions::default(),
            uploads: crate::actions::uploads::Uploads::new(
                tempfile::tempdir().expect("tempdir").keep(),
            ),
            activities: journal,
            push: crate::actions::push::Push::default(),
            speech: crate::actions::speech::Speech::default(),
            notices: crate::actions::Notices::default(),
            client_id: "test-client".to_owned(),
        }
    }

    fn inbound(fields: serde_json::Value) -> Inbound {
        Inbound::decode(&serde_json::to_vec(&fields).unwrap()).expect("inbound")
    }

    #[test]
    fn record_stamps_id_timestamp_and_action_details() {
        let journal = Journal::default();
        let entry = journal.record(entry("hello"));
        // `{unixnano}-{sequence}-{paneID}` — pane ids contain '-', so
        // split the `{nanos}-{seq}` head off the pane suffix instead.
        let head = entry
            .id
            .strip_suffix("-pane-1")
            .expect("id ends with the pane id");
        let mut head = head.rsplitn(2, '-');
        assert!(head.next().unwrap().parse::<u64>().is_ok(), "sequence");
        let nanos = head.next().unwrap();
        assert!(
            nanos.len() >= 19 && nanos.parse::<u64>().is_ok(),
            "unix nanos"
        );
        assert!(entry.timestamp > 0);
        assert_eq!(entry.kind, "send_text");
        assert_eq!(entry.status, "sent");
        assert_eq!(entry.summary, "hello");
        assert_eq!(entry.pane_id, "pane-1");
        assert_eq!(entry.request_id, "req-1");
        // `recordActivity` fills `details.action` with the kind.
        assert_eq!(
            entry.details.as_ref().and_then(|d| d.get("action")),
            Some(&serde_json::json!("send_text"))
        );
    }

    #[test]
    fn record_ids_are_unique() {
        let journal = Journal::default();
        let mut ids = std::collections::BTreeSet::new();
        for _ in 0..64 {
            let id = journal.record(entry("x")).id;
            assert!(ids.insert(id.clone()), "duplicate id {id}");
        }
    }

    #[test]
    fn record_timestamps_non_decreasing() {
        let journal = Journal::default();
        let mut last = 0;
        for _ in 0..16 {
            let timestamp = journal.record(entry("x")).timestamp;
            assert!(
                timestamp >= last,
                "timestamp regressed: {timestamp} < {last}"
            );
            last = timestamp;
        }
    }

    #[test]
    fn timestamp_override_stamps_transition_at() {
        let journal = Journal::default();
        let mut entry = entry("finished");
        entry.timestamp = Some(1_750_000_000_000);
        assert_eq!(journal.record(entry).timestamp, 1_750_000_000_000);
    }

    #[test]
    fn recent_answers_oldest_first_with_newest_suffix() {
        let journal = Journal::default();
        for i in 0..10 {
            journal.record(entry(&format!("e{i}")));
        }
        let all = journal.recent(0);
        assert_eq!(all.len(), 10);
        assert_eq!(all[0].summary, "e0");
        assert_eq!(all[9].summary, "e9");
        // `Recent(limit)` keeps the newest `limit` in append order.
        let tail = journal.recent(3);
        assert_eq!(
            tail.iter().map(|e| e.summary.as_str()).collect::<Vec<_>>(),
            ["e7", "e8", "e9"]
        );
        // Over-large limit answers everything, like the oracle's clamp.
        assert_eq!(journal.recent(500).len(), 10);
    }

    #[test]
    fn ring_evicts_oldest_past_max_items() {
        let journal = Journal::default();
        for i in 0..(MAX_ITEMS + 25) {
            journal.record(entry(&format!("e{i}")));
        }
        let retained = journal.recent(0);
        assert_eq!(retained.len(), MAX_ITEMS);
        assert_eq!(retained[0].summary, "e25");
        assert_eq!(
            retained[MAX_ITEMS - 1].summary,
            format!("e{}", MAX_ITEMS + 24)
        );
    }

    #[test]
    fn byte_bound_evicts_oldest_keeps_newest() {
        let journal = Journal::default();
        // ~100KB serialized each — 40 entries blow past 2 MiB.
        for i in 0..40 {
            let fill = char::from(b'a' + (i % 26) as u8)
                .to_string()
                .repeat(100_000);
            journal.record(entry("large").with_extract(fill));
        }
        let retained = journal.recent(0);
        assert!(
            !retained.is_empty() && retained.len() < 40,
            "byte bound must retain a nonempty suffix, got {}",
            retained.len()
        );
        assert!(retained.iter().all(|e| e.summary == "large"));
        // The newest entry always survives the byte bound.
        let newest = journal.record(entry("latest"));
        assert_eq!(journal.recent(1)[0].id, newest.id);
    }

    #[test]
    fn extract_truncates_at_max_runes() {
        let journal = Journal::default();
        let recorded =
            journal.record(entry("big").with_extract("x".repeat(MAX_EXTRACT_CHARS + 1000)));
        assert_eq!(recorded.extract.chars().count(), MAX_EXTRACT_CHARS);
        // The bound counts runes, not bytes (NormalizeEntry).
        let recorded =
            journal.record(entry("wide").with_extract("é".repeat(MAX_EXTRACT_CHARS + 10)));
        assert_eq!(recorded.extract.chars().count(), MAX_EXTRACT_CHARS);
    }

    #[test]
    fn clear_drains_and_emits_cleared_event() {
        let journal = Journal::default();
        let mut rx = journal.subscribe();
        journal.record(entry("a"));
        journal.record(entry("b"));
        assert!(matches!(rx.try_recv(), Ok(JournalEvent::Recorded(_))));
        assert!(matches!(rx.try_recv(), Ok(JournalEvent::Recorded(_))));
        journal.clear();
        assert!(journal.recent(0).is_empty());
        assert!(matches!(rx.try_recv(), Ok(JournalEvent::Cleared)));
    }

    #[test]
    fn journal_events_map_to_broadcast_frames() {
        let journal = Journal::default();
        let entry = journal.record(entry("x"));
        let Outbound::Activity(message) =
            JournalEvent::Recorded(Box::new(entry.clone())).into_outbound()
        else {
            panic!("recorded event must map to `activity`")
        };
        assert_eq!(message.r#type, "activity");
        assert_eq!(
            message.activity.and_then(|a| a.into_value()).unwrap().id,
            entry.id
        );
        let Outbound::ActivityHistory(message) = JournalEvent::Cleared.into_outbound() else {
            panic!("cleared event must map to `activity_history`")
        };
        assert_eq!(message.r#type, "activity_history");
        assert!(
            matches!(message.activities, Some(MaybeNull::Value(v)) if v.is_empty()),
            "clear broadcasts `activities:[]`, never null"
        );
    }

    #[tokio::test]
    async fn get_activity_answers_history_oldest_first() {
        let journal = Journal::default();
        journal.record(entry("one"));
        journal.record(entry("two"));
        let frames = get_activity(
            test_context(journal),
            "r1",
            "a1",
            &inbound(serde_json::json!({"type": "get_activity"})),
        )
        .await;
        let [Outbound::ActivityHistory(message)] = frames.as_slice() else {
            panic!("get_activity answers a single activity_history frame, got {frames:?}")
        };
        let entries = message
            .activities
            .as_ref()
            .and_then(MaybeNull::value)
            .expect("entries present");
        assert_eq!(
            entries
                .iter()
                .map(|e| e.summary.as_str())
                .collect::<Vec<_>>(),
            ["one", "two"]
        );
    }

    #[tokio::test]
    async fn get_activity_limit_clamps_like_the_oracle() {
        let journal = Journal::default();
        for i in 0..10 {
            journal.record(entry(&format!("e{i}")));
        }
        for (limit, want) in [
            (serde_json::json!(3), 3),
            (serde_json::json!(3.0), 3), // integral float — `messageInt` reads it
            (serde_json::json!(0), 10),  // below range → 500 → all
            (serde_json::json!(600), 10), // above range → 500 → all
            (serde_json::json!(-2), 10), // negative → 500 → all
            (serde_json::json!(null), 10), // absent/null → default → all
        ] {
            let frames = get_activity(
                test_context(journal.clone()),
                "r1",
                "a1",
                &inbound(serde_json::json!({"type": "get_activity", "limit": limit})),
            )
            .await;
            let [Outbound::ActivityHistory(message)] = frames.as_slice() else {
                panic!("expected activity_history, got {frames:?}")
            };
            let entries = message
                .activities
                .as_ref()
                .and_then(MaybeNull::value)
                .expect("entries present");
            assert_eq!(entries.len(), want, "limit {limit}");
        }
        // limit=3 keeps the newest three.
        let frames = get_activity(
            test_context(journal),
            "r1",
            "a1",
            &inbound(serde_json::json!({"type": "get_activity", "limit": 3})),
        )
        .await;
        let [Outbound::ActivityHistory(message)] = frames.as_slice() else {
            panic!()
        };
        let entries = message
            .activities
            .as_ref()
            .and_then(MaybeNull::value)
            .unwrap();
        assert_eq!(entries[0].summary, "e7");
    }

    #[tokio::test]
    async fn get_activity_empty_journal_is_null() {
        let frames = get_activity(
            test_context(Journal::default()),
            "r1",
            "a1",
            &inbound(serde_json::json!({"type": "get_activity"})),
        )
        .await;
        let [Outbound::ActivityHistory(message)] = frames.as_slice() else {
            panic!()
        };
        // The oracle serializes an empty view as `"activities":null`.
        assert!(matches!(message.activities, Some(MaybeNull::Null)));
    }

    #[tokio::test]
    async fn clear_activities_drains_then_reports_completed() {
        let journal = Journal::default();
        journal.record(entry("gone"));
        let observer = journal.clone();
        let frames = clear_activities(
            test_context(journal),
            "r1",
            "a1",
            &inbound(serde_json::json!({"type": "clear_activities"})),
        )
        .await;
        assert!(observer.recent(0).is_empty(), "journal drained");
        let mut iter = frames.into_iter();
        let Outbound::CommandResult(result) = iter.next().expect("command_result") else {
            panic!("expected command_result first")
        };
        assert_eq!(result.ok, Some(true));
        assert_eq!(result.phase.as_deref(), Some("completed"));
        assert_eq!(result.action.as_deref(), Some("clear_activities"));
        let Outbound::ActionReceipt(receipt) = iter.next().expect("receipt") else {
            panic!("expected action_receipt last")
        };
        assert_eq!(
            receipt.receipt.expect("receipt payload").phase.as_str(),
            "confirmed"
        );
        assert!(iter.next().is_none(), "result + receipt and nothing else");
    }
}
