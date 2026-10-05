//! Pane-size leasing through Herdr's native terminal controller.
//!
//! One persistent controller per leased pane owns both the VT grid and PTY
//! dimensions. Read-only process/TTY resolution captures the height for
//! width-only clients; releasing the controller restores host layout ownership.
//! The narrowest active request wins, release keeps a 10s grace window, and
//! the 1s sweeper expires abandoned leases.
//!
//! Wire surface: `lease_pane_size`/`release_pane_size` answer with a bare
//! `command_result` (the retired implementation emits no `action_receipt` for them), and
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
/// Deadline for baseline resolution and native controller operations.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(3);
/// `paneResizeSettleWindow` — `read_pane` flags `resize_settling` inside it
/// (`pane_watch.go:29` — 3 s; observed up to ~2 s for omp under load).
pub(crate) const RESIZE_SETTLE_WINDOW: Duration = Duration::from_secs(3);

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
    controller: Option<Box<dyn PaneController>>,
    baseline_rows: i64,
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

/// Read-only `ps`/`stty size` execution for width-only baseline compatibility.
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
        Box::pin(async move {
            let child = tokio::process::Command::new(name)
                .args(args)
                .kill_on_drop(true)
                .output();
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

type ControlFuture<'a, T> = Pin<Box<dyn Future<Output = io::Result<T>> + Send + 'a>>;

trait PaneController: Send {
    fn is_live(&self) -> bool;
    fn resize(&mut self, columns: u16, rows: u16) -> ControlFuture<'_, ()>;
    fn release(self: Box<Self>) -> ControlFuture<'static, ()>;
}

impl PaneController for lerdr_herdr::control::ControlStream {
    fn is_live(&self) -> bool {
        self.is_live()
    }

    fn resize(&mut self, columns: u16, rows: u16) -> ControlFuture<'_, ()> {
        Box::pin(self.resize(columns, rows))
    }

    fn release(self: Box<Self>) -> ControlFuture<'static, ()> {
        Box::pin((*self).release())
    }
}

trait ControlProvider: Send + Sync {
    fn spawn<'a>(
        &'a self,
        pane_id: &'a str,
        columns: u16,
        rows: u16,
    ) -> ControlFuture<'a, Box<dyn PaneController>>;
}

struct NativeControlProvider {
    bin: std::path::PathBuf,
    socket: Option<std::path::PathBuf>,
}

impl ControlProvider for NativeControlProvider {
    fn spawn<'a>(
        &'a self,
        pane_id: &'a str,
        columns: u16,
        rows: u16,
    ) -> ControlFuture<'a, Box<dyn PaneController>> {
        Box::pin(async move {
            let controller = lerdr_herdr::control::ControlStream::spawn_sized(
                &self.bin,
                pane_id,
                self.socket.as_deref(),
                columns,
                rows,
            )
            .await?;
            Ok(Box::new(controller) as Box<dyn PaneController>)
        })
    }
}

impl PaneState {
    fn is_live(&self) -> bool {
        self.controller
            .as_ref()
            .is_some_and(|control| control.is_live())
    }
}

struct LeaseInner {
    state: Mutex<LeaseState>,
    provider: SharedProvider,
    runner: SharedRunner,
    controls: Arc<dyn ControlProvider>,
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
                controls: Arc::new(NativeControlProvider {
                    bin: client.resolved_herdr_bin(),
                    socket: client.socket_path_hint(),
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
        controls: Arc<dyn ControlProvider>,
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
                controls,
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
                    _ = cancel.cancelled() => break,
                    _ = ticker.tick() => {
                        if let Err(err) = leases.sweep_expired().await {
                            tracing::warn!(error = %err, "pane size lease expiry sweep failed");
                        }
                    }
                }
            }
            if let Err(err) = leases.shutdown().await {
                tracing::warn!(error = %err, "native pane controller shutdown failed");
            }
        });
    }

    /// Validate, resolve the baseline, and confirm the native controller's
    /// minimum effective geometry before recording success.
    ///
    /// `owner_alive` is the client connection's cancellation token — the
    /// retired implementation checks `ctx.Err()` before and after pane resolution.
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
        if !state.panes.contains_key(pane_id) {
            let pane = tokio::select! {
                _ = owner_alive.cancelled() => return Err(ERR_OWNER_GONE),
                result = self.resolve_pane(pane_id) => result?,
            };
            state.panes.insert(pane_id.to_owned(), pane);
        } else {
            remove_expired(state.panes.get_mut(pane_id).expect("checked above"), now);
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
        let target_rows = match minimum_rows(&pane.leases) {
            0 => pane.baseline_rows,
            rows => rows,
        };
        let result = tokio::select! {
            _ = owner_alive.cancelled() => Err(ERR_OWNER_GONE),
            result = self.apply_size(pane, pane_id, target_columns, target_rows) => result,
        };
        let result = if owner_alive.is_cancelled() {
            Err(ERR_OWNER_GONE)
        } else {
            result
        };
        if let Err(err) = result {
            match previous {
                Some(previous) => {
                    pane.leases.insert(client_id.to_owned(), previous);
                }
                None => {
                    pane.leases.remove(client_id);
                }
            }
            // A cancelled/failed resize may have reached Herdr. Release this
            // controller rather than trusting its previous confirmed size.
            let _ = self.release_control(pane).await;
            return Err(err);
        }
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
                    && pane.is_live()
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

    /// Atomic geometry confirmed by the native controller. A grace-held or
    /// expired-but-unswept controller remains authoritative until released.
    pub(crate) async fn capture_size(&self, pane_id: &str) -> Option<(u16, u16)> {
        let inner = &*self.inner;
        let state = inner.state.lock().await;
        if inner.closed.load(Ordering::SeqCst) {
            return None;
        }
        let pane = state.panes.get(pane_id)?;
        if !pane.is_live() {
            return None;
        }
        Some((
            u16::try_from(pane.applied_columns).ok()?,
            u16::try_from(pane.applied_rows).ok()?,
        ))
    }

    /// `Manager.ActiveColumns` — the narrowest unexpired lease for a pane.
    pub(crate) async fn active_columns(&self, pane_id: &str) -> Option<i64> {
        let inner = &*self.inner;
        let state = inner.state.lock().await;
        if inner.closed.load(Ordering::SeqCst) {
            return None;
        }
        let pane = state.panes.get(pane_id)?;
        if !pane.is_live() {
            return None;
        }
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

    /// Smallest unexpired row lease, else the baseline height while any
    /// lease is active. Capture geometry is queried atomically separately.
    pub(crate) async fn active_rows(&self, pane_id: &str) -> Option<i64> {
        let inner = &*self.inner;
        let state = inner.state.lock().await;
        if inner.closed.load(Ordering::SeqCst) {
            return None;
        }
        let pane = state.panes.get(pane_id)?;
        if !pane.is_live() {
            return None;
        }
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

    /// Release only controllers owned by this manager on relay teardown.
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

    /// Read the initial terminal height for width-only compatibility. Native
    /// controller release, not this baseline, restores desktop geometry.
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
            controller: None,
            baseline_rows: size.rows,
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

    /// Ensure there is one live controller at the confirmed effective size.
    async fn apply_size(
        &self,
        pane: &mut PaneState,
        pane_id: &str,
        columns: i64,
        rows: i64,
    ) -> Result<(), &'static str> {
        let columns_u16 = u16::try_from(columns).map_err(|_| ERR_RESIZE_FAILED)?;
        let rows_u16 = u16::try_from(rows).map_err(|_| ERR_RESIZE_FAILED)?;
        if pane.is_live() && pane.applied_columns == columns && pane.applied_rows == rows {
            return Ok(());
        }
        if !pane.is_live() {
            let _ = self.release_control(pane).await;
            let controller = tokio::time::timeout(
                COMMAND_TIMEOUT,
                self.inner.controls.spawn(pane_id, columns_u16, rows_u16),
            )
            .await
            .map_err(|_| ERR_RESIZE_FAILED)?
            .map_err(|_| ERR_RESIZE_FAILED)?;
            pane.controller = Some(controller);
        } else {
            let result = tokio::time::timeout(
                COMMAND_TIMEOUT,
                pane.controller
                    .as_mut()
                    .expect("live controller")
                    .resize(columns_u16, rows_u16),
            )
            .await;
            if !matches!(result, Ok(Ok(()))) {
                let _ = self.release_control(pane).await;
                return Err(ERR_RESIZE_FAILED);
            }
        }
        if !pane.is_live() {
            let _ = self.release_control(pane).await;
            return Err(ERR_RESIZE_FAILED);
        }
        // A fresh claim can rebuild the VT grid even when the read-only PTY
        // baseline already matched. Only unchanged live renewals skip above.
        pane.resized_at = Some((self.inner.now)());
        pane.applied_columns = columns;
        pane.applied_rows = rows;
        Ok(())
    }

    /// Taking the controller before awaiting prevents timed-out or failed
    /// operations from leaving an apparently reusable ownership handle.
    async fn release_control(&self, pane: &mut PaneState) -> Result<(), &'static str> {
        let Some(controller) = pane.controller.take() else {
            return Ok(());
        };
        match tokio::time::timeout(COMMAND_TIMEOUT, controller.release()).await {
            Ok(Ok(())) => Ok(()),
            _ => Err(ERR_RESIZE_FAILED),
        }
    }

    /// Apply the remaining minimum or release native ownership entirely.
    async fn reconcile(&self, state: &mut LeaseState, pane_id: &str) -> Result<(), String> {
        let Some(pane) = state.panes.get_mut(pane_id) else {
            return Ok(());
        };
        let (columns, active) = minimum_columns(&pane.leases);
        if !active {
            let result = self.release_control(pane).await.map_err(str::to_owned);
            state.panes.remove(pane_id);
            return result;
        }
        let rows = match minimum_rows(&pane.leases) {
            0 => pane.baseline_rows,
            rows => rows,
        };
        self.apply_size(pane, pane_id, columns, rows)
            .await
            .map_err(str::to_owned)
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
/// the retired implementation's `sendCommandResult`).
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

    struct StubRunner;

    impl ExecRunner for StubRunner {
        fn output<'a>(
            &'a self,
            name: &'a str,
            _args: &'a [String],
        ) -> Pin<Box<dyn Future<Output = io::Result<Vec<u8>>> + Send + 'a>> {
            Box::pin(async move {
                match name {
                    "ps" => Ok(b"pts/4\n".to_vec()),
                    "stty" => Ok(b"24 80\n".to_vec()),
                    _ => Err(io::Error::new(io::ErrorKind::NotFound, name.to_owned())),
                }
            })
        }
    }

    #[derive(Debug, PartialEq)]
    enum ControlEvent {
        Claimed(String, u16, u16),
        Resized(String, u16, u16),
        Released(String),
    }

    #[derive(Default)]
    struct StubControls {
        events: Mutex<Vec<ControlEvent>>,
        refuse_claim: AtomicBool,
        fail_resize: AtomicBool,
        live: AtomicBool,
        cancel_claim: Mutex<Option<tokio_util::sync::CancellationToken>>,
    }

    struct StubController {
        controls: Arc<StubControls>,
        pane_id: String,
    }

    impl PaneController for StubController {
        fn is_live(&self) -> bool {
            self.controls.live.load(Ordering::SeqCst)
        }

        fn resize(&mut self, columns: u16, rows: u16) -> ControlFuture<'_, ()> {
            Box::pin(async move {
                if self.controls.fail_resize.load(Ordering::SeqCst) {
                    return Err(io::Error::other("resize not confirmed"));
                }
                self.controls
                    .events
                    .lock()
                    .await
                    .push(ControlEvent::Resized(self.pane_id.clone(), columns, rows));
                Ok(())
            })
        }

        fn release(self: Box<Self>) -> ControlFuture<'static, ()> {
            Box::pin(async move {
                self.controls
                    .events
                    .lock()
                    .await
                    .push(ControlEvent::Released(self.pane_id));
                Ok(())
            })
        }
    }

    impl ControlProvider for Arc<StubControls> {
        fn spawn<'a>(
            &'a self,
            pane_id: &'a str,
            columns: u16,
            rows: u16,
        ) -> ControlFuture<'a, Box<dyn PaneController>> {
            Box::pin(async move {
                if self.refuse_claim.load(Ordering::SeqCst) {
                    return Err(io::Error::other("external controller is busy"));
                }
                self.live.store(true, Ordering::SeqCst);
                self.events.lock().await.push(ControlEvent::Claimed(
                    pane_id.to_owned(),
                    columns,
                    rows,
                ));
                if let Some(token) = self.cancel_claim.lock().await.take() {
                    token.cancel();
                }
                Ok(Box::new(StubController {
                    controls: self.clone(),
                    pane_id: pane_id.to_owned(),
                }) as Box<dyn PaneController>)
            })
        }
    }

    fn virtual_now() -> Instant {
        tokio::time::Instant::now().into_std()
    }

    fn test_leases() -> (Leases, Arc<StubControls>) {
        let controls = Arc::new(StubControls::default());
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
                Arc::new(StubRunner),
                Arc::new(controls.clone()),
                LEASE_TTL,
                RELEASE_GRACE,
                virtual_now,
            ),
            controls,
        )
    }

    #[tokio::test(start_paused = true)]
    async fn acquire_resizes_to_minimum_and_renews_without_repainting() {
        let (leases, controls) = test_leases();
        let alive = tokio_util::sync::CancellationToken::new();
        assert_eq!(
            leases.acquire(&alive, "c1", "p1", 70, 0).await,
            Ok((70, 24))
        );
        assert_eq!(
            leases.acquire(&alive, "c2", "p1", 60, 0).await,
            Ok((60, 24))
        );
        assert_eq!(leases.active_rows("p1").await, Some(24));
        assert_eq!(leases.capture_size("p1").await, Some((60, 24)));
        let events = controls.events.lock().await.len();
        tokio::time::advance(Duration::from_secs(4)).await;
        assert_eq!(
            leases.acquire(&alive, "c2", "p1", 60, 0).await,
            Ok((60, 24))
        );
        assert_eq!(
            leases.acquire(&alive, "c3", "p1", 90, 0).await,
            Ok((60, 24))
        );
        assert_eq!(controls.events.lock().await.len(), events);
        assert!(!leases.resized_within("p1", RESIZE_SETTLE_WINDOW).await);
        assert_eq!(
            *controls.events.lock().await,
            vec![
                ControlEvent::Claimed("p1".into(), 70, 24),
                ControlEvent::Resized("p1".into(), 60, 24),
            ],
        );
    }

    #[tokio::test(start_paused = true)]
    async fn native_claim_marks_settling_even_when_pty_baseline_matches() {
        let (leases, controls) = test_leases();
        let alive = tokio_util::sync::CancellationToken::new();
        leases.acquire(&alive, "c1", "p1", 80, 24).await.unwrap();
        assert!(leases.resized_within("p1", RESIZE_SETTLE_WINDOW).await);
        tokio::time::advance(RESIZE_SETTLE_WINDOW).await;
        leases.acquire(&alive, "c1", "p1", 80, 24).await.unwrap();
        assert!(!leases.resized_within("p1", RESIZE_SETTLE_WINDOW).await);
        assert_eq!(controls.events.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn phone_height_changes_and_mixed_leases_restore_width_only_height() {
        let (leases, controls) = test_leases();
        let alive = tokio_util::sync::CancellationToken::new();
        assert_eq!(
            leases.acquire(&alive, "phone", "p1", 57, 36).await,
            Ok((57, 36))
        );
        assert_eq!(
            leases.acquire(&alive, "phone", "p1", 57, 12).await,
            Ok((57, 12))
        );
        assert_eq!(
            leases.acquire(&alive, "width", "p1", 70, 0).await,
            Ok((57, 12))
        );
        assert_eq!(
            leases.acquire(&alive, "other", "p1", 65, 20).await,
            Ok((57, 12))
        );
        leases.release_client("phone").await.unwrap();
        assert_eq!(leases.active_columns("p1").await, Some(65));
        assert_eq!(leases.active_rows("p1").await, Some(20));
        leases.release_client("other").await.unwrap();
        assert_eq!(leases.active_rows("p1").await, Some(24));
        assert_eq!(leases.capture_size("p1").await, Some((70, 24)));
        assert_eq!(
            *controls.events.lock().await,
            vec![
                ControlEvent::Claimed("p1".into(), 57, 36),
                ControlEvent::Resized("p1".into(), 57, 12),
                ControlEvent::Resized("p1".into(), 65, 20),
                ControlEvent::Resized("p1".into(), 70, 24),
            ],
        );
    }

    #[tokio::test(start_paused = true)]
    async fn release_grace_and_ttl_release_owned_controller() {
        let (leases, controls) = test_leases();
        let alive = tokio_util::sync::CancellationToken::new();
        leases.acquire(&alive, "c1", "p1", 60, 0).await.unwrap();
        leases.release("c1", "p1").await.unwrap();
        assert_eq!(leases.active_columns("p1").await, Some(60));
        assert_eq!(leases.capture_size("p1").await, Some((60, 24)));
        tokio::time::advance(RELEASE_GRACE - Duration::from_secs(1)).await;
        leases.sweep_expired().await.unwrap();
        assert_eq!(controls.events.lock().await.len(), 1);
        tokio::time::advance(Duration::from_secs(1)).await;
        leases.sweep_expired().await.unwrap();
        assert_eq!(leases.active_columns("p1").await, None);
        assert_eq!(leases.capture_size("p1").await, None);
        assert_eq!(
            controls.events.lock().await.last(),
            Some(&ControlEvent::Released("p1".into()))
        );
        leases.acquire(&alive, "c1", "p1", 60, 0).await.unwrap();
        tokio::time::advance(LEASE_TTL).await;
        leases.sweep_expired().await.unwrap();
        assert_eq!(leases.active_rows("p1").await, None);
        assert_eq!(controls.events.lock().await.len(), 4);
    }

    #[tokio::test(start_paused = true)]
    async fn same_size_renewal_extends_ttl_and_cancels_release_grace() {
        let (leases, controls) = test_leases();
        let alive = tokio_util::sync::CancellationToken::new();
        leases.acquire(&alive, "c1", "p1", 57, 12).await.unwrap();
        leases.release("c1", "p1").await.unwrap();
        tokio::time::advance(RELEASE_GRACE - Duration::from_secs(1)).await;
        leases.acquire(&alive, "c1", "p1", 57, 12).await.unwrap();
        tokio::time::advance(LEASE_TTL - Duration::from_secs(1)).await;
        leases.sweep_expired().await.unwrap();
        assert_eq!(leases.capture_size("p1").await, Some((57, 12)));
        assert_eq!(controls.events.lock().await.len(), 1);
        leases.acquire(&alive, "c1", "p1", 57, 12).await.unwrap();
        tokio::time::advance(Duration::from_secs(1)).await;
        leases.sweep_expired().await.unwrap();
        assert_eq!(leases.capture_size("p1").await, Some((57, 12)));
        tokio::time::advance(LEASE_TTL - Duration::from_secs(1)).await;
        assert_eq!(leases.active_rows("p1").await, None);
        // Native ownership, unlike query TTL metadata, is authoritative until
        // the sweep releases it.
        assert_eq!(leases.capture_size("p1").await, Some((57, 12)));
        leases.sweep_expired().await.unwrap();
        assert_eq!(leases.capture_size("p1").await, None);
        assert_eq!(controls.events.lock().await.len(), 2);
    }

    #[tokio::test]
    async fn release_client_and_shutdown_release_only_owned_panes() {
        let (leases, controls) = test_leases();
        let alive = tokio_util::sync::CancellationToken::new();
        leases.acquire(&alive, "c1", "p1", 60, 0).await.unwrap();
        leases.acquire(&alive, "c2", "p2", 70, 20).await.unwrap();
        leases.release_client("unknown").await.unwrap();
        leases.release("c1", "unowned").await.unwrap();
        assert_eq!(controls.events.lock().await.len(), 2);
        leases.release_client("c1").await.unwrap();
        assert_eq!(leases.active_columns("p1").await, None);
        assert_eq!(leases.active_columns("p2").await, Some(70));
        leases.shutdown().await.unwrap();
        leases.shutdown().await.unwrap();
        assert_eq!(
            *controls.events.lock().await,
            vec![
                ControlEvent::Claimed("p1".into(), 60, 24),
                ControlEvent::Claimed("p2".into(), 70, 20),
                ControlEvent::Released("p1".into()),
                ControlEvent::Released("p2".into()),
            ],
        );
        assert_eq!(
            leases.acquire(&alive, "c2", "p2", 70, 20).await,
            Err(ERR_CLOSED)
        );
    }

    #[tokio::test]
    async fn failed_resize_rolls_back_and_replaces_poisoned_controller() {
        let (leases, controls) = test_leases();
        let alive = tokio_util::sync::CancellationToken::new();
        leases.acquire(&alive, "c1", "p1", 70, 20).await.unwrap();
        controls.fail_resize.store(true, Ordering::SeqCst);
        assert_eq!(
            leases.acquire(&alive, "c1", "p1", 57, 12).await,
            Err(ERR_RESIZE_FAILED)
        );
        assert_eq!(leases.active_columns("p1").await, None);
        assert_eq!(leases.capture_size("p1").await, None);
        assert_eq!(
            leases.inner.state.lock().await.panes["p1"].leases["c1"].columns,
            70
        );
        controls.fail_resize.store(false, Ordering::SeqCst);
        leases.sweep_expired().await.unwrap();
        assert_eq!(leases.active_columns("p1").await, Some(70));
        assert_eq!(
            *controls.events.lock().await,
            vec![
                ControlEvent::Claimed("p1".into(), 70, 20),
                ControlEvent::Released("p1".into()),
                ControlEvent::Claimed("p1".into(), 70, 20),
            ],
        );
        controls.fail_resize.store(true, Ordering::SeqCst);
        assert_eq!(
            leases.acquire(&alive, "c2", "p1", 60, 12).await,
            Err(ERR_RESIZE_FAILED)
        );
        assert!(!leases.inner.state.lock().await.panes["p1"]
            .leases
            .contains_key("c2"));
    }

    #[tokio::test]
    async fn busy_claim_never_creates_owned_controller() {
        let (leases, controls) = test_leases();
        let alive = tokio_util::sync::CancellationToken::new();
        controls.refuse_claim.store(true, Ordering::SeqCst);
        assert_eq!(
            leases.acquire(&alive, "c1", "p1", 57, 12).await,
            Err(ERR_RESIZE_FAILED)
        );
        leases.shutdown().await.unwrap();
        assert!(controls.events.lock().await.is_empty());
    }

    #[tokio::test]
    async fn dead_controller_cannot_satisfy_same_size_renewal() {
        let (leases, controls) = test_leases();
        let alive = tokio_util::sync::CancellationToken::new();
        leases.acquire(&alive, "c1", "p1", 57, 12).await.unwrap();
        controls.live.store(false, Ordering::SeqCst);
        assert_eq!(leases.active_columns("p1").await, None);
        assert_eq!(leases.capture_size("p1").await, None);
        controls.refuse_claim.store(true, Ordering::SeqCst);
        assert_eq!(
            leases.acquire(&alive, "c1", "p1", 57, 12).await,
            Err(ERR_RESIZE_FAILED)
        );
        assert_eq!(controls.events.lock().await.len(), 2);
    }

    #[tokio::test]
    async fn owner_cancellation_after_claim_rolls_back_and_releases() {
        let (leases, controls) = test_leases();
        let alive = tokio_util::sync::CancellationToken::new();
        *controls.cancel_claim.lock().await = Some(alive.clone());
        assert_eq!(
            leases.acquire(&alive, "c1", "p1", 57, 12).await,
            Err(ERR_OWNER_GONE)
        );
        assert_eq!(leases.active_columns("p1").await, None);
        assert!(leases.inner.state.lock().await.panes["p1"]
            .leases
            .is_empty());
        assert_eq!(
            *controls.events.lock().await,
            vec![
                ControlEvent::Claimed("p1".into(), 57, 12),
                ControlEvent::Released("p1".into()),
            ],
        );
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
