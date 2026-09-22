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
//! The Go journal is durable: `<cacheDir>/activity.jsonl` (0600) holds
//! one `encodeEntry` JSON object per line and `activity.tombstones` is a
//! write-ahead set of doomed entry ids making clear/compaction
//! crash-safe — on open the file is authoritative modulo tombstones, it
//! is rewritten (`compactEntriesLocked`) when it drifted from canonical
//! form, and recovered tombstones are cleaned. [`Journal::open`] is
//! `OpenJournal`; [`Journal::default`] is the same ring-buffer contract
//! (bounds, ordering, id/timestamp stamping, live-feed events) purely in
//! memory for tests and callers that cannot surface I/O.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
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
/// `filepath.Join(cacheDir, "activity.jsonl")` — the journal file.
const JOURNAL_FILENAME: &str = "activity.jsonl";
/// `filepath.Join(cacheDir, "activity.tombstones")` — the write-ahead
/// doomed-id set guarding clear/compaction rewrites.
const TOMBSTONE_FILENAME: &str = "activity.tombstones";

/// `entryIDSequence` — process-global like the oracle's package atomic,
/// so ids keep climbing across journals and clears.
static ENTRY_ID_SEQUENCE: AtomicU64 = AtomicU64::new(0);
/// `os.CreateTemp`'s disambiguator — unique sibling temp names under the
/// journal directory.
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
    /// `path`/`tombstonePath`/`dir` — `None` keeps the journal a pure
    /// in-memory ring ([`Journal::default`]); `Some` is the durable pair
    /// [`Journal::open`] recovered and every mutation maintains.
    storage: Option<Arc<Storage>>,
    /// `d.broadcast` — every mutation emits a [`JournalEvent`] for the
    /// fanout task (no receivers is a silent no-op, like a nil hub).
    events: broadcast::Sender<JournalEvent>,
}

/// `Journal.path`/`tombstonePath`/`dir` — the file layout under the
/// relay's cache dir.
struct Storage {
    /// The containing directory — fsync'd after every rename/removal so
    /// the change itself is durable.
    dir: PathBuf,
    /// `activity.jsonl` — one `encodeEntry` JSON object per line,
    /// append-only between compactions.
    journal: PathBuf,
    /// `activity.tombstones` — the write-ahead doomed-id set; applied on
    /// `open`, then removed.
    tombstones: PathBuf,
}

impl Storage {
    fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
            journal: dir.join(JOURNAL_FILENAME),
            tombstones: dir.join(TOMBSTONE_FILENAME),
        }
    }
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
            storage: None,
            // The oracle's worker mailbox is 64 deep; the fanout buffer
            // only bounds a lagging subscriber's backlog.
            events: broadcast::channel(64).0,
        }
    }
}

impl Journal {
    /// `OpenJournal` — open (creating) the durable journal under `dir`
    /// and replay it. Failure means the directory cannot serve the
    /// oracle's durability contract; the factory falls back to
    /// [`Journal::default`]'s in-memory ring.
    #[allow(dead_code)] // consumed once the orchestrator wires the runtime dir
    pub(crate) fn open(dir: &Path) -> io::Result<Journal> {
        // `os.MkdirAll(cacheDir, 0o700)` + `os.Chmod` — create the
        // directory and repair its mode on every open.
        fs::create_dir_all(dir).map_err(|error| io_err("create activity directory", error))?;
        set_mode(dir, 0o700).map_err(|error| io_err("protect activity directory", error))?;
        let journal = Journal {
            inner: Arc::new(Mutex::new(Inner::default())),
            storage: Some(Arc::new(Storage::new(dir))),
            events: broadcast::channel(64).0,
        };
        journal.load()?;
        Ok(journal)
    }

    /// `(*Journal).load` — the file is authoritative modulo tombstones:
    /// replay `activity.jsonl` bounded at `maxBytes*4`, drop doomed ids,
    /// bound the ring, rewrite the journal when it drifted
    /// (`needsCompact`), then clean recovered tombstones.
    fn load(&self) -> io::Result<()> {
        let storage = self
            .storage
            .as_ref()
            .expect("load runs under Journal::open")
            .clone();
        let tombstones = load_tombstones(&storage.tombstones)?;
        let file = match File::open(&storage.journal) {
            Err(error) if error.kind() == ErrorKind::NotFound => {
                // Tombstones without a journal are the orphans of a
                // finished clear — remove them and start empty.
                if !tombstones.is_empty() {
                    write_tombstones(&storage, &BTreeSet::new())
                        .map_err(|error| io_err("clean orphaned activity tombstones", error))?;
                }
                return Ok(());
            }
            Err(error) => return Err(io_err("read activity journal", error)),
            Ok(file) => file,
        };
        let size = file
            .metadata()
            .map_err(|error| io_err("read activity journal", error))?
            .len();
        if size > MAX_BYTES as u64 * 4 {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                format!("activity journal is corrupt or oversized: {size} bytes"),
            ));
        }
        // `io.LimitReader(file, maxBytes*4+1)` — the stat bound already
        // caps it; the belt covers the stat-then-read race.
        let mut contents = Vec::new();
        file.take(MAX_BYTES as u64 * 4 + 1)
            .read_to_end(&mut contents)
            .map_err(|error| io_err("scan activity journal", error))?;
        let mut needs_compact = !tombstones.is_empty() || size > MAX_BYTES as u64;
        let mut scanned = Vec::new();
        for raw in contents.split(|byte| *byte == b'\n') {
            let line = strip_cr(raw);
            if line.is_empty() {
                continue;
            }
            if line.len() > MAX_BYTES {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    "scan activity journal: line exceeds the journal limit",
                ));
            }
            // `json.Unmarshal` failures skip the line; the rewrite below
            // drops them from the file.
            let Some(mut entry) = decode_entry(line) else {
                continue;
            };
            // `entryIDSequence` continuity — never reissue a loaded id.
            bump_sequence(&entry.id);
            if tombstones.contains(&entry.id) {
                continue;
            }
            let normalized = truncate_chars(&entry.extract, MAX_EXTRACT_CHARS);
            if normalized != entry.extract {
                entry.extract = normalized;
                needs_compact = true;
            }
            scanned.push(entry);
        }
        let scanned_len = scanned.len();
        let retained = retain_within_limits(scanned)
            .map_err(|error| io_err("normalize activity journal", error))?;
        let retained_bytes: usize = retained.iter().map(|(size, _)| *size).sum();
        if retained.len() != scanned_len || retained_bytes as u64 != size {
            needs_compact = true;
        }
        if needs_compact {
            compact(&storage, retained.iter().map(|(_, entry)| entry))
                .map_err(|error| io_err("compact activity journal", error))?;
        }
        if !tombstones.is_empty() {
            write_tombstones(&storage, &BTreeSet::new())
                .map_err(|error| io_err("clean recovered activity tombstones", error))?;
        }
        let mut inner = self.inner.lock().expect("activity journal poisoned");
        inner.entries = retained.into_iter().collect();
        inner.bytes = retained_bytes;
        Ok(())
    }

    /// Record one completed/failed action — the oracle's
    /// `recordActivity`/`Commit`: normalize, stamp `id` + `timestamp`,
    /// append, evict oldest past the ring bounds, return the committed
    /// entry so the caller can wire the `activity` push.
    ///
    /// Durable journals write `activity.jsonl` first and mutate the ring
    /// only after the write+fsync lands; a failed append drops the entry
    /// (the oracle's caller logs `activity append failed` and skips the
    /// broadcast) so memory and file never diverge — the file is
    /// authoritative on the next [`Journal::open`].
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
        {
            let mut inner = self.inner.lock().expect("activity journal poisoned");
            if let Some(storage) = &self.storage {
                if let Err(error) = append_durable(storage, &mut inner, entry.clone()) {
                    // `recordActivity` — the oracle logs
                    // "activity append failed" and skips the broadcast.
                    tracing::warn!("activity append failed: {error}");
                    return entry;
                }
            } else {
                let size = encoded_len(&entry);
                inner.bytes += size;
                inner.entries.push_back((size, entry.clone()));
                // `retainWithinLimits` — the newest entries win both
                // bounds. The byte bound always keeps at least the
                // newest entry; a single entry exceeding `maxBytes` is
                // unreachable here (extract, the only unbounded field,
                // is truncated far below it), where the oracle would
                // reject the append outright.
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
    /// broadcast is the oracle's empty-history republish. Durable I/O
    /// failures are swallowed here — callers with an error surface
    /// (`clear_activities`) use [`Journal::try_clear`].
    #[allow(dead_code)] // tests + callers without an error surface
    pub(crate) fn clear(&self) {
        let _ = self.try_clear();
    }

    /// `Journal.Clear` — write the tombstone set of drained ids FIRST
    /// (the crash window: if we die mid-clear, the tombstones name the
    /// lines the next open must drop), rewrite the journal empty, then
    /// restore the pre-clear tombstones — durably empty, then cleanup.
    /// A compaction failure restores the original tombstones before the
    /// error escapes.
    fn try_clear(&self) -> io::Result<()> {
        let mut inner = self.inner.lock().expect("activity journal poisoned");
        if let Some(storage) = &self.storage {
            let previous = load_tombstones(&storage.tombstones)?;
            let mut tombstones = previous.clone();
            for (_, entry) in &inner.entries {
                if !entry.id.is_empty() {
                    tombstones.insert(entry.id.clone());
                }
            }
            write_tombstones(storage, &tombstones)
                .map_err(|error| io_err("prepare activity clear", error))?;
            if let Err(error) = compact(storage, std::iter::empty::<&ActivityEntry>()) {
                return match write_tombstones(storage, &previous) {
                    Ok(()) => Err(io_err("clear activity journal", error)),
                    Err(restore) => Err(io::Error::other(format!(
                        "clear activity journal: {error}; \
                         restore activity tombstones: {restore}"
                    ))),
                };
            }
            // The journal is durably empty. A leftover tombstone file is
            // harmless — the next `open` recovery removes it.
            let _ = write_tombstones(storage, &previous);
        }
        inner.entries.clear();
        inner.bytes = 0;
        drop(inner);
        let _ = self.events.send(JournalEvent::Cleared);
        Ok(())
    }

    /// `d.broadcast` — subscribe to journal events for fanout to every
    /// session's client sink.
    #[allow(dead_code)] // consumed once the orchestrator wires per-session fanout
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<JournalEvent> {
        self.events.subscribe()
    }
}

// ── durable substrate ────────────────────────────────────────────────

/// `encodeEntry` — `json.Marshal(entry)` + the JSONL newline.
fn encode_line(entry: &ActivityEntry) -> io::Result<Vec<u8>> {
    let mut data = lerdr_core::json::to_vec(entry).map_err(io::Error::other)?;
    data.push(b'\n');
    Ok(data)
}

/// `json.Unmarshal` into `activity.Entry` — the caller skips `None`
/// (corrupt lines). `MilliTimestamp.UnmarshalJSON` additionally accepts
/// an RFC3339 string; patch it into the millis integer `ActivityEntry`
/// models and retry, so the read path covers every journal the oracle
/// can open.
fn decode_entry(line: &[u8]) -> Option<ActivityEntry> {
    if let Ok(entry) = serde_json::from_slice::<ActivityEntry>(line) {
        return Some(entry);
    }
    let mut value = serde_json::from_slice::<serde_json::Value>(line).ok()?;
    let timestamp = value.get_mut("timestamp")?;
    let serde_json::Value::String(text) = timestamp else {
        return None;
    };
    *timestamp = serde_json::Value::from(rfc3339_millis(text)?);
    serde_json::from_value(value).ok()
}

/// `time.Parse(time.RFC3339)` → `UnixMilli` — `MilliTimestamp`'s string
/// half. Strict `YYYY-MM-DDTHH:MM:SS[.frac]` + `Z`/`z` or `±HH:MM`.
fn rfc3339_millis(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() < 20 {
        return None;
    }
    let digits = |offset: usize, count: usize| -> Option<i64> {
        let slice = bytes.get(offset..offset + count)?;
        if !slice.iter().all(u8::is_ascii_digit) {
            return None;
        }
        Some(
            slice
                .iter()
                .fold(0i64, |acc, b| acc * 10 + i64::from(b - b'0')),
        )
    };
    if bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let year = digits(0, 4)?;
    let month = digits(5, 2)?;
    let day = digits(8, 2)?;
    if !(1..=12).contains(&month) || day < 1 || day > i64::from(days_in_month(month, year)) {
        return None;
    }
    if bytes[10] != b'T' && bytes[10] != b't' {
        return None;
    }
    let hour = digits(11, 2)?;
    let minute = digits(14, 2)?;
    let second = digits(17, 2)?;
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let mut index = 19;
    let mut sub_nanos = 0u64;
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        let start = index;
        let mut seen = 0u32;
        while bytes.get(index).is_some_and(|b| b.is_ascii_digit()) {
            if seen < 9 {
                sub_nanos = sub_nanos * 10 + u64::from(bytes[index] - b'0');
                seen += 1;
            }
            index += 1;
        }
        if index == start {
            return None;
        }
        sub_nanos *= 10u64.pow(9 - seen);
    }
    let offset_seconds: i64 = match bytes.get(index) {
        Some(b'Z') | Some(b'z') => {
            index += 1;
            0
        }
        Some(b'+') | Some(b'-') => {
            let sign = if bytes[index] == b'-' { -1i64 } else { 1 };
            let offset_hour = digits(index + 1, 2)?;
            if bytes.get(index + 3) != Some(&b':') {
                return None;
            }
            let offset_minute = digits(index + 4, 2)?;
            if offset_hour > 23 || offset_minute > 59 {
                return None;
            }
            index += 6;
            sign * (offset_hour * 3600 + offset_minute * 60)
        }
        _ => return None,
    };
    if index != bytes.len() {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset_seconds;
    let nanos = i128::from(seconds) * 1_000_000_000 + i128::from(sub_nanos);
    i64::try_from(nanos.div_euclid(1_000_000)).ok()
}

/// Howard Hinnant's civil date helpers — the same proleptic-Gregorian
/// pair `uploads.rs` carries for its RFC3339 timestamps.
fn days_in_month(month: i64, year: i64) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_prime = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// `retainWithinLimits` — the newest `maxItems` survive, then the oldest
/// evict until `maxBytes`; a lone entry over the byte bound is an error
/// like the oracle's (unreachable post-`NormalizeEntry`).
fn retain_within_limits(entries: Vec<ActivityEntry>) -> io::Result<Vec<(usize, ActivityEntry)>> {
    let skip = entries.len().saturating_sub(MAX_ITEMS);
    let mut retained = Vec::with_capacity(entries.len() - skip);
    for entry in entries.into_iter().skip(skip) {
        let size = encode_line(&entry)?.len();
        retained.push((size, entry));
    }
    let mut total: usize = retained.iter().map(|(size, _)| *size).sum();
    while total > MAX_BYTES && retained.len() > 1 {
        total -= retained.remove(0).0;
    }
    if total > MAX_BYTES {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            format!("activity entry exceeds {MAX_BYTES} byte journal limit"),
        ));
    }
    Ok(retained)
}

/// `{unixnano}-{sequence}-{paneID}` — lift `entryIDSequence` past every
/// loaded sequence so a reopened journal never reissues an id.
fn bump_sequence(id: &str) {
    let mut parts = id.split('-');
    if parts
        .next()
        .and_then(|nanos| nanos.parse::<u64>().ok())
        .is_none()
    {
        return;
    }
    if let Some(seq) = parts.next().and_then(|seq| seq.parse::<u64>().ok()) {
        ENTRY_ID_SEQUENCE.fetch_max(seq, Ordering::Relaxed);
    }
}

/// `bufio.ScanLines` — `\n` delimits; a trailing `\r` is stripped.
fn strip_cr(raw: &[u8]) -> &[u8] {
    match raw.last() {
        Some(b'\r') => &raw[..raw.len() - 1],
        _ => raw,
    }
}

/// `Append` — write first, mutate the ring after the durable write so a
/// failed append leaves memory and file identically behind. The fast
/// path appends one line; once the bounds would evict, the retained set
/// is rewritten whole (`compactEntriesLocked`).
fn append_durable(storage: &Storage, inner: &mut Inner, entry: ActivityEntry) -> io::Result<()> {
    let data = encode_line(&entry)?;
    if data.len() > MAX_BYTES {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            format!("activity entry exceeds {MAX_BYTES} byte journal limit"),
        ));
    }
    if inner.entries.len() < MAX_ITEMS && inner.bytes + data.len() <= MAX_BYTES {
        append_line(&storage.journal, &data)?;
        inner.entries.push_back((data.len(), entry));
        inner.bytes += data.len();
        return Ok(());
    }
    // `retainWithinLimits` on the candidate — newest wins both bounds;
    // at least the appended entry stays.
    let mut retained = inner.entries.clone();
    retained.push_back((data.len(), entry));
    while retained.len() > MAX_ITEMS {
        retained.pop_front();
    }
    let mut total: usize = retained.iter().map(|(size, _)| *size).sum();
    while total > MAX_BYTES && retained.len() > 1 {
        if let Some((size, _)) = retained.pop_front() {
            total -= size;
        }
    }
    let bytes = compact(storage, retained.iter().map(|(_, entry)| entry))?;
    inner.entries = retained;
    inner.bytes = bytes as usize;
    Ok(())
}

/// `os.OpenFile(path, O_WRONLY|O_CREATE|O_APPEND, 0o600)` — open per
/// append like the oracle, repair the mode, write + fsync + close.
fn append_line(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| io_err("open activity journal", error))?;
    file.set_permissions(Permissions::from_mode(0o600))
        .map_err(|error| io_err("protect activity journal", error))?;
    file.write_all(data)
        .and_then(|()| file.sync_all())
        .map_err(|error| io_err("append activity journal", error))?;
    Ok(())
}

/// `compactEntriesLocked` — rewrite `activity.jsonl` as exactly
/// `entries` through sibling temp + fsync + rename + dir fsync; returns
/// the bytes written (the new `bytes` accounting).
fn compact<'a>(
    storage: &Storage,
    entries: impl Iterator<Item = &'a ActivityEntry>,
) -> io::Result<u64> {
    let mut payload = Vec::new();
    for entry in entries {
        payload.extend_from_slice(&encode_line(entry)?);
    }
    let bytes = payload.len() as u64;
    write_atomic(&storage.dir, ".activity.jsonl", &storage.journal, &payload)?;
    Ok(bytes)
}

/// `loadTombstones` — the doomed-id set, one id per line; absent is an
/// empty set, oversized is corruption. The mode repairs to 0600.
fn load_tombstones(path: &Path) -> io::Result<BTreeSet<String>> {
    let mut file = match File::open(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => return Err(io_err("read activity tombstones", error)),
        Ok(file) => file,
    };
    let size = file
        .metadata()
        .map_err(|error| io_err("read activity tombstones", error))?
        .len();
    if size > MAX_BYTES as u64 {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            format!("activity tombstones are corrupt or oversized: {size} bytes"),
        ));
    }
    file.set_permissions(Permissions::from_mode(0o600))
        .map_err(|error| io_err("protect activity tombstones", error))?;
    let mut contents = Vec::new();
    file.read_to_end(&mut contents)
        .map_err(|error| io_err("scan activity tombstones", error))?;
    let mut tombstones = BTreeSet::new();
    for raw in contents.split(|byte| *byte == b'\n') {
        let line = strip_cr(raw);
        if line.len() > MAX_BYTES {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "scan activity tombstones: line exceeds the journal limit",
            ));
        }
        let id = String::from_utf8_lossy(line);
        let id = id.trim();
        if !id.is_empty() {
            tombstones.insert(id.to_owned());
        }
    }
    Ok(tombstones)
}

/// `writeTombstonesLocked` — the doomed-id set sorted (`sort.Strings`
/// falls out of the BTreeSet) through sibling temp + rename + dir fsync.
/// An empty set removes the file — the recovery gate, not a crash gap.
fn write_tombstones(storage: &Storage, tombstones: &BTreeSet<String>) -> io::Result<()> {
    if tombstones.is_empty() {
        match fs::remove_file(&storage.tombstones) {
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            result => result?,
        }
        return sync_directory(&storage.dir);
    }
    let mut payload = String::new();
    for id in tombstones {
        if !id.trim().is_empty() {
            payload.push_str(id);
            payload.push('\n');
        }
    }
    write_atomic(
        &storage.dir,
        ".activity.tombstones",
        &storage.tombstones,
        payload.as_bytes(),
    )
}

/// `os.CreateTemp(dir, "<prefix>.*.tmp")` — a unique sibling temp file
/// carrying the oracle's 0600 mode.
fn create_temp(dir: &Path, prefix: &str) -> io::Result<(File, PathBuf)> {
    for _ in 0..32 {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let seq = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!("{prefix}.{nanos:x}-{seq:x}.tmp"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => return Ok((file, path)),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        ErrorKind::AlreadyExists,
        "activity temp file name exhausted",
    ))
}

/// `temp.Chmod(0o600)` + write + `Sync` + rename + `syncDirectory` —
/// the oracle's durable-rewrite shape for both journal and tombstones.
fn write_atomic(dir: &Path, prefix: &str, target: &Path, payload: &[u8]) -> io::Result<()> {
    let (mut temp, temp_path) = create_temp(dir, prefix)?;
    let result = (|| {
        temp.set_permissions(Permissions::from_mode(0o600))?;
        temp.write_all(payload)?;
        temp.sync_all()?;
        drop(temp);
        fs::rename(&temp_path, target)?;
        sync_directory(dir)
    })();
    // `defer os.Remove(tempPath)` — cleanup after failure; post-rename
    // the path is already gone.
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

/// `syncDirectory` — fsync the containing dir so a rename/removal is
/// durable; EINVAL/ENOTSUP/ENOSYS mean the filesystem cannot fsync a
/// directory, which the oracle treats as success.
fn sync_directory(dir: &Path) -> io::Result<()> {
    let dir = File::open(dir)?;
    match dir.sync_all() {
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::InvalidInput | ErrorKind::Unsupported
            ) =>
        {
            Ok(())
        }
        result => result,
    }
}

/// `os.Chmod` — permission repair; the crate targets unix only (the
/// `uploads` port already relies on `std::os::unix`).
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    fs::set_permissions(path, Permissions::from_mode(mode))
}

/// `fmt.Errorf("context: %w", err)` — keep the `io::ErrorKind` so
/// callers can still match `NotFound` underneath the context.
fn io_err(context: &str, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{context}: {error}"))
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
/// (`completed`) + receipt. A durable-clear failure answers `failed`
/// with the oracle's "Activity history could not be cleared" and skips
/// the empty-history republish; the `activityW == nil` "storage is
/// unavailable" branch stays unreachable — the factory always builds a
/// journal (in-memory is the fallback).
pub(crate) async fn clear_activities(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    if ctx.activities.try_clear().is_err() {
        return Outcome::failed("", "Activity history could not be cleared").frames(
            request_id,
            "clear_activities",
            action_id,
        );
    }
    Outcome::completed("", None).frames(request_id, "clear_activities", action_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::Acks;
    use crate::topology::Topology;
    use crate::TopologyActor;
    use std::os::unix::fs::MetadataExt;
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

    // ── durability ───────────────────────────────────────────────────

    /// `writeJournalFixture` — serialize entries as `encodeEntry` lines.
    fn write_journal(dir: &Path, entries: &[ActivityEntry]) {
        let mut data = Vec::new();
        for entry in entries {
            data.extend_from_slice(&lerdr_core::json::to_vec(entry).unwrap());
            data.push(b'\n');
        }
        fs::write(dir.join(JOURNAL_FILENAME), data).unwrap();
    }

    #[test]
    fn open_creates_private_dir_and_defers_the_journal_file() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache");
        let journal = Journal::open(&cache).unwrap();
        // `os.MkdirAll(cacheDir, 0o700)` + `os.Chmod` — created private.
        assert_eq!(cache.metadata().unwrap().mode() & 0o777, 0o700);
        assert!(journal.recent(0).is_empty());
        // No journal file exists until the first append writes it.
        assert!(!cache.join(JOURNAL_FILENAME).exists());
    }

    #[test]
    fn durable_round_trip_preserves_ids_order_and_timestamps() {
        let dir = tempfile::tempdir().unwrap();
        let recorded = {
            let journal = Journal::open(dir.path()).unwrap();
            vec![
                journal.record(entry("one").with_attribution(
                    "claude",
                    "proj",
                    "relay-host",
                    "sess-1",
                )),
                journal.record(entry("two")),
                journal.record(entry("three")),
            ]
        };
        let journal = Journal::open(dir.path()).unwrap();
        // The file is authoritative — every field round-trips.
        let recent = journal.recent(0);
        assert_eq!(recent, recorded);
        assert_eq!(recent[0].agent, "claude");
        assert_eq!(recent[0].project, "proj");
        assert_eq!(recent[0].host, "relay-host");
        assert_eq!(recent[0].session, "sess-1");
    }

    #[test]
    fn each_record_appends_one_private_line() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(dir.path()).unwrap();
        journal.record(entry("a"));
        journal.record(entry("b"));
        let path = dir.path().join(JOURNAL_FILENAME);
        // `OpenFile` 0600 + the `Chmod` repair.
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        let raw = fs::read_to_string(&path).unwrap();
        assert_eq!(raw.lines().count(), 2);
        assert!(raw.lines().all(|line| line.contains("\"summary\"")));
    }

    #[test]
    fn reopened_journal_appends_without_losing_history() {
        let dir = tempfile::tempdir().unwrap();
        let first = {
            let journal = Journal::open(dir.path()).unwrap();
            journal.record(entry("first"))
        };
        let journal = Journal::open(dir.path()).unwrap();
        let second = journal.record(entry("second"));
        assert_eq!(
            journal
                .recent(0)
                .iter()
                .map(|e| e.summary.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert_eq!(
            fs::read_to_string(dir.path().join(JOURNAL_FILENAME))
                .unwrap()
                .lines()
                .count(),
            2
        );
        assert_ne!(first.id, second.id);
    }

    #[test]
    fn tombstones_filter_recovered_entries_and_get_cleaned() {
        // `TestLegacyTombstoneRecoveryFiltersStaleEntry` — a clear/discard
        // died after writing tombstones but before rewriting the journal.
        let dir = tempfile::tempdir().unwrap();
        let (stale, current) = {
            let journal = Journal::open(dir.path()).unwrap();
            (
                journal.record(entry("stale")),
                journal.record(entry("current")),
            )
        };
        fs::write(
            dir.path().join(TOMBSTONE_FILENAME),
            format!("{}\n", stale.id),
        )
        .unwrap();
        let journal = Journal::open(dir.path()).unwrap();
        let recent = journal.recent(0);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].id, current.id);
        // Recovered tombstones are removed and the journal compacted
        // without the doomed line.
        assert!(!dir.path().join(TOMBSTONE_FILENAME).exists());
        let raw = fs::read_to_string(dir.path().join(JOURNAL_FILENAME)).unwrap();
        assert_eq!(raw.lines().count(), 1);
        assert!(raw.contains(&current.id));
        assert!(!raw.contains(&stale.id));
    }

    #[test]
    fn crash_mid_clear_recovers_as_empty() {
        // `TestClearCrashWindowRecoversAsEmpty` — tombstones name every
        // drained id; the stale journal is rewritten empty.
        let dir = tempfile::tempdir().unwrap();
        let ids = {
            let journal = Journal::open(dir.path()).unwrap();
            vec![
                journal.record(entry("first")).id,
                journal.record(entry("second")).id,
            ]
        };
        fs::write(
            dir.path().join(TOMBSTONE_FILENAME),
            format!("{}\n", ids.join("\n")),
        )
        .unwrap();
        let journal = Journal::open(dir.path()).unwrap();
        assert!(journal.recent(0).is_empty());
        assert_eq!(
            fs::read(dir.path().join(JOURNAL_FILENAME)).unwrap().len(),
            0
        );
        assert!(!dir.path().join(TOMBSTONE_FILENAME).exists());
    }

    #[test]
    fn orphaned_tombstones_without_a_journal_are_cleaned() {
        // `load`'s ErrNotExist branch — the journal file is gone but a
        // tombstone set survived; open removes it and starts empty.
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(TOMBSTONE_FILENAME), "dead-id\n").unwrap();
        let journal = Journal::open(dir.path()).unwrap();
        assert!(journal.recent(0).is_empty());
        assert!(!dir.path().join(TOMBSTONE_FILENAME).exists());
    }

    #[test]
    fn open_compacts_a_journal_past_max_bytes() {
        // `info.Size() > maxBytes` → `needsCompact` on open; the retained
        // suffix is byte-bounded like `retainWithinLimits`.
        let dir = tempfile::tempdir().unwrap();
        let fill: Vec<ActivityEntry> = {
            let journal = Journal::default();
            (0..40)
                .map(|i| {
                    journal.record(
                        entry(&format!("e{i}")).with_extract(
                            char::from(b'a' + (i % 26) as u8)
                                .to_string()
                                .repeat(100_000),
                        ),
                    )
                })
                .collect()
        };
        write_journal(dir.path(), &fill);
        let journal = Journal::open(dir.path()).unwrap();
        let retained = journal.recent(0);
        assert!(
            !retained.is_empty() && retained.len() < 40,
            "a nonempty byte-bounded suffix, got {}",
            retained.len()
        );
        assert_eq!(retained.last().unwrap().summary, "e39");
        let size = fs::metadata(dir.path().join(JOURNAL_FILENAME))
            .unwrap()
            .len();
        assert!(size <= MAX_BYTES as u64, "compacted to {size} bytes");
    }

    #[test]
    fn open_rejects_an_oversized_journal() {
        // `info.Size() > maxBytes*4` — the corrupt-or-oversized hard error.
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(JOURNAL_FILENAME),
            vec![b'x'; MAX_BYTES * 4 + 1],
        )
        .unwrap();
        assert!(Journal::open(dir.path()).is_err());
    }

    #[test]
    fn corrupt_lines_are_skipped_and_compacted_away() {
        // `TestCorruptJournalSkipsBadLines` — unparseable lines drop from
        // the ring and, since the file drifted, from the rewrite too.
        let dir = tempfile::tempdir().unwrap();
        let good = Journal::default().record(entry("good"));
        let mut data = b"{not-json}\n".to_vec();
        data.extend_from_slice(&lerdr_core::json::to_vec(&good).unwrap());
        data.push(b'\n');
        fs::write(dir.path().join(JOURNAL_FILENAME), data).unwrap();
        let journal = Journal::open(dir.path()).unwrap();
        let recent = journal.recent(0);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].id, good.id);
        let raw = fs::read_to_string(dir.path().join(JOURNAL_FILENAME)).unwrap();
        assert_eq!(raw.lines().count(), 1);
    }

    #[test]
    fn rfc3339_timestamp_strings_decode_like_milli_timestamp() {
        // `MilliTimestamp.UnmarshalJSON`'s string branch — a legacy or
        // hand-written line keeps its entry with the parsed millis.
        let dir = tempfile::tempdir().unwrap();
        let mut data =
            br#"{"id":"1700000000000-7-p","timestamp":"2025-01-02T03:04:05Z","kind":"k","status":"s","summary":"legacy"}"#.to_vec();
        data.push(b'\n');
        fs::write(dir.path().join(JOURNAL_FILENAME), data).unwrap();
        let journal = Journal::open(dir.path()).unwrap();
        let recent = journal.recent(0);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].timestamp, 1_735_787_045_000);
        assert_eq!(recent[0].summary, "legacy");
        // The compact rewrite stores the canonical integer form.
        let raw = fs::read_to_string(dir.path().join(JOURNAL_FILENAME)).unwrap();
        assert!(raw.contains("\"timestamp\":1735787045000"));
    }

    #[test]
    fn clear_is_durable() {
        // `TestClear` — drained stays drained after reopen; the
        // tombstone file is removed and the journal is empty on disk.
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(dir.path()).unwrap();
        journal.record(entry("gone"));
        journal.record(entry("too"));
        journal.clear();
        assert!(journal.recent(0).is_empty());
        assert_eq!(
            fs::read(dir.path().join(JOURNAL_FILENAME)).unwrap().len(),
            0
        );
        assert!(!dir.path().join(TOMBSTONE_FILENAME).exists());
        let reopened = Journal::open(dir.path()).unwrap();
        assert!(reopened.recent(0).is_empty());
    }

    #[test]
    fn record_eviction_rewrites_the_file() {
        // Past `maxItems` the append path compacts instead of appending.
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(dir.path()).unwrap();
        for i in 0..(MAX_ITEMS + 10) {
            journal.record(entry(&format!("e{i}")));
        }
        let raw = fs::read_to_string(dir.path().join(JOURNAL_FILENAME)).unwrap();
        assert_eq!(raw.lines().count(), MAX_ITEMS);
        assert_eq!(journal.recent(0)[0].summary, "e10");
        let reopened = Journal::open(dir.path()).unwrap();
        assert_eq!(reopened.recent(0).len(), MAX_ITEMS);
    }

    #[test]
    fn id_sequence_climbs_past_loaded_ids() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = {
            let journal = Journal::open(dir.path()).unwrap();
            journal.record(entry("a")).id
        };
        let journal = Journal::open(dir.path()).unwrap();
        let fresh = journal.record(entry("b")).id;
        // `{unixnano}-{sequence}-{paneID}` — the sequence never reissues.
        let seq = |id: &str| {
            id.split('-')
                .nth(1)
                .and_then(|s| s.parse::<u64>().ok())
                .expect("sequence component")
        };
        assert!(seq(&fresh) > seq(&loaded));
        assert_ne!(fresh, loaded);
    }

    #[test]
    fn clear_failure_keeps_entries_and_skips_broadcast() {
        // A clear that cannot write tombstones fails like the oracle —
        // nothing drains, no empty-history republish.
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(dir.path()).unwrap();
        let mut rx = journal.subscribe();
        journal.record(entry("a"));
        assert!(matches!(rx.try_recv(), Ok(JournalEvent::Recorded(_))));
        // `loadTombstones` on a directory is a hard read error.
        fs::create_dir(dir.path().join(TOMBSTONE_FILENAME)).unwrap();
        assert!(journal.try_clear().is_err());
        assert_eq!(journal.recent(0).len(), 1);
        assert!(rx.try_recv().is_err(), "no Cleared event on failure");
    }

    #[test]
    fn append_failure_drops_the_entry_and_skips_broadcast() {
        // `recordActivity`'s commit-failure path — warn, no ring slot,
        // no `activity` push; memory and file stay identical.
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(dir.path()).unwrap();
        let mut rx = journal.subscribe();
        // `O_APPEND` on a directory fails the append.
        fs::create_dir(dir.path().join(JOURNAL_FILENAME)).unwrap();
        let stamped = journal.record(entry("dropped"));
        assert!(!stamped.id.is_empty(), "the stamped entry still returns");
        assert!(journal.recent(0).is_empty());
        assert!(rx.try_recv().is_err(), "no Recorded event on failure");
    }

    #[tokio::test]
    async fn clear_activities_reports_durable_failure() {
        // `HandleClearActivities`'s error branch — `d.fail(requestID,
        // "clear_activities", "", "Activity history could not be cleared")`.
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(dir.path()).unwrap();
        journal.record(entry("a"));
        fs::create_dir(dir.path().join(TOMBSTONE_FILENAME)).unwrap();
        let observer = journal.clone();
        let frames = clear_activities(
            test_context(journal),
            "r1",
            "a1",
            &inbound(serde_json::json!({"type": "clear_activities"})),
        )
        .await;
        let Some(Outbound::CommandResult(result)) = frames.first() else {
            panic!("expected command_result, got {frames:?}")
        };
        assert_eq!(result.ok, Some(false));
        assert_eq!(result.phase.as_deref(), Some("failed"));
        assert_eq!(
            result.error.as_deref(),
            Some("Activity history could not be cleared")
        );
        assert_eq!(observer.recent(0).len(), 1, "failed clear drains nothing");
    }
}
