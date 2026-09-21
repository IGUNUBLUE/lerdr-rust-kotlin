//! Pane-size lease arbitration — the pure decision logic of
//! `internal/panesize/manager.go`, decoupled from `ps`/`stty` execution.
//!
//! Semantics preserved verbatim:
//! - One lease per client per pane; re-acquisition overwrites + renews.
//! - `rows == 0` leases width only and never constrains height.
//! - Effective columns = minimum across live leases; effective rows =
//!   minimum positive row lease, else the pane's baseline rows.
//! - [`LeaseManager::release`] lapses the lease at `now + RELEASE_GRACE`
//!   (a re-acquire inside the window resizes nothing);
//!   [`LeaseManager::release_client`] deletes immediately (disconnect path).
//! - Expiry is removed lazily by `acquire`/`sweep_expired`;
//!   `minimum_columns` itself does NOT consult expiry — only the sweep does.
//! - A local resize observed on the next `acquire` rebaselines per dimension.
//! - When the last lease leaves, `restore` resets the pane to baseline and
//!   drops all pane state.
//!
//! The clock is injected: callers drive [`LeaseManager::advance`] (or set
//! the time source directly); nothing in this module touches real time,
//! processes, or TTYs — the [`PaneHost`] trait stands in for `stty`.

use std::collections::BTreeMap;
use std::time::Duration;

/// `MinColumns`.
pub const MIN_COLUMNS: i64 = 40;
/// `MaxColumns`.
pub const MAX_COLUMNS: i64 = 240;
/// `MinRows`.
pub const MIN_ROWS: i64 = 10;
/// `MaxRows`.
pub const MAX_ROWS: i64 = 120;
/// `LeaseTTL` — survives hidden-tab timer clamping (~60-65s measured).
pub const LEASE_TTL: Duration = Duration::from_secs(120);
/// `ReleaseGrace` — a released width stays applied this long so a returning
/// client re-acquires it without a double resize.
pub const RELEASE_GRACE: Duration = Duration::from_secs(10);

/// The manager's error taxonomy — same set as `manager.go`, snake_cased for
/// wire/fixture comparison via [`LeaseError::as_str`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LeaseError {
    #[error("Pane size leasing is shut down")]
    Closed,
    #[error("Columns must be between 40 and 240")]
    InvalidColumns,
    #[error("Rows must be between 10 and 120")]
    InvalidRows,
    #[error("Pane and lease owner are required")]
    InvalidLease,
    #[error("Pane size lease owner is disconnected")]
    LeaseOwnerGone,
    #[error("Pane foreground process information is unavailable")]
    ProcessUnavailable,
    #[error("Pane foreground process does not have a TTY")]
    TtyUnavailable,
    #[error("Pane terminal size is unavailable")]
    SizeUnavailable,
    #[error("Pane terminal size could not be changed")]
    ResizeFailed,
    #[error("Pane size leasing is unsupported on this platform")]
    UnsupportedOs,
}

impl LeaseError {
    /// Fixture-stable snake_case name (`expect_error` values).
    pub fn as_str(&self) -> &'static str {
        match self {
            LeaseError::Closed => "closed",
            LeaseError::InvalidColumns => "invalid_columns",
            LeaseError::InvalidRows => "invalid_rows",
            LeaseError::InvalidLease => "invalid_lease",
            LeaseError::LeaseOwnerGone => "lease_owner_gone",
            LeaseError::ProcessUnavailable => "process_unavailable",
            LeaseError::TtyUnavailable => "tty_unavailable",
            LeaseError::SizeUnavailable => "size_unavailable",
            LeaseError::ResizeFailed => "resize_failed",
            LeaseError::UnsupportedOs => "unsupported_os",
        }
    }
}

/// `terminalSize` — pane dimensions as `stty` reports them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TerminalSize {
    pub columns: i64,
    pub rows: i64,
}

/// What the manager needs from the outside world — the `provider` +
/// `commandRunner` seam of `manager.go` with process/TTY resolution folded
/// into "read the pane's size" (the pure model does not care how the size is
/// obtained).
pub trait PaneHost {
    /// `resolvePane`/`readSize` — the pane's current terminal size. Fails
    /// with `ProcessUnavailable`/`TtyUnavailable`/`SizeUnavailable` style
    /// errors when the pane cannot be resolved or its size read.
    fn pane_size(&mut self, pane_id: &str) -> Result<TerminalSize, LeaseError>;

    /// `setSize` — `stty cols <columns> [rows <rows>]`; `rows == 0` means
    /// "leave the height alone" (width-only panes never get a row argument).
    fn set_size(&mut self, pane_id: &str, columns: i64, rows: i64) -> Result<(), LeaseError>;
}

/// `Lease` — one client's claim on a pane's size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lease {
    pub columns: i64,
    /// Zero = width-only: the pane keeps its own height.
    pub rows: i64,
    pub expires_at: Duration,
}

#[derive(Debug)]
struct PaneState {
    baseline_columns: i64,
    baseline_rows: i64,
    applied_columns: i64,
    applied_rows: i64,
    resized_at: Option<Duration>,
    leases: BTreeMap<String, Lease>,
}

/// The arbitration core. `now` is a monotonic injected clock; the sweep that
/// Go runs on a 1s ticker is the explicit [`sweep_expired`](Self::sweep_expired).
pub struct LeaseManager<H> {
    host: H,
    ttl: Duration,
    grace: Duration,
    now: Duration,
    panes: BTreeMap<String, PaneState>,
    closed: bool,
}

impl<H: PaneHost> LeaseManager<H> {
    /// Production manager: `LeaseTTL`/`ReleaseGrace`, clock at zero.
    pub fn new(host: H) -> Self {
        Self {
            host,
            ttl: LEASE_TTL,
            grace: RELEASE_GRACE,
            now: Duration::ZERO,
            panes: BTreeMap::new(),
            closed: false,
        }
    }

    /// Custom TTL/grace for tests (`newManager`'s knobs).
    pub fn with_timeouts(host: H, ttl: Duration, grace: Duration) -> Self {
        Self {
            ttl,
            grace,
            ..Self::new(host)
        }
    }

    /// The wrapped host (e.g. to script `local_resize` or read the tty size).
    pub fn host(&self) -> &H {
        &self.host
    }

    /// Mutable host access for out-of-band changes (local resizes).
    pub fn host_mut(&mut self) -> &mut H {
        &mut self.host
    }

    /// Advance the injected clock. Does NOT sweep — the caller decides when
    /// expiry is enforced, exactly like Go's separate ticker.
    pub fn advance(&mut self, by: Duration) {
        self.now += by;
    }

    /// Current injected time.
    pub fn now(&self) -> Duration {
        self.now
    }

    /// `Acquire` — validate, resolve pane state on first touch, renew/write
    /// the client's lease, resize the pane to the new effective minimum.
    ///
    /// `owner_gone` stands in for a cancelled client context: checked before
    /// any pane work (and again after the size read, though a cancelled
    /// context is constant for one call in practice).
    ///
    /// Returns the effective `(columns, rows)` applied.
    pub fn acquire(
        &mut self,
        client_id: &str,
        pane_id: &str,
        columns: i64,
        rows: i64,
        owner_gone: bool,
    ) -> Result<(i64, i64), LeaseError> {
        if client_id.is_empty() || pane_id.is_empty() {
            return Err(LeaseError::InvalidLease);
        }
        if !(MIN_COLUMNS..=MAX_COLUMNS).contains(&columns) {
            return Err(LeaseError::InvalidColumns);
        }
        if rows != 0 && !(MIN_ROWS..=MAX_ROWS).contains(&rows) {
            return Err(LeaseError::InvalidRows);
        }
        if self.closed {
            return Err(LeaseError::Closed);
        }
        if owner_gone {
            return Err(LeaseError::LeaseOwnerGone);
        }

        let now = self.now;
        let new_state = !self.panes.contains_key(pane_id);
        let current;
        if new_state {
            let size = self.host.pane_size(pane_id)?;
            self.panes.insert(
                pane_id.to_owned(),
                PaneState {
                    baseline_columns: size.columns,
                    baseline_rows: size.rows,
                    applied_columns: size.columns,
                    applied_rows: size.rows,
                    resized_at: None,
                    leases: BTreeMap::new(),
                },
            );
            current = size;
        } else {
            let state = self.panes.get_mut(pane_id).expect("checked");
            remove_expired(state, now);
            current = self.host.pane_size(pane_id)?;
            // A local resize while leased becomes the new restore point,
            // per dimension.
            if current.columns != state.applied_columns {
                state.baseline_columns = current.columns;
            }
            if current.rows != state.applied_rows {
                state.baseline_rows = current.rows;
            }
        }
        if owner_gone {
            return Err(LeaseError::LeaseOwnerGone);
        }

        let state = self.panes.get_mut(pane_id).expect("checked");
        let previous = state.leases.get(client_id).copied();
        state.leases.insert(
            client_id.to_owned(),
            Lease {
                columns,
                rows,
                expires_at: now + self.ttl,
            },
        );
        let (target_columns, _) = minimum_columns(&state.leases);
        let mut target_rows = minimum_rows(&state.leases);
        let constrained_rows = target_rows > 0;
        if !constrained_rows {
            target_rows = state.baseline_rows;
        }
        // Width-only panes never get their height sent to stty.
        let mut stty_rows = target_rows;
        if !constrained_rows && target_rows == state.applied_rows {
            stty_rows = 0;
        }
        // Renewals that change nothing skip the resize syscall entirely.
        let mut resize_needed = target_columns != current.columns;
        if stty_rows > 0 && target_rows != current.rows {
            resize_needed = true;
        }
        if resize_needed {
            if let Err(err) = self.host.set_size(pane_id, target_columns, stty_rows) {
                match previous {
                    Some(lease) => {
                        self.panes
                            .get_mut(pane_id)
                            .expect("checked")
                            .leases
                            .insert(client_id.to_owned(), lease);
                    }
                    None => {
                        self.panes
                            .get_mut(pane_id)
                            .expect("checked")
                            .leases
                            .remove(client_id);
                    }
                }
                // Go keeps the freshly-created pane state on failure.
                return Err(err);
            }
            state.resized_at = Some(now);
        }
        state.applied_columns = target_columns;
        state.applied_rows = target_rows;
        Ok((target_columns, target_rows))
    }

    /// `Release` — lapse the lease at `now + grace` instead of deleting it.
    /// An already-dead lease is removed and reconciled immediately.
    pub fn release(&mut self, client_id: &str, pane_id: &str) -> Result<(), LeaseError> {
        if client_id.is_empty() || pane_id.is_empty() {
            return Err(LeaseError::InvalidLease);
        }
        if self.closed {
            return Ok(());
        }
        let Some(state) = self.panes.get_mut(pane_id) else {
            return Ok(());
        };
        let Some(mut lease) = state.leases.get(client_id).copied() else {
            return Ok(());
        };
        let now = self.now;
        let lapse = now + self.grace;
        if lease.expires_at > lapse {
            lease.expires_at = lapse;
            state.leases.insert(client_id.to_owned(), lease);
        }
        if lease.expires_at > now {
            return Ok(());
        }
        state.leases.remove(client_id);
        self.reconcile(pane_id)
    }

    /// `ReleaseClient` — the disconnect path: delete every lease the client
    /// holds and reconcile each affected pane immediately (no grace).
    pub fn release_client(&mut self, client_id: &str) -> Result<(), LeaseError> {
        if client_id.is_empty() {
            return Err(LeaseError::InvalidLease);
        }
        if self.closed {
            return Ok(());
        }
        let mut result: Result<(), LeaseError> = Ok(());
        let pane_ids: Vec<String> = self.panes.keys().cloned().collect();
        for pane_id in pane_ids {
            let owned = self
                .panes
                .get_mut(&pane_id)
                .is_some_and(|state| state.leases.remove(client_id).is_some());
            let active = self
                .panes
                .get(&pane_id)
                .map(|state| minimum_columns(&state.leases).1)
                .unwrap_or(false);
            if !owned && active {
                continue;
            }
            if let Err(err) = self.reconcile(&pane_id) {
                result = Err(err);
            }
        }
        result
    }

    /// `SweepExpired` — the ticker body: drop lapsed leases and reconcile
    /// every pane whose effective size changed (including restores).
    pub fn sweep_expired(&mut self) -> Result<(), LeaseError> {
        if self.closed {
            return Ok(());
        }
        let now = self.now;
        let mut result: Result<(), LeaseError> = Ok(());
        let pane_ids: Vec<String> = self.panes.keys().cloned().collect();
        for pane_id in pane_ids {
            let (removed, active, settled) = {
                let Some(state) = self.panes.get_mut(&pane_id) else {
                    continue;
                };
                let removed = remove_expired(state, now);
                let (target, active) = minimum_columns(&state.leases);
                let mut target_rows = minimum_rows(&state.leases);
                if target_rows == 0 {
                    target_rows = state.baseline_rows;
                }
                let settled = state.applied_columns == target && state.applied_rows == target_rows;
                (removed, active, settled)
            };
            if !removed && active && settled {
                continue;
            }
            if let Err(err) = self.reconcile(&pane_id) {
                result = Err(err);
            }
        }
        result
    }

    /// `ActiveColumns` — the narrowest unexpired column lease.
    pub fn active_columns(&self, pane_id: &str) -> Option<i64> {
        if self.closed {
            return None;
        }
        let state = self.panes.get(pane_id)?;
        let now = self.now;
        let mut minimum = 0i64;
        for lease in state.leases.values() {
            if lease.expires_at <= now {
                continue;
            }
            if minimum == 0 || lease.columns < minimum {
                minimum = lease.columns;
            }
        }
        (minimum != 0).then_some(minimum)
    }

    /// `ActiveRows` — smallest unexpired row lease, or baseline rows when a
    /// leased pane has no row constraint.
    pub fn active_rows(&self, pane_id: &str) -> Option<i64> {
        if self.closed {
            return None;
        }
        let state = self.panes.get(pane_id)?;
        let now = self.now;
        let mut active = false;
        let mut minimum = 0i64;
        for lease in state.leases.values() {
            if lease.expires_at <= now {
                continue;
            }
            active = true;
            if lease.rows > 0 && (minimum == 0 || lease.rows < minimum) {
                minimum = lease.rows;
            }
        }
        if !active {
            return None;
        }
        Some(if minimum == 0 {
            state.baseline_rows
        } else {
            minimum
        })
    }

    /// `ResizedWithin` — whether a lease actually changed the width within
    /// `window` (renewals that keep the same columns don't count).
    pub fn resized_within(&self, pane_id: &str, window: Duration) -> bool {
        if self.closed {
            return false;
        }
        self.panes
            .get(pane_id)
            .and_then(|state| state.resized_at)
            .is_some_and(|resized_at| self.now.saturating_sub(resized_at) < window)
    }

    /// `Shutdown` — clear all leases and restore every pane to baseline.
    pub fn shutdown(&mut self) -> Result<(), LeaseError> {
        self.closed = true;
        let mut result: Result<(), LeaseError> = Ok(());
        let pane_ids: Vec<String> = self.panes.keys().cloned().collect();
        for pane_id in pane_ids {
            if let Some(state) = self.panes.get_mut(&pane_id) {
                state.leases.clear();
            }
            if let Err(err) = self.restore(&pane_id) {
                result = Err(err);
            }
        }
        result
    }

    /// Live lease holders for a pane — sorted client ids whose
    /// `expires_at > now` (released-in-grace leases still count).
    pub fn holders(&self, pane_id: &str) -> Vec<String> {
        let now = self.now;
        self.panes
            .get(pane_id)
            .map(|state| {
                state
                    .leases
                    .iter()
                    .filter(|(_, lease)| lease.expires_at > now)
                    .map(|(client, _)| client.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// `reconcile` — apply the effective minimum, or restore when no leases
    /// remain.
    fn reconcile(&mut self, pane_id: &str) -> Result<(), LeaseError> {
        let state = self.panes.get(pane_id).expect("pane state");
        let (target, active) = minimum_columns(&state.leases);
        if !active {
            return self.restore(pane_id);
        }
        let mut target_rows = minimum_rows(&state.leases);
        let constrained_rows = target_rows > 0;
        if !constrained_rows {
            target_rows = state.baseline_rows;
        }
        if target == state.applied_columns && target_rows == state.applied_rows {
            return Ok(());
        }
        let mut stty_rows = target_rows;
        if !constrained_rows && target_rows == state.applied_rows {
            stty_rows = 0;
        }
        self.host.set_size(pane_id, target, stty_rows)?;
        let state = self.panes.get_mut(pane_id).expect("pane state");
        state.resized_at = Some(self.now);
        state.applied_columns = target;
        state.applied_rows = target_rows;
        Ok(())
    }

    /// `restore` — pane back to baseline, then drop all state for it.
    fn restore(&mut self, pane_id: &str) -> Result<(), LeaseError> {
        let state = self.panes.get(pane_id).expect("pane state");
        let mut stty_rows = state.baseline_rows;
        if state.applied_rows == state.baseline_rows {
            // The height was never leased away; leave the tty's rows alone.
            stty_rows = 0;
        }
        self.host
            .set_size(pane_id, state.baseline_columns, stty_rows)?;
        let state = self.panes.get_mut(pane_id).expect("pane state");
        state.applied_columns = state.baseline_columns;
        state.applied_rows = state.baseline_rows;
        self.panes.remove(pane_id);
        Ok(())
    }
}

/// `minimumColumns` — smallest lease across the map; no expiry check
/// (expiry is enforced by `remove_expired`/sweep, not here).
fn minimum_columns(leases: &BTreeMap<String, Lease>) -> (i64, bool) {
    let mut minimum = 0i64;
    for lease in leases.values() {
        if minimum == 0 || lease.columns < minimum {
            minimum = lease.columns;
        }
    }
    (minimum, minimum != 0)
}

/// `minimumRows` — smallest positive row constraint; 0 when every lease is
/// width-only.
fn minimum_rows(leases: &BTreeMap<String, Lease>) -> i64 {
    let mut minimum = 0i64;
    for lease in leases.values() {
        if lease.rows <= 0 {
            continue;
        }
        if minimum == 0 || lease.rows < minimum {
            minimum = lease.rows;
        }
    }
    minimum
}

/// `removeExpired` — drop every lease whose `expires_at <= now`.
fn remove_expired(state: &mut PaneState, now: Duration) -> bool {
    let before = state.leases.len();
    state.leases.retain(|_, lease| lease.expires_at > now);
    state.leases.len() != before
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// In-memory pane host: a map of pane -> current tty size.
    #[derive(Default)]
    struct MemoryHost {
        sizes: HashMap<String, TerminalSize>,
    }

    impl PaneHost for MemoryHost {
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

    fn manager(cols: i64, rows: i64) -> LeaseManager<MemoryHost> {
        let mut host = MemoryHost::default();
        host.sizes.insert(
            "pane".to_owned(),
            TerminalSize {
                columns: cols,
                rows,
            },
        );
        LeaseManager::new(host)
    }

    #[test]
    fn width_only_keeps_height() {
        let mut m = manager(120, 40);
        assert_eq!(m.acquire("a", "pane", 80, 0, false), Ok((80, 40)));
        assert_eq!(m.holders("pane"), vec!["a"]);
    }

    #[test]
    fn release_grace_then_expiry_restores() {
        let mut m = manager(160, 42);
        m.acquire("a", "pane", 110, 0, false).unwrap();
        m.acquire("b", "pane", 76, 0, false).unwrap();
        m.release("b", "pane").unwrap();
        assert_eq!(m.holders("pane"), vec!["a", "b"]);
        m.advance(Duration::from_secs(9));
        m.sweep_expired().unwrap();
        assert_eq!(m.holders("pane"), vec!["a", "b"]);
        m.advance(Duration::from_secs(1));
        m.sweep_expired().unwrap();
        assert_eq!(m.holders("pane"), vec!["a"]);
        assert_eq!(m.active_columns("pane"), Some(110));
    }

    #[test]
    fn release_client_is_immediate() {
        let mut m = manager(120, 40);
        m.acquire("a", "pane", 80, 0, false).unwrap();
        m.release_client("a").unwrap();
        assert_eq!(m.holders("pane"), Vec::<String>::new());
        assert_eq!(m.active_columns("pane"), None);
        assert_eq!(m.host.sizes["pane"].columns, 120);
    }

    #[test]
    fn owner_gone_fails_before_state() {
        let mut m = manager(120, 40);
        assert_eq!(
            m.acquire("a", "pane", 80, 0, true),
            Err(LeaseError::LeaseOwnerGone)
        );
        assert!(m.panes.is_empty());
    }

    #[test]
    fn validation_order() {
        let mut m = manager(120, 40);
        assert_eq!(
            m.acquire("", "pane", 80, 0, false),
            Err(LeaseError::InvalidLease)
        );
        assert_eq!(
            m.acquire("a", "pane", 30, 0, false),
            Err(LeaseError::InvalidColumns)
        );
        assert_eq!(
            m.acquire("a", "pane", 80, 5, false),
            Err(LeaseError::InvalidRows)
        );
        assert!(m.acquire("a", "pane", 80, 0, false).is_ok());
    }
}
