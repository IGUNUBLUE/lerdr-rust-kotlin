//! Pane-size leasing — the `panesize.Manager` port.
//!
//! This subsystem is local-OS, not Herdr: `pane.process_info` locates the
//! foreground process, `ps -o tty=` resolves its TTY, and `stty` reads and
//! applies terminal dimensions (the relay is co-located with Herdr, so the
//! device nodes are shared). Leases are per client: the narrowest active
//! column request wins, `Release` lapses into a grace window so a phone
//! stepping away and back does not double-SIGWINCH the agent, and a 1s
//! sweeper restores baselines when leases expire.
//!
//! Wire surface: `lease_pane_size`/`release_pane_size` answer with a bare
//! `command_result` (the oracle emits no `action_receipt` for them), and
//! the router consults [`Leases::active_columns`]/[`Leases::active_rows`]
//! when building `read_pane` requests.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::Mutex;

use lerdr_core::protocol::{CommandResultMessage, Inbound, Outbound};

use super::ActionContext;

// panesize manager.go bounds.
const MIN_COLUMNS: i64 = 40;
const MAX_COLUMNS: i64 = 240;
const MIN_ROWS: i64 = 10;
const MAX_ROWS: i64 = 120;
/// `LeaseTTL` — twice the ~60s hidden-tab timer clamp so a renewing but
/// occluded client keeps its lease.
const LEASE_TTL: Duration = Duration::from_secs(120);
/// `ReleaseGrace` — a released width survives this long so stepping back
/// into the terminal does not resize twice.
const RELEASE_GRACE: Duration = Duration::from_secs(10);
/// `sweepInterval`.
const SWEEP_INTERVAL: Duration = Duration::from_secs(1);
/// `commandTimeout` — every `ps`/`stty` exec.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(3);
/// `paneResizeSettleWindow` — `read_pane` flags `resize_settling` inside it.
pub(crate) const RESIZE_SETTLE_WINDOW: Duration = Duration::from_secs(4);

const ERR_INVALID_COLUMNS: &str = "Columns must be between 40 and 240";
const ERR_INVALID_ROWS: &str = "Rows must be between 10 and 120";
const ERR_INVALID_LEASE: &str = "Pane and lease owner are required";
const ERR_OWNER_GONE: &str = "Pane size lease owner is disconnected";
const ERR_PROCESS_UNAVAILABLE: &str = "Pane foreground process information is unavailable";
const ERR_TTY_UNAVAILABLE: &str = "Pane foreground process does not have a TTY";
const ERR_SIZE_UNAVAILABLE: &str = "Pane terminal size is unavailable";
const ERR_RESIZE_FAILED: &str = "Pane terminal size could not be changed";
const ERR_CLOSED: &str = "Pane size leasing is shut down";

/// A client's outstanding width/height request (`Rows == 0` is width-only —
/// the pane keeps its own height and old clients stay valid).
#[derive(Clone, Copy)]
struct Lease {
    columns: i64,
    rows: i64,
    expires_at: Instant,
}

/// Per-pane bookkeeping — `paneState`.
struct PaneState {
    tty: String,
    baseline_rows: i64,
    baseline_columns: i64,
    applied_rows: i64,
    applied_columns: i64,
    resized_at: Option<Instant>,
    leases: HashMap<String, Lease>,
}

#[derive(Clone, Copy)]
struct TerminalSize {
    rows: i64,
    columns: i64,
}

/// `pane.process_info` result — only the fields `foregroundPID` consumes.
#[derive(Debug, serde::Deserialize, Default)]
pub(crate) struct PaneProcessInfo {
    #[serde(default)]
    pub pane_id: String,
    #[serde(default)]
    pub foreground_process_group_id: i64,
    #[serde(default)]
    pub foreground_processes: Vec<PaneProcess>,
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct PaneProcess {
    #[serde(default)]
    pub pid: i64,
}

/// Process-info source — production impl wraps the Herdr client; tests
/// inject a stub.
pub(crate) trait ProcessInfoProvider: Send + Sync {
    fn pane_process_info<'a>(
        &'a self,
        pane_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<PaneProcessInfo, ()>> + Send + 'a>>;
}

impl ProcessInfoProvider for lerdr_herdr::Client {
    fn pane_process_info<'a>(
        &'a self,
        pane_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<PaneProcessInfo, ()>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Serialize)]
            struct Params<'a> {
                pane_id: &'a str,
            }
            let value = self
                .call_with_timeout(
                    "pane.process_info",
                    &Params { pane_id },
                    Some(COMMAND_TIMEOUT),
                )
                .await
                .map_err(|_| ())?;
            serde_json::from_value(
                value
                    .pointer("/process_info")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            )
            .map_err(|_| ())
        })
    }
}

/// `commandRunner` — `ps`/`stty` exec. Production runs the real binaries;
/// tests record calls and return scripted output.
pub(crate) trait ExecRunner: Send + Sync {
    fn output<'a>(
        &'a self,
        name: &'a str,
        args: &'a [String],
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<u8>>> + Send + 'a>>;
}

struct SystemRunner;

impl ExecRunner for SystemRunner {
    fn output<'a>(
        &'a self,
        name: &'a str,
        args: &'a [String],
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<u8>>> + Send + 'a>> {
        let args: Vec<String> = args.to_vec();
        let name = name.to_owned();
        Box::pin(async move {
            let child = tokio::process::Command::new(&name).args(&args).output();
            match tokio::time::timeout(COMMAND_TIMEOUT, child).await {
                Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "exec timeout")),
                Ok(Err(err)) => Err(err),
                Ok(Ok(out)) if out.status.success() => Ok(out.stdout),
                Ok(Ok(out)) => Err(io::Error::other(format!("{name} exited {}", out.status))),
            }
        })
    }
}

type SharedProvider = Arc<dyn ProcessInfoProvider>;
type SharedRunner = Arc<dyn ExecRunner>;

struct LeaseInner {
    state: Mutex<LeaseState>,
    provider: SharedProvider,
    runner: SharedRunner,
    ttl: Duration,
    grace: Duration,
    now: fn() -> Instant,
    closed: AtomicBool,
}

struct LeaseState {
    panes: HashMap<String, PaneState>,
}

/// Cloneable handle shared by the `ActionContext`, the disconnect path, and
/// the background sweeper.
#[derive(Clone)]
pub(crate) struct Leases {
    inner: Arc<LeaseInner>,
}

impl Leases {
    /// `NewManager` — production provider/runner.
    pub(crate) fn new(client: lerdr_herdr::Client) -> Self {
        Leases {
            inner: Arc::new(LeaseInner {
                state: Mutex::new(LeaseState {
                    panes: HashMap::new(),
                }),
                provider: Arc::new(client),
                runner: Arc::new(SystemRunner),
                ttl: LEASE_TTL,
                grace: RELEASE_GRACE,
                now: Instant::now,
                closed: AtomicBool::new(false),
            }),
        }
    }

    /// Test seam — stubbed provider/runner/clock.
    #[cfg(test)]
    fn with_parts(
        provider: SharedProvider,
        runner: SharedRunner,
        ttl: Duration,
        grace: Duration,
        now: fn() -> Instant,
    ) -> Self {
        Leases {
            inner: Arc::new(LeaseInner {
                state: Mutex::new(LeaseState {
                    panes: HashMap::new(),
                }),
                provider,
                runner,
                ttl,
                grace,
                now,
                closed: AtomicBool::new(false),
            }),
        }
    }

    /// `Manager.Run` — the 1s expiry sweep; stops on cancellation. A no-op
    /// outside a runtime (test construction of the factory).
    pub(crate) fn spawn_sweeper(&self, cancel: tokio_util::sync::CancellationToken) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let leases = self.clone();
        runtime.spawn(async move {
            let mut ticker = tokio::time::interval(SWEEP_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = ticker.tick() => {
                        if let Err(err) = leases.sweep_expired().await {
                            tracing::warn!(error = %err, "pane size lease expiry sweep failed");
                        }
                    }
                }
            }
        });
    }

    /// `Manager.Acquire` — validate, resolve or refresh the pane state,
    /// record the lease, apply the minimum via `stty`.
    ///
    /// `owner_alive` is the client connection's cancellation token — the
    /// oracle checks `ctx.Err()` before and after pane resolution.
    pub(crate) async fn acquire(
        &self,
        owner_alive: &tokio_util::sync::CancellationToken,
        client_id: &str,
        pane_id: &str,
        columns: i64,
        rows: i64,
    ) -> Result<(i64, i64), &'static str> {
        if client_id.is_empty() || pane_id.is_empty() {
            return Err(ERR_INVALID_LEASE);
        }
        if !(MIN_COLUMNS..=MAX_COLUMNS).contains(&columns) {
            return Err(ERR_INVALID_COLUMNS);
        }
        if rows != 0 && !(MIN_ROWS..=MAX_ROWS).contains(&rows) {
            return Err(ERR_INVALID_ROWS);
        }
        let inner = &*self.inner;
        let mut state = inner.state.lock().await;
        if inner.closed.load(Ordering::SeqCst) {
            return Err(ERR_CLOSED);
        }
        if owner_alive.is_cancelled() {
            return Err(ERR_OWNER_GONE);
        }

        let now = (inner.now)();
        let new_state = !state.panes.contains_key(pane_id);
        let current: TerminalSize;
        if new_state {
            let pane = self.resolve_pane(pane_id).await?;
            current = TerminalSize {
                rows: pane.applied_rows,
                columns: pane.applied_columns,
            };
            state.panes.insert(pane_id.to_owned(), pane);
        } else {
            let pane = state.panes.get_mut(pane_id).expect("checked above");
            remove_expired(pane, now);
            current = self.read_size(&pane.tty).await?;
            // A local terminal resize while the lease is active becomes the
            // new restore point, per dimension.
            if current.columns != pane.applied_columns {
                pane.baseline_columns = current.columns;
            }
            if current.rows != pane.applied_rows {
                pane.baseline_rows = current.rows;
            }
        }
        if owner_alive.is_cancelled() {
            return Err(ERR_OWNER_GONE);
        }

        let pane = state.panes.get_mut(pane_id).expect("inserted above");
        let previous = pane.leases.insert(
            client_id.to_owned(),
            Lease {
                columns,
                rows,
                expires_at: now + inner.ttl,
            },
        );
        let (target_columns, _) = minimum_columns(&pane.leases);
        let mut target_rows = minimum_rows(&pane.leases);
        let constrained_rows = target_rows > 0;
        if !constrained_rows {
            target_rows = pane.baseline_rows;
        }
        // A width-only pane never gets its height touched: rows reach stty
        // only while a row lease constrains them or a lapsed one must be
        // undone.
        let mut stty_rows = target_rows;
        if !constrained_rows && target_rows == pane.applied_rows {
            stty_rows = 0;
        }
        // A renewal extends the lease only — calling stty with unchanged
        // dimensions still hits the resize syscall and some stacks repaint.
        let mut resize_needed = target_columns != current.columns;
        if stty_rows > 0 && target_rows != current.rows {
            resize_needed = true;
        }
        if resize_needed {
            if let Err(err) = self.set_size(&pane.tty, target_columns, stty_rows).await {
                match previous {
                    Some(prev) => {
                        pane.leases.insert(client_id.to_owned(), prev);
                    }
                    None => {
                        pane.leases.remove(client_id);
                    }
                }
                return Err(err);
            }
            pane.resized_at = Some(now);
        }
        pane.applied_columns = target_columns;
        pane.applied_rows = target_rows;
        Ok((target_columns, target_rows))
    }

    /// `Manager.Release` — lapse into the grace window instead of restoring
    /// immediately.
    pub(crate) async fn release(&self, client_id: &str, pane_id: &str) -> Result<(), String> {
        if client_id.is_empty() || pane_id.is_empty() {
            return Err(ERR_INVALID_LEASE.to_owned());
        }
        let inner = &*self.inner;
        let mut state = inner.state.lock().await;
        if inner.closed.load(Ordering::SeqCst) {
            return Ok(());
        }
        let Some(pane) = state.panes.get_mut(pane_id) else {
            return Ok(());
        };
        let Some(mut lease) = pane.leases.get(client_id).copied() else {
            return Ok(());
        };
        let now = (inner.now)();
        let lapse = now + inner.grace;
        if lease.expires_at > lapse {
            lease.expires_at = lapse;
            pane.leases.insert(client_id.to_owned(), lease);
        }
        if lease.expires_at > now {
            return Ok(());
        }
        pane.leases.remove(client_id);
        self.reconcile(&mut state, pane_id).await
    }

    /// `Manager.ReleaseClient` — disconnect path: the client's leases vanish
    /// immediately (no grace) and every affected pane reconciles.
    pub(crate) async fn release_client(&self, client_id: &str) -> Result<(), String> {
        if client_id.is_empty() {
            return Err(ERR_INVALID_LEASE.to_owned());
        }
        let inner = &*self.inner;
        let mut state = inner.state.lock().await;
        if inner.closed.load(Ordering::SeqCst) {
            return Ok(());
        }
        let mut result: Vec<String> = Vec::new();
        let pane_ids: Vec<String> = state.panes.keys().cloned().collect();
        for pane_id in pane_ids {
            {
                let Some(pane) = state.panes.get_mut(&pane_id) else {
                    continue;
                };
                let owned = pane.leases.remove(client_id).is_some();
                let (_, active) = minimum_columns(&pane.leases);
                if !owned && active {
                    continue;
                }
            }
            if let Err(err) = self.reconcile(&mut state, &pane_id).await {
                result.push(format!("pane {pane_id}: {err}"));
            }
        }
        if result.is_empty() {
            Ok(())
        } else {
            Err(result.join("; "))
        }
    }

    /// `Manager.SweepExpired`.
    pub(crate) async fn sweep_expired(&self) -> Result<(), String> {
        let inner = &*self.inner;
        let mut state = inner.state.lock().await;
        if inner.closed.load(Ordering::SeqCst) {
            return Ok(());
        }
        let now = (inner.now)();
        let mut result: Vec<String> = Vec::new();
        let pane_ids: Vec<String> = state.panes.keys().cloned().collect();
        for pane_id in pane_ids {
            {
                let Some(pane) = state.panes.get_mut(&pane_id) else {
                    continue;
                };
                let removed = remove_expired(pane, now);
                let (target, active) = minimum_columns(&pane.leases);
                let mut target_rows = minimum_rows(&pane.leases);
                if target_rows == 0 {
                    target_rows = pane.baseline_rows;
                }
                if !removed
                    && active
                    && pane.applied_columns == target
                    && pane.applied_rows == target_rows
                {
                    continue;
                }
            }
            if let Err(err) = self.reconcile(&mut state, &pane_id).await {
                result.push(format!("pane {pane_id}: {err}"));
            }
        }
        if result.is_empty() {
            Ok(())
        } else {
            Err(result.join("; "))
        }
    }

    /// `Manager.ActiveColumns` — the narrowest unexpired lease for a pane.
    pub(crate) async fn active_columns(&self, pane_id: &str) -> Option<i64> {
        let inner = &*self.inner;
        let state = inner.state.lock().await;
        if inner.closed.load(Ordering::SeqCst) {
            return None;
        }
        let pane = state.panes.get(pane_id)?;
        let now = (inner.now)();
        let mut minimum = 0i64;
        for lease in pane.leases.values() {
            if lease.expires_at <= now {
                continue;
            }
            if minimum == 0 || lease.columns < minimum {
                minimum = lease.columns;
            }
        }
        (minimum != 0).then_some(minimum)
    }

    /// `Manager.ActiveRows` — smallest unexpired row lease, else the
    /// baseline height while any lease is active.
    pub(crate) async fn active_rows(&self, pane_id: &str) -> Option<i64> {
        let inner = &*self.inner;
        let state = inner.state.lock().await;
        if inner.closed.load(Ordering::SeqCst) {
            return None;
        }
        let pane = state.panes.get(pane_id)?;
        let now = (inner.now)();
        let mut active = false;
        let mut minimum = 0i64;
        for lease in pane.leases.values() {
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
            pane.baseline_rows
        } else {
            minimum
        })
    }

    /// `Manager.ResizedWithin` — a lease actually changed the pane's width
    /// inside `window` (renewals that keep the same columns do not count).
    pub(crate) async fn resized_within(&self, pane_id: &str, window: Duration) -> bool {
        let inner = &*self.inner;
        let state = inner.state.lock().await;
        if inner.closed.load(Ordering::SeqCst) {
            return false;
        }
        let Some(pane) = state.panes.get(pane_id) else {
            return false;
        };
        let Some(resized_at) = pane.resized_at else {
            return false;
        };
        (inner.now)().saturating_duration_since(resized_at) < window
    }

    /// `Manager.Shutdown` — restore every pane; used on relay teardown.
    #[allow(dead_code)] // invoked when the relay gains a shutdown path
    pub(crate) async fn shutdown(&self) -> Result<(), String> {
        let inner = &*self.inner;
        let mut state = inner.state.lock().await;
        inner.closed.store(true, Ordering::SeqCst);
        let mut result: Vec<String> = Vec::new();
        let pane_ids: Vec<String> = state.panes.keys().cloned().collect();
        for pane_id in pane_ids {
            if let Some(pane) = state.panes.get_mut(&pane_id) {
                pane.leases.clear();
            }
            if let Err(err) = self.reconcile(&mut state, &pane_id).await {
                result.push(format!("pane {pane_id}: {err}"));
            }
        }
        if result.is_empty() {
            Ok(())
        } else {
            Err(result.join("; "))
        }
    }

    // ── internals ────────────────────────────────────────────────────────

    /// `resolvePane` — process info → foreground pid → `ps` tty → `stty`
    /// size → baseline pane state.
    async fn resolve_pane(&self, pane_id: &str) -> Result<PaneState, &'static str> {
        let info = self
            .inner
            .provider
            .pane_process_info(pane_id)
            .await
            .map_err(|_| ERR_PROCESS_UNAVAILABLE)?;
        let pid = foreground_pid(&info, pane_id)?;
        let output = self
            .inner
            .runner
            .output(
                "ps",
                &["-o".into(), "tty=".into(), "-p".into(), pid.to_string()],
            )
            .await
            .map_err(|_| ERR_TTY_UNAVAILABLE)?;
        let tty = tty_path(&output)?;
        let size = self.read_size(&tty).await?;
        Ok(PaneState {
            tty,
            baseline_rows: size.rows,
            baseline_columns: size.columns,
            applied_rows: size.rows,
            applied_columns: size.columns,
            resized_at: None,
            leases: HashMap::new(),
        })
    }

    /// `readSize` — `stty -F <tty> size` → "rows columns".
    async fn read_size(&self, tty: &str) -> Result<TerminalSize, &'static str> {
        let flag = stty_device_flag()?;
        let output = self
            .inner
            .runner
            .output("stty", &[flag.into(), tty.to_owned(), "size".into()])
            .await
            .map_err(|_| ERR_SIZE_UNAVAILABLE)?;
        let text = String::from_utf8_lossy(&output);
        let fields: Vec<&str> = text.split_whitespace().collect();
        if fields.len() != 2 {
            return Err(ERR_SIZE_UNAVAILABLE);
        }
        let rows = fields[0].parse::<i64>().unwrap_or(0);
        let columns = fields[1].parse::<i64>().unwrap_or(0);
        if rows < 1 || columns < 1 {
            return Err(ERR_SIZE_UNAVAILABLE);
        }
        Ok(TerminalSize { rows, columns })
    }

    /// `setSize` — `stty -F <tty> cols C [rows R]`; `rows == 0` leaves the
    /// height alone.
    async fn set_size(&self, tty: &str, columns: i64, rows: i64) -> Result<(), &'static str> {
        let flag = stty_device_flag()?;
        let mut args = vec![
            flag.into(),
            tty.to_owned(),
            "cols".into(),
            columns.to_string(),
        ];
        if rows > 0 {
            args.push("rows".into());
            args.push(rows.to_string());
        }
        self.inner
            .runner
            .output("stty", &args)
            .await
            .map_err(|_| ERR_RESIZE_FAILED)?;
        Ok(())
    }

    /// `reconcile` — apply the minimum active lease or `restore` the
    /// baseline (which deletes the pane entry, like the oracle's `delete`).
    async fn reconcile(&self, state: &mut LeaseState, pane_id: &str) -> Result<(), String> {
        enum Step {
            /// No active leases: apply the baseline and drop the entry.
            Restore {
                tty: String,
                columns: i64,
                stty_rows: i64,
                baseline_rows: i64,
            },
            /// Apply the narrowest lease.
            Apply {
                tty: String,
                columns: i64,
                stty_rows: i64,
                applied_rows: i64,
            },
        }
        let step = {
            let Some(pane) = state.panes.get_mut(pane_id) else {
                return Ok(());
            };
            let (target, active) = minimum_columns(&pane.leases);
            if !active {
                // The height was never leased away → leave the tty's rows
                // alone (`stty_rows == 0`).
                let stty_rows = if pane.applied_rows == pane.baseline_rows {
                    0
                } else {
                    pane.baseline_rows
                };
                Step::Restore {
                    tty: pane.tty.clone(),
                    columns: pane.baseline_columns,
                    stty_rows,
                    baseline_rows: pane.baseline_rows,
                }
            } else {
                let mut target_rows = minimum_rows(&pane.leases);
                let constrained_rows = target_rows > 0;
                if !constrained_rows {
                    target_rows = pane.baseline_rows;
                }
                if target == pane.applied_columns && target_rows == pane.applied_rows {
                    return Ok(());
                }
                let stty_rows = if !constrained_rows && target_rows == pane.applied_rows {
                    0
                } else {
                    target_rows
                };
                Step::Apply {
                    tty: pane.tty.clone(),
                    columns: target,
                    stty_rows,
                    applied_rows: target_rows,
                }
            }
        };
        match step {
            Step::Restore {
                tty,
                columns,
                stty_rows,
                baseline_rows,
            } => {
                self.set_size(&tty, columns, stty_rows)
                    .await
                    .map_err(str::to_owned)?;
                if let Some(pane) = state.panes.get_mut(pane_id) {
                    pane.applied_columns = columns;
                    pane.applied_rows = baseline_rows;
                }
                state.panes.remove(pane_id);
            }
            Step::Apply {
                tty,
                columns,
                stty_rows,
                applied_rows,
            } => {
                self.set_size(&tty, columns, stty_rows)
                    .await
                    .map_err(str::to_owned)?;
                if let Some(pane) = state.panes.get_mut(pane_id) {
                    pane.resized_at = Some((self.inner.now)());
                    pane.applied_columns = columns;
                    pane.applied_rows = applied_rows;
                }
            }
        }
        Ok(())
    }
}

/// `foregroundPID` — the process group leader, else the first live pid.
fn foreground_pid(info: &PaneProcessInfo, pane_id: &str) -> Result<i64, &'static str> {
    if info.pane_id.is_empty()
        || info.pane_id != pane_id
        || info.foreground_process_group_id <= 0
        || info.foreground_processes.is_empty()
    {
        return Err(ERR_PROCESS_UNAVAILABLE);
    }
    for process in &info.foreground_processes {
        if process.pid == info.foreground_process_group_id {
            return Ok(process.pid);
        }
    }
    for process in &info.foreground_processes {
        if process.pid > 0 {
            return Ok(process.pid);
        }
    }
    Err(ERR_PROCESS_UNAVAILABLE)
}

/// `sttyDeviceFlag` — linux `-F`, macOS `-f`.
fn stty_device_flag() -> Result<&'static str, &'static str> {
    #[cfg(target_os = "linux")]
    {
        Ok("-F")
    }
    #[cfg(target_os = "macos")]
    {
        Ok("-f")
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err("Pane size leasing is unsupported on this platform")
    }
}

/// `ttyPath` — `ps` prints `ttys003`/`pts/4`; reject non-tty answers and
/// anything that escapes `/dev`.
fn tty_path(output: &[u8]) -> Result<String, &'static str> {
    let text = String::from_utf8_lossy(output);
    let fields: Vec<&str> = text.split_whitespace().collect();
    if fields.len() != 1 || fields[0] == "?" || fields[0] == "??" || fields[0] == "-" {
        return Err(ERR_TTY_UNAVAILABLE);
    }
    let tty = fields[0].strip_prefix("/dev/").unwrap_or(fields[0]);
    if tty.is_empty() || tty.starts_with('/') {
        return Err(ERR_TTY_UNAVAILABLE);
    }
    let clean = std::path::Path::new(tty);
    if clean
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(ERR_TTY_UNAVAILABLE);
    }
    let joined = std::path::Path::new("/dev").join(clean);
    Ok(joined.to_string_lossy().into_owned())
}

fn minimum_columns(leases: &HashMap<String, Lease>) -> (i64, bool) {
    let mut minimum = 0i64;
    for lease in leases.values() {
        if minimum == 0 || lease.columns < minimum {
            minimum = lease.columns;
        }
    }
    (minimum, minimum != 0)
}

fn minimum_rows(leases: &HashMap<String, Lease>) -> i64 {
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

fn remove_expired(pane: &mut PaneState, now: Instant) -> bool {
    let before = pane.leases.len();
    pane.leases.retain(|_, lease| lease.expires_at > now);
    pane.leases.len() != before
}

// ── wire surface ──────────────────────────────────────────────────────────

/// `lease_pane_size` — `Acquire`, bare `command_result` (no receipt, matching
/// the oracle's `sendCommandResult`).
pub(crate) async fn lease_pane_size(
    ctx: ActionContext,
    owner_alive: &tokio_util::sync::CancellationToken,
    request_id: &str,
    message: &Inbound,
) -> Outbound {
    match ctx
        .leases
        .acquire(
            owner_alive,
            &ctx.client_id,
            &message.pane_id,
            message.columns,
            message.rows,
        )
        .await
    {
        Err(error) => lease_result(request_id, &message.pane_id, false, error, None),
        Ok((columns, rows)) => lease_result(
            request_id,
            &message.pane_id,
            true,
            "",
            Some(serde_json::json!({ "columns": columns, "rows": rows })),
        ),
    }
}

/// `release_pane_size` — `Release`, bare `command_result`.
pub(crate) async fn release_pane_size(
    ctx: ActionContext,
    request_id: &str,
    message: &Inbound,
) -> Outbound {
    match ctx.leases.release(&ctx.client_id, &message.pane_id).await {
        Err(error) => lease_result(request_id, &message.pane_id, false, &error, None),
        Ok(()) => lease_result(request_id, &message.pane_id, true, "", None),
    }
}

fn lease_result(
    request_id: &str,
    pane_id: &str,
    ok: bool,
    error: &str,
    data: Option<serde_json::Value>,
) -> Outbound {
    use lerdr_core::json::{MaybeNull, RawJson};
    Outbound::CommandResult(CommandResultMessage {
        r#type: "command_result".to_owned(),
        request_id: (!request_id.is_empty()).then(|| request_id.to_owned()),
        action: Some("lease_pane_size".to_owned()),
        ok: Some(ok),
        phase: Some(if ok { "completed" } else { "failed" }.to_owned()),
        error: Some(error.to_owned()),
        pane_id: Some(pane_id.to_owned()),
        data: data.and_then(|v| {
            serde_json::value::to_raw_value(&v)
                .ok()
                .map(|raw| MaybeNull::Value(RawJson(raw)))
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    struct StubProvider {
        info: PaneProcessInfo,
    }

    impl ProcessInfoProvider for StubProvider {
        fn pane_process_info<'a>(
            &'a self,
            pane_id: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<PaneProcessInfo, ()>> + Send + 'a>> {
            let mut info = PaneProcessInfo {
                pane_id: pane_id.to_owned(),
                foreground_process_group_id: self.info.foreground_process_group_id,
                foreground_processes: Vec::new(),
            };
            info.foreground_processes = self
                .info
                .foreground_processes
                .iter()
                .map(|p| PaneProcess { pid: p.pid })
                .collect();
            Box::pin(async move { Ok(info) })
        }
    }

    struct StubRunner {
        calls: StdMutex<Vec<(String, Vec<String>)>>,
        ps_output: &'static str,
        size_output: &'static str,
        fail_set: bool,
    }

    impl ExecRunner for StubRunner {
        fn output<'a>(
            &'a self,
            name: &'a str,
            args: &'a [String],
        ) -> Pin<Box<dyn Future<Output = io::Result<Vec<u8>>> + Send + 'a>> {
            self.calls
                .lock()
                .unwrap()
                .push((name.to_owned(), args.to_vec()));
            let result: io::Result<Vec<u8>> = match name {
                "ps" => Ok(self.ps_output.as_bytes().to_vec()),
                "stty" if args.last().map(String::as_str) == Some("size") => {
                    Ok(self.size_output.as_bytes().to_vec())
                }
                "stty" if self.fail_set => Err(io::Error::other("stty failed")),
                "stty" => Ok(Vec::new()),
                _ => Err(io::Error::new(io::ErrorKind::NotFound, name.to_owned())),
            };
            Box::pin(async move { result })
        }
    }

    fn test_leases() -> (Leases, Arc<StubRunner>) {
        let runner = Arc::new(StubRunner {
            calls: StdMutex::new(Vec::new()),
            ps_output: "pts/4\n",
            size_output: "24 80\n",
            fail_set: false,
        });
        let provider = Arc::new(StubProvider {
            info: PaneProcessInfo {
                pane_id: "p1".to_owned(),
                foreground_process_group_id: 42,
                foreground_processes: vec![PaneProcess { pid: 42 }],
            },
        });
        (
            Leases::with_parts(
                provider,
                runner.clone(),
                Duration::from_secs(120),
                Duration::from_secs(10),
                Instant::now,
            ),
            runner,
        )
    }

    #[tokio::test]
    async fn acquire_resizes_to_minimum_and_renews() {
        let (leases, runner) = test_leases();
        let alive = tokio_util::sync::CancellationToken::new();
        // Baseline is 80x24 — a wider lease still narrows the pane.
        let (cols, rows) = leases
            .acquire(&alive, "c1", "p1", 70, 0)
            .await
            .expect("acquire");
        assert_eq!((cols, rows), (70, 24));
        // Second, narrower client wins; baseline height is preserved.
        let (cols, rows) = leases
            .acquire(&alive, "c2", "p1", 60, 0)
            .await
            .expect("acquire c2");
        assert_eq!((cols, rows), (60, 24));
        let calls = runner.calls.lock().unwrap();
        let sets: Vec<_> = calls
            .iter()
            .filter(|(n, a)| n == "stty" && a.iter().any(|x| x == "cols"))
            .collect();
        assert_eq!(sets.len(), 2);
        assert!(sets[0].1.contains(&"70".to_owned()));
        assert!(sets[1].1.contains(&"60".to_owned()));
    }

    #[tokio::test]
    async fn release_lapses_then_restores() {
        let (leases, _) = test_leases();
        let alive = tokio_util::sync::CancellationToken::new();
        leases
            .acquire(&alive, "c1", "p1", 60, 0)
            .await
            .expect("acquire");
        // Release keeps the lease (grace) — still active.
        leases.release("c1", "p1").await.expect("release");
        assert_eq!(leases.active_columns("p1").await, Some(60));
    }

    #[tokio::test]
    async fn release_client_restores_immediately() {
        let (leases, runner) = test_leases();
        let alive = tokio_util::sync::CancellationToken::new();
        leases
            .acquire(&alive, "c1", "p1", 60, 0)
            .await
            .expect("acquire");
        leases.release_client("c1").await.expect("release_client");
        assert_eq!(leases.active_columns("p1").await, None);
        let calls = runner.calls.lock().unwrap();
        // Last stty restores baseline 80.
        let sets: Vec<_> = calls
            .iter()
            .filter(|(n, a)| n == "stty" && a.iter().any(|x| x == "cols"))
            .collect();
        assert!(sets.last().unwrap().1.contains(&"80".to_owned()));
    }

    #[tokio::test]
    async fn validation_rejects_bad_dimensions() {
        let (leases, _) = test_leases();
        let alive = tokio_util::sync::CancellationToken::new();
        assert_eq!(
            leases.acquire(&alive, "c1", "p1", 10, 0).await,
            Err(ERR_INVALID_COLUMNS)
        );
        assert_eq!(
            leases.acquire(&alive, "c1", "p1", 80, 5).await,
            Err(ERR_INVALID_ROWS)
        );
        assert_eq!(
            leases.acquire(&alive, "c1", "", 80, 0).await,
            Err(ERR_INVALID_LEASE)
        );
    }

    #[tokio::test]
    async fn cancelled_owner_fails_before_dispatch() {
        let (leases, _) = test_leases();
        let alive = tokio_util::sync::CancellationToken::new();
        alive.cancel();
        assert_eq!(
            leases.acquire(&alive, "c1", "p1", 80, 0).await,
            Err(ERR_OWNER_GONE)
        );
    }

    #[test]
    fn tty_path_sanitizes() {
        assert_eq!(tty_path(b"pts/4\n").unwrap(), "/dev/pts/4");
        assert_eq!(tty_path(b"ttys003").unwrap(), "/dev/ttys003");
        assert_eq!(tty_path(b"/dev/pts/4").unwrap(), "/dev/pts/4");
        assert!(tty_path(b"?").is_err());
        assert!(tty_path(b"??").is_err());
        assert!(tty_path(b"-").is_err());
        assert!(tty_path(b"../etc/passwd").is_err());
        assert!(tty_path(b"pts/4 extra").is_err());
    }
}
