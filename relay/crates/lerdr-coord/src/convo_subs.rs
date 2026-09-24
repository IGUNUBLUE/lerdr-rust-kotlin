//! `convo_sub` — Phase-5 Track-B per-pane conversation subscriptions
//! (docs/13-phase5-wire-spec.md §2.3).
//!
//! One task per (client, pane): `subscribe_conversation` resolves its
//! `target` to a live pane — `pane_id` (top-level or `target.pane_id`)
//! directly, `target.agent_session_id` through the topology's session
//! index — spawns the feed, and immediately pushes the current
//! conversation as a `conversation_update` with `reset:true`. Later
//! polls push `reset:false` frames carrying only the entries appended
//! since the last emission — the append tail anchors on the previous
//! page's last entry id, so a transcript that moved underneath (source
//! rotation, a new Claude continuation segment, a mid-read tuple swap)
//! falls back to a full `reset:true` page instead of feeding the client
//! a torn prefix.
//!
//! The loop mirrors `pane_watch`'s two-tier cadence, cheaper on both
//! tiers because the transcript lives on local disk:
//!
//! - **Periodic tick** ([`POLL_INTERVAL`]) — stat the emitted source
//!   path ([`SourceStamp`]: dev/ino/len/mtime); an identical stamp means
//!   the page the client holds is still current. A moved stamp re-reads
//!   page 1 through the shared [`ConversationBrowser`] on
//!   `spawn_blocking` — bounded file I/O and the sqlite subprocesses
//!   must not stall the executor.
//! - **Invalidation fast path** — `pane.*` broadcasts naming the pane
//!   (or global wakes) poll early once past [`READ_FRESHNESS`]; inside
//!   the window the tick covers the change.
//! - **Fences** — the read captures `(generation, conversation_tuple)`
//!   before and re-checks against the committed view after: a pane
//!   replacement, agent swap, or topology move mid-read discards the
//!   page rather than emitting a `conversation_update` the pane no
//!   longer owns. The pane epoch rides every frame — a generation bump
//!   forces `reset:true` so the client's staleness ref follows the live
//!   pane.
//! - **Tombstone** — a pane that leaves the snapshot emits one
//!   `reset:true` frame with an empty `messages` (the feed's terminal
//!   state); the task keeps polling so a pane that returns re-reads and
//!   pushes a fresh reset.
//! - **Teardown** — `unsubscribe_conversation` aborts the task;
//!   `HerdRouter::drop` drains the set. `conversation_update` is a
//!   replaceable send-buffer type, so a lagging client coalesces the
//!   queue instead of drowning.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use lerdr_core::json::{MaybeNull, RawJson};
use lerdr_core::protocol::{ConversationUpdateMessage, Inbound, Outbound, TargetRef};
use lerdr_herdr::AgentInfo;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn, Instrument};

use crate::actions::conversation::{conversation_tuple, ConversationTuple};
use crate::actions::pane_of;
use crate::actor::{Invalidation, TopologyHandle};
use crate::conversation::{BrowseRequest, BrowseScope, BrowseState, ConversationBrowser, Entry};
use crate::topology::Topology;
use crate::watches::FrameSink;

/// Tick period — the transcripts are local files, so this sits well
/// under the pane-watch floor and still amortizes: a settled source
/// costs one `stat` per tick.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Invalidation fast-path freshness — a `pane.*` event inside this
/// window is covered by the regular tick; past it the feed polls early.
const READ_FRESHNESS: Duration = Duration::from_millis(500);

/// One `subscribe_conversation` distilled for the feed task — the pane
/// the router resolved; the frame `target` is rebuilt per emission from
/// the live pane row so epoch/session moves stay accurate.
pub(crate) struct ConvoSubSpec {
    pub(crate) pane_id: String,
}

/// The ambient handles a feed task needs — everything except `sink` is
/// session-shared.
#[derive(Clone)]
pub(crate) struct ConvoSubDeps {
    pub(crate) handle: TopologyHandle,
    /// The relay-shared conversation browser — one `Reader` behind it
    /// keeps transcript locations and source identity consistent with
    /// the resolver and `get_conversation_history`.
    pub(crate) browser: Arc<ConversationBrowser>,
    /// This client's push endpoint.
    pub(crate) sink: Arc<dyn FrameSink>,
    /// Session-scoped kill switch.
    pub(crate) cancel: CancellationToken,
    /// Test seam — fires inside the read window so a test can land a
    /// topology mutation mid-read. `None` in production.
    pub(crate) on_read: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// Per-client conversation subscription set — the `convo_sub` analog of
/// [`crate::watches::WatchSet`], keyed by resolved pane id (the
/// `agent_session_id` address form resolves to the same key).
#[derive(Default)]
pub(crate) struct ConvoSubSet {
    entries: HashMap<String, JoinHandle<()>>,
}

impl ConvoSubSet {
    /// `true` when a feed is already live for `pane_id`.
    #[cfg(test)]
    pub(crate) fn subscribed(&self, pane_id: &str) -> bool {
        self.entries.contains_key(pane_id)
    }

    /// `subscribe_conversation` — spawn the feed. Re-subscribing a pane
    /// replaces the live feed (`WatchSet::start` parity — clients
    /// re-issue subscribe to retarget rather than unsubscribing first).
    pub(crate) fn start(&mut self, spec: ConvoSubSpec, deps: ConvoSubDeps) {
        if let Some(previous) = self.entries.remove(&spec.pane_id) {
            previous.abort();
        }
        let pane = spec.pane_id.clone();
        let invalidations = deps.handle.invalidations.subscribe();
        let task = tokio::spawn(
            feed_loop(spec, deps, invalidations)
                .instrument(tracing::info_span!("convo_sub", pane = %pane)),
        );
        self.entries.insert(pane, task);
    }

    /// `unsubscribe_conversation` — stop and drop the feed; an unknown
    /// pane is a no-op like `unwatch_pane`.
    pub(crate) fn stop(&mut self, pane_id: &str) -> bool {
        if let Some(task) = self.entries.remove(pane_id) {
            task.abort();
            true
        } else {
            false
        }
    }

    /// Session teardown — abort every feed.
    pub(crate) fn stop_all(&mut self) {
        for (_, task) in self.entries.drain() {
            task.abort();
        }
    }
}

/// `subscribe_conversation` target resolution — `pane_id` (top-level or
/// `target.pane_id`) names the pane and must be live; otherwise
/// `target.agent_session_id` resolves through the topology's session
/// index. `None` when the address resolves to no live pane — the router
/// answers the oracle-family "Agent is unavailable" failure.
pub(crate) fn subscribe_pane(topology: &Topology, message: &Inbound) -> Option<String> {
    let pane_id = pane_of(message);
    if !pane_id.is_empty() {
        return topology.pane_of(pane_id).map(|info| info.pane_id.clone());
    }
    session_pane(topology, message)
}

/// `unsubscribe_conversation` — cleanup, not a read: a pane address
/// stops that feed verbatim (the pane may already be gone — the slot is
/// still the client's to clear); a session address resolves best-effort
/// through the live index.
pub(crate) fn unsubscribe_pane(topology: &Topology, message: &Inbound) -> Option<String> {
    let pane_id = pane_of(message);
    if !pane_id.is_empty() {
        return Some(pane_id.to_owned());
    }
    session_pane(topology, message)
}

/// `target.agent_session_id` → live pane — the trimmed-id match
/// `focus_agent` uses (`Topology::pane_for_session`).
fn session_pane(topology: &Topology, message: &Inbound) -> Option<String> {
    let target = message.target.as_ref()?;
    if target.agent_session_id.is_empty() {
        return None;
    }
    topology
        .pane_for_session(&target.agent_session_id)
        .map(|info| info.pane_id.clone())
}

/// stat signature of the transcript source — `metadata()` answered.
/// `MISSING` never equals a captured `exists` stamp, so a vanished file
/// keeps re-reading (the read surfaces the new availability itself).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceStamp {
    exists: bool,
    dev: u64,
    ino: u64,
    len: u64,
    mtime_ns: i128,
}

impl SourceStamp {
    const MISSING: Self = Self {
        exists: false,
        dev: 0,
        ino: 0,
        len: 0,
        mtime_ns: 0,
    };

    fn of(path: &str) -> Self {
        use std::os::unix::fs::MetadataExt;
        let Ok(info) = std::fs::metadata(path) else {
            return Self::MISSING;
        };
        Self {
            exists: true,
            dev: info.dev(),
            ino: info.ino(),
            len: info.len(),
            mtime_ns: info.mtime() as i128 * 1_000_000_000 + info.mtime_nsec() as i128,
        }
    }
}

/// What the last emission looked like — enough to decide the next
/// frame's shape without retaining the entries.
#[derive(Debug, Default, PartialEq)]
struct Sig {
    /// `false` marks the tombstone signature (pane left the snapshot) —
    /// a resolved pane coming back always resets against it.
    resolved: bool,
    /// `page.available` — transcript presence flips are resets.
    available: bool,
    reason_code: String,
    /// The browser's source identity (`native_source_revision` — path +
    /// file identity): a source swap means the ids below describe a
    /// different history even when the visible tail looks alike.
    source_revision: String,
    /// The emitted page's entry ids, oldest→newest — the append anchor
    /// is `ids.last()`; suffix-overlap against the previous page decides
    /// whether the diff is a pure append.
    ids: Vec<String>,
    total: Option<i64>,
}

impl Sig {
    fn of_page(page: &crate::conversation::BrowsePage) -> Self {
        Sig {
            resolved: true,
            available: page.available,
            reason_code: page.reason_code.clone(),
            source_revision: page.source_revision.clone(),
            ids: page.entries.iter().map(|entry| entry.id.clone()).collect(),
            total: page.total,
        }
    }
}

/// One confirmed emission's bookkeeping.
struct LastRead {
    /// Pane epoch the emission rode — a bump forces the next frame
    /// `reset:true` (the client re-keys its staleness ref).
    generation: i64,
    /// The conversation tuple the read resolved under — compared on the
    /// probe-skip path and inside the mid-read fence.
    tuple: Option<ConversationTuple>,
    /// stat signature of `probe_path` captured at read time.
    stamp: SourceStamp,
    /// The file `stamp` describes — the page's reported probe path
    /// (Claude: the chain tip; flat readers: the located source). Empty
    /// disables the skip — no statable source means every tick reads.
    probe_path: String,
    /// The emitted page's signature.
    sig: Sig,
}

/// The feed loop — exits on session cancel, a closed invalidation feed,
/// or router abort (`unsubscribe_conversation`/teardown). §2.3 has no
/// resync/ack surface: the client re-subscribes to restart.
async fn feed_loop(
    spec: ConvoSubSpec,
    deps: ConvoSubDeps,
    mut invalidations: broadcast::Receiver<Invalidation>,
) {
    let pane_id = spec.pane_id.as_str();
    let mut last: Option<LastRead> = None;

    // §2.3 "immediately emit the current conversation" — the first
    // `conversation_update` rides `reset:true` whatever it carries.
    poll(pane_id, &deps, &mut last).await;

    let mut next_tick = tokio::time::Instant::now() + POLL_INTERVAL;
    let mut next_read = next_tick;
    loop {
        tokio::select! {
            biased;
            _ = deps.cancel.cancelled() => break,
            _ = tokio::time::sleep_until(next_tick) => {
                next_tick = tokio::time::Instant::now() + POLL_INTERVAL;
                poll(pane_id, &deps, &mut last).await;
                next_read = tokio::time::Instant::now() + READ_FRESHNESS;
            }
            inv = invalidations.recv() => {
                let triggered = match inv {
                    Ok(inv) => {
                        inv.pane_id.as_deref() == Some(pane_id) || inv.pane_id.is_none()
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        // Lag is correctness-safe — the next poll re-reads.
                        debug!(skipped = n, "invalidation lag — resyncing");
                        true
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                // Fast path only — inside the freshness window the tick
                // covers it; the probe makes a redundant poll a `stat`.
                if triggered && tokio::time::Instant::now() >= next_read {
                    poll(pane_id, &deps, &mut last).await;
                    next_read = tokio::time::Instant::now() + READ_FRESHNESS;
                }
            }
        }
    }
    debug!(pane_id, "conversation feed stopped");
}

/// One pass: resolve the pane → probe → read → fence → emit.
async fn poll(pane_id: &str, deps: &ConvoSubDeps, last: &mut Option<LastRead>) {
    let snapshot = {
        let topology = deps.handle.topology.borrow();
        topology
            .pane_of(pane_id)
            .cloned()
            .map(|info| (topology.generation_of(pane_id), info))
    };
    let Some((generation, info)) = snapshot else {
        // The pane left the snapshot — one tombstone marks the feed's
        // terminal state; silence after that until (if) it returns.
        emit_tombstone(pane_id, deps, last);
        return;
    };
    let tuple = conversation_tuple(&info);
    // Probe tier: same pane epoch + same tuple + unmoved source → the
    // page the client holds is still current — no read needed.
    if let Some(prev) = last.as_ref() {
        let settled = prev.sig.resolved
            && prev.generation == generation
            && prev.tuple.as_ref() == Some(&tuple)
            && !prev.probe_path.is_empty()
            && SourceStamp::of(&prev.probe_path) == prev.stamp;
        if settled {
            return;
        }
    }
    let request = browse_request(pane_id, &info, generation);
    let browser = deps.browser.clone();
    let page = match tokio::task::spawn_blocking(move || browser.read_page_sync(request)).await {
        Ok(page) => page,
        Err(_) => return,
    };
    if let Some(on_read) = &deps.on_read {
        on_read();
    }
    // Post-read fence — the committed view must still be the view the
    // read captured: pane replaced (generation), agent swapped (tuple),
    // or pane gone. Emitting here would feed the client a transcript
    // the pane no longer owns.
    {
        let topology = deps.handle.topology.borrow();
        let stale = match topology.pane_of(pane_id) {
            Some(current) => {
                topology.generation_of(pane_id) != generation
                    || conversation_tuple(current) != tuple
            }
            None => true,
        };
        if stale {
            debug!(pane_id, "mid-read topology change — discarding page");
            return;
        }
    }
    if page.state == BrowseState::Failed {
        return;
    }
    let stamp = SourceStamp::of(&page.probe_path);
    let sig = Sig::of_page(&page);
    let Some((messages, reset)) = decide(last.as_ref(), generation, &sig, &page.entries) else {
        // Page unchanged since the last emission — refresh the source
        // stamp so later probes compare against what the read saw.
        if let Some(prev) = last.as_mut() {
            prev.stamp = stamp;
            prev.probe_path = page.probe_path.clone();
        }
        return;
    };
    let Ok(raw) = serde_json::value::to_raw_value(&messages) else {
        return;
    };
    let frame = Outbound::ConversationUpdate(ConversationUpdateMessage {
        generation: Some(u64::try_from(generation).unwrap_or(0)),
        messages: Some(MaybeNull::Value(RawJson(raw))),
        reset: Some(reset),
        target: Some(MaybeNull::Value(frame_target(&info, generation))),
        r#type: "conversation_update".to_owned(),
    });
    if !deps.sink.try_send(&frame) {
        warn!(
            pane_id,
            "conversation_update refused — client queue gone or full"
        );
        return;
    }
    *last = Some(LastRead {
        generation,
        tuple: Some(tuple),
        stamp,
        probe_path: page.probe_path.clone(),
        sig,
    });
}

/// Pane left the snapshot — the feed's terminal state is one empty
/// `reset:true` frame; quiet after that until the pane returns.
fn emit_tombstone(pane_id: &str, deps: &ConvoSubDeps, last: &mut Option<LastRead>) {
    let generation = deps.handle.topology.borrow().generation_of(pane_id);
    let sig = Sig::default();
    if last.as_ref().is_some_and(|prev| prev.sig == sig) {
        return;
    }
    let Ok(raw) = serde_json::value::to_raw_value(&Vec::<Entry>::new()) else {
        return;
    };
    let frame = Outbound::ConversationUpdate(ConversationUpdateMessage {
        generation: Some(u64::try_from(generation).unwrap_or(0)),
        messages: Some(MaybeNull::Value(RawJson(raw))),
        reset: Some(true),
        target: Some(MaybeNull::Value(TargetRef {
            server_session_id: "primary".to_owned(),
            pane_id: pane_id.to_owned(),
            generation,
            ..TargetRef::default()
        })),
        r#type: "conversation_update".to_owned(),
    });
    if !deps.sink.try_send(&frame) {
        warn!(
            pane_id,
            "conversation tombstone refused — client queue gone or full"
        );
        return;
    }
    *last = Some(LastRead {
        generation,
        tuple: None,
        stamp: SourceStamp::MISSING,
        probe_path: String::new(),
        sig,
    });
}

/// Emit decision for a freshly read page: `Some((entries, reset))`
/// ships a frame; `None` means the page matches the last emission.
fn decide(
    last: Option<&LastRead>,
    generation: i64,
    sig: &Sig,
    entries: &[Entry],
) -> Option<(Vec<Entry>, bool)> {
    let Some(prev) = last else {
        // Initial frame — §2.3 requires the current conversation
        // immediately, whatever it contains.
        return Some((entries.to_vec(), true));
    };
    // A new pane epoch or a rebirth after the tombstone is always a
    // rebuild — even an identical-looking transcript belongs to a new
    // session.
    if prev.generation != generation || !prev.sig.resolved {
        return Some((entries.to_vec(), true));
    }
    if *sig == prev.sig {
        return None;
    }
    // Source rotation/replacement, availability flips, and reason-code
    // moves are rebuilds; only an unmoved source can append.
    if sig.source_revision != prev.sig.source_revision
        || sig.available != prev.sig.available
        || sig.reason_code != prev.sig.reason_code
    {
        return Some((entries.to_vec(), true));
    }
    match append_delta(&prev.sig, sig, entries) {
        Some(appended) => Some((appended, false)),
        None => Some((entries.to_vec(), true)),
    }
}

/// Pure-append detection — the new page's entries up to and including
/// the previous tail id must equal a suffix of the previous page's ids
/// (the overlap region unchanged); the tail past it ships `reset:false`.
/// Anything else — anchor missing, prefix drift — is a rebuild.
fn append_delta(prev: &Sig, sig: &Sig, entries: &[Entry]) -> Option<Vec<Entry>> {
    let anchor = prev.ids.last()?;
    let pos = sig.ids.iter().position(|id| id == anchor)?;
    let overlap = &sig.ids[..=pos];
    if overlap.len() > prev.ids.len() || overlap != &prev.ids[prev.ids.len() - overlap.len()..] {
        return None;
    }
    let appended = entries[pos + 1..].to_vec();
    (!appended.is_empty()).then_some(appended)
}

/// The `target` echo — rebuilt per emission from the live pane row so a
/// replaced session reports its current ids (matching `agent_update`'s
/// broadcast fields).
fn frame_target(info: &AgentInfo, generation: i64) -> TargetRef {
    TargetRef {
        server_session_id: "primary".to_owned(),
        pane_id: info.pane_id.clone(),
        terminal_id: info.terminal_id.clone(),
        generation,
        agent_session_id: info
            .agent_session
            .as_ref()
            .map(|s| s.value.trim().to_owned())
            .unwrap_or_default(),
        workspace_id: info.workspace_id.clone(),
        tab_id: info.tab_id.clone(),
    }
}

/// The page-1 `BrowseRequest` — the same scope `get_conversation_history`
/// builds for the pane (`actions/conversation.rs`), minus cursor/retry.
fn browse_request(pane_id: &str, info: &AgentInfo, generation: i64) -> BrowseRequest {
    BrowseRequest {
        scope: BrowseScope {
            provider: info.agent.clone().unwrap_or_default(),
            cwd: info.cwd.clone().unwrap_or_default(),
            foreground_cwd: info.foreground_cwd.clone().unwrap_or_default(),
            session_id: info
                .agent_session
                .as_ref()
                .map(|s| s.value.clone())
                .unwrap_or_default(),
            pane_id: pane_id.to_owned(),
            server_session_id: String::new(),
            terminal_id: info.terminal_id.clone(),
            generation,
        },
        cursor: None,
        limit: 0,
        retry: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::Reader;
    use crate::watches::test_support::{recording_sink, FakeHerdr};
    use lerdr_herdr::{AgentSessionInfo, AgentSessionRefKind, AgentStatus, SessionSnapshot};
    use std::collections::HashMap as StdMap;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tempfile::TempDir;
    use tokio::sync::{mpsc, watch};

    const PANE: &str = "wE:p1";
    const SESSION: &str = "sess-1";

    /// One Claude transcript under the temp HOME — `cwd` projects to
    /// `-work-repo` (`claude_project_name`), `session_id` names the file.
    fn transcript(home: &TempDir, session_id: &str, records: &[String]) -> PathBuf {
        let dir = home.path().join(".claude/projects/-work-repo");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join(format!("{session_id}.jsonl"));
        let mut body = records.join("\n");
        if !body.is_empty() {
            body.push('\n');
        }
        std::fs::write(&path, body).expect("write transcript");
        path
    }

    fn transcript_path(home: &TempDir, session_id: &str) -> PathBuf {
        home.path()
            .join(format!(".claude/projects/-work-repo/{session_id}.jsonl"))
    }

    fn user(text: &str) -> String {
        format!(r#"{{"type":"user","message":{{"content":"{text}"}}}}"#)
    }

    fn assistant(text: &str) -> String {
        format!(r#"{{"type":"assistant","message":{{"content":"{text}"}}}}"#)
    }

    /// A topology whose only pane is `PANE` — Claude, `/work/repo`,
    /// session `session_id`.
    fn topology(session_id: &str) -> Topology {
        let mut topology = Topology::default();
        topology.accept(SessionSnapshot {
            agents: vec![AgentInfo {
                pane_id: PANE.into(),
                terminal_id: "term-1".into(),
                workspace_id: "wE".into(),
                tab_id: "wE:t1".into(),
                agent_status: AgentStatus::Working,
                agent: Some("claude".into()),
                cwd: Some("/work/repo".into()),
                agent_session: Some(AgentSessionInfo {
                    kind: AgentSessionRefKind::Id,
                    value: session_id.into(),
                    ..AgentSessionInfo::default()
                }),
                ..AgentInfo::default()
            }],
            ..SessionSnapshot::default()
        });
        topology
    }

    /// `exportFixtureHome` — temp HOME + XDG-scoped env, shared-reader
    /// browser like production wiring.
    fn browser(home: &TempDir, xdg: &TempDir) -> Arc<ConversationBrowser> {
        let mut env: StdMap<String, String> = StdMap::new();
        env.insert(
            "XDG_DATA_HOME".into(),
            xdg.path().to_string_lossy().into_owned(),
        );
        Arc::new(ConversationBrowser::with_reader(Arc::new(
            Reader::new_with_env(
                home.path().to_path_buf(),
                Box::new(move |key: &str| env.get(key).cloned()),
            ),
        )))
    }

    /// A subscription against `topology(SESSION)` — the pieces a test
    /// mutates live on the struct.
    struct Harness {
        home: TempDir,
        _xdg: TempDir,
        subs: ConvoSubSet,
        deps: ConvoSubDeps,
        topology_tx: watch::Sender<Arc<Topology>>,
        invalidations: broadcast::Sender<Invalidation>,
        rx: mpsc::UnboundedReceiver<Outbound>,
    }

    fn harness() -> Harness {
        let home = TempDir::new().expect("home");
        let xdg = TempDir::new().expect("xdg");
        let herdr = FakeHerdr::serving(vec![]);
        let (sink, rx) = recording_sink();
        let (invalidations, _) = broadcast::channel(32);
        let (handle, topology_tx) = TopologyHandle::for_test(
            herdr.client(),
            Arc::new(topology(SESSION)),
            invalidations.clone(),
        );
        Harness {
            deps: ConvoSubDeps {
                handle,
                browser: browser(&home, &xdg),
                sink,
                cancel: CancellationToken::new(),
                on_read: None,
            },
            home,
            _xdg: xdg,
            subs: ConvoSubSet::default(),
            topology_tx,
            invalidations,
            rx,
        }
    }

    fn spec() -> ConvoSubSpec {
        ConvoSubSpec {
            pane_id: PANE.to_owned(),
        }
    }

    /// Receive the next `conversation_update`, asserting the type.
    async fn recv_update(rx: &mut mpsc::UnboundedReceiver<Outbound>) -> ConversationUpdateMessage {
        match tokio::time::timeout(Duration::from_secs(10), rx.recv()).await {
            Ok(Some(Outbound::ConversationUpdate(update))) => update,
            other => panic!("expected conversation_update, got {other:?}"),
        }
    }

    /// `(entries, reset, generation)` decoded off a frame.
    fn parts(update: &ConversationUpdateMessage) -> (Vec<serde_json::Value>, bool, u64) {
        let entries: Vec<serde_json::Value> = update
            .messages
            .clone()
            .and_then(MaybeNull::into_value)
            .map(|raw| serde_json::from_str(raw.get()).expect("entries decode"))
            .unwrap_or_default();
        (
            entries,
            update.reset.unwrap_or(false),
            update.generation.unwrap_or(u64::MAX),
        )
    }

    /// §2.3: subscribing immediately emits the current conversation as a
    /// `reset:true` frame with the resolved target echo.
    #[tokio::test(start_paused = true)]
    async fn initial_frame_is_full_reset() {
        let mut h = harness();
        transcript(&h.home, SESSION, &[user("hi"), assistant("reply")]);
        h.subs.start(spec(), h.deps.clone());

        let update = recv_update(&mut h.rx).await;
        let (entries, reset, generation) = parts(&update);
        assert!(reset, "initial frame is reset:true");
        assert_eq!(generation, 0);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["role"], "user");
        assert_eq!(entries[0]["text"], "hi");
        assert_eq!(entries[1]["text"], "reply");
        let target = update
            .target
            .clone()
            .and_then(MaybeNull::into_value)
            .expect("target echo");
        assert_eq!(target.pane_id, PANE);
        assert_eq!(target.terminal_id, "term-1");
        assert_eq!(target.agent_session_id, SESSION);
        assert_eq!(target.workspace_id, "wE");
        assert_eq!(target.tab_id, "wE:t1");

        // A quiet transcript produces no further frames.
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(h.rx.try_recv().is_err(), "no duplicate emissions");
    }

    /// An appended record rides `reset:false` — only the new tail ships.
    #[tokio::test(start_paused = true)]
    async fn append_emits_tail_only() {
        let mut h = harness();
        let path = transcript(&h.home, SESSION, &[user("hi")]);
        h.subs.start(spec(), h.deps.clone());
        let first = recv_update(&mut h.rx).await;
        assert_eq!(parts(&first).0.len(), 1);

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("append");
        writeln!(file, "{}", assistant("second")).expect("write");
        drop(file);

        tokio::time::sleep(Duration::from_millis(1200)).await;
        let update = recv_update(&mut h.rx).await;
        let (entries, reset, _) = parts(&update);
        assert!(!reset, "append is reset:false");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["role"], "assistant");
        assert_eq!(entries[0]["text"], "second");
    }

    /// A replaced transcript (new inode) is a rebuild — `reset:true`
    /// with the full new page, even when the tail happens to look alike.
    #[tokio::test(start_paused = true)]
    async fn rotated_source_emits_reset() {
        let mut h = harness();
        let path = transcript(&h.home, SESSION, &[user("hi"), assistant("reply")]);
        h.subs.start(spec(), h.deps.clone());
        recv_update(&mut h.rx).await;

        std::fs::remove_file(&path).expect("remove");
        transcript(&h.home, SESSION, &[user("brand new")]);

        tokio::time::sleep(Duration::from_millis(1200)).await;
        let update = recv_update(&mut h.rx).await;
        let (entries, reset, _) = parts(&update);
        assert!(reset, "source rotation is a rebuild");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["text"], "brand new");
    }

    /// A pane-generation bump is a new epoch — `reset:true` stamped with
    /// the new generation even when the transcript itself is unchanged.
    #[tokio::test(start_paused = true)]
    async fn generation_bump_emits_reset() {
        let mut h = harness();
        transcript(&h.home, SESSION, &[user("hi")]);
        h.subs.start(spec(), h.deps.clone());
        let first = recv_update(&mut h.rx).await;
        assert_eq!(parts(&first).2, 0);

        let mut replaced = topology(SESSION);
        replaced.bump_generation(PANE);
        h.topology_tx
            .send(Arc::new(replaced))
            .expect("publish topology");

        tokio::time::sleep(Duration::from_millis(1200)).await;
        let update = recv_update(&mut h.rx).await;
        let (_, reset, generation) = parts(&update);
        assert!(reset);
        assert_eq!(generation, 1);
    }

    /// The mid-read fence: a committed topology change landing inside
    /// the read window discards the stale page — the first post-swap
    /// frame is the new session's `reset:true`, never the old page.
    #[tokio::test(start_paused = true)]
    async fn mid_read_tuple_change_suppresses_stale_page() {
        let mut h = harness();
        transcript(&h.home, SESSION, &[user("old session")]);
        transcript(&h.home, "sess-2", &[user("new session")]);
        h.subs.start(spec(), h.deps.clone());
        recv_update(&mut h.rx).await;

        let armed = Arc::new(AtomicBool::new(false));
        let tx = h.topology_tx.clone();
        let flag = armed.clone();
        h.deps.on_read = Some(Arc::new(move || {
            if flag.swap(false, Ordering::SeqCst) {
                let _ = tx.send(Arc::new(topology("sess-2")));
            }
        }));
        // Re-start so the task picks up the hooked deps.
        h.subs.start(spec(), h.deps.clone());
        recv_update(&mut h.rx).await; // fresh initial frame
        armed.store(true, Ordering::SeqCst);

        // Move the transcript so the next tick actually reads — the hook
        // swaps the pane to sess-2 inside that read's window.
        let path = transcript_path(&h.home, SESSION);
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("append");
        writeln!(file, "{}", assistant("stale line")).expect("write");
        drop(file);
        tokio::time::sleep(Duration::from_millis(1200)).await;
        // The tick read sess-1, the hook swapped the pane to sess-2
        // mid-read — the fence must have discarded that page. The next
        // tick re-reads under sess-2 and emits its reset.
        tokio::time::sleep(Duration::from_millis(1200)).await;
        let update = recv_update(&mut h.rx).await;
        let (entries, reset, _) = parts(&update);
        assert!(reset, "session swap is a rebuild");
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0]["text"], "new session",
            "the stale sess-1 read must not have been emitted first"
        );
        assert!(h.rx.try_recv().is_err());
    }

    /// `unsubscribe_conversation` stops pushes — the task is gone, so a
    /// moved transcript produces nothing.
    #[tokio::test(start_paused = true)]
    async fn unsubscribe_stops_pushes() {
        let mut h = harness();
        let path = transcript(&h.home, SESSION, &[user("hi")]);
        h.subs.start(spec(), h.deps.clone());
        recv_update(&mut h.rx).await;

        assert!(h.subs.stop(PANE));
        assert!(!h.subs.subscribed(PANE));

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("append");
        writeln!(file, "{}", assistant("late")).expect("write");
        drop(file);
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(h.rx.try_recv().is_err(), "no pushes after unsubscribe");
        assert!(!h.subs.stop(PANE), "second stop is a no-op");
    }

    /// `stop_all` — the session-teardown drain — kills every feed.
    #[tokio::test(start_paused = true)]
    async fn stop_all_drains_feeds() {
        let mut h = harness();
        let path = transcript(&h.home, SESSION, &[user("hi")]);
        h.subs.start(spec(), h.deps.clone());
        recv_update(&mut h.rx).await;

        h.subs.stop_all();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("append");
        writeln!(file, "{}", assistant("late")).expect("write");
        drop(file);
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(h.rx.try_recv().is_err(), "no pushes after teardown");
    }

    /// Re-subscribing the same pane replaces the feed — a fresh
    /// `reset:true` initial frame, no duplicate task.
    #[tokio::test(start_paused = true)]
    async fn resubscribe_replaces_feed() {
        let mut h = harness();
        transcript(&h.home, SESSION, &[user("hi")]);
        h.subs.start(spec(), h.deps.clone());
        recv_update(&mut h.rx).await;

        h.subs.start(spec(), h.deps.clone());
        let update = recv_update(&mut h.rx).await;
        assert!(parts(&update).1, "the replacement emits a fresh reset");
        assert!(h.subs.subscribed(PANE));
    }

    /// A pane that leaves the snapshot gets one tombstone
    /// (`reset:true`, empty `messages`); returning re-reads and resets.
    #[tokio::test(start_paused = true)]
    async fn gone_pane_tombstones_then_rebirth_resets() {
        let mut h = harness();
        transcript(&h.home, SESSION, &[user("hi")]);
        h.subs.start(spec(), h.deps.clone());
        recv_update(&mut h.rx).await;

        h.topology_tx
            .send(Arc::new(Topology::default()))
            .expect("publish empty topology");
        tokio::time::sleep(Duration::from_millis(1200)).await;
        let tombstone = recv_update(&mut h.rx).await;
        let (entries, reset, _) = parts(&tombstone);
        assert!(reset, "tombstone is reset:true");
        assert!(entries.is_empty(), "tombstone carries no entries");

        // Quiet while gone.
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(h.rx.try_recv().is_err(), "one tombstone only");

        h.topology_tx
            .send(Arc::new(topology(SESSION)))
            .expect("publish restored topology");
        tokio::time::sleep(Duration::from_millis(1200)).await;
        let rebirth = recv_update(&mut h.rx).await;
        let (entries, reset, _) = parts(&rebirth);
        assert!(reset, "rebirth is a rebuild");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["text"], "hi");
    }

    /// A pane without an agent session still gets its initial frame —
    /// the unavailable page rides `reset:true` with empty `messages`.
    #[tokio::test(start_paused = true)]
    async fn sessionless_pane_initial_reset() {
        let home = TempDir::new().expect("home");
        let xdg = TempDir::new().expect("xdg");
        let herdr = FakeHerdr::serving(vec![]);
        let (sink, mut rx) = recording_sink();
        let (invalidations, _) = broadcast::channel(32);
        let mut topology = Topology::default();
        topology.accept(SessionSnapshot {
            agents: vec![AgentInfo {
                pane_id: PANE.into(),
                terminal_id: "term-1".into(),
                agent_status: AgentStatus::Working,
                agent: Some("claude".into()),
                cwd: Some("/work/repo".into()),
                ..AgentInfo::default()
            }],
            ..SessionSnapshot::default()
        });
        let (handle, _tx) =
            TopologyHandle::for_test(herdr.client(), Arc::new(topology), invalidations.clone());
        let mut subs = ConvoSubSet::default();
        subs.start(
            spec(),
            ConvoSubDeps {
                handle,
                browser: browser(&home, &xdg),
                sink,
                cancel: CancellationToken::new(),
                on_read: None,
            },
        );
        let update = recv_update(&mut rx).await;
        let (entries, reset, _) = parts(&update);
        assert!(reset);
        assert!(entries.is_empty());
    }

    /// `pane.*` invalidations past the freshness window poll early —
    /// the append frame arrives well before the next tick.
    #[tokio::test(start_paused = true)]
    async fn invalidation_polls_before_the_tick() {
        let mut h = harness();
        let path = transcript(&h.home, SESSION, &[user("hi")]);
        h.subs.start(spec(), h.deps.clone());
        recv_update(&mut h.rx).await;

        // First tick at t=1s polls (unchanged), then next_read = 1.5s.
        tokio::time::sleep(Duration::from_millis(1600)).await;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("append");
        writeln!(file, "{}", assistant("fast")).expect("write");
        drop(file);
        h.invalidations
            .send(crate::watches::test_support::invalidate(Some(PANE)))
            .expect("send invalidation");

        // The frame lands within ~300ms of virtual time — the next tick
        // is ~400ms out, so arrival proves the fast path ran.
        let update = tokio::time::timeout(Duration::from_millis(300), h.rx.recv())
            .await
            .expect("invalidation fast-path frame")
            .expect("frame");
        let Outbound::ConversationUpdate(update) = update else {
            panic!("expected conversation_update");
        };
        let (entries, reset, _) = parts(&update);
        assert!(!reset);
        assert_eq!(entries[0]["text"], "fast");
    }

    /// `subscribe_pane`/`unsubscribe_pane` resolution — pane addresses
    /// win over session addresses, live panes only for subscribe,
    /// verbatim cleanup for unsubscribe.
    #[test]
    fn target_resolution() {
        let t = topology(SESSION);
        let inbound = |v: serde_json::Value| {
            let mut map = v.as_object().expect("object").clone();
            map.insert(
                "type".to_owned(),
                serde_json::Value::String("subscribe_conversation".to_owned()),
            );
            Inbound::decode_map(&map).expect("decode")
        };

        // pane_id — top-level or under target — must resolve live.
        for v in [
            serde_json::json!({"pane_id": PANE}),
            serde_json::json!({"target": {"pane_id": PANE}}),
        ] {
            assert_eq!(subscribe_pane(&t, &inbound(v)).as_deref(), Some(PANE));
        }
        // Session addresses resolve through the topology index.
        let by_session = inbound(serde_json::json!({"target": {"agent_session_id": "sess-1"}}));
        assert_eq!(subscribe_pane(&t, &by_session).as_deref(), Some(PANE));
        // Unknown addresses resolve nothing.
        for v in [
            serde_json::json!({"pane_id": "wE:p9"}),
            serde_json::json!({"target": {"pane_id": "wE:p9"}}),
            serde_json::json!({"target": {"agent_session_id": "sess-9"}}),
            serde_json::json!({}),
            serde_json::json!({"target": {}}),
        ] {
            assert_eq!(subscribe_pane(&t, &inbound(v)), None);
        }

        // Unsubscribe: pane addresses verbatim (gone panes still stop),
        // session addresses through the index, nothing otherwise.
        assert_eq!(
            unsubscribe_pane(&t, &inbound(serde_json::json!({"pane_id": "wE:p9"}))).as_deref(),
            Some("wE:p9"),
            "a gone pane still owns its subscription slot"
        );
        assert_eq!(
            unsubscribe_pane(&t, &by_session).as_deref(),
            Some(PANE),
            "session address maps to the live pane"
        );
        assert_eq!(unsubscribe_pane(&t, &inbound(serde_json::json!({}))), None);
    }
}
