//! Bounded, shared recovery of application-owned terminal history. Herdr's
//! text read can temporarily scroll a TUI: all watchers suppress those frames
//! while a reservation is live, then restart from the restored viewport.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::pane_stream::HostHistory;

const MAX_PANES: usize = 32;
const MAX_ATTEMPTS: u8 = 3;
const RETRY_DELAY: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Epoch {
    pub generation: i64,
    pub terminal: String,
    pub session: String,
    pub activity: u64,
    pub size: Option<(u16, u16)>,
}

impl Epoch {
    fn same_terminal(&self, other: &Self) -> bool {
        self.generation == other.generation
            && self.terminal == other.terminal
            && self.session == other.session
            && self.size == other.size
    }
}

#[derive(Clone, Default)]
pub(crate) struct Store(Arc<Mutex<State>>);

#[derive(Default)]
struct State {
    entries: HashMap<String, Entry>,
    version: u64,
}

struct Entry {
    epoch: Epoch,
    snapshot: Option<Arc<HostHistory>>,
    version: u64,
    busy: bool,
    complete: bool,
    lines: u32,
    attempts: u8,
    attempted: Option<Instant>,
    touched: Instant,
}

#[derive(Default)]
pub(crate) struct View {
    pub snapshot: Option<Arc<HostHistory>>,
    pub version: u64,
    pub busy: bool,
}

/// Dropping a cancelled reader also releases every other watcher's gate.
pub(crate) struct Reservation {
    store: Store,
    pane: String,
    pub lines: u32,
}

impl Store {
    pub fn read_version(&self, pane: &str) -> Option<u64> {
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match state.entries.get(pane) {
            Some(entry) if entry.busy => None,
            Some(entry) => Some(entry.version),
            None => Some(0),
        }
    }

    /// Share one read per idle epoch/grid. Smaller consumers reuse a larger
    /// cached window; failed reads retry at most three times, five seconds apart.
    pub fn observe(
        &self,
        pane: &str,
        epoch: Epoch,
        lines: u32,
        eligible: bool,
        now: Instant,
    ) -> (View, Option<Reservation>) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if !state.entries.contains_key(pane) {
            if state.entries.len() >= MAX_PANES {
                let oldest = state
                    .entries
                    .iter()
                    .filter(|(_, entry)| !entry.busy)
                    .min_by_key(|(_, entry)| entry.touched)
                    .map(|(key, _)| key.clone());
                let Some(oldest) = oldest else {
                    return (View::default(), None);
                };
                state.entries.remove(&oldest);
            }
            state.version += 1;
            let version = state.version;
            state.entries.insert(
                pane.to_owned(),
                Entry {
                    epoch: epoch.clone(),
                    snapshot: None,
                    version,
                    busy: false,
                    complete: false,
                    lines,
                    attempts: 0,
                    attempted: None,
                    touched: now,
                },
            );
        }
        let entry = state.entries.get_mut(pane).expect("inserted above");
        entry.touched = now;
        if !entry.busy && (entry.epoch != epoch || lines > entry.lines) {
            if !entry.epoch.same_terminal(&epoch) {
                entry.snapshot = None;
            }
            entry.epoch = epoch;
            entry.lines = entry.lines.max(lines);
            entry.complete = false;
            entry.attempts = 0;
            entry.attempted = None;
        }
        let due = eligible
            && !entry.busy
            && !entry.complete
            && entry.attempts < MAX_ATTEMPTS
            && entry
                .attempted
                .is_none_or(|at| now.duration_since(at) >= RETRY_DELAY);
        if due {
            entry.busy = true;
            entry.attempts += 1;
            entry.attempted = Some(now);
        }
        let view = View {
            snapshot: entry.snapshot.clone(),
            version: entry.version,
            busy: entry.busy,
        };
        let reservation = due.then(|| Reservation {
            store: self.clone(),
            pane: pane.to_owned(),
            lines: entry.lines,
        });
        (view, reservation)
    }
}

impl Reservation {
    pub fn finish(mut self, history: Option<HostHistory>) {
        self.release(history);
        self.pane.clear();
    }

    fn release(&self, history: Option<HostHistory>) {
        let mut state = self.store.0.lock().unwrap_or_else(|e| e.into_inner());
        state.version += 1;
        let version = state.version;
        if let Some(entry) = state.entries.get_mut(&self.pane) {
            entry.busy = false;
            entry.version = version;
            if let Some(history) = history {
                entry.snapshot = Some(Arc::new(history));
                entry.complete = true;
            }
        }
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.pane.is_empty() {
            self.release(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn epoch(activity: u64) -> Epoch {
        Epoch {
            generation: 1,
            terminal: "term".into(),
            session: "session".into(),
            activity,
            size: Some((60, 20)),
        }
    }

    #[test]
    fn watchers_share_recovery_and_cancellation_releases_the_gate() {
        let store = Store::default();
        let now = Instant::now();
        let before = store.read_version("p");
        let (view, owner) = store.observe("p", epoch(1), 400, true, now);
        assert!(view.busy);
        assert_eq!(store.read_version("p"), None);
        let (view, follower) = store.clone().observe("p", epoch(1), 400, true, now);
        assert!(view.busy);
        assert!(follower.is_none());
        drop(owner);
        assert_ne!(
            store.read_version("p"),
            before,
            "reject a read spanning recovery even after the gate reopens"
        );
        let (view, retry) = store.observe("p", epoch(1), 400, true, now);
        assert!(!view.busy);
        assert!(retry.is_none());
        assert!(store
            .observe("p", epoch(1), 400, true, now + RETRY_DELAY)
            .1
            .is_some());
    }

    #[test]
    fn cached_history_is_reused_until_a_new_turn_or_grid() {
        let store = Store::default();
        let now = Instant::now();
        let (_, owner) = store.observe("p", epoch(1), 400, true, now);
        owner.unwrap().finish(Some(HostHistory {
            text: "history".into(),
            truncated: false,
        }));
        let (view, owner) = store.observe("p", epoch(1), 30, true, now + RETRY_DELAY);
        assert_eq!(view.snapshot.unwrap().text, "history");
        assert!(owner.is_none());
        let (_, owner) = store.observe("p", epoch(2), 400, false, now);
        assert!(owner.is_none(), "never harvest a working or blocked agent");
        let (_, owner) = store.observe("p", epoch(2), 400, true, now);
        drop(owner);
        let mut replacement = epoch(3);
        replacement.generation = 2;
        let (view, _) = store.observe("p", replacement, 400, false, now);
        assert!(
            view.snapshot.is_none(),
            "never leak a replaced terminal's history"
        );
    }

    #[test]
    fn failures_and_cache_size_are_bounded() {
        let store = Store::default();
        let now = Instant::now();
        for n in 0..MAX_ATTEMPTS {
            let (_, owner) =
                store.observe("p", epoch(1), 400, true, now + RETRY_DELAY * u32::from(n));
            assert!(owner.is_some());
            drop(owner);
        }
        assert!(store
            .observe("p", epoch(1), 400, true, now + RETRY_DELAY * 10)
            .1
            .is_none());
        for n in 0..MAX_PANES * 2 {
            store.observe(&n.to_string(), epoch(1), 400, false, now);
        }
        assert_eq!(store.0.lock().unwrap().entries.len(), MAX_PANES);
    }
}
