//! `internal/update/worker.go` + `stage.go` — the detached update worker.
//!
//! The manager ([`crate::actions::misc`]) writes `update-job-*.json` and
//! launches `lerdr-relay update-worker JOB.json` as a transient unit
//! (`systemd-run --user` / `launchctl submit`), which lands here. The
//! worker is deliberately synchronous: no tokio runtime, matching the
//! oracle's `update.Run`.
//!
//! Flow — `Worker.Run`: read + validate the job, flock
//! `<release_root>/update.lock`, then `preparing` (download, checksum,
//! verify, transport-compat check — staging only, the staged tree is
//! discarded), `installing` (`herdr plugin install` — Herdr performs the
//! actual swap + restart), `restarting` (poll `/healthz` for the new
//! binary's identity), then `succeeded`/`failed` in `update-state.json`.
//! There is no in-worker rollback: the oracle has none — the `failed`
//! state is terminal and the job file stays for the retry path.
//!
//! Divergences from the oracle:
//! - Archive asset name is `lerdr-relay_<ver>_<target>.tar.gz` only; the
//!   oracle's `lerdr_`/`herdr-mobile-relay_` names hold the Go binary
//!   and are never installable here.
//! - Archive extraction uses the `tar` CLI (the speech runtime's
//!   convention); entry-count/byte caps and member-name checks are
//!   enforced around it rather than mid-stream.
//! - Downloads run through `curl` (the update-check transport in
//!   misc.rs), with `--max-time` standing in for the Go client timeouts
//!   and a capped pipe read for `io.LimitReader`.
//! - `context.Context` becomes a `deadline: Instant`; the expired-ctx
//!   error spells `context deadline exceeded` like `ctx.Err()`.

use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::actions::misc::{
    compact, dirname, mkdir_all, now_rfc3339, now_unix, path_escape, semver_valid, valid_revision,
    write_state, UpdateJob, UpdateState,
};
use crate::release::{self, Manifest};

/// `updateWorkerTimeout` — the 15-minute `context.WithTimeout` around a
/// worker run.
const WORKER_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// `processTermGrace` — TERM → KILL escalation window.
const PROCESS_TERM_GRACE: Duration = Duration::from_secs(2);
/// `processWaitDelay` — post-KILL bound on `Wait` returning.
const PROCESS_WAIT_DELAY: Duration = Duration::from_secs(4);
/// `updateRepository` — the Herdr plugin id the worker installs.
const UPDATE_REPOSITORY: &str = "IGUNUBLUE/lerdr";

/// `canonicalReleaseAssets`.
const CANONICAL_RELEASE_ASSETS: &str = "https://github.com/IGUNUBLUE/lerdr/releases/download";
/// `maxChecksumBytes`.
const MAX_CHECKSUM_BYTES: usize = 1024 * 1024;
/// `maxArchiveBytes`.
const MAX_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
/// `maxExtractedBytes` — enforced post-extract (the archive is already
/// checksum-pinned at that point; see module notes).
const MAX_EXTRACTED_BYTES: u64 = 256 * 1024 * 1024;
/// `maxArchiveEntries`.
const MAX_ARCHIVE_ENTRIES: usize = 4096;
/// The stage client's 2-minute transfer bound (`http.Client{Timeout}`).
const STAGE_TIMEOUT: Duration = Duration::from_secs(120);

/// `verifyHealth`'s fixed 15 s deadline.
const HEALTH_DEADLINE: Duration = Duration::from_secs(15);
/// The 500 ms poll tick between health checks.
const HEALTH_POLL: Duration = Duration::from_millis(500);
/// `http.Client{Timeout: 2s}` for one health poll.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
/// `io.LimitReader(response.Body, 64*1024)` on the health reply.
const HEALTH_BODY_CAP: usize = 64 * 1024;

/// `ErrConcurrent` — the update.lock is held; the bin maps it to exit 3.
/// Every other failure is `Failed` (exit 1).
#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    /// `ErrConcurrent` — `errors.Is` checked by the CLI dispatcher.
    #[error("another update is already running")]
    Concurrent,
    /// `Worker.Run`'s ordinary failure — also what `fail()` returns after
    /// persisting the `failed` state.
    #[error("{0}")]
    Failed(String),
}

impl WorkerError {
    /// `errors.Is(err, ErrConcurrent)`.
    pub fn is_concurrent(&self) -> bool {
        matches!(self, WorkerError::Concurrent)
    }
}

fn failed(message: impl Into<String>) -> WorkerError {
    WorkerError::Failed(message.into())
}

/// `update.Run` — `Worker{}.Run(ctx, jobPath)` under the 15-minute bound.
pub fn run(job_path: &Path) -> Result<(), WorkerError> {
    UpdateWorker::default().run(job_path)
}

/// Injectable seams — `Worker.Prepare`/`Install`/`Verify` in the oracle.
type PrepareFn = Box<dyn Fn(&UpdateJob, Instant) -> Result<StagedRelease, String> + Send + Sync>;
type InstallFn = Box<dyn Fn(&UpdateJob, Instant) -> Result<(), String> + Send + Sync>;
type VerifyFn = Box<dyn Fn(&str, &Manifest, Instant) -> Result<(), String> + Send + Sync>;

/// `update.Worker` — `Prepare`/`Install`/`Verify` are injectable seams,
/// `worker_timeout`/`health_*` shrink the oracle's fixed durations in
/// tests.
#[derive(Default)]
pub(crate) struct UpdateWorker {
    prepare: Option<PrepareFn>,
    install: Option<InstallFn>,
    verify: Option<VerifyFn>,
    worker_timeout: Option<Duration>,
    health_deadline: Option<Duration>,
    health_poll: Option<Duration>,
}

/// `stagedRelease` — the verified staged tree + its manifest. `Drop` is
/// the oracle's `defer os.RemoveAll(staged.Root)`.
pub(crate) struct StagedRelease {
    pub(crate) root: PathBuf,
    pub(crate) manifest: Manifest,
}

impl Drop for StagedRelease {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl UpdateWorker {
    /// `Worker.Run` — the job lifecycle with `fail`/`failStartup` state
    /// writes at exactly the oracle's boundaries.
    pub(crate) fn run(&self, job_path: &Path) -> Result<(), WorkerError> {
        let deadline = Instant::now() + self.worker_timeout.unwrap_or(WORKER_TIMEOUT);
        let job = load_job(job_path)?;
        let mut state = UpdateState {
            state: "scheduled".to_owned(),
            target_version: job.target_version.clone(),
            target_revision: job.target_revision.clone(),
            target: release::current_target(),
            started_at: now_rfc3339(),
            mode: "plugin".to_owned(),
            eligible: true,
            ..UpdateState::default()
        };
        // `failStartup` — write `failed` only when the job's state path is
        // usable; an unusable path returns the raw error.
        let fail_startup = |err: String| -> WorkerError {
            if job.state_path.is_empty() || !Path::new(&job.state_path).is_absolute() {
                return failed(err);
            }
            fail(&job.state_path, &state, err)
        };
        if let Err(err) = validate_job(&job) {
            return Err(fail_startup(err));
        }
        if let Err(err) = mkdir_all(Path::new(&job.release_root), 0o700) {
            return Err(fail_startup(err.to_string()));
        }
        // `acquireLock` — held for the run; drop closes the fd and
        // releases the flock.
        let _lock = match acquire_lock(&Path::new(&job.release_root).join("update.lock")) {
            Ok(lock) => lock,
            Err(WorkerError::Concurrent) => return Err(WorkerError::Concurrent),
            Err(err) => return Err(fail_startup(err.to_string())),
        };

        state.state = "preparing".to_owned();
        if let Err(err) = write_state(Path::new(&job.state_path), &state) {
            return Err(failed(format!("write preparing state: {err}")));
        }
        let prepare = self.prepare.as_ref().map(|f| f.as_ref());
        let staged = match prepare {
            Some(prepare) => prepare(&job, deadline),
            None => prepare_target_release(&job, deadline),
        };
        let staged = match staged {
            Ok(staged) => staged,
            Err(err) => {
                return Err(fail(
                    &job.state_path,
                    &state,
                    format!("prepare target release: {err}"),
                ));
            }
        };

        state.state = "installing".to_owned();
        if let Err(err) = write_state(Path::new(&job.state_path), &state) {
            return Err(failed(format!("write installing state: {err}")));
        }
        let install = self.install.as_ref().map(|f| f.as_ref());
        let installed = match install {
            Some(install) => install(&job, deadline),
            None => install_plugin(&job, deadline),
        };
        if let Err(err) = installed {
            return Err(fail(&job.state_path, &state, err));
        }

        state.state = "restarting".to_owned();
        if let Err(err) = write_state(Path::new(&job.state_path), &state) {
            // The oracle returns this one unwrapped.
            return Err(failed(err.to_string()));
        }
        let verify = self.verify.as_ref().map(|f| f.as_ref());
        let verified = match verify {
            Some(verify) => verify(&job.health_url, &staged.manifest, deadline),
            None => self.verify_health(&job.health_url, &staged.manifest, deadline),
        };
        if let Err(err) = verified {
            return Err(fail(
                &job.state_path,
                &state,
                format!("verify Herdr plugin update: {err}"),
            ));
        }

        state.state = "succeeded".to_owned();
        state.current_version = job.target_version.clone();
        state.current_revision = job.target_revision.clone();
        state.checked_at = now_unix();
        state.finished_at = now_rfc3339();
        state.error.clear();
        write_state(Path::new(&job.state_path), &state).map_err(|e| failed(e.to_string()))?;
        // A completed job file is consumed, not retried.
        let _ = std::fs::remove_file(job_path);
        Ok(())
    }

    /// `verifyHealth` — poll `/healthz` every 500 ms for up to 15 s until
    /// it reports exactly the staged release (status ok, release_version,
    /// revision case-insensitive, bundle hash when the manifest carries
    /// one).
    fn verify_health(
        &self,
        health_url: &str,
        manifest: &Manifest,
        deadline: Instant,
    ) -> Result<(), String> {
        let health_deadline = Instant::now() + self.health_deadline.unwrap_or(HEALTH_DEADLINE);
        let poll = self.health_poll.unwrap_or(HEALTH_POLL);
        loop {
            if let Ok((200, body)) = curl_get(health_url, None, HEALTH_TIMEOUT, deadline) {
                // `io.LimitReader(response.Body, 64*1024)` — a longer body
                // decodes as a truncated prefix, i.e. a mismatch.
                let health: HealthReply =
                    serde_json::from_slice(&body[..body.len().min(HEALTH_BODY_CAP)])
                        .unwrap_or_default();
                if health.status == "ok"
                    && health.release_version == manifest.version
                    && health.revision.eq_ignore_ascii_case(&manifest.revision)
                    && (manifest.web_hash.as_deref().unwrap_or_default().is_empty()
                        || health
                            .bundle_hash
                            .eq_ignore_ascii_case(manifest.web_hash.as_deref().unwrap_or_default()))
                {
                    return Ok(());
                }
            }
            // `select ctx.Done() | <-deadline.C | <-ticker.C` — the worker
            // deadline wins ties arbitrarily, like Go's select.
            let now = Instant::now();
            if now >= deadline {
                return Err("context deadline exceeded".to_owned());
            }
            if now >= health_deadline {
                return Err(format!(
                    "relay did not report release {} ({})",
                    manifest.version, manifest.revision
                ));
            }
            thread::sleep(poll.min(health_deadline.min(deadline) - now));
        }
    }
}

/// `loadJob` — `read update job` / `parse update job` wraps.
fn load_job(job_path: &Path) -> Result<UpdateJob, WorkerError> {
    let data = std::fs::read(job_path).map_err(|e| failed(format!("read update job: {e}")))?;
    serde_json::from_slice(&data).map_err(|e| failed(format!("parse update job: {e}")))
}

/// `validateJob` — absolute paths, an executable herdr_bin, semver +
/// 40-hex revision, and a loopback HTTP health URL.
fn validate_job(job: &UpdateJob) -> Result<(), String> {
    if job.release_root.is_empty() || !Path::new(&job.release_root).is_absolute() {
        return Err("release_root must be absolute".to_owned());
    }
    if job.herdr_bin.is_empty() || !Path::new(&job.herdr_bin).is_absolute() {
        return Err("herdr_bin must be absolute".to_owned());
    }
    let executable = std::fs::metadata(&job.herdr_bin)
        .map(|info| {
            use std::os::unix::fs::PermissionsExt;
            info.is_file() && info.permissions().mode() & 0o111 != 0
        })
        .unwrap_or(false);
    if !executable {
        return Err("herdr_bin must be an executable file".to_owned());
    }
    if !semver_valid(&job.target_version) {
        return Err("target_version must be semantic versioned".to_owned());
    }
    if !valid_revision(&job.target_revision) {
        return Err("target_revision must be an exact commit".to_owned());
    }
    if job.state_path.is_empty() || !Path::new(&job.state_path).is_absolute() {
        return Err("state_path must be absolute".to_owned());
    }
    match url::Url::parse(&job.health_url) {
        Ok(health) if health.scheme() == "http" && is_loopback_host(&health) => {}
        _ => return Err("health_url must use HTTP on loopback".to_owned()),
    }
    Ok(())
}

/// `isLoopback(health.Hostname())` — the oracle's literal allowlist:
/// `localhost`, `127.0.0.1`, `::1`. `Url::host()` strips the IPv6
/// brackets `Hostname()` also drops.
fn is_loopback_host(health: &url::Url) -> bool {
    match health.host() {
        Some(url::Host::Domain(domain)) => domain == "localhost",
        Some(url::Host::Ipv4(ip)) => ip.to_string() == "127.0.0.1",
        Some(url::Host::Ipv6(ip)) => ip.to_string() == "::1",
        None => false,
    }
}

/// `acquireLock` — create the lock file, flock it non-blocking; any flock
/// failure is `ErrConcurrent`. The returned `File` is the guard: closing
/// it releases the lock.
fn acquire_lock(path: &Path) -> Result<std::fs::File, WorkerError> {
    use std::os::fd::AsRawFd;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map_err(|e| failed(e.to_string()))?;
    // flock(2) — LOCK_EX|LOCK_NB; the oracle maps every failure to
    // ErrConcurrent, errno-insensitive.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(WorkerError::Concurrent);
    }
    let _ = file.set_len(0);
    let _ = writeln!(file, "{}", std::process::id());
    let _ = file.sync_all();
    Ok(file)
}

/// `fail` — persist `failed` + compacted error + finish stamp (best
/// effort) and return the original error.
fn fail(state_path: &str, state: &UpdateState, err: String) -> WorkerError {
    let failed_state = UpdateState {
        state: "failed".to_owned(),
        error: compact(&err, 500),
        finished_at: now_rfc3339(),
        ..state.clone()
    };
    let _ = write_state(Path::new(state_path), &failed_state);
    failed(err)
}

// ── stage.go — download + verify the target release into a scratch dir ─

/// `prepareTargetRelease` — the canonical assets base.
fn prepare_target_release(job: &UpdateJob, deadline: Instant) -> Result<StagedRelease, String> {
    prepare_target_release_from(job, CANONICAL_RELEASE_ASSETS, deadline)
}

/// `prepareTargetReleaseFrom` — staged verification: download
/// `checksums.txt`, pick the archive line, download + checksum the
/// tarball, extract, `release.Verify`, identity match, and the transport
/// compatibility gate. The staged root is discarded by the caller — the
/// real swap is `installPlugin` via Herdr.
fn prepare_target_release_from(
    job: &UpdateJob,
    asset_base: &str,
    deadline: Instant,
) -> Result<StagedRelease, String> {
    let current = release::load(&Path::new(&job.release_root).join("current"))
        .map_err(|e| format!("load current release: {e}"))?;

    let target = release::current_target().replace('/', "_");
    // `lerdr-relay_<ver>_<target>` — no `lerdr_`/`herdr-mobile-relay_`
    // fallback: pre-rename assets carry the Go binary (README declares
    // the divergence).
    let archive_name = format!("lerdr-relay_{}_{}.tar.gz", job.target_version, target);
    let base = format!(
        "{}/v{}",
        asset_base.trim_end_matches('/'),
        path_escape(&job.target_version)
    );
    let checksums = download_bytes(
        &format!("{base}/checksums.txt"),
        MAX_CHECKSUM_BYTES,
        deadline,
    )
    .map_err(|e| format!("download release checksums: {e}"))?;
    let expected_checksum = checksum_for_archive(&checksums, &archive_name)?;

    // `os.MkdirTemp(filepath.Dir(job.StatePath), ".update-stage-")`.
    let stage_root = temp_dir_in(&dirname(&job.state_path), ".update-stage-")
        .map_err(|e| format!("create update stage: {e}"))?;
    let mut prepared = StagedRelease {
        root: stage_root,
        manifest: Manifest::default(),
    };

    let result = (|| -> Result<(), String> {
        let archive_path = prepared.root.join(&archive_name);
        let actual_checksum = download_file(
            &format!("{base}/{}", path_escape(&archive_name)),
            &archive_path,
            MAX_ARCHIVE_BYTES,
            deadline,
        )
        .map_err(|e| format!("download target release: {e}"))?;
        if !actual_checksum.eq_ignore_ascii_case(&expected_checksum) {
            return Err("target release archive checksum mismatch".to_owned());
        }
        let release_root = prepared.root.join("release");
        mkdir_all(&release_root, 0o700).map_err(|e| e.to_string())?;
        extract_release_archive(&archive_path, &release_root)
            .map_err(|e| format!("extract target release: {e}"))?;
        std::fs::remove_file(&archive_path).map_err(|e| format!("remove staged archive: {e}"))?;

        let manifest = release::verify(&release_root, &release::current_target())
            .map_err(|e| format!("verify target release: {e}"))?;
        if manifest.version != job.target_version
            || !manifest.revision.eq_ignore_ascii_case(&job.target_revision)
        {
            return Err("target release identity does not match the advertised update".to_owned());
        }
        validate_upgrade_compatibility(&current, &manifest)
            .map_err(|e| format!("transport compatibility: {e}"))?;
        prepared.manifest = manifest;
        Ok(())
    })();
    // `defer func() { if !keep { RemoveAll(stageRoot) } }()` — a failed
    // stage cleans up immediately; success hands the guard to the caller.
    if let Err(err) = result {
        let _ = std::fs::remove_dir_all(&prepared.root);
        return Err(err);
    }
    Ok(prepared)
}

/// `checksumForArchive` — `<sha256>  <name>` (or `*<name>`) lines.
fn checksum_for_archive(data: &[u8], archive_name: &str) -> Result<String, String> {
    for line in String::from_utf8_lossy(data).lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let named = fields.get(1).map(|n| n.strip_prefix('*').unwrap_or(n));
        if fields.len() < 2 || named != Some(archive_name) {
            continue;
        }
        match hex::decode(fields[0]) {
            Ok(decoded) if decoded.len() == 32 => return Ok(fields[0].to_lowercase()),
            _ => return Err("release archive checksum is invalid".to_owned()),
        }
    }
    Err(format!("checksums.txt does not list {archive_name}"))
}

/// `downloadBytes` — GET to memory with the size cap; exceeding it is the
/// oracle's `response exceeds size limit`.
fn download_bytes(endpoint: &str, maximum: usize, deadline: Instant) -> Result<Vec<u8>, String> {
    let (code, body) = curl_get(
        endpoint,
        Some("lerdr-update-stage"),
        STAGE_TIMEOUT,
        deadline,
    )?;
    if code != 200 {
        return Err(format!("HTTP {code}"));
    }
    if body.len() > maximum {
        return Err("response exceeds size limit".to_owned());
    }
    Ok(body)
}

/// `downloadFile` — stream to `filename` (reserved O_EXCL first, like
/// `os.OpenFile(CREATE|EXCL)`), reject over the cap, return the SHA-256.
fn download_file(
    endpoint: &str,
    filename: &Path,
    maximum: u64,
    deadline: Instant,
) -> Result<String, String> {
    // Reserve the name exclusively before curl takes it over.
    let _reserved = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(filename)
        .map_err(|e| e.to_string())?;
    let budget = STAGE_TIMEOUT.min(remaining(deadline));
    let output = Command::new("curl")
        .arg("-sS")
        .arg("-L")
        .arg("--max-redirs")
        .arg("10")
        .arg("--max-time")
        .arg(format!("{}", budget.as_secs_f64()))
        .arg("-A")
        .arg("lerdr-update-stage")
        .arg("-o")
        .arg(filename)
        .arg(endpoint)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Get {endpoint:?}: {e}"))?;
    if !output.status.success() {
        let detail = compact(&String::from_utf8_lossy(&output.stderr), 300);
        return Err(format!("Get {endpoint:?}: {detail}"));
    }
    let size = std::fs::metadata(filename).map(|m| m.len()).unwrap_or(0);
    if size > maximum {
        return Err("response exceeds size limit".to_owned());
    }
    Ok(sha256_file(filename))
}

/// The health reply's parsed keys — `verifyHealth`'s anonymous struct.
#[derive(Deserialize, Default)]
struct HealthReply {
    #[serde(default)]
    status: String,
    #[serde(default)]
    release_version: String,
    #[serde(default)]
    revision: String,
    #[serde(default)]
    bundle_hash: String,
}

/// GET via curl — the sync sibling of `misc::curl_get`: `-sS -L`, the
/// status code rides a `\n%{http_code}` trailer on stdout, `--max-time`
/// is the lesser of the call timeout and the remaining worker budget.
///
/// Errors spell `Get "<url>": <detail>` — the Go `url.Error` shape.
fn curl_get(
    endpoint: &str,
    user_agent: Option<&str>,
    timeout: Duration,
    deadline: Instant,
) -> Result<(u16, Vec<u8>), String> {
    if Instant::now() >= deadline {
        return Err(format!("Get {endpoint:?}: context deadline exceeded"));
    }
    let budget = timeout.min(remaining(deadline));
    let mut command = Command::new("curl");
    command
        .arg("-sS")
        .arg("-L")
        .arg("--max-redirs")
        .arg("10")
        .arg("--max-time")
        .arg(format!("{}", budget.as_secs_f64()));
    if let Some(ua) = user_agent {
        command.arg("-A").arg(ua);
    }
    let output = command
        .arg("-w")
        .arg("\n%{http_code}")
        .arg(endpoint)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Get {endpoint:?}: {e}"))?;
    if !output.status.success() {
        let detail = compact(&String::from_utf8_lossy(&output.stderr), 300);
        return Err(format!("Get {endpoint:?}: {detail}"));
    }
    let stdout = output.stdout;
    let Some(pos) = stdout.iter().rposition(|b| *b == b'\n') else {
        return Err(format!("Get {endpoint:?}: malformed curl output"));
    };
    let code: u16 = String::from_utf8_lossy(&stdout[pos + 1..])
        .trim()
        .parse()
        .map_err(|_| format!("Get {endpoint:?}: malformed curl status"))?;
    Ok((code, stdout[..pos].to_vec()))
}

/// `time.Until` — zero when past.
fn remaining(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

/// `os.MkdirTemp` — retry loop over a pid+nanos+counter suffix.
fn temp_dir_in(parent: &Path, prefix: &str) -> std::io::Result<PathBuf> {
    for _ in 0..64 {
        let candidate = parent.join(format!("{prefix}{}", unique_suffix()));
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(std::io::Error::other("could not create temp dir"))
}

fn unique_suffix() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    format!(
        "{:x}-{:x}-{:x}",
        std::process::id(),
        nanos,
        SEQ.fetch_add(1, AtomicOrdering::Relaxed)
    )
}

/// `sha256.New()` over the file — always succeeds once written (a failed
/// read yields "", which fails the checksum compare like the oracle's).
fn sha256_file(path: &Path) -> String {
    use sha2::Digest as _;
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(_) => return String::new(),
        }
    }
    hex::encode(hasher.finalize())
}

// ── installPlugin + process control ─────────────────────────────────────

/// `installPlugin` — `herdr plugin install IGUNUBLUE/lerdr --ref
/// <lowercase revision> --yes` with both no-auto-setup spellings. Herdr
/// performs the actual binary swap + restart.
fn install_plugin(job: &UpdateJob, deadline: Instant) -> Result<(), String> {
    let mut command = Command::new(&job.herdr_bin);
    command.args([
        "plugin",
        "install",
        UPDATE_REPOSITORY,
        "--ref",
        &job.target_revision.to_lowercase(),
        "--yes",
    ]);
    // Both spellings: staged bundles may come from before the rename.
    command.env("LERDR_NO_AUTO_SETUP", "1");
    command.env("HERDR_MOBILE_RELAY_NO_AUTO_SETUP", "1");
    let (output, error) = run_command(&mut command, deadline);
    if let Some(error) = error {
        return Err(format!(
            "Herdr plugin install failed: {error}: {}",
            compact(&String::from_utf8_lossy(&output), 500)
        ));
    }
    Ok(())
}

/// `runCommandContext` — capture both pipes; past `deadline` the process
/// GROUP is TERM'd then KILL'd (`terminateProcessGroup`) and the error is
/// `context deadline exceeded`. Returns `(combined output, error)` —
/// output accompanies failure like the oracle's, except when the wait
/// never completed (Go returns nil there too).
fn run_command(command: &mut Command, deadline: Instant) -> (Vec<u8>, Option<String>) {
    if Instant::now() >= deadline {
        return (Vec::new(), Some("context deadline exceeded".to_owned()));
    }
    command.process_group(0); // SysProcAttr{Setpgid: true}
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => return (Vec::new(), Some(err.to_string())),
    };
    // Continuous drains — the oracle wires buffers via io.Copy goroutines;
    // pipe-and-read-after-exit would deadlock a chatty child. Joins are
    // bounded by `command.WaitDelay` (processWaitDelay).
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let combined = |stdout, stderr| -> Vec<u8> {
        let mut output = drain_bounded(stdout, PROCESS_WAIT_DELAY);
        output.extend(drain_bounded(stderr, PROCESS_WAIT_DELAY));
        output
    };
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = combined(stdout, stderr);
                if status.success() {
                    return (output, None);
                }
                return (output, Some(exit_status_text(status)));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let wait_completed = terminate_process_group(&mut child);
                    if wait_completed {
                        return (
                            combined(stdout, stderr),
                            Some("context deadline exceeded".to_owned()),
                        );
                    }
                    return (Vec::new(), Some("context deadline exceeded".to_owned()));
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(err) => {
                return (combined(stdout, stderr), Some(err.to_string()));
            }
        }
    }
}

/// `command.WaitDelay` applied to a drain thread: join it while the
/// bound holds, then abandon it (the thread finishes on its own when the
/// pipe's last writer goes away — a detached grandchild can't wedge the
/// caller).
fn drain_bounded(handle: thread::JoinHandle<Vec<u8>>, bound: Duration) -> Vec<u8> {
    let end = Instant::now() + bound;
    while !handle.is_finished() && Instant::now() < end {
        thread::sleep(Duration::from_millis(5));
    }
    if handle.is_finished() {
        return handle.join().unwrap_or_default();
    }
    std::mem::forget(handle);
    Vec::new()
}

/// `exit status 1` / `signal: killed` — Go's ExitError wording.
fn exit_status_text(status: std::process::ExitStatus) -> String {
    if let Some(code) = status.code() {
        return format!("exit status {code}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            let name = match signal {
                libc::SIGTERM => "terminated".to_owned(),
                libc::SIGKILL => "killed".to_owned(),
                libc::SIGINT => "interrupt".to_owned(),
                other => format!("signal {other}"),
            };
            return format!("signal: {name}");
        }
    }
    status.to_string()
}

/// `terminateProcessGroup` — TERM the group, allow the grace window for
/// it to drain, then KILL; bound `Wait` and the group's final death by
/// the oracle's delays. Returns `waitCompleted` — whether the child was
/// reaped (its output is only meaningful then).
fn terminate_process_group(child: &mut Child) -> bool {
    let pgid = child.id() as i32;
    unsafe {
        libc::kill(-pgid, libc::SIGTERM);
    }
    let mut wait_completed = false;
    let grace = Instant::now() + PROCESS_TERM_GRACE;
    loop {
        if !wait_completed && child.try_wait().map(|s| s.is_some()).unwrap_or(true) {
            wait_completed = true;
            // `if !processGroupAlive(pgid) { return true }` — nothing left
            // to escalate when the whole group is already gone.
            if !process_group_alive(pgid) {
                return true;
            }
        }
        if Instant::now() >= grace {
            break;
        }
        thread::sleep(Duration::from_millis(10));
        if wait_completed && !process_group_alive(pgid) {
            return true;
        }
    }
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
    if !wait_completed {
        let wait_deadline = Instant::now() + PROCESS_WAIT_DELAY;
        while Instant::now() < wait_deadline {
            if child.try_wait().map(|s| s.is_some()).unwrap_or(true) {
                wait_completed = true;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    let end = Instant::now() + PROCESS_TERM_GRACE;
    while process_group_alive(pgid) && Instant::now() < end {
        thread::sleep(Duration::from_millis(10));
    }
    wait_completed
}

/// `processGroupAlive` — `kill(-pgid, 0)` succeeds or is EPERM.
fn process_group_alive(pgid: i32) -> bool {
    if unsafe { libc::kill(-pgid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Read a pipe to EOF on a helper thread (the drain goroutine).
fn drain(pipe: Option<impl Read + Send + 'static>) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut data = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut data);
        }
        data
    })
}

// ── release archive extraction ──────────────────────────────────────────

/// `extractReleaseArchive` — `tar -xzf` after a member-name sweep, with
/// the oracle's entry cap and post-extract byte cap. The tarball is
/// SHA-256-pinned by this point, so the caps are belt-and-braces.
fn extract_release_archive(archive: &Path, destination: &Path) -> Result<(), String> {
    let listing = Command::new("tar")
        .arg("-t")
        .arg("-z")
        .arg("-f")
        .arg(archive)
        .output()
        .map_err(|e| e.to_string())?;
    if !listing.status.success() {
        return Err(String::from_utf8_lossy(&listing.stderr).trim().to_owned());
    }
    let names: Vec<String> = String::from_utf8_lossy(&listing.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    if names.len() > MAX_ARCHIVE_ENTRIES {
        return Err("release archive has too many entries".to_owned());
    }
    for name in &names {
        clean_archive_path(name)?;
    }
    let output = Command::new("tar")
        .arg("-x")
        .arg("-z")
        .arg("-f")
        .arg(archive)
        .arg("-C")
        .arg(destination)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    // Oracle's per-entry caps applied post-extract (the staging dir is
    // discarded on any failure): cumulative size bound, and the
    // 0644/0755 normalization `extractRegularFile`/`MkdirAll` apply.
    let mut extracted: u64 = 0;
    normalize_tree(destination, &mut extracted)?;
    if extracted > MAX_EXTRACTED_BYTES {
        return Err("release archive exceeds extracted size limit".to_owned());
    }
    Ok(())
}

/// `cleanArchivePath` — `path.Clean`, reject escapes and absolute names;
/// a name cleaning to `.` is a no-op dir entry the extractor tolerates.
fn clean_archive_path(name: &str) -> Result<(), String> {
    let mut stack: Vec<&str> = Vec::new();
    for part in name.split('/') {
        match part {
            "" | "." => {}
            ".." => match stack.last() {
                Some(&top) if top != ".." => {
                    stack.pop();
                }
                _ => stack.push(".."),
            },
            part => stack.push(part),
        }
    }
    if name.starts_with('/') || stack.first() == Some(&"..") {
        return Err(format!(
            "release archive path {name:?} escapes its destination"
        ));
    }
    Ok(())
}

/// Walk the extracted tree: enforce the cumulative byte cap input and
/// normalize modes — dirs 0755, files 0644/0755 by any-exec-bit
/// (`extractRegularFile`/`MkdirAll`'s modes).
fn normalize_tree(dir: &Path, extracted: &mut u64) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mut entries = std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type().map_err(|e| e.to_string())?;
        if file_type.is_dir() {
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
            normalize_tree(&path, extracted)?;
        } else if file_type.is_file() {
            let info = std::fs::metadata(&path).map_err(|e| e.to_string())?;
            *extracted += info.len();
            if *extracted > MAX_EXTRACTED_BYTES {
                return Err("release archive exceeds extracted size limit".to_owned());
            }
            let mode = if info.permissions().mode() & 0o111 != 0 {
                0o755
            } else {
                0o644
            };
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode));
        } else {
            // Symlinks/specials: the oracle's extractor rejects them
            // outright — release::verify would too.
            return Err(format!(
                "release archive contains unsupported entry {:?}",
                path.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
    }
    Ok(())
}

// ── compatibility.go — transport rollout gate ───────────────────────────

/// `maxTransportCapabilities`.
const MAX_TRANSPORT_CAPABILITIES: usize = 8;
/// `legacyEncryptedWebSocketSubprotocol` — pre-metadata releases spoke
/// E2EE v1 only.
const LEGACY_TRANSPORT: &str = "herdr-e2ee-v1";

/// `TransportCapabilities` — empty lists mean the legacy E2EE-v1 release.
fn transport_capabilities(manifest: &Manifest) -> (Vec<String>, Vec<String>) {
    let app = if manifest.app_transports.is_empty() {
        vec![LEGACY_TRANSPORT.to_owned()]
    } else {
        manifest.app_transports.clone()
    };
    let relay = if manifest.relay_transports.is_empty() {
        vec![LEGACY_TRANSPORT.to_owned()]
    } else {
        manifest.relay_transports.clone()
    };
    (app, relay)
}

/// `ValidateUpgradeCompatibility` — both rollout directions must keep an
/// intersecting transport; a break needs a bridge release.
fn validate_upgrade_compatibility(current: &Manifest, target: &Manifest) -> Result<(), String> {
    if target.app_transports.is_empty() || target.relay_transports.is_empty() {
        return Err("target release does not declare app/relay transport compatibility".to_owned());
    }
    validate_transport_capabilities("target app", &target.app_transports)?;
    validate_transport_capabilities("target relay", &target.relay_transports)?;
    let (current_app, current_relay) = transport_capabilities(current);
    validate_transport_capabilities("current app", &current_app)?;
    validate_transport_capabilities("current relay", &current_relay)?;
    if !transports_intersect(&target.app_transports, &current_relay) {
        return Err(
            "target app cannot connect to the current relay; install a bridge release first"
                .to_owned(),
        );
    }
    if !transports_intersect(&current_app, &target.relay_transports) {
        return Err(
            "current app cannot connect to the target relay; install a bridge release first"
                .to_owned(),
        );
    }
    Ok(())
}

fn validate_transport_capabilities(owner: &str, transports: &[String]) -> Result<(), String> {
    if transports.len() > MAX_TRANSPORT_CAPABILITIES {
        return Err(format!("{owner} declares too many transports"));
    }
    let mut seen = std::collections::HashSet::with_capacity(transports.len());
    for transport in transports {
        if transport.is_empty() || transport.trim() != transport || transport.len() > 64 {
            return Err(format!("{owner} declares invalid transport {transport:?}"));
        }
        if !seen.insert(transport) {
            return Err(format!(
                "{owner} declares duplicate transport {transport:?}"
            ));
        }
    }
    Ok(())
}

fn transports_intersect(left: &[String], right: &[String]) -> bool {
    left.iter().any(|candidate| right.contains(candidate))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::Mutex as StdMutex;

    use super::*;
    use crate::actions::misc::write_json_atomic;

    /// `currentTestRevision`/`nextTestRevision` — the oracle's fixtures.
    const CURRENT_REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
    const NEXT_REVISION: &str = "89abcdef0123456789abcdef0123456789abcdef";

    /// `testHerdrBinary` — a no-op `#!/bin/sh` stub (absolute, executable).
    fn test_herdr_binary(dir: &Path) -> PathBuf {
        let path = dir.join("herdr");
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        path
    }

    /// `writeWorkerTestJob` — a valid job under a fresh tempdir.
    fn write_worker_test_job() -> (tempfile::TempDir, PathBuf, UpdateJob) {
        let root = tempfile::tempdir().unwrap();
        let job = UpdateJob {
            release_root: root.path().join("installed").to_string_lossy().into_owned(),
            herdr_bin: test_herdr_binary(root.path())
                .to_string_lossy()
                .into_owned(),
            target_version: "1.2.4".to_owned(),
            target_revision: NEXT_REVISION.to_owned(),
            state_path: root
                .path()
                .join("runtime")
                .join("update-state.json")
                .to_string_lossy()
                .into_owned(),
            health_url: "http://127.0.0.1:18375/healthz".to_owned(),
        };
        let job_path = root.path().join("runtime").join("update-job.json");
        write_json_atomic(&job_path, &job).unwrap();
        (root, job_path, job)
    }

    /// `workerTestStagedRelease` — a staged dir + manifest matching the job.
    fn worker_test_staged(job: &UpdateJob) -> StagedRelease {
        let root = temp_dir_in(&dirname(&job.state_path), ".worker-stage-").unwrap();
        StagedRelease {
            root,
            manifest: Manifest {
                version: job.target_version.clone(),
                revision: job.target_revision.clone(),
                ..Manifest::default()
            },
        }
    }

    fn read_test_state(path: &str) -> UpdateState {
        crate::actions::misc::read_state(Path::new(path)).unwrap()
    }

    /// `TestWorkerRunsPluginInstallAndPersistsSuccess` — injected seams run
    /// in order, the state lands `succeeded`, and the job file is consumed.
    #[test]
    fn worker_runs_plugin_install_and_persists_success() {
        let (_root, job_path, job) = write_worker_test_job();
        let calls = Arc::new(StdMutex::new(Vec::new()));
        let expected_url = job.health_url.clone();
        let expected_version = job.target_version.clone();
        let expected_revision = job.target_revision.clone();
        let worker = UpdateWorker {
            prepare: Some(Box::new({
                let calls = Arc::clone(&calls);
                move |got: &UpdateJob, _deadline| {
                    calls
                        .lock()
                        .unwrap()
                        .push(format!("prepare:{}", got.target_version));
                    Ok(worker_test_staged(got))
                }
            })),
            install: Some(Box::new({
                let calls = Arc::clone(&calls);
                move |got: &UpdateJob, _deadline| {
                    calls
                        .lock()
                        .unwrap()
                        .push(format!("install:{}", got.target_revision));
                    Ok(())
                }
            })),
            verify: Some(Box::new({
                let calls = Arc::clone(&calls);
                move |url, manifest, _deadline| {
                    calls
                        .lock()
                        .unwrap()
                        .push(format!("verify:{}", manifest.version));
                    assert_eq!(url, expected_url);
                    assert_eq!(manifest.version, expected_version);
                    assert_eq!(manifest.revision, expected_revision);
                    Ok(())
                }
            })),
            ..UpdateWorker::default()
        };
        worker.run(&job_path).unwrap();
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                "prepare:1.2.4".to_owned(),
                format!("install:{NEXT_REVISION}"),
                "verify:1.2.4".to_owned(),
            ]
        );
        let state = read_test_state(&job.state_path);
        assert_eq!(state.state, "succeeded");
        assert_eq!(state.current_version, "1.2.4");
        assert_eq!(state.current_revision, NEXT_REVISION);
        assert!(!state.finished_at.is_empty());
        assert!(!job_path.exists(), "completed job still exists");
    }

    /// `TestWorkerInstallFailureIsRetryable` — a failed install persists
    /// `failed` state and keeps the job file for the retry path.
    #[test]
    fn worker_install_failure_is_retryable() {
        let (_root, job_path, job) = write_worker_test_job();
        let worker = UpdateWorker {
            prepare: Some(Box::new(|got, _| Ok(worker_test_staged(got)))),
            install: Some(Box::new(|_, _| {
                Err("injected plugin install failure".to_owned())
            })),
            ..UpdateWorker::default()
        };
        let err = worker.run(&job_path).unwrap_err();
        assert!(err.to_string().contains("injected plugin install failure"));
        let state = read_test_state(&job.state_path);
        assert_eq!(state.state, "failed");
        assert!(state.error.contains("injected plugin install failure"));
        assert!(!state.finished_at.is_empty());
        assert!(job_path.exists(), "failed job was removed");
    }

    /// The verify-failure path — the oracle's `fail(state)` once Herdr has
    /// run: `failed` state, the error persisted compacted, job retained.
    /// (Rollback itself belongs to Herdr's install step, which owns the
    /// swap — the worker never sees a half-activated tree to undo.)
    #[test]
    fn worker_verify_failure_persists_failed_state() {
        let (_root, job_path, job) = write_worker_test_job();
        let worker = UpdateWorker {
            prepare: Some(Box::new(|got, _| Ok(worker_test_staged(got)))),
            install: Some(Box::new(|_, _| Ok(()))),
            verify: Some(Box::new(|_, _, _| {
                Err("relay did not report release 1.2.4".to_owned())
            })),
            ..UpdateWorker::default()
        };
        let err = worker.run(&job_path).unwrap_err();
        assert!(err
            .to_string()
            .contains("verify Herdr plugin update: relay did not report release 1.2.4"));
        let state = read_test_state(&job.state_path);
        assert_eq!(state.state, "failed");
        assert!(state.error.contains("relay did not report release"));
        assert!(job_path.exists(), "failed job was removed");
    }

    /// `TestWorkerStartupFailureDoesNotLeaveUpdateScheduled` — a
    /// `release_root` that cannot be a directory fails via `failStartup`:
    /// the state file still lands `failed`.
    #[test]
    fn worker_startup_failure_does_not_leave_update_scheduled() {
        let root = tempfile::tempdir().unwrap();
        let release_root = root.path().join("installed");
        std::fs::write(&release_root, "not a directory").unwrap();
        let state_path = root
            .path()
            .join("runtime")
            .join("update-state.json")
            .to_string_lossy()
            .into_owned();
        let job = UpdateJob {
            release_root: release_root.to_string_lossy().into_owned(),
            herdr_bin: test_herdr_binary(root.path())
                .to_string_lossy()
                .into_owned(),
            target_version: "1.2.4".to_owned(),
            target_revision: NEXT_REVISION.to_owned(),
            state_path: state_path.clone(),
            health_url: "http://127.0.0.1/healthz".to_owned(),
        };
        let job_path = root.path().join("update-job.json");
        write_json_atomic(&job_path, &job).unwrap();
        write_state(
            Path::new(&state_path),
            &UpdateState {
                state: "scheduled".to_owned(),
                target_version: job.target_version.clone(),
                target_revision: job.target_revision.clone(),
                ..UpdateState::default()
            },
        )
        .unwrap();

        assert!(run(&job_path).is_err());
        let state = read_test_state(&state_path);
        assert_eq!(state.state, "failed");
        assert!(!state.error.is_empty());
        assert!(!state.finished_at.is_empty());
    }

    /// A second worker on the same lock file exits `ErrConcurrent`
    /// (exit 3 at the CLI) without touching the state file.
    #[test]
    fn worker_concurrent_lock_exits_concurrent() {
        let (_root, job_path, job) = write_worker_test_job();
        mkdir_all(Path::new(&job.release_root), 0o700).unwrap();
        let lock = acquire_lock(&Path::new(&job.release_root).join("update.lock")).unwrap();
        let err = run(&job_path).unwrap_err();
        assert!(err.is_concurrent(), "{err:?}");
        drop(lock);
    }

    /// `TestInstallPluginPinsExactCommitAndSuppressesSetup` — argv is
    /// `plugin install IGUNUBLUE/lerdr --ref <lowercase revision> --yes`
    /// and `LERDR_NO_AUTO_SETUP=1` lands in the child's environment.
    #[test]
    fn install_plugin_pins_exact_commit_and_suppresses_setup() {
        let root = tempfile::tempdir().unwrap();
        let args_path = root.path().join("args");
        let env_path = root.path().join("env");
        let herdr_bin = root.path().join("herdr");
        std::fs::write(
            &herdr_bin,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$LERDR_TEST_ARGS\"\nprintf '%s\\n' \"$LERDR_NO_AUTO_SETUP\" > \"$LERDR_TEST_ENV\"\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&herdr_bin, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::env::set_var("LERDR_TEST_ARGS", &args_path);
        std::env::set_var("LERDR_TEST_ENV", &env_path);
        std::env::set_var("LERDR_NO_AUTO_SETUP", "0");

        let job = UpdateJob {
            herdr_bin: herdr_bin.to_string_lossy().into_owned(),
            target_revision: NEXT_REVISION.to_uppercase(),
            ..UpdateJob::default()
        };
        install_plugin(&job, Instant::now() + Duration::from_secs(30)).unwrap();

        let args = std::fs::read_to_string(&args_path).unwrap();
        assert_eq!(
            args,
            format!("plugin\ninstall\nIGUNUBLUE/lerdr\n--ref\n{NEXT_REVISION}\n--yes\n")
        );
        assert_eq!(std::fs::read_to_string(&env_path).unwrap(), "1\n");
        std::env::remove_var("LERDR_TEST_ARGS");
        std::env::remove_var("LERDR_TEST_ENV");
        std::env::remove_var("LERDR_NO_AUTO_SETUP");
    }

    /// `TestRunCommandContextTerminatesDescendantHoldingOutputPipe` — a
    /// child holding the stdout pipe across the deadline is group-killed;
    /// the error spells `context deadline exceeded`.
    #[test]
    fn run_command_terminates_descendant_holding_output_pipe() {
        let root = tempfile::tempdir().unwrap();
        let pid_path = root.path().join("child.pid");
        let script_path = root.path().join("spawn-child.sh");
        std::fs::write(
            &script_path,
            format!(
                "#!/bin/sh\n(trap '' TERM; sleep 30) &\nchild=$!\nprintf '%s\\n' \"$child\" > {:?}\nwait\n",
                pid_path.display().to_string()
            ),
        )
        .unwrap();
        let mut command = Command::new("/bin/sh");
        command.arg(&script_path);
        let start = Instant::now();
        let (_output, error) =
            run_command(&mut command, Instant::now() + Duration::from_millis(500));
        assert_eq!(error.as_deref(), Some("context deadline exceeded"));
        assert!(
            start.elapsed() < Duration::from_secs(8),
            "run_command did not return after cancelling a descendant-held pipe"
        );
        let child_pid: i32 = std::fs::read_to_string(&pid_path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // The descendant must die — the TERM→KILL group escalation.
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let alive = unsafe { libc::kill(child_pid, 0) } == 0
                || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
            if !alive {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "descendant process {child_pid} survived process-group cancellation"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// `TestVerifyHealthRequiresExactInstalledIdentity` — a canned
    /// `/healthz` reporting the expected identity (revision uppercased,
    /// like Go's `strings.EqualFold`) passes; a wrong revision or
    /// bundle_hash spins until the health deadline.
    #[test]
    fn verify_health_requires_exact_installed_identity() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                if stream.read(&mut buf).unwrap_or(0) == 0 {
                    continue;
                }
                let body = format!(
                    "{{\"status\":\"ok\",\"release_version\":\"1.2.4\",\"revision\":\"{}\",\"bundle_hash\":\"web-hash\"}}\n",
                    NEXT_REVISION.to_uppercase()
                );
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .as_bytes(),
                );
            }
        });
        let health_url = format!("http://127.0.0.1:{port}/healthz");
        let worker = UpdateWorker {
            health_deadline: Some(Duration::from_millis(150)),
            health_poll: Some(Duration::from_millis(10)),
            ..UpdateWorker::default()
        };

        let expected = Manifest {
            version: "1.2.4".to_owned(),
            revision: NEXT_REVISION.to_owned(),
            web_hash: Some("web-hash".to_owned()),
            ..Manifest::default()
        };
        worker
            .verify_health(
                &health_url,
                &expected,
                Instant::now() + Duration::from_secs(30),
            )
            .unwrap();

        // Wrong revision — polls until the health deadline.
        let mismatched = Manifest {
            revision: CURRENT_REVISION.to_owned(),
            ..expected.clone()
        };
        let err = worker
            .verify_health(
                &health_url,
                &mismatched,
                Instant::now() + Duration::from_secs(30),
            )
            .unwrap_err();
        assert!(err.contains("relay did not report release"), "{err}");

        // Wrong bundle hash — same.
        let mismatched = Manifest {
            web_hash: Some("other-web-hash".to_owned()),
            ..expected.clone()
        };
        let err = worker
            .verify_health(
                &health_url,
                &mismatched,
                Instant::now() + Duration::from_secs(30),
            )
            .unwrap_err();
        assert!(err.contains("relay did not report release"), "{err}");
        drop(server);
    }

    /// `TestPrepareTargetReleaseDownloadsVerifiesAndChecksCompatibility` —
    /// a canned release server hands out `checksums.txt` + the archive;
    /// the staged manifest matches the job and the extracted tree is real.
    #[test]
    fn prepare_target_release_downloads_verifies_and_checks_compatibility() {
        let root = tempfile::tempdir().unwrap();
        let release_root = root.path().join("installed");
        let current_root = release_root.join("releases").join("current-test");
        write_worker_test_release(&current_root, "1.2.3", CURRENT_REVISION);
        std::os::unix::fs::symlink(
            Path::new("releases").join("current-test"),
            release_root.join("current"),
        )
        .unwrap();

        let target_root = root.path().join("target");
        write_worker_test_release(&target_root, "1.2.4", NEXT_REVISION);
        let archive = archive_tar_gz(&target_root);
        let checksum = sha256_file(&archive);
        let archive_bytes = std::fs::read(&archive).unwrap();
        let archive_name = format!(
            "lerdr-relay_1.2.4_{}.tar.gz",
            release::current_target().replace('/', "_")
        );

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let checksums = format!("{checksum}  {archive_name}\n");
        let checksum_path = "/v1.2.4/checksums.txt".to_owned();
        let archive_path = format!("/v1.2.4/{archive_name}");
        thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 8192];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).into_owned();
                let path = request.split_whitespace().nth(1).unwrap_or("").to_owned();
                let (code, body) = if path == checksum_path {
                    ("200 OK", checksums.clone().into_bytes())
                } else if path == archive_path {
                    ("200 OK", archive_bytes.clone())
                } else {
                    ("404 Not Found", Vec::new())
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {code}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                );
                let _ = stream.write_all(&body);
            }
        });

        let runtime_dir = root.path().join("runtime");
        mkdir_all(&runtime_dir, 0o700).unwrap();
        let job = UpdateJob {
            release_root: release_root.to_string_lossy().into_owned(),
            target_version: "1.2.4".to_owned(),
            target_revision: NEXT_REVISION.to_owned(),
            state_path: runtime_dir
                .join("update-state.json")
                .to_string_lossy()
                .into_owned(),
            ..UpdateJob::default()
        };
        let staged = prepare_target_release_from(
            &job,
            &format!("http://127.0.0.1:{port}"),
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap();
        assert_eq!(staged.manifest.version, "1.2.4");
        assert_eq!(staged.manifest.revision, NEXT_REVISION);
        assert!(staged
            .root
            .join("release")
            .join("scripts/common.sh")
            .exists());
    }

    /// `TestExtractReleaseArchiveRejectsPathTraversal` — a `../` member is
    /// rejected by the name sweep before `tar -x` runs.
    #[test]
    fn extract_release_archive_rejects_path_traversal() {
        let root = tempfile::tempdir().unwrap();
        let archive_path = root.path().join("malicious.tar.gz");
        write_tar_gz_entry(&archive_path, "../escaped", b"no");
        let destination = root.path().join("release");
        std::fs::create_dir(&destination).unwrap();
        let err = extract_release_archive(&archive_path, &destination).unwrap_err();
        assert!(err.contains("escapes"), "{err}");
        assert!(!root.path().join("escaped").exists());
    }

    /// `writeWorkerTestRelease` — the Rust tarball's required file set.
    fn write_worker_test_release(root: &Path, version: &str, revision: &str) {
        let files = [
            "lerdr-relay",
            "README.md",
            "scripts/common.sh",
            "scripts/plugin-on-event.sh",
            "scripts/plugin-on-startup.sh",
            "scripts/setup-link.sh",
            "scripts/tailscale-serve.sh",
            "scripts/tailscale-service.sh",
        ];
        for name in files {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, format!("{name}\n")).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = if name == "lerdr-relay" { 0o755 } else { 0o644 };
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            }
        }
        release::build(root, version, revision, &release::current_target()).unwrap();
    }

    /// `releaseArchive` — `tar -czf` the release root into a sibling file.
    fn archive_tar_gz(root: &Path) -> PathBuf {
        let archive = root.with_extension("tar.gz");
        let output = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(root)
            .arg(".")
            .output()
            .unwrap();
        assert!(output.status.success());
        archive
    }

    /// Hand-write a one-member tar.gz — GNU tar won't create `../`
    /// members, so the traversal fixture is assembled byte-level then
    /// compressed via `gzip`.
    fn write_tar_gz_entry(archive: &Path, name: &str, contents: &[u8]) {
        let mut tar = Vec::new();
        let mut header = [0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..108].copy_from_slice(b"0000644\0");
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        header[124..136].copy_from_slice(format!("{:011o}\0", contents.len()).as_bytes());
        header[136..148].copy_from_slice(b"00000000000\0");
        header[156] = b'0'; // regular file
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        header[148..156].copy_from_slice(b"        ");
        let sum: u32 = header.iter().map(|b| *b as u32).sum();
        header[148..156].copy_from_slice(format!("{:06o}\0 ", sum).as_bytes());
        tar.extend_from_slice(&header);
        tar.extend_from_slice(contents);
        tar.resize(tar.len().div_ceil(512) * 512, 0);
        tar.resize(tar.len() + 1024, 0); // two zero blocks
        let raw = root_raw_path(archive);
        std::fs::write(&raw, &tar).unwrap();
        let output = Command::new("gzip").arg("-f").arg(&raw).output().unwrap();
        assert!(output.status.success());
        let gz = raw.with_extension("tar.gz");
        if gz != archive {
            std::fs::rename(&gz, archive).unwrap();
        }
    }

    fn root_raw_path(archive: &Path) -> PathBuf {
        archive.with_extension("tar")
    }

    // ── compatibility.go ────────────────────────────────────────────────

    /// `TestValidateUpgradeCompatibilityTreatsLegacyReleaseAsE2EEV1`.
    #[test]
    fn compatibility_treats_legacy_release_as_e2ee_v1() {
        let legacy = Manifest::default();
        let target = Manifest {
            app_transports: vec![lerdr_core::protocol::ENCRYPTED_WEBSOCKET_SUBPROTOCOL.to_owned()],
            relay_transports: vec![lerdr_core::protocol::ENCRYPTED_WEBSOCKET_SUBPROTOCOL.to_owned()],
            ..Manifest::default()
        };
        let err = validate_upgrade_compatibility(&legacy, &target).unwrap_err();
        assert!(err.contains("bridge release"), "{err}");
    }

    /// `TestValidateUpgradeCompatibilityRequiresBothRolloutDirections`.
    #[test]
    fn compatibility_requires_both_rollout_directions() {
        let current = Manifest {
            app_transports: vec!["transport-v1".to_owned()],
            relay_transports: vec!["transport-v1".to_owned()],
            ..Manifest::default()
        };
        for target in [
            Manifest {
                app_transports: vec!["transport-v2".to_owned()],
                relay_transports: vec!["transport-v1".to_owned()],
                ..Manifest::default()
            },
            Manifest {
                app_transports: vec!["transport-v1".to_owned()],
                relay_transports: vec!["transport-v2".to_owned()],
                ..Manifest::default()
            },
        ] {
            let err = validate_upgrade_compatibility(&current, &target).unwrap_err();
            assert!(err.contains("bridge release"), "{err}");
        }
    }

    /// `TestValidateUpgradeCompatibilityAllowsBridgeCutover`.
    #[test]
    fn compatibility_allows_bridge_cutover() {
        let current = Manifest {
            app_transports: vec!["transport-v1".to_owned(), "transport-v2".to_owned()],
            relay_transports: vec!["transport-v1".to_owned(), "transport-v2".to_owned()],
            ..Manifest::default()
        };
        let target = Manifest {
            app_transports: vec!["transport-v2".to_owned()],
            relay_transports: vec!["transport-v2".to_owned()],
            ..Manifest::default()
        };
        validate_upgrade_compatibility(&current, &target).unwrap();
    }
}
