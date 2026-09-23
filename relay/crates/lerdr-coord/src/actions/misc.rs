//! Miscellaneous local actions — updates, slash commands, inventory,
//! copy, app-origin registration.
//!
//! - `check_update`/`install_update`: the oracle's `update.Manager` port —
//!   the persisted `update-state.json` machine, the GitHub release probe
//!   (API → redirect/atom fallback) through `curl`, and the transient-unit
//!   worker schedule. The `update_status` broadcasts Go fans out to every
//!   client are emitted to the requester only.
//! - `list_slash_commands`: provider builtins plus the INI `[skills]`/
//!   `[commands]` escape hatch (`discoverGenericSkills`). Per-agent native
//!   filesystem discovery (`.claude/commands`, provider settings, trust
//!   rules) is not ported — the catalog is smaller than the oracle's but
//!   never invented.
//! - `inventory_status`: the typed `inventory_status` frame the client
//!   already decodes, derived from the topology projection (a committed
//!   snapshot is the Go `inventoryReady` analogue).
//! - `copy_agent_response`: validation order preserved; the clipboard
//!   backend does not exist here, so the answer is the oracle's
//!   clipboard-unavailable failure — never a fabricated payload.
//! - `register_app_origin`: `storePhoneAppOrigin` — validate, persist
//!   `<runtime_dir>/phone-app-origin`, emit nothing either way.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lerdr_core::json::{MaybeNull, RawJson};
use lerdr_core::protocol::{
    error_codes, ActionReceiptPhase, Inbound, Outbound, UpdateStatusMessage,
};
use lerdr_core::sendbuffer::MAX_OUTBOUND_MESSAGE_BYTES;
use lerdr_herdr::AgentInfo;
use tokio::sync::Mutex as AsyncMutex;

use super::{api_error_plain, ActionContext, Outcome};

// ── shared env/path/time helpers ──────────────────────────────────────

/// `relayEnv` — `LERDR_<key>` first, then `HERDR_<key>`; empty is unset.
fn relay_env(key: &str) -> Option<String> {
    std::env::var(format!("LERDR_{key}"))
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| {
            std::env::var(format!("HERDR_{key}"))
                .ok()
                .filter(|v| !v.is_empty())
        })
}

/// `envOr` — a directly-read host variable; empty is unset.
fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// `relayEnvIntOr` — non-numeric values fall back silently.
fn relay_env_int(key: &str) -> Option<i64> {
    relay_env(key).and_then(|v| v.parse().ok())
}

/// `homeDir`.
fn home_dir() -> PathBuf {
    super::local::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// `XDG_CONFIG_HOME`, else `~/.config`.
fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| home_dir().join(".config"))
}

/// `XDG_DATA_HOME`, else `~/.local/share`.
fn data_home() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| home_dir().join(".local/share"))
}

/// `resolveRuntimeDir` — `LERDR_RELAY_ENV`'s directory, then the Herdr
/// plugin config dir, then the adopted config dir. A `--runtime-dir` CLI
/// override is invisible to the action layer, matching env-based service
/// installs exactly.
fn runtime_dir() -> PathBuf {
    relay_env("RELAY_ENV")
        .map(|p| dirname(&p))
        .or_else(|| env_nonempty("HERDR_PLUGIN_CONFIG_DIR").map(PathBuf::from))
        .unwrap_or_else(|| {
            let base = config_home();
            adopt_legacy_dir(base.join("lerdr"), base.join("herdr-mobile-relay"))
        })
}

/// `filepath.Dir` — a bare filename resolves to `.`.
fn dirname(path: &str) -> PathBuf {
    let parent = Path::new(path).parent().unwrap_or_else(|| Path::new(""));
    if parent.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        parent.to_path_buf()
    }
}

/// `adoptLegacyDir` — the renamed dir wins when present, a pre-rename
/// install stays reachable, a fresh install lands on the new name.
fn adopt_legacy_dir(dir: PathBuf, legacy: PathBuf) -> PathBuf {
    if dir.is_dir() || !legacy.is_dir() {
        dir
    } else {
        legacy
    }
}

/// `os.MkdirAll(dir, mode)` — the mode applies to every directory the
/// call creates; existing directories keep their permissions.
fn mkdir_all(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(mode)
        .create(path)
}

/// `expandTilde` — `~` and `~/x` expand against home.
fn expand_tilde(path: &str, home: &Path) -> String {
    if path == "~" {
        return home.to_string_lossy().into_owned();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home.join(rest).to_string_lossy().into_owned();
    }
    path.to_owned()
}

/// `compact` — whitespace-collapsed, rune-limited.
fn compact(value: &str, limit: usize) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(limit).collect()
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

/// `time.Now().UTC().Format(RFC3339)` — civil conversion, no time crate.
fn now_rfc3339() -> String {
    let secs = now_unix();
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// Howard Hinnant's `civil_from_days` — days since epoch → (y, m, d).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn is_zero(value: &i64) -> bool {
    *value == 0
}

// ── update.State / update.Job ─────────────────────────────────────────

/// `update.State` — the persisted `update-state.json` shape. `eligible`
/// and `can_install` are emitted unconditionally, like the Go struct.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct UpdateState {
    state: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    current_version: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    current_revision: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    available_version: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    available_revision: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    upstream_version: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    upstream_revision: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    target_version: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    target_revision: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    target: String,
    #[serde(skip_serializing_if = "is_zero")]
    checked_at: i64,
    #[serde(skip_serializing_if = "String::is_empty")]
    started_at: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    finished_at: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    mode: String,
    eligible: bool,
    can_install: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    reason: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    error: String,
}

/// `update.Job` — the transient `update-job-<ns>.json` payload the worker
/// consumes.
#[derive(Debug, serde::Serialize)]
struct UpdateJob {
    release_root: String,
    herdr_bin: String,
    target_version: String,
    target_revision: String,
    state_path: String,
    health_url: String,
}

#[derive(Debug, Clone, Default)]
struct ReleaseMetadata {
    version: String,
    revision: String,
}

const CANONICAL_API: &str = "https://api.github.com/repos/IGUNUBLUE/lerdr";
const CANONICAL_WEB: &str = "https://github.com/IGUNUBLUE/lerdr";
/// `updateHTTPTimeout` — the Go client's 15 s request bound.
const HTTP_TIMEOUT_SECS: &str = "15";
/// `io.LimitReader(response.Body, 2*1024*1024)`.
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
/// `updateStartupGrace`.
const STARTUP_GRACE_SECS: i64 = 30;
/// Whether this binary ships the `update-worker` subcommand the launcher
/// spawns. It does not yet — the schedule/launch path is kept for parity
/// but `eligibility` reports the capability gap instead of letting a
/// doomed transient unit report `scheduled` and die.
const UPDATE_WORKER_SUPPORTED: bool = false;
/// `workerEnvironmentKeys`.
const WORKER_ENV_KEYS: [&str; 3] = [
    "LERDR_RELAY_ENV",
    "HERDR_RELAY_ENV",
    "HERDR_PLUGIN_CONFIG_DIR",
];

/// `update.Manager` — env-derived configuration plus the persisted state
/// machine. One process-wide instance behind a mutex, like the oracle's
/// `s.updateM`.
struct UpdateManager {
    release_root: PathBuf,
    runtime_dir: PathBuf,
    herdr_bin: String,
    version: String,
    revision: String,
    health_url: String,
    api_base: String,
    web_base: String,
    token_file: PathBuf,
    /// In-memory metadata cache (`m.metadata`); the state file persists
    /// the rest, so this is the only field that dies with the process.
    metadata: ReleaseMetadata,
    state: UpdateState,
}

impl Clone for UpdateManager {
    fn clone(&self) -> Self {
        Self {
            release_root: self.release_root.clone(),
            runtime_dir: self.runtime_dir.clone(),
            herdr_bin: self.herdr_bin.clone(),
            version: self.version.clone(),
            revision: self.revision.clone(),
            health_url: self.health_url.clone(),
            api_base: self.api_base.clone(),
            web_base: self.web_base.clone(),
            token_file: self.token_file.clone(),
            metadata: self.metadata.clone(),
            state: self.state.clone(),
        }
    }
}

fn update_manager() -> &'static AsyncMutex<UpdateManager> {
    static MANAGER: LazyLock<AsyncMutex<UpdateManager>> =
        LazyLock::new(|| AsyncMutex::new(UpdateManager::from_env()));
    &MANAGER
}

impl UpdateManager {
    /// `update.NewManager` — configuration from the environment, then the
    /// one-time `recoverOrphan(true)` startup pass and a state reload.
    fn from_env() -> Self {
        let release_root = relay_env("RELEASE_ROOT").map(PathBuf::from);
        let release_root = release_root
            .or_else(installed_release_root)
            .unwrap_or_else(|| {
                adopt_legacy_dir(
                    data_home().join("lerdr"),
                    data_home().join("herdr-mobile-relay"),
                )
            });
        let herdr_bin = env_nonempty("HERDR_BIN").unwrap_or_else(find_herdr_bin);
        let port = relay_env_int("RELAY_PORT").unwrap_or(8375);
        let mut manager = Self {
            release_root,
            runtime_dir: runtime_dir(),
            herdr_bin,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            revision: option_env!("LERDR_REVISION").unwrap_or("dev").to_owned(),
            health_url: format!("http://127.0.0.1:{port}/healthz"),
            api_base: CANONICAL_API.to_owned(),
            web_base: CANONICAL_WEB.to_owned(),
            token_file: github_token_file(),
            metadata: ReleaseMetadata::default(),
            state: UpdateState::default(),
        };
        manager.state = manager.load_state();
        manager.recover_orphan_sync(true);
        manager.state = manager.load_state();
        manager
    }

    /// `m.Check(ctx)` — first locked half: reconcile + publish `checking`.
    /// Returns the early answer when a transient state is on disk.
    async fn check_begin(&mut self) -> Option<UpdateState> {
        self.recover_orphan(false).await;
        let current = self.load_state();
        if transient_update_state(&current.state) {
            self.state = current;
            return Some(self.public_state(self.state.clone()));
        }
        self.state = current;
        self.state.started_at.clear();
        self.state.finished_at.clear();
        self.state.target_version.clear();
        self.state.target_revision.clear();
        self.state.state = "checking".to_owned();
        self.state.error.clear();
        self.state.current_version = self.version.clone();
        self.state.current_revision = self.revision.clone();
        let _ = write_state(&self.state_path(), &self.state);
        None
    }

    /// `m.Check(ctx)` — second locked half after the fetch.
    async fn check_finish(&mut self, fetched: Result<ReleaseMetadata, String>) -> UpdateState {
        self.recover_orphan(false).await;
        let current = self.load_state();
        if transient_update_state(&current.state) {
            self.state = current;
            return self.public_state(self.state.clone());
        }
        let metadata = match fetched {
            Err(err) => {
                // `m.state` keeps the pre-fetch "checking" view — the
                // reloaded state was only consulted for the transient
                // check, exactly as the oracle.
                self.state.state = "failed".to_owned();
                self.state.can_install = false;
                self.state.eligible = false;
                self.state.error = compact(&err, 500);
                self.state.checked_at = now_unix();
                let _ = write_state(&self.state_path(), &self.state);
                return self.public_state(self.state.clone());
            }
            Ok(metadata) => metadata,
        };
        self.metadata = metadata.clone();
        let newer = newer_version(&metadata.version, &self.version);
        let (eligible, mode, reason) = self.eligibility();
        self.state = UpdateState {
            state: "current".to_owned(),
            current_version: self.version.clone(),
            current_revision: self.revision.clone(),
            upstream_version: metadata.version.clone(),
            upstream_revision: metadata.revision.clone(),
            checked_at: now_unix(),
            target: current_target(),
            mode,
            eligible,
            can_install: newer && eligible,
            ..UpdateState::default()
        };
        if newer {
            self.state.available_version = metadata.version.clone();
            self.state.available_revision = short_revision(&metadata.revision);
            self.state.target_version = metadata.version;
            self.state.target_revision = metadata.revision;
            if eligible {
                self.state.state = "available".to_owned();
            } else {
                self.state.state = "blocked".to_owned();
                self.state.reason = reason;
            }
        }
        let _ = write_state(&self.state_path(), &self.state);
        self.public_state(self.state.clone())
    }

    /// `m.Schedule(ctx, expectedVersion, expectedRevision)` → `(job, state)`.
    /// The `Err` pair is boxed — `UpdateState` is large.
    async fn schedule(
        &mut self,
        expected_version: &str,
        expected_revision: &str,
    ) -> Result<(String, UpdateState), Box<(String, UpdateState)>> {
        // `Schedule` does not run the orphan pass — `loadState` only.
        let current = self.load_state();
        if !current.state.is_empty() {
            self.state = current;
        }
        let fail = |manager: &UpdateManager, message: &str| {
            Box::new((
                message.to_owned(),
                manager.public_state(manager.state.clone()),
            ))
        };
        if self.state.state != "available" || !self.state.can_install {
            let reason = if self.state.reason.is_empty() {
                "No installable update is available"
            } else {
                self.state.reason.as_str()
            };
            return Err(fail(self, reason));
        }
        if expected_version != self.state.available_version
            || expected_revision != self.state.target_revision
        {
            return Err(fail(
                self,
                "The advertised update changed; check again before installing",
            ));
        }
        if self.metadata.version != expected_version || self.metadata.revision != expected_revision
        {
            match self.fetch_release().await {
                Ok(metadata)
                    if metadata.version == expected_version
                        && metadata.revision == expected_revision =>
                {
                    self.metadata = metadata;
                }
                _ => {
                    return Err(fail(
                        self,
                        "The advertised update changed; check again before installing",
                    ));
                }
            }
        }
        // `os.MkdirAll(m.runtimeDir, 0o700)` — the mode only lands on
        // newly-created directories; existing ones keep theirs.
        if let Err(err) = mkdir_all(&self.runtime_dir, 0o700) {
            return Err(fail(self, &err.to_string()));
        }
        let job_path = self
            .runtime_dir
            .join(format!("update-job-{}.json", now_nanos()));
        let job = UpdateJob {
            release_root: self.release_root.to_string_lossy().into_owned(),
            herdr_bin: self.herdr_bin.clone(),
            target_version: self.metadata.version.clone(),
            target_revision: self.metadata.revision.clone(),
            state_path: self.state_path().to_string_lossy().into_owned(),
            health_url: self.health_url.clone(),
        };
        if let Err(err) = write_json_atomic(&job_path, &job) {
            return Err(fail(self, &format!("persist update job: {err}")));
        }
        self.state.state = "scheduled".to_owned();
        self.state.can_install = false;
        self.state.eligible = true;
        self.state.reason.clear();
        self.state.error.clear();
        self.state.started_at = now_rfc3339();
        self.state.finished_at.clear();
        if let Err(err) = write_state(&self.state_path(), &self.state) {
            let _ = std::fs::remove_file(&job_path);
            return Err(fail(self, &format!("persist scheduled update: {err}")));
        }
        if let Err(err) = launch_worker(&job_path).await {
            self.state.state = "failed".to_owned();
            self.state.error = compact(&err, 500);
            let _ = write_state(&self.state_path(), &self.state);
            let _ = std::fs::remove_file(&job_path);
            return Err(fail(self, &err));
        }
        let name = job_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok((name, self.public_state(self.state.clone())))
    }

    /// `m.fetchRelease` — GitHub API first; the web redirect + atom feed
    /// only when the API rate-limits (403/429).
    async fn fetch_release(&self) -> Result<ReleaseMetadata, String> {
        let api_result = async {
            let release: GithubRelease = self
                .get_json(&format!("{}/releases/latest", self.api_base))
                .await?;
            self.release_metadata_for_tag(&release.tag_name, release.draft, release.prerelease)
                .await
        }
        .await;
        match api_result {
            Ok(metadata) => Ok(metadata),
            Err(api_err) => {
                if !github_api_rate_limited(&api_err) {
                    return Err(format!("read canonical release: {api_err}"));
                }
                match self.fetch_latest_stable_release().await {
                    Ok(metadata) => Ok(metadata),
                    Err(fallback_err) => Err(format!(
                        "read canonical release: {api_err}; web fallback: {fallback_err}"
                    )),
                }
            }
        }
    }

    async fn release_metadata_for_tag(
        &self,
        tag: &str,
        draft: bool,
        prerelease: bool,
    ) -> Result<ReleaseMetadata, String> {
        if draft || prerelease {
            return Err("canonical latest release is not a stable published release".to_owned());
        }
        let version = tag.strip_prefix('v').unwrap_or(tag);
        if !semver_valid(version) {
            return Err(format!("release tag {tag:?} is not semantic versioned"));
        }
        let revision = self.fetch_tag_revision(tag).await?;
        Ok(ReleaseMetadata {
            version: version.to_owned(),
            revision,
        })
    }

    async fn fetch_tag_revision(&self, tag: &str) -> Result<String, String> {
        #[derive(serde::Deserialize, Default)]
        struct TagRef {
            #[serde(default)]
            object: GitObject,
        }
        let tag_url = format!("{}/git/ref/tags/{}", self.api_base, path_escape(tag));
        let mut object = self
            .get_json::<TagRef>(&tag_url)
            .await
            .map_err(|e| format!("resolve release tag: {e}"))?
            .object;
        for _ in 0..3 {
            if object.object_type == "commit" && valid_revision(&object.sha) {
                return Ok(object.sha.to_lowercase());
            }
            if object.object_type != "tag" || object.url.is_empty() {
                break;
            }
            let annotated: GitObject = self
                .get_json(&object.url)
                .await
                .map_err(|e| format!("resolve annotated release tag: {e}"))?;
            let Some(inner) = annotated.object.map(|boxed| *boxed) else {
                break;
            };
            object = GitObject {
                sha: inner.sha,
                object_type: inner.object_type,
                url: inner.url,
                object: None,
            };
        }
        Err("release tag did not resolve to an exact commit".to_owned())
    }

    async fn fetch_latest_stable_release(&self) -> Result<ReleaseMetadata, String> {
        let tag = self.fetch_latest_stable_tag().await?;
        let revision = self
            .fetch_feed_tag_revision(&tag)
            .await
            .map_err(|e| format!("resolve release tag {tag}: {e}"))?;
        Ok(ReleaseMetadata {
            version: tag.strip_prefix('v').unwrap_or(&tag).to_owned(),
            revision,
        })
    }

    /// `m.fetchLatestStableTag` — read the `/releases/latest` redirect
    /// without following it; the Location names the newest stable tag.
    async fn fetch_latest_stable_tag(&self) -> Result<String, String> {
        let endpoint = format!("{}/releases/latest", self.web_base.trim_end_matches('/'));
        let headers = [
            "Accept: text/html".to_owned(),
            "User-Agent: lerdr-update-check".to_owned(),
        ];
        let (code, location) = curl_redirect(&endpoint, &headers).await?;
        if !matches!(code, 301 | 302 | 303 | 307 | 308) {
            return Err(format!("latest release did not redirect: HTTP {code}"));
        }
        let location = location.trim();
        if location.is_empty() {
            return Err("latest release redirect has no Location header".to_owned());
        }
        // `url.Parse` keeps relative locations usable — fall back to the
        // raw value as the path when it is not absolute.
        let path = url::Url::parse(location)
            .map(|u| u.path().to_owned())
            .unwrap_or_else(|_| location.to_owned());
        let tag_path = path.trim_end_matches('/');
        let tag = tag_path.rsplit('/').next().unwrap_or(tag_path);
        if !tag.starts_with('v') || !semver_valid(tag.trim_start_matches('v')) {
            return Err(format!(
                "latest release tag {tag:?} is not semantic versioned"
            ));
        }
        Ok(tag.to_owned())
    }

    /// `m.fetchFeedTagRevision` — `<web>/commits/<tag>.atom`, first entry
    /// id's trailing revision.
    async fn fetch_feed_tag_revision(&self, tag: &str) -> Result<String, String> {
        let feed_url = format!(
            "{}/commits/{}.atom",
            self.web_base.trim_end_matches('/'),
            path_escape(tag)
        );
        let body = self
            .get_xml(&feed_url)
            .await
            .map_err(|e| format!("read commit feed: {e}"))?;
        for id in atom_entry_ids(&body) {
            let id = id.trim();
            let revision = id.rsplit('/').next().unwrap_or(id);
            if valid_revision(revision) {
                return Ok(revision.to_lowercase());
            }
        }
        Err("commit feed did not contain an exact revision".to_owned())
    }

    /// `m.getJSON` — Accept + UA + optional token, 200-or-error, 2 MiB cap.
    async fn get_json<T: serde::de::DeserializeOwned>(&self, endpoint: &str) -> Result<T, String> {
        let mut headers = vec![
            "Accept: application/vnd.github+json".to_owned(),
            "User-Agent: lerdr-update-check".to_owned(),
        ];
        if let Some(token) = self.token() {
            headers.push(format!("Authorization: token {token}"));
        }
        let (code, body) = curl_get(endpoint, &headers).await?;
        if code != 200 {
            return Err(format!("HTTP {code}"));
        }
        let body = &body[..body.len().min(MAX_RESPONSE_BYTES)];
        serde_json::from_slice(body).map_err(|e| e.to_string())
    }

    /// `m.getXML` — same transport, atom accept header, raw body.
    async fn get_xml(&self, endpoint: &str) -> Result<Vec<u8>, String> {
        let headers = [
            "Accept: application/atom+xml, application/xml".to_owned(),
            "User-Agent: lerdr-update-check".to_owned(),
        ];
        let (code, body) = curl_get(endpoint, &headers).await?;
        if code != 200 {
            return Err(format!("HTTP {code}"));
        }
        Ok(body[..body.len().min(MAX_RESPONSE_BYTES)].to_vec())
    }

    /// `m.token` — trimmed token file contents, when configured.
    fn token(&self) -> Option<String> {
        if self.token_file.as_os_str().is_empty() {
            return None;
        }
        std::fs::read_to_string(&self.token_file)
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    }

    /// `m.eligibility` — managed updates need a released build and an
    /// absolute, executable Herdr path.
    fn eligibility(&self) -> (bool, String, String) {
        if !semver_valid(&self.version) || !valid_revision(&self.revision) {
            return (
                false,
                "unsupported".to_owned(),
                "Managed updates require a released relay build".to_owned(),
            );
        }
        let bin = Path::new(&self.herdr_bin);
        if !bin.is_absolute() {
            return (
                false,
                "unsupported".to_owned(),
                "Managed updates require an absolute Herdr executable path".to_owned(),
            );
        }
        let executable = std::fs::metadata(bin)
            .map(|m| {
                use std::os::unix::fs::PermissionsExt;
                m.is_file() && m.permissions().mode() & 0o111 != 0
            })
            .unwrap_or(false);
        if !executable {
            return (
                false,
                "unsupported".to_owned(),
                "The Herdr executable is unavailable".to_owned(),
            );
        }
        // The `update-worker` subcommand is not implemented in this
        // binary, so no managed update could ever run — surface that as
        // ineligibility rather than a failed schedule attempt.
        if !UPDATE_WORKER_SUPPORTED {
            return (
                false,
                "unsupported".to_owned(),
                "Managed updates are unavailable in this build".to_owned(),
            );
        }
        (true, "plugin".to_owned(), String::new())
    }

    /// `m.loadState` — persisted state with the running build's identity
    /// stamped, transient/target reconciliation, and the no-longer-newer
    /// demotion of `available`/`blocked`.
    fn load_state(&self) -> UpdateState {
        let default_state = || UpdateState {
            state: "checking".to_owned(),
            current_version: self.version.clone(),
            current_revision: self.revision.clone(),
            target: current_target(),
            ..UpdateState::default()
        };
        let Ok(data) = std::fs::read(self.state_path()) else {
            return default_state();
        };
        let Ok(mut state) = serde_json::from_slice::<UpdateState>(&data) else {
            return default_state();
        };
        if !valid_state(&state.state) {
            return default_state();
        }
        state.current_version = self.version.clone();
        state.current_revision = self.revision.clone();
        if (transient_update_state(&state.state)
            || (state.state == "failed" && !state.started_at.is_empty()))
            && state.target_version == self.version
            && state.target_revision.eq_ignore_ascii_case(&self.revision)
        {
            state.can_install = false;
            state.eligible = true;
            state.mode = "plugin".to_owned();
            state.error.clear();
            if state.finished_at.is_empty() {
                state.finished_at = now_rfc3339();
            }
            let _ = write_state(&self.state_path(), &state);
            return state;
        }
        if state.state == "available" || state.state == "blocked" {
            let candidate = if state.available_version.is_empty() {
                state.target_version.clone()
            } else {
                state.available_version.clone()
            };
            if !newer_version(&candidate, &self.version) {
                let upstream_version = if state.upstream_version.is_empty() {
                    candidate
                } else {
                    state.upstream_version.clone()
                };
                let upstream_revision = if state.upstream_revision.is_empty() {
                    state.target_revision.clone()
                } else {
                    state.upstream_revision.clone()
                };
                return UpdateState {
                    state: "current".to_owned(),
                    current_version: self.version.clone(),
                    current_revision: self.revision.clone(),
                    upstream_version,
                    upstream_revision,
                    checked_at: state.checked_at,
                    target: current_target(),
                    mode: state.mode.clone(),
                    eligible: state.eligible,
                    ..UpdateState::default()
                };
            }
        }
        state
    }

    /// `m.recoverOrphan` — a transient state whose target is the running
    /// build became `succeeded`; a transient state whose worker is gone
    /// (past the startup grace, lock acquirable) became `failed`.
    /// `orphan_candidate` runs every check up to the lock probe; the
    /// caller supplies the probe so the startup pass can stay sync.
    fn orphan_candidate(&self, include_scheduled: bool) -> Option<(PathBuf, UpdateState)> {
        let state_path = self.state_path();
        let Ok(mut state) = read_state(&state_path) else {
            return None;
        };
        let reconcilable = transient_update_state(&state.state)
            || (state.state == "failed" && !state.started_at.is_empty());
        if !reconcilable {
            return None;
        }
        if state.target_version == self.version
            && state.target_revision.eq_ignore_ascii_case(&self.revision)
        {
            state.state = "succeeded".to_owned();
            state.current_version = self.version.clone();
            state.current_revision = self.revision.clone();
            state.mode = "plugin".to_owned();
            state.eligible = true;
            state.can_install = false;
            state.error.clear();
            state.finished_at = now_rfc3339();
            let _ = write_state(&state_path, &state);
            return None;
        }
        if state.state == "failed" {
            return None;
        }
        let started = parse_rfc3339(&state.started_at);
        if let Some(started) = started {
            if now_unix() - started < STARTUP_GRACE_SECS {
                return None;
            }
        }
        if state.state == "scheduled" && !include_scheduled && started.is_none() {
            return None;
        }
        if self.release_root.as_os_str().is_empty() || !self.release_root.is_absolute() {
            return None;
        }
        if mkdir_all(&self.release_root, 0o700).is_err() {
            return None;
        }
        Some((state_path, state))
    }

    fn finish_orphan(&self, state_path: &Path, mut state: UpdateState) {
        state.state = "failed".to_owned();
        state.finished_at = now_rfc3339();
        state.error =
            "Herdr plugin update worker stopped before completion; run the update again".to_owned();
        let _ = write_state(state_path, &state);
    }

    /// Async probe (`flock -n -x` via tokio) — the per-check variant.
    /// A `false` answer also covers "no flock binary", the same skip Go
    /// takes when the lock cannot be acquired.
    async fn recover_orphan(&self, include_scheduled: bool) {
        let Some((state_path, state)) = self.orphan_candidate(include_scheduled) else {
            return;
        };
        if lock_acquirable(&self.release_root.join("update.lock")).await {
            self.finish_orphan(&state_path, state);
        }
    }

    /// `NewManager`'s startup pass — synchronous `flock` probe.
    fn recover_orphan_sync(&self, include_scheduled: bool) {
        let Some((state_path, state)) = self.orphan_candidate(include_scheduled) else {
            return;
        };
        if lock_acquirable_sync(&self.release_root.join("update.lock")) {
            self.finish_orphan(&state_path, state);
        }
    }

    /// `m.publicState` — stamp identity, bound free-text fields, shorten
    /// the advertised revision.
    fn public_state(&self, mut state: UpdateState) -> UpdateState {
        state.current_version = self.version.clone();
        state.current_revision = self.revision.clone();
        state.error = compact(&state.error, 500);
        state.reason = compact(&state.reason, 500);
        state.available_revision = short_revision(&state.available_revision);
        state
    }

    fn state_path(&self) -> PathBuf {
        self.runtime_dir.join("update-state.json")
    }
}

#[derive(serde::Deserialize, Default)]
struct GithubRelease {
    #[serde(default)]
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

#[derive(serde::Deserialize, Default)]
struct GitObject {
    #[serde(default)]
    sha: String,
    #[serde(default, rename = "type")]
    object_type: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    object: Option<Box<GitObject>>,
}

/// `githubAPIRateLimited` — the fallback triggers only on 403/429.
fn github_api_rate_limited(err: &str) -> bool {
    err.contains("HTTP 403") || err.contains("HTTP 429")
}

/// `validState`.
fn valid_state(value: &str) -> bool {
    matches!(
        value,
        "current"
            | "checking"
            | "available"
            | "blocked"
            | "scheduled"
            | "preparing"
            | "installing"
            | "restarting"
            | "recovering"
            | "succeeded"
            | "failed"
            | "rolled_back"
            | "unsupported"
    )
}

/// `transientUpdateState`.
fn transient_update_state(value: &str) -> bool {
    matches!(
        value,
        "scheduled" | "preparing" | "installing" | "restarting" | "recovering"
    )
}

/// `validRevision` — exactly 40 hex characters.
fn valid_revision(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `shortRevision` — at most 12 bytes.
fn short_revision(value: &str) -> String {
    value.get(..12).unwrap_or(value).to_owned()
}

/// `semverPattern` — `X.Y.Z`, no leading zeros.
fn semver_valid(value: &str) -> bool {
    parse_semver(value).is_some()
}

/// `parseSemver` — three dot-separated numeric groups.
fn parse_semver(value: &str) -> Option<[u64; 3]> {
    let mut parts = value.split('.');
    let mut out = [0u64; 3];
    for slot in &mut out {
        let part = parts.next()?;
        if part.is_empty()
            || (part.len() > 1 && part.starts_with('0'))
            || !part.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        *slot = part.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some(out)
}

/// `NewerVersion` — strict semver comparison.
fn newer_version(candidate: &str, current: &str) -> bool {
    let (Some(next), Some(installed)) = (parse_semver(candidate), parse_semver(current)) else {
        return false;
    };
    next > installed
}

/// `release.CurrentTarget` — `GOOS/GOARCH` naming.
fn current_target() -> String {
    let goos = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    let goarch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    };
    format!("{goos}/{goarch}")
}

/// `url.PathEscape` for a path segment — unreserved + sub-delims + `:@`.
fn path_escape(value: &str) -> String {
    const KEEP: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~!$&'()*+,;=:@";
    let mut out = String::new();
    for &b in value.as_bytes() {
        if KEEP.contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `installedReleaseRoot` — the relay lives at
/// `<root>/releases/<ver>/herdr` with a `<root>/current` entry.
fn installed_release_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let resolved = std::fs::canonicalize(&exe).unwrap_or(exe);
    let release_dir = resolved.parent()?;
    let releases_dir = release_dir.parent()?;
    if releases_dir.file_name()? != "releases" {
        return None;
    }
    let root = releases_dir.parent()?;
    if root.join("current").symlink_metadata().is_err() {
        return None;
    }
    Some(root.to_path_buf())
}

/// `findHerdrBin` — PATH first, then the known install locations.
fn find_herdr_bin() -> String {
    if let Some(path) = look_path("herdr") {
        return path;
    }
    for candidate in [
        home_dir().join(".local/bin/herdr"),
        PathBuf::from("/opt/homebrew/bin/herdr"),
        PathBuf::from("/usr/local/bin/herdr"),
        PathBuf::from("/home/linuxbrew/.linuxbrew/bin/herdr"),
        PathBuf::from("/home/linuxbrew/.linuxbrew/opt/herdr/bin/herdr"),
    ] {
        if candidate.is_file() {
            return candidate.to_string_lossy().into_owned();
        }
    }
    "herdr".to_owned()
}

/// `exec.LookPath` — regular file with any execute bit.
fn look_path(name: &str) -> Option<String> {
    #[cfg(unix)]
    fn executable(path: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    fn executable(path: &Path) -> bool {
        path.is_file()
    }
    if name.contains('/') {
        return executable(Path::new(name)).then(|| name.to_owned());
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if executable(&candidate) {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

fn github_token_file() -> PathBuf {
    for key in ["LERDR_GITHUB_TOKEN_FILE", "HERDR_GITHUB_TOKEN_FILE"] {
        if let Some(value) = env_nonempty(key) {
            let path = PathBuf::from(value.trim());
            if path.is_absolute() {
                return path;
            }
        }
    }
    PathBuf::new()
}

/// `writeJSONAtomic` — temp in the same dir, 0600, write+sync, rename.
fn write_json_atomic<T: serde::Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let data = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    let mut data = data;
    data.push(b'\n');
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    mkdir_all(dir, 0o700)?;
    let base = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "tmp".to_owned());
    let temp = dir.join(format!(".{base}.{}", now_nanos()));
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(&data)?;
        file.sync_all()?;
    }
    let result = std::fs::rename(&temp, path);
    let _ = std::fs::remove_file(&temp);
    result
}

/// `writeState` — atomic write plus a directory fsync.
fn write_state(path: &Path, state: &UpdateState) -> std::io::Result<()> {
    write_json_atomic(path, state)?;
    if let Ok(dir) = std::fs::File::open(path.parent().unwrap_or_else(|| Path::new("."))) {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// `readState`.
fn read_state(path: &Path) -> std::io::Result<UpdateState> {
    let data = std::fs::read(path)?;
    serde_json::from_slice(&data).map_err(std::io::Error::other)
}

/// `acquireLock` probe — `flock -n -x <file> -c true` succeeds only when
/// no worker holds the lock. `false` also covers "no flock binary", the
/// conservative answer.
async fn lock_acquirable(path: &Path) -> bool {
    {
        use std::os::unix::fs::OpenOptionsExt;
        let created = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path);
        if created.is_err() {
            return false;
        }
    }
    tokio::process::Command::new("flock")
        .arg("-n")
        .arg("-x")
        .arg(path)
        .arg("-c")
        .arg("true")
        .output()
        .await
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// The blocking probe for `NewManager`'s startup pass — same `flock`
/// semantics through `std::process`.
fn lock_acquirable_sync(path: &Path) -> bool {
    {
        use std::os::unix::fs::OpenOptionsExt;
        let created = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path);
        if created.is_err() {
            return false;
        }
    }
    std::process::Command::new("flock")
        .arg("-n")
        .arg("-x")
        .arg(path)
        .arg("-c")
        .arg("true")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// `updateWorkerLaunch` — `launchctl submit` on darwin, `systemd-run
/// --user` elsewhere; the worker env keys pass through.
async fn launch_worker(job_path: &Path) -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let label = format!("lerdr-update-{}", now_unix());
    let assignments: Vec<String> = WORKER_ENV_KEYS
        .iter()
        .filter_map(|&key| {
            std::env::var(key)
                .ok()
                .filter(|v| !v.trim().is_empty())
                .map(|value| format!("{key}={value}"))
        })
        .collect();
    let worker = [
        executable.to_string_lossy().into_owned(),
        "update-worker".to_owned(),
        job_path.to_string_lossy().into_owned(),
    ];
    let (application, args) = if cfg!(target_os = "macos") {
        let mut args = vec!["submit".to_owned(), "-l".to_owned(), label, "--".to_owned()];
        if !assignments.is_empty() {
            args.push("/usr/bin/env".to_owned());
            args.extend(assignments);
        }
        args.extend(worker);
        ("launchctl".to_owned(), args)
    } else {
        let mut args = vec![
            "--user".to_owned(),
            "--collect".to_owned(),
            format!("--unit={label}"),
        ];
        args.extend(assignments.iter().map(|a| format!("--setenv={a}")));
        args.extend(worker);
        ("systemd-run".to_owned(), args)
    };
    let output = tokio::process::Command::new(application)
        .args(&args)
        .output()
        .await
        .map_err(|e| format!("schedule update worker: {e}"))?;
    if !output.status.success() {
        let mut combined = output.stdout;
        combined.extend_from_slice(&output.stderr);
        return Err(format!(
            "schedule update worker: {}: {}",
            output.status,
            compact(&String::from_utf8_lossy(&combined), 300)
        ));
    }
    Ok(())
}

/// `curl -sS -L` — status in the trailer, body on stdout. `--max-time`
/// matches the Go client timeout; a spawn failure is the `client.Do`
/// error analogue.
async fn curl_get(endpoint: &str, headers: &[String]) -> Result<(u16, Vec<u8>), String> {
    let mut command = tokio::process::Command::new("curl");
    command
        .arg("-sS")
        .arg("-L")
        .arg("--max-redirs")
        .arg("10")
        .arg("--max-time")
        .arg(HTTP_TIMEOUT_SECS);
    for header in headers {
        command.arg("-H").arg(header);
    }
    command.arg("-w").arg("\n%{http_code}").arg(endpoint);
    let output = tokio::time::timeout(Duration::from_secs(20), command.output())
        .await
        .map_err(|_| "curl timed out".to_owned())?
        .map_err(|e| format!("curl: {e}"))?;
    if !output.status.success() {
        let detail = compact(&String::from_utf8_lossy(&output.stderr), 300);
        return Err(format!("curl failed: {detail}"));
    }
    let stdout = output.stdout;
    let Some(pos) = stdout.iter().rposition(|b| *b == b'\n') else {
        return Err("malformed curl output".to_owned());
    };
    let code: u16 = String::from_utf8_lossy(&stdout[pos + 1..])
        .trim()
        .parse()
        .map_err(|_| "malformed curl status".to_owned())?;
    Ok((code, stdout[..pos].to_vec()))
}

/// `curl` redirect probe — status + Location without following.
async fn curl_redirect(endpoint: &str, headers: &[String]) -> Result<(u16, String), String> {
    let mut command = tokio::process::Command::new("curl");
    command
        .arg("-sS")
        .arg("-o")
        .arg("/dev/null")
        .arg("--max-time")
        .arg(HTTP_TIMEOUT_SECS);
    for header in headers {
        command.arg("-H").arg(header);
    }
    command
        .arg("-w")
        .arg("%{http_code}\n%{redirect_url}")
        .arg(endpoint);
    let output = tokio::time::timeout(Duration::from_secs(20), command.output())
        .await
        .map_err(|_| "curl timed out".to_owned())?
        .map_err(|e| format!("curl: {e}"))?;
    if !output.status.success() {
        let detail = compact(&String::from_utf8_lossy(&output.stderr), 300);
        return Err(format!("curl failed: {detail}"));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines = text.lines();
    let code: u16 = lines
        .next()
        .unwrap_or_default()
        .trim()
        .parse()
        .map_err(|_| "malformed curl status".to_owned())?;
    let location = lines.next().unwrap_or_default().to_owned();
    Ok((code, location))
}

/// The atom feed's `<entry><id>` values — a bounded scan scoped to entry
/// elements (the feed-level `<id>` is not an entry, matching Go's
/// `xml:"entry"` unmarshal), not an XML parser.
fn atom_entry_ids(body: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(body);
    let mut ids = Vec::new();
    let mut rest = text.as_ref();
    while let Some(entry_start) = rest.find("<entry") {
        rest = &rest[entry_start + 6..];
        let entry_end = rest.find("</entry>").unwrap_or(rest.len());
        let entry = &rest[..entry_end];
        if let Some(id_start) = entry.find("<id>") {
            if let Some(id_end) = entry[id_start + 4..].find("</id>") {
                ids.push(entry[id_start + 4..id_start + 4 + id_end].to_owned());
            }
        }
        rest = &rest[entry_end..];
    }
    ids
}

/// Minimal RFC3339 `time.Parse` — `YYYY-MM-DDTHH:MM:SSZ` (the only shape
/// `writeState` produces; a parse failure preserves Go's `startedErr`
/// semantics).
fn parse_rfc3339(value: &str) -> Option<i64> {
    let v = value.trim();
    if v.len() < 20 {
        return None;
    }
    let year: i64 = v.get(0..4)?.parse().ok()?;
    let month: i64 = v.get(5..7)?.parse().ok()?;
    let day: i64 = v.get(8..10)?.parse().ok()?;
    let hour: i64 = v.get(11..13)?.parse().ok()?;
    let minute: i64 = v.get(14..16)?.parse().ok()?;
    let second: i64 = v.get(17..19)?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Days-from-civil (Hinnant) → unix seconds.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
}

// ── handlers ──────────────────────────────────────────────────────────

fn update_status_frame(update: serde_json::Value) -> Outbound {
    let raw = serde_json::value::to_raw_value(&update).ok();
    Outbound::UpdateStatus(UpdateStatusMessage {
        r#type: "update_status".to_owned(),
        update: raw.map(|r| MaybeNull::Value(RawJson(r))),
    })
}

/// `check_update` — `update_status` notices plus a `command_result`
/// carrying `{"update": <state>}`.
pub(crate) async fn check_update(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    let checking = update_status_frame(serde_json::json!({
        "state": "checking",
        "current_version": env!("CARGO_PKG_VERSION"),
        "current_revision": option_env!("LERDR_REVISION").unwrap_or("dev"),
    }));
    // `s.hub.Broadcast` — peer sessions see the check begin too.
    ctx.notices.send(checking.clone(), ctx.client_id.clone());
    let mut frames = vec![checking];
    // The network fetch runs unlocked, like the oracle's mutex release —
    // concurrent state()/schedule() calls observe the `checking` state.
    let (early, fetcher) = {
        let mut manager = update_manager().lock().await;
        (manager.check_begin().await, manager.clone())
    };
    let state = match early {
        Some(state) => state,
        None => {
            let fetched = fetcher.fetch_release().await;
            update_manager().lock().await.check_finish(fetched).await
        }
    };
    let checked = update_status_frame(serde_json::to_value(&state).unwrap_or_default());
    ctx.notices.send(checked.clone(), ctx.client_id.clone());
    frames.push(checked);
    frames.extend(
        Outcome::completed("", Some(serde_json::json!({ "update": state }))).frames(
            request_id,
            "check_update",
            action_id,
        ),
    );
    frames
}

/// `install_update` — schedules the pending update; `command_result`
/// carries `{"job","update"}` on success, `{"update"}` on failure.
pub(crate) async fn install_update(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    let result = update_manager()
        .lock()
        .await
        .schedule(&message.expected_version, &message.expected_revision)
        .await;
    match result {
        Ok((job, state)) => {
            let scheduled = update_status_frame(serde_json::to_value(&state).unwrap_or_default());
            // `watchJobStates` — the schedule change reaches every client.
            ctx.notices.send(scheduled.clone(), ctx.client_id.clone());
            let mut frames = vec![scheduled];
            frames.extend(
                Outcome {
                    ok: true,
                    phase: "scheduled",
                    error: String::new(),
                    pane_id: String::new(),
                    data: Some(serde_json::json!({ "job": job, "update": state })),
                    receipt_phase: ActionReceiptPhase::CONFIRMED,
                    receipt_error: None,
                }
                .frames(request_id, "install_update", action_id),
            );
            frames
        }
        Err(err) => {
            let (message, state) = *err;
            Outcome {
                ok: false,
                phase: "failed",
                error: message.clone(),
                pane_id: String::new(),
                data: Some(serde_json::json!({ "update": state })),
                receipt_phase: ActionReceiptPhase::FAILED_BEFORE_DISPATCH,
                receipt_error: Some(api_error_plain(error_codes::INVALID_REQUEST, &message)),
            }
            .frames(request_id, "install_update", action_id)
        }
    }
}

/// `list_slash_commands` — the pane agent's slash-command catalog.
pub(crate) async fn slash_commands(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    let pane_id = message.pane_id.as_str();
    if pane_id.is_empty() {
        return Outcome::failed(pane_id, "Agent is required").frames(
            request_id,
            "list_slash_commands",
            action_id,
        );
    }
    let Some(pane) = ctx.topology.pane_of(pane_id) else {
        return Outcome::failed(pane_id, "Agent pane not found").frames(
            request_id,
            "list_slash_commands",
            action_id,
        );
    };
    let agent = pane.agent.clone().unwrap_or_default();
    let cwd = pane.cwd.clone().unwrap_or_default();
    let foreground = pane.foreground_cwd.clone().unwrap_or_default();

    let profile_id = ctx
        .profiles
        .resolve_pane(&ctx.client, pane_id, &agent)
        .await;
    let (skill_dirs, command_format, suppress_native) = command_discovery(&profile_id);
    // Go's `agentVersion`/`agentDir`/`project` inputs feed native
    // discovery only — unneeded while that stays unported.
    let mut catalog = catalog_for_profile(
        &profile_id,
        &agent,
        &skill_dirs,
        &command_format,
        suppress_native,
    );
    // `Generation` re-check — the fields the catalog consumed must be
    // unchanged in the latest topology.
    let current = ctx.handle.topology.borrow();
    match current.pane_of(pane_id) {
        Some(now)
            if now.agent.as_deref().unwrap_or_default() == agent
                && now.cwd.as_deref().unwrap_or_default() == cwd
                && now.foreground_cwd.as_deref().unwrap_or_default() == foreground => {}
        _ => {
            return Outcome::failed(
                pane_id,
                "The agent pane was replaced while commands were being listed",
            )
            .frames(request_id, "list_slash_commands", action_id);
        }
    }
    catalog = fit_catalog(catalog, request_id, pane_id);
    Outcome::completed(
        pane_id,
        Some(serde_json::to_value(&catalog).unwrap_or_default()),
    )
    .frames(request_id, "list_slash_commands", action_id)
}

/// `inventory_status` — the typed frame the client already consumes,
/// then the result/receipt pair. The committed view's poll ledger drives
/// the same `inventoryStatusLocked` projection the broadcast uses.
pub(crate) async fn inventory_status(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    let mut frames = vec![Outbound::InventoryStatus(
        crate::snapshot::inventory_status(&ctx.topology),
    )];
    frames.extend(Outcome::completed("", None).frames(request_id, "inventory_status", action_id));
    frames
}

/// `copy_agent_response` — the oracle's validation order, then the
/// clipboard-unavailable failure this relay always produces (no host
/// clipboard backend exists here; Go answers the same on such hosts).
pub(crate) async fn copy_agent_response(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    let pane_id = message.pane_id.as_str();
    if pane_id.is_empty() {
        return Outcome::failed(pane_id, "Agent is required").frames(
            request_id,
            "copy_agent_response",
            action_id,
        );
    }
    let outcome = match copy_preflight(ctx.topology.pane_of(pane_id)) {
        Some(error) => Outcome::failed(pane_id, error),
        None => Outcome::failed(pane_id, "Host clipboard is unavailable"),
    };
    outcome.frames(request_id, "copy_agent_response", action_id)
}

/// The pane/status half of `copyAgentResponse` — pane presence and
/// `working` status. The oracle's attention-kind branch (`blocked` +
/// question/approval) has no field in the Herdr snapshot, so a blocked
/// pane falls through to the clipboard check — the same path Go takes
/// when `AttentionKind` does not match.
fn copy_preflight(pane: Option<&AgentInfo>) -> Option<&'static str> {
    let Some(pane) = pane else {
        return Some("Agent pane not found");
    };
    if pane.agent_status == lerdr_herdr::AgentStatus::Working {
        return Some("Agent is still working; wait for the current turn to finish");
    }
    None
}

/// `register_app_origin` — `storePhoneAppOrigin`: validate, persist
/// `<runtime_dir>/phone-app-origin`, never emit a frame.
pub(crate) async fn register_app_origin(
    _ctx: ActionContext,
    _request_id: &str,
    _action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    if let Err(error) = store_phone_app_origin(&message.origin) {
        // The oracle logs the failure and moves on — no result frame.
        tracing::warn!(%error, "phone app origin was not stored");
    }
    Vec::new()
}

/// `storePhoneAppOrigin` — `https` origin only, no userinfo, no
/// path/query/fragment; persisted as `<origin>\n` via temp+rename.
fn store_phone_app_origin(raw: &str) -> Result<(), String> {
    if raw.is_empty() {
        return Err("origin is required".to_owned());
    }
    let parsed = url::Url::parse(raw).map_err(|_| "origin must be an HTTPS origin".to_owned())?;
    // `parsed.User != nil` — any `@` before the host is userinfo.
    let authority = &parsed[..url::Position::BeforeHost];
    let host_port = &parsed[url::Position::BeforeHost..url::Position::AfterPort];
    if parsed.scheme() != "https"
        || parsed.host_str().unwrap_or_default().is_empty()
        || authority.contains('@')
    {
        return Err("origin must be an HTTPS origin".to_owned());
    }
    let path = parsed.path();
    if !(path.is_empty() || path == "/")
        || parsed.query().is_some_and(|q| !q.is_empty())
        || parsed.fragment().is_some_and(|f| !f.is_empty())
    {
        return Err("origin must not contain a path, query, or fragment".to_owned());
    }
    let origin = format!("https://{host_port}");
    let dir = runtime_dir();
    mkdir_all(&dir, 0o700).map_err(|e| e.to_string())?;
    let path = dir.join("phone-app-origin");
    let temp = dir.join("phone-app-origin.tmp");
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        file.write_all(format!("{origin}\n").as_bytes())
            .map_err(|e| e.to_string())?;
    }
    std::fs::rename(&temp, &path).map_err(|e| e.to_string())
}

// ── slash-command catalog ─────────────────────────────────────────────

/// `slashcmd` bounds.
const MAX_CUSTOM_FILES: usize = 2000;
const MAX_ENTRIES: usize = 4096;
const MAX_METADATA_BYTES: u64 = 64 * 1024;

/// `slashcmd.Command`.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct SlashCommand {
    command: String,
    description: String,
    source: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    argument_hint: String,
}

/// `slashcmd.Catalog`.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct SlashCatalog {
    commands: Vec<SlashCommand>,
    truncated: bool,
}

fn cmd(command: &str, description: &str, argument_hint: &str) -> SlashCommand {
    SlashCommand {
        command: command.to_owned(),
        description: description.to_owned(),
        source: "builtin".to_owned(),
        argument_hint: argument_hint.to_owned(),
    }
}

/// `claudeBuiltins`.
fn claude_builtins() -> Vec<SlashCommand> {
    vec![
        cmd("/add-dir", "Add another working directory", "<path>"),
        cmd("/agents", "Manage agent configurations", ""),
        cmd(
            "/batch",
            "Run independent work in parallel worktrees",
            "[task]",
        ),
        cmd(
            "/background",
            "Move the current session to the background",
            "",
        ),
        cmd("/branch", "Fork an earlier conversation", "[session]"),
        cmd("/clear", "Start a fresh conversation", ""),
        cmd(
            "/compact",
            "Summarize the conversation to free context",
            "[instructions]",
        ),
        cmd(
            "/copy",
            "Copy Claude's last response to clipboard (or /copy N for the Nth-latest)",
            "[N]",
        ),
        cmd("/config", "Open Claude Code settings", ""),
        cmd("/context", "Show context-window usage", ""),
        cmd(
            "/debug",
            "Troubleshoot the current Claude Code session",
            "[description]",
        ),
        cmd("/diff", "Show changes in the working tree", ""),
        cmd("/doctor", "Check the Claude Code installation", ""),
        cmd("/effort", "Change the reasoning effort", ""),
        cmd("/exit", "Exit Claude Code", ""),
        cmd("/export", "Export the current conversation", "[path]"),
        cmd("/extra-usage", "Configure extra usage", ""),
        cmd("/feedback", "Report an issue with session context", ""),
        cmd("/fork", "Fork the current conversation", ""),
        cmd(
            "/goal",
            "Set or clear a persistent goal",
            "[condition|clear]",
        ),
        cmd("/help", "Show help and available commands", ""),
        cmd("/hooks", "View hook configuration", ""),
        cmd("/ide", "Manage IDE integrations", ""),
        cmd("/init", "Create a CLAUDE.md project guide", ""),
        cmd("/insights", "Analyze Claude Code session patterns", ""),
        cmd("/login", "Sign in to Claude Code", ""),
        cmd("/logout", "Sign out of Claude Code", ""),
        cmd("/mcp", "Manage MCP servers", ""),
        cmd("/memory", "View or edit project memory", ""),
        cmd("/mobile", "Show the Claude mobile app QR code", ""),
        cmd("/model", "Choose the active Claude model", ""),
        cmd("/permissions", "View or change permission rules", ""),
        cmd("/plan", "Enter plan mode", "[planning prompt]"),
        cmd("/plugin", "Browse and manage plugins", ""),
        cmd("/reload-plugins", "Reload installed plugins", ""),
        cmd(
            "/remote-control",
            "Continue this session from another device",
            "[name]",
        ),
        cmd("/rename", "Rename the current session", "[name]"),
        cmd("/resume", "Resume a saved conversation", "[session]"),
        cmd("/review", "Review a pull request", "[PR]"),
        cmd("/rewind", "Return to an earlier checkpoint", ""),
        cmd(
            "/security-review",
            "Review the current branch for security issues",
            "",
        ),
        cmd(
            "/simplify",
            "Review recent changes for reusable improvements",
            "",
        ),
        cmd("/skills", "Browse available skills", ""),
        cmd("/stats", "Show account usage statistics", ""),
        cmd(
            "/status",
            "Show version, model, account, and connectivity",
            "",
        ),
        cmd("/tasks", "Show background tasks", ""),
        cmd(
            "/teleport",
            "Pull a web session into this terminal",
            "[session]",
        ),
        cmd("/theme", "Choose the terminal theme", ""),
        cmd("/usage", "Show plan and usage information", ""),
        cmd(
            "/verify",
            "Build and observe the application to verify changes",
            "[instructions]",
        ),
        cmd("/voice", "Configure voice dictation", "[hold|tap|off]"),
    ]
}

/// `codexBuiltinsBase` (the only versioned set the oracle ships today).
fn codex_builtins() -> Vec<SlashCommand> {
    vec![
        cmd(
            "/permissions",
            "Change approval and sandbox permissions",
            "",
        ),
        cmd(
            "/ide",
            "Include available IDE context in the next prompt",
            "[instructions]",
        ),
        cmd("/keymap", "View or change terminal keyboard shortcuts", ""),
        cmd("/vim", "Toggle Vim editing mode", ""),
        cmd("/agent", "Switch to another agent thread", ""),
        cmd("/subagents", "Switch to another agent thread", ""),
        cmd("/apps", "Browse available apps and connectors", ""),
        cmd("/plugins", "Browse and manage plugins", ""),
        cmd("/hooks", "View and manage lifecycle hooks", ""),
        cmd("/clear", "Clear the terminal and start a new task", ""),
        cmd("/rename", "Rename the current task", "[name]"),
        cmd("/archive", "Archive the current session and exit", ""),
        cmd(
            "/delete",
            "Permanently delete the current session and exit",
            "",
        ),
        cmd("/compact", "Summarize the conversation to free context", ""),
        cmd("/copy", "Copy the latest completed response", ""),
        cmd("/diff", "Show the current Git diff", ""),
        cmd("/exit", "Exit Codex", ""),
        cmd("/quit", "Exit Codex", ""),
        cmd("/experimental", "Configure experimental features", ""),
        cmd("/approve", "Retry a recent automatic-review denial", ""),
        cmd("/memories", "Configure memory use and generation", ""),
        cmd("/skills", "Browse and use available skills", ""),
        cmd("/import", "Import supported Claude Code configuration", ""),
        cmd("/feedback", "Send feedback and optional diagnostics", ""),
        cmd("/init", "Create an AGENTS.md scaffold", ""),
        cmd("/logout", "Sign out of Codex", ""),
        cmd("/mcp", "Show configured MCP servers and tools", "[verbose]"),
        cmd("/mention", "Attach a file or folder", "[path]"),
        cmd("/model", "Choose the active model and reasoning effort", ""),
        cmd("/fast", "Toggle the Fast service tier when available", ""),
        cmd("/plan", "Switch to plan mode", "[planning prompt]"),
        cmd(
            "/goal",
            "Set or manage a persistent task goal",
            "[objective|edit|pause|resume|clear]",
        ),
        cmd("/personality", "Choose the response style", ""),
        cmd("/ps", "Show background terminals", ""),
        cmd("/stop", "Stop all background terminals", ""),
        cmd("/fork", "Fork the current task", ""),
        cmd("/side", "Start a temporary side conversation", "[question]"),
        cmd("/btw", "Start a temporary side conversation", "[question]"),
        cmd("/raw", "Toggle raw scrollback mode", "[on|off]"),
        cmd("/resume", "Resume a saved conversation", "[session]"),
        cmd("/new", "Start a new task", ""),
        cmd("/review", "Review the working tree", "[instructions]"),
        cmd(
            "/status",
            "Show session configuration and context usage",
            "",
        ),
        cmd(
            "/usage",
            "Show account token usage",
            "[daily|weekly|cumulative]",
        ),
        cmd("/debug-config", "Show configuration layer diagnostics", ""),
        cmd("/statusline", "Configure terminal status-line fields", ""),
        cmd("/title", "Configure the terminal title", ""),
        cmd("/theme", "Choose a syntax-highlighting theme", ""),
        cmd("/pets", "Choose or hide a terminal pet", ""),
        cmd("/pet", "Choose or hide a terminal pet", ""),
    ]
}

/// `qoderBuiltins`.
fn qoder_builtins() -> Vec<SlashCommand> {
    vec![
        cmd("/clear", "Start a fresh conversation", ""),
        cmd(
            "/compact",
            "Summarize and compact conversation history",
            "[instructions]",
        ),
        cmd(
            "/copy",
            "Copy the last assistant response to clipboard, or /copy N for the Nth-latest",
            "[N]",
        ),
        cmd("/config", "View or modify configuration", "[key] [value]"),
        cmd("/cost", "Show token usage and cost", ""),
        cmd("/help", "Show available commands", ""),
        cmd("/model", "Switch the AI model", "[model-name]"),
        cmd("/permissions", "View or modify permissions", ""),
        cmd("/status", "Show session status", ""),
    ]
}

/// `openCodeBuiltins`.
fn opencode_builtins() -> Vec<SlashCommand> {
    vec![
        cmd("/agents", "Switch agent", ""),
        cmd("/connect", "Connect provider", ""),
        cmd("/debug", "View debug info", ""),
        cmd("/diff", "Open diff viewer", ""),
        cmd("/editor", "Open editor", ""),
        cmd("/exit", "Exit the app", ""),
        cmd("/help", "Help", ""),
        cmd("/init", "Guided AGENTS.md setup", "<focus>"),
        cmd("/mcps", "Toggle MCPs", ""),
        cmd("/models", "Switch model", ""),
        cmd("/move", "Move session to another project directory", ""),
        cmd("/new", "New session", ""),
        cmd("/review", "Review changes", "[commit|branch|pr]"),
        cmd("/sessions", "Switch session", ""),
        cmd("/skills", "Browse skills", ""),
        cmd("/status", "View status", ""),
        cmd("/themes", "Switch theme", ""),
    ]
}

/// `hermesBuiltins`.
fn hermes_builtins() -> Vec<SlashCommand> {
    vec![
        cmd(
            "/model",
            "Switch model or view active configuration",
            "[model] [--provider name]",
        ),
        cmd(
            "/usage",
            "Show session tokens, context window, and costs",
            "",
        ),
        cmd("/clear", "Clear screen and start a new session", ""),
        cmd(
            "/new",
            "Start a new session (fresh session ID + history)",
            "[name]",
        ),
        cmd("/reset", "Reset session history", ""),
        cmd("/sessions", "Browse and resume previous sessions", ""),
        cmd("/resume", "Resume a previously-named session", "[name]"),
        cmd(
            "/skills",
            "Search, install, inspect, or manage skills",
            "[search|inspect|install]",
        ),
        cmd(
            "/tools",
            "Manage tools and view tool definitions",
            "[list|enable|disable]",
        ),
        cmd(
            "/compress",
            "Compress conversation context",
            "[focus topic]",
        ),
        cmd(
            "/branch",
            "Branch the current session to explore alternatives",
            "[name]",
        ),
        cmd("/fork", "Fork the current session", "[name]"),
        cmd("/undo", "Back up N user turns and re-prompt", "[N]"),
        cmd("/retry", "Retry the last message (resend to agent)", ""),
        cmd(
            "/status",
            "Show session, model, token, and context info",
            "",
        ),
        cmd(
            "/copy",
            "Copy the last assistant response to clipboard",
            "[number]",
        ),
        cmd(
            "/fast",
            "Toggle fast mode / priority processing",
            "[normal|fast|status]",
        ),
        cmd(
            "/reasoning",
            "Manage reasoning effort and display",
            "[none|low|medium|high]",
        ),
        cmd(
            "/yolo",
            "Toggle YOLO mode (skip dangerous command approvals)",
            "",
        ),
        cmd(
            "/goal",
            "Set a standing goal across turns until achieved",
            "[objective]",
        ),
        cmd("/help", "Show available interactive commands", ""),
        cmd("/exit", "Exit the session", ""),
        cmd("/quit", "Quit Hermes", ""),
    ]
}

/// `kimiBuiltins`.
fn kimi_builtins() -> Vec<SlashCommand> {
    vec![
        cmd(
            "/yolo",
            "Toggle YOLO mode: auto-approve tool actions, but the agent may still ask questions",
            "",
        ),
        cmd(
            "/auto",
            "Toggle Auto mode: fully autonomous, agent decides everything without asking",
            "",
        ),
        cmd("/permission", "Select permission mode", ""),
        cmd("/settings", "Open TUI settings", ""),
        cmd("/plan", "Toggle plan mode", ""),
        cmd(
            "/swarm",
            "Toggle swarm mode or run one task in swarm mode",
            "[on|off] | <task>",
        ),
        cmd("/model", "Switch LLM model", ""),
        cmd("/effort", "Switch thinking effort", ""),
        cmd(
            "/provider",
            "Manage AI providers (add / delete / refresh)",
            "",
        ),
        cmd("/btw", "Ask a forked side agent a question", ""),
        cmd("/help", "Show available commands and shortcuts", ""),
        cmd("/new", "Start a fresh session in the current workspace", ""),
        cmd("/sessions", "Browse and resume sessions", ""),
        cmd("/tasks", "Browse background tasks", ""),
        cmd("/mcp", "Show MCP server status", ""),
        cmd("/plugins", "Manage plugins", ""),
        cmd(
            "/add-dir",
            "Add or list an additional workspace directory",
            "[list] | <path>",
        ),
        cmd("/experiments", "Manage experimental features", ""),
        cmd(
            "/reload",
            "Reload session and apply config.toml settings plus tui.toml UI preferences",
            "",
        ),
        cmd("/reload-tui", "Reload only tui.toml UI preferences", ""),
        cmd(
            "/compact",
            "Compact the conversation context",
            "<instruction>",
        ),
        cmd(
            "/goal",
            "Start or manage an autonomous goal",
            "[status|pause|resume|cancel|replace|next] | <objective>",
        ),
        cmd("/init", "Analyze the codebase and generate AGENTS.md", ""),
        cmd("/fork", "Fork the current session", ""),
        cmd("/title", "Set or show session title", "<title>"),
        cmd(
            "/usage",
            "Show session tokens, context window, and plan quotas",
            "",
        ),
        cmd("/status", "Show current session and runtime status", ""),
        cmd("/feedback", "Send feedback to make Kimi Code better", ""),
        cmd("/undo", "Withdraw the last prompt from the transcript", ""),
        cmd("/editor", "Set the external editor for Ctrl-G", ""),
        cmd("/theme", "Set the terminal UI theme", ""),
        cmd("/logout", "Log out of a configured provider", ""),
        cmd("/login", "Select a platform and authenticate", ""),
        cmd(
            "/export-md",
            "Export current session as a Markdown file",
            "",
        ),
        cmd(
            "/export-debug-zip",
            "Export current session as a debug ZIP archive",
            "",
        ),
        cmd(
            "/copy",
            "Copy the last assistant message to the clipboard",
            "",
        ),
        cmd(
            "/web",
            "Open the current session in the Web UI by starting a new server",
            "",
        ),
        cmd("/exit", "Exit the application", ""),
        cmd("/version", "Show version information", ""),
    ]
}

/// `piBuiltins`.
fn pi_builtins() -> Vec<SlashCommand> {
    vec![
        cmd("/settings", "Open settings menu", ""),
        cmd("/model", "Select the active model", "<provider/model>"),
        cmd("/scoped-models", "Choose models for keyboard cycling", ""),
        cmd("/export", "Export the current session", "[file]"),
        cmd("/import", "Import and resume a JSONL session", "<file>"),
        cmd("/share", "Share the session as a secret GitHub gist", ""),
        cmd("/copy", "Copy the last agent message to the clipboard", ""),
        cmd("/name", "Set the session display name", "<name>"),
        cmd("/session", "Show session information and statistics", ""),
        cmd("/changelog", "Show changelog entries", ""),
        cmd("/hotkeys", "Show all keyboard shortcuts", ""),
        cmd("/fork", "Create a fork from a previous user message", ""),
        cmd(
            "/clone",
            "Duplicate the current session at its current position",
            "",
        ),
        cmd("/tree", "Navigate the session tree", ""),
        cmd(
            "/trust",
            "Save the project trust decision for future sessions",
            "",
        ),
        cmd("/login", "Configure provider authentication", "[provider]"),
        cmd("/logout", "Remove provider authentication", "[provider]"),
        cmd("/new", "Start a new session", ""),
        cmd(
            "/compact",
            "Manually compact the session context",
            "[instructions]",
        ),
        cmd("/resume", "Resume a different session", "[session]"),
        cmd(
            "/reload",
            "Reload keybindings, extensions, skills, prompts, themes, and context files",
            "",
        ),
        cmd("/quit", "Quit Pi", ""),
    ]
}

/// `ompBuiltins`.
fn omp_builtins() -> Vec<SlashCommand> {
    vec![
        cmd("/settings", "Open settings menu", ""),
        cmd("/setup", "Open provider setup", "[providers]"),
        cmd("/plan", "Toggle plan mode", "[prompt]"),
        cmd("/plan-review", "Reopen the latest plan review", ""),
        cmd("/vibe", "Toggle persistent fast-worker mode", "[prompt]"),
        cmd(
            "/goal",
            "Manage the persistent autonomous goal",
            "[objective]",
        ),
        cmd(
            "/guided-goal",
            "Interview and refine a goal before enabling it",
            "[rough objective]",
        ),
        cmd(
            "/loop",
            "Repeat the next prompt after every yield",
            "[count|duration] [prompt]",
        ),
        cmd(
            "/queue",
            "Queue a message for after the agent yields",
            "<message>",
        ),
        cmd("/model", "Switch the model for this session", "[model]"),
        cmd("/switch", "Switch the model for this session", "[model]"),
        cmd("/fast", "Toggle priority service tier", "[on|off|status]"),
        cmd(
            "/computer",
            "Toggle the native computer-use tool",
            "[on|off|status]",
        ),
        cmd(
            "/vision",
            "Control the inspect_image delegation tool",
            "[on|off|auto|status]",
        ),
        cmd("/prewalk", "Switch to a fast model at the next action", ""),
        cmd(
            "/advisor",
            "Manage the second-model advisor",
            "[on|off|status|dump|configure]",
        ),
        cmd("/export", "Export the session to HTML", "[--themes] [path]"),
        cmd("/dump", "Copy the transcript and write request JSON", ""),
        cmd("/share", "Share the session through an encrypted link", ""),
        cmd(
            "/collab",
            "Share this session live through a relay",
            "[start|view|stop|status] [relay URL]",
        ),
        cmd("/join", "Join a shared collaboration session", "<link>"),
        cmd("/leave", "Leave the collaboration session", ""),
        cmd(
            "/browser",
            "Toggle browser headless or visible mode",
            "[headless|visible]",
        ),
        cmd("/copy", "Pick conversation text or code to copy", ""),
        cmd(
            "/todo",
            "View or modify the agent todo list",
            "<subcommand>",
        ),
        cmd(
            "/session",
            "Manage the current session",
            "[info|delete|pin]",
        ),
        cmd("/jobs", "Show background job status", ""),
        cmd("/usage", "Show provider usage and limits", "[show|reset]"),
        cmd(
            "/stats",
            "Launch the local statistics dashboard",
            "[--port <port>]",
        ),
        cmd("/changelog", "Show changelog entries", "[full]"),
        cmd("/hotkeys", "Show all keyboard shortcuts", ""),
        cmd("/tools", "Show tools visible to the agent", ""),
        cmd("/context", "Show estimated context usage", ""),
        cmd("/extensions", "Open the Extension Control Center", ""),
        cmd("/agents", "Open the Agent Control Center", ""),
        cmd("/branch", "Create a branch from a previous message", ""),
        cmd("/fork", "Create a fork from a previous message", ""),
        cmd("/tree", "Navigate the session tree", ""),
        cmd(
            "/login",
            "Log in with an OAuth provider",
            "[provider|redirect URL]",
        ),
        cmd("/logout", "Log out from an OAuth provider", "[provider]"),
        cmd("/mcp", "Manage MCP servers", "<subcommand>"),
        cmd("/ssh", "Manage SSH hosts", "<subcommand>"),
        cmd("/new", "Start a new session", ""),
        cmd(
            "/fresh",
            "Reset provider state without changing the transcript",
            "",
        ),
        cmd(
            "/drop",
            "Delete the current session and start a new one",
            "",
        ),
        cmd(
            "/compact",
            "Manually compact the session context",
            "[mode] [focus]",
        ),
        cmd(
            "/shake",
            "Drop heavy content from context",
            "[elide|images]",
        ),
        cmd(
            "/handoff",
            "Hand off context to a new session",
            "[focus instructions]",
        ),
        cmd("/resume", "Resume a different session", "[session ID]"),
        cmd(
            "/btw",
            "Ask an ephemeral question using current context",
            "<question>",
        ),
        cmd(
            "/tan",
            "Run a background agent on tangential work",
            "<work>",
        ),
        cmd(
            "/omfg",
            "Forge a rule from a recurring-behavior complaint",
            "<complaint>",
        ),
        cmd("/retry", "Retry the last failed agent turn", ""),
        cmd("/debug", "Open the debug tools selector", ""),
        cmd("/memory", "Inspect and maintain memory", "<subcommand>"),
        cmd("/rename", "Rename the current session", "<title>"),
        cmd("/move", "Move the session to another directory", "[path]"),
        cmd("/add-dir", "Add a workspace directory", "<path>"),
        cmd("/remove-dir", "Remove a workspace directory", "<path>"),
        cmd("/dirs", "List workspace directories", ""),
        cmd("/exit", "Exit OMP", ""),
        cmd("/marketplace", "Manage plugin marketplaces", "<subcommand>"),
        cmd(
            "/plugins",
            "View and manage installed plugins",
            "[list|enable|disable]",
        ),
        cmd(
            "/reload-plugins",
            "Reload plugins, skills, commands, hooks, tools, agents, and MCP",
            "",
        ),
        cmd(
            "/force",
            "Force the next turn to use a specific tool",
            "<tool-name> [prompt]",
        ),
        cmd("/live", "Start Codex-backed realtime voice mode", ""),
        cmd("/pause", "Freeze all agents until resumed", ""),
        cmd("/quit", "Quit OMP", ""),
    ]
}

/// `providers` — the registered provider ids.
fn provider_builtins(id: &str) -> Option<Vec<SlashCommand>> {
    match id {
        "claude" => Some(claude_builtins()),
        "codex" => Some(codex_builtins()),
        "qoder" => Some(qoder_builtins()),
        "opencode" => Some(opencode_builtins()),
        "hermes" => Some(hermes_builtins()),
        "kimi" => Some(kimi_builtins()),
        "pi" => Some(pi_builtins()),
        "omp" => Some(omp_builtins()),
        _ => None,
    }
}

/// `profileIDForAgentName` — reported agent name → provider id.
fn profile_id_for_agent_name(agent: &str) -> &'static str {
    match agent.trim().to_lowercase().as_str() {
        "claude" | "claude-code" | "claude code" => "claude",
        "codex" => "codex",
        "qoder" | "qodercli" => "qoder",
        "pi" | "pi-coding-agent" => "pi",
        "omp" | "oh my pi" | "oh-my-pi" => "omp",
        "kimi" | "kimi code" | "kimi-code" | "kimi-cli" => "kimi",
        "opencode" | "open code" | "open-code" => "opencode",
        "hermes" | "hermes-agent" | "hermes agent" => "hermes",
        _ => "",
    }
}

/// `CatalogForProfileWithSuppression` — builtins plus the INI escape
/// hatch. Per-agent native discovery (project/personal command and skill
/// trees, provider settings files, trust rules, `agentVersion`-gated
/// builtin sets) is the one piece of the oracle's `Discover` not ported
/// here, so the `cwd`/`home`/`agentVersion`/`agentDir` inputs it would
/// consume are not threaded through.
fn catalog_for_profile(
    profile_id: &str,
    reported_agent: &str,
    skill_dirs: &[String],
    command_format: &str,
    suppress_native: bool,
) -> SlashCatalog {
    let mut provider = provider_builtins(profile_id.trim().to_lowercase().as_str());
    if provider.is_none() && !reported_agent.is_empty() {
        provider = provider_builtins(profile_id_for_agent_name(reported_agent));
    }
    let (commands, truncated) = match provider {
        // codex/opencode discover nothing beyond builtins in the oracle.
        Some(builtins) if suppress_native => (builtins, false),
        Some(builtins) if !command_format.is_empty() => {
            // pi/omp/kimi honor the INI format as an escape hatch; hermes
            // appends it after builtins with dedupe; claude/qoder ignore
            // it — for them the configured dirs still apply via hermes'
            // rule only when the profile IS hermes.
            let id = profile_id.trim().to_lowercase();
            let id = if id.is_empty() {
                profile_id_for_agent_name(reported_agent)
            } else {
                id.as_str()
            };
            match id {
                "pi" | "omp" | "kimi" => {
                    let (custom, truncated) = discover_generic_skills(skill_dirs, command_format);
                    (builtins_with_custom(builtins, custom), truncated)
                }
                "hermes" => {
                    let (custom, truncated) = discover_generic_skills(skill_dirs, command_format);
                    (append_dedup(builtins, custom), truncated)
                }
                _ => (builtins, false),
            }
        }
        Some(builtins) => {
            // hermes also folds configured dirs into its native pass with
            // a `/{name}` default — keep that path even without a format.
            let id = profile_id.trim().to_lowercase();
            let id = if id.is_empty() {
                profile_id_for_agent_name(reported_agent)
            } else {
                id.as_str()
            };
            if id == "hermes" && !skill_dirs.is_empty() {
                let (custom, truncated) = discover_generic_skills(skill_dirs, "/{name}");
                (append_dedup(builtins, custom), truncated)
            } else {
                (builtins, false)
            }
        }
        None => discover_generic_skills(skill_dirs, command_format),
    };
    finalize_catalog(commands, truncated)
}

/// `builtinsWithCustom` — builtins first, customs appended unless the
/// name collides.
fn builtins_with_custom(
    builtins: Vec<SlashCommand>,
    custom: Vec<SlashCommand>,
) -> Vec<SlashCommand> {
    let mut commands = Vec::with_capacity(builtins.len() + custom.len());
    let mut seen: std::collections::HashSet<String> =
        builtins.iter().map(|c| c.command.clone()).collect();
    commands.extend(builtins);
    for c in custom {
        if seen.insert(c.command.clone()) {
            commands.push(c);
        }
    }
    commands
}

/// hermes' configured-dir merge — first-wins by command name.
fn append_dedup(builtins: Vec<SlashCommand>, custom: Vec<SlashCommand>) -> Vec<SlashCommand> {
    builtins_with_custom(builtins, custom)
}

/// `finalizeCatalog` — cap at `maxEntries`, flag truncation.
fn finalize_catalog(mut commands: Vec<SlashCommand>, mut truncated: bool) -> SlashCatalog {
    if commands.len() > MAX_ENTRIES {
        commands.truncate(MAX_ENTRIES);
        truncated = true;
    }
    SlashCatalog {
        commands,
        truncated,
    }
}

/// `discoverGenericSkills` — each configured dir contributes
/// `<entry>/SKILL.md` rendered through the `{name}` format; index 0 is
/// `personal`, the rest `project`.
fn discover_generic_skills(dirs: &[String], format: &str) -> (Vec<SlashCommand>, bool) {
    if format.is_empty() || format.matches("{name}").count() != 1 {
        return (Vec::new(), false);
    }
    let mut commands = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut scanned = 0usize;
    let mut truncated = false;
    'dirs: for (index, dir) in dirs.iter().enumerate() {
        let source = if index == 0 { "personal" } else { "project" };
        let Ok(mut entries) = std::fs::read_dir(dir).map(|rd| rd.flatten().collect::<Vec<_>>())
        else {
            continue;
        };
        entries.sort_by(|a, b| {
            let (al, bl) = (
                a.file_name().to_string_lossy().to_lowercase(),
                b.file_name().to_string_lossy().to_lowercase(),
            );
            al.cmp(&bl).then_with(|| a.file_name().cmp(&b.file_name()))
        });
        for entry in entries {
            if scanned >= MAX_CUSTOM_FILES {
                truncated = true;
                break 'dirs;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if !is_dir || name.starts_with('.') {
                continue;
            }
            let path = Path::new(dir).join(&name).join("SKILL.md");
            let is_regular = std::fs::metadata(&path)
                .map(|m| m.is_file())
                .unwrap_or(false);
            if !is_regular {
                continue;
            }
            scanned += 1;
            let Some(metadata) = read_skill_metadata(&path) else {
                continue;
            };
            let name = metadata.get("name").cloned().unwrap_or_default();
            if !command_name_valid(&name) || seen.contains(&name) || !user_invocable(&metadata) {
                continue;
            }
            seen.insert(name.clone());
            let description = metadata
                .get("description")
                .filter(|d| !d.is_empty())
                .cloned()
                .unwrap_or_else(|| {
                    let mut chars = name.chars();
                    match chars.next() {
                        Some(first) => format!("{}{} skill", first.to_uppercase(), chars.as_str()),
                        None => " skill".to_owned(),
                    }
                });
            let replaced = format.replacen("{name}", &name, 1);
            // `strings.TrimPrefix(replaced, "/")` — a single leading slash.
            commands.push(SlashCommand {
                command: format!("/{}", replaced.strip_prefix('/').unwrap_or(&replaced)),
                description: compact(&description, 240),
                source: source.to_owned(),
                argument_hint: compact(
                    metadata
                        .get("argument-hint")
                        .map(String::as_str)
                        .unwrap_or_default(),
                    120,
                ),
            });
        }
    }
    (commands, truncated)
}

/// `commandNamePattern` — `[A-Za-z0-9][A-Za-z0-9._:-]{0,119}`.
fn command_name_valid(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 120 {
        return false;
    }
    bytes[0].is_ascii_alphanumeric()
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

/// `userInvocable` — only explicit false-ish values suppress.
fn user_invocable(metadata: &BTreeMap<String, String>) -> bool {
    !matches!(
        metadata
            .get("user-invocable")
            .map(|v| v.trim().to_lowercase())
            .as_deref(),
        Some("false" | "no" | "off" | "0")
    )
}

/// `readSkillMetadata` — at most `maxMetadataSize`, frontmatter parsed.
fn read_skill_metadata(path: &Path) -> Option<BTreeMap<String, String>> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut data = Vec::new();
    use std::io::Read;
    let _ = file
        .by_ref()
        .take(MAX_METADATA_BYTES + 1)
        .read_to_end(&mut data)
        .ok()?;
    if data.len() as u64 > MAX_METADATA_BYTES {
        return None;
    }
    Some(parse_frontmatter(&String::from_utf8_lossy(&data)))
}

/// `parseFrontmatterBytes` — `key: value` lines inside `---` fences, with
/// quoted-scalar unwrapping and YAML block scalars.
fn parse_frontmatter(data: &str) -> BTreeMap<String, String> {
    let normalized = data.replace("\r\n", "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();
    let mut result = BTreeMap::new();
    if lines.first().map(|l| l.trim()) != Some("---") {
        return result;
    }
    let mut index = 1;
    while index < lines.len() {
        let line = lines[index];
        if line.trim() == "---" {
            return result;
        }
        let Some((key, value)) = frontmatter_entry(line) else {
            index += 1;
            continue;
        };
        let key = key.to_lowercase();
        let value = if let Some(folded) = block_scalar_header(&value) {
            let (value, consumed) = fold_block_scalar(&lines[index + 1..], folded);
            index += consumed;
            value
        } else if value.len() >= 2
            && value.as_bytes()[0] == value.as_bytes()[value.len() - 1]
            && matches!(value.as_bytes()[0], b'\'' | b'"')
        {
            value[1..value.len() - 1].to_owned()
        } else {
            value
        };
        result.insert(key, value);
        index += 1;
    }
    result
}

/// `frontmatterKeyPattern` — `key:` plus a trimmed value.
fn frontmatter_entry(line: &str) -> Option<(String, String)> {
    let colon = line.find(':')?;
    let key = &line[..colon];
    if key.is_empty()
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return None;
    }
    Some((key.to_owned(), line[colon + 1..].trim().to_owned()))
}

/// `blockScalarHeader` — `|` (literal) or `>` (folded), optional chomping
/// and indentation indicators, optional trailing comment.
fn block_scalar_header(value: &str) -> Option<bool> {
    let first = value.as_bytes().first()?;
    let folded = match first {
        b'|' => false,
        b'>' => true,
        _ => return None,
    };
    let header = match value.find('#') {
        Some(at) if at > 0 && matches!(value.as_bytes()[at - 1], b' ' | b'\t') => {
            value[..at].trim()
        }
        _ => value,
    };
    if header[1..]
        .bytes()
        .all(|b| b == b'+' || b == b'-' || b.is_ascii_digit())
    {
        Some(folded)
    } else {
        None
    }
}

/// `foldBlockScalar` — strip common indentation; literal keeps newlines,
/// folded joins with spaces; returns (value, lines consumed).
fn fold_block_scalar(rest: &[&str], folded: bool) -> (String, usize) {
    let mut indent = usize::MAX;
    let mut consumed = 0usize;
    let mut parts: Vec<&str> = Vec::new();
    for line in rest {
        let trimmed = line.trim();
        let line_indent = line.len() - line.trim_start().len();
        if !trimmed.is_empty() && line_indent == 0 {
            break;
        }
        consumed += 1;
        if trimmed.is_empty() {
            parts.push("");
            continue;
        }
        if line_indent < indent {
            indent = line_indent;
        }
        parts.push(line);
    }
    while parts.last() == Some(&"") {
        parts.pop();
    }
    if parts.is_empty() {
        return (String::new(), consumed);
    }
    let stripped: Vec<String> = parts
        .iter()
        .map(|part| {
            if part.is_empty() {
                String::new()
            } else {
                part.chars().skip(indent.min(part.len())).collect()
            }
        })
        .collect();
    let separator = if folded { " " } else { "\n" };
    (stripped.join(separator), consumed)
}

/// `CommandDiscovery` — the INI `[skills]`/`[commands]` sections: `off`
/// suppresses, an invalid or absent format disables custom discovery.
fn command_discovery(profile_id: &str) -> (Vec<String>, String, bool) {
    let ini = read_ini(&config_home().join("herdr/agent-profiles.ini"));
    let pid = profile_id.trim().to_lowercase();
    let mut dirs: HashMap<String, Vec<String>> = HashMap::new();
    let mut formats: HashMap<String, String> = HashMap::new();
    if let Some(ini) = &ini {
        if let Some(section) = ini.get("skills") {
            for (profile, raw) in section {
                let home = home_dir();
                let paths: Vec<String> = std::env::split_paths(std::ffi::OsStr::new(raw))
                    .filter_map(|p| {
                        let value = p.to_string_lossy().trim().to_owned();
                        (!value.is_empty()).then(|| expand_tilde(&value, &home))
                    })
                    .collect();
                if !paths.is_empty() {
                    dirs.insert(profile.trim().to_lowercase(), paths);
                }
            }
        }
        if let Some(section) = ini.get("commands") {
            for (profile, format) in section {
                formats.insert(profile.trim().to_lowercase(), format.trim().to_owned());
            }
        }
    }
    let Some(format) = formats.get(&pid) else {
        return (Vec::new(), String::new(), false);
    };
    let format = format.trim();
    if format.eq_ignore_ascii_case("off") {
        return (Vec::new(), String::new(), true);
    }
    if format.is_empty() || !valid_command_format(format) {
        return (Vec::new(), String::new(), false);
    }
    (
        dirs.get(&pid).cloned().unwrap_or_default(),
        format.to_owned(),
        false,
    )
}

/// `validCommandFormat` — exactly one `{name}` and no other braces.
fn valid_command_format(value: &str) -> bool {
    value.matches("{name}").count() == 1
        && !value.replacen("{name}", "", 1).contains('{')
        && !value.replacen("{name}", "", 1).contains('}')
}

/// `fitSlashCommandCatalog` — binary-search the largest command prefix
/// whose `command_result` frame fits the outbound byte cap.
fn fit_catalog(catalog: SlashCatalog, request_id: &str, pane_id: &str) -> SlashCatalog {
    let fits = |commands: &[SlashCommand], truncated: bool| -> bool {
        let frame = super::local::command_result(
            request_id,
            "list_slash_commands",
            true,
            "completed",
            "",
            pane_id,
            serde_json::to_value(&SlashCatalog {
                commands: commands.to_vec(),
                truncated,
            })
            .ok(),
        );
        frame.encode().len() <= MAX_OUTBOUND_MESSAGE_BYTES
    };
    if fits(&catalog.commands, catalog.truncated) {
        return catalog;
    }
    let commands = catalog.commands;
    let (mut low, mut high) = (0usize, commands.len());
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        if fits(&commands[..mid], true) {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    SlashCatalog {
        commands: commands[..low].to_vec(),
        truncated: true,
    }
}

/// The `config.ParseINI` reader — same rules as `profiles::load_ini`:
/// `[section]`, `key = value`/`key: value`, `#`/`;` comments, lowercase
/// names, last section wins, error → nothing.
fn read_ini(path: &Path) -> Option<BTreeMap<String, BTreeMap<String, String>>> {
    let data = std::fs::read_to_string(path).ok()?;
    parse_ini(&data)
}

fn parse_ini(data: &str) -> Option<BTreeMap<String, BTreeMap<String, String>>> {
    let mut ini: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    ini.insert(String::new(), BTreeMap::new());
    let mut current = String::new();
    for raw_line in data.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('[') {
            let end = rest.find(']')?;
            current = rest[..end].trim().to_lowercase();
            ini.entry(current.clone()).or_default();
            continue;
        }
        let pos = trimmed.find(['=', ':'])?;
        let key = trimmed[..pos].trim().to_lowercase();
        let value = trimmed[pos + 1..].trim().to_owned();
        let section = ini.entry(current.clone()).or_default();
        if section.insert(key, value).is_some() {
            return None; // duplicate key
        }
    }
    Some(ini)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inbound(fields: serde_json::Value) -> Inbound {
        serde_json::from_value(fields).expect("inbound")
    }

    // ── update state machine ─────────────────────────────────────────

    #[test]
    fn semver_rules_match_oracle() {
        assert!(semver_valid("0.0.0"));
        assert!(semver_valid("1.2.3"));
        assert!(!semver_valid("01.2.3"));
        assert!(!semver_valid("1.2"));
        assert!(!semver_valid("1.2.3.4"));
        assert!(!semver_valid("v1.2.3"));
        assert!(newer_version("1.2.4", "1.2.3"));
        assert!(!newer_version("1.2.3", "1.2.3"));
        assert!(!newer_version("1.2.2", "1.2.3"));
        assert!(!newer_version("dev", "1.2.3"));
        assert!(!newer_version("1.2.3", "dev"));
    }

    #[test]
    fn state_predicates_match_oracle() {
        assert!(transient_update_state("scheduled"));
        assert!(transient_update_state("installing"));
        assert!(!transient_update_state("checking"));
        assert!(!transient_update_state("failed"));
        assert!(valid_state("rolled_back"));
        assert!(!valid_state("bogus"));
        assert!(valid_revision(&"a".repeat(40)));
        assert!(!valid_revision(&"g".repeat(40)));
        assert!(!valid_revision(&"a".repeat(39)));
        assert_eq!(short_revision(&"a".repeat(40)), "a".repeat(12));
    }

    #[test]
    fn write_and_read_state_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update-state.json");
        let state = UpdateState {
            state: "available".to_owned(),
            available_version: "1.2.3".to_owned(),
            available_revision: "abc".to_owned(),
            target_version: "1.2.3".to_owned(),
            target_revision: "d".repeat(40),
            can_install: true,
            eligible: true,
            ..UpdateState::default()
        };
        write_state(&path, &state).unwrap();
        let loaded = read_state(&path).unwrap();
        assert_eq!(loaded.state, "available");
        assert_eq!(loaded.available_version, "1.2.3");
        assert!(loaded.can_install);
        // `eligible`/`can_install` serialize unconditionally.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("\"eligible\": true"));
        assert!(raw.contains("\"can_install\": true"));
    }

    #[test]
    fn load_state_demotes_stale_available() {
        let dir = tempfile::tempdir().unwrap();
        let mut manager = UpdateManager {
            runtime_dir: dir.path().to_path_buf(),
            version: "9.9.9".to_owned(),
            ..UpdateManager::from_env()
        };
        manager.release_root = dir.path().to_path_buf();
        write_state(
            &manager.state_path(),
            &UpdateState {
                state: "available".to_owned(),
                available_version: "1.0.0".to_owned(),
                target_version: "1.0.0".to_owned(),
                can_install: true,
                ..UpdateState::default()
            },
        )
        .unwrap();
        // Installed version is newer → the persisted "available" demotes
        // to "current", like `loadState`.
        let loaded = manager.load_state();
        assert_eq!(loaded.state, "current");
        assert!(!loaded.can_install);
    }

    #[test]
    fn eligibility_requires_released_build() {
        let mut manager = UpdateManager::from_env();
        manager.version = "dev".to_owned();
        manager.revision = "unknown".to_owned();
        let (eligible, mode, reason) = manager.eligibility();
        assert!(!eligible);
        assert_eq!(mode, "unsupported");
        assert_eq!(reason, "Managed updates require a released relay build");
        // A semver version without a git revision is still unsupported.
        manager.version = "1.2.3".to_owned();
        let (eligible, _, _) = manager.eligibility();
        assert!(!eligible);
    }

    #[test]
    fn eligibility_needs_absolute_executable_herdr() {
        let mut manager = UpdateManager::from_env();
        manager.version = "1.2.3".to_owned();
        manager.revision = "a".repeat(40);
        manager.herdr_bin = "herdr".to_owned();
        let (eligible, _, reason) = manager.eligibility();
        assert!(!eligible);
        assert_eq!(
            reason,
            "Managed updates require an absolute Herdr executable path"
        );
        manager.herdr_bin = "/nonexistent/herdr".to_owned();
        let (eligible, _, reason) = manager.eligibility();
        assert!(!eligible);
        assert_eq!(reason, "The Herdr executable is unavailable");
    }

    #[test]
    fn rfc3339_roundtrips_through_parse() {
        let stamp = now_rfc3339();
        let parsed = parse_rfc3339(&stamp).expect("parse");
        assert!((now_unix() - parsed).abs() < 5);
        assert_eq!(parse_rfc3339("not-a-time"), None);
        assert_eq!(parse_rfc3339("2024-03-01T12:00:00Z"), Some(1_709_294_400));
    }

    #[test]
    fn path_escape_encodes_like_go() {
        assert_eq!(path_escape("v1.2.3"), "v1.2.3");
        assert_eq!(path_escape("a/b"), "a%2Fb");
        assert_eq!(path_escape("a b"), "a%20b");
        assert_eq!(path_escape("x@y"), "x@y");
    }

    #[test]
    fn atom_feed_extracts_revision() {
        let feed = br#"<?xml version="1.0"?><feed><entry><id>tag:github.com,2008:Repository/123/abcdef0123456789abcdef0123456789abcdef01</id></entry></feed>"#;
        let ids = atom_entry_ids(feed);
        assert_eq!(ids.len(), 1);
        let revision = ids[0].trim().rsplit('/').next().unwrap();
        assert!(valid_revision(revision));
    }

    // ── origins ──────────────────────────────────────────────────────

    #[test]
    fn origin_validation_matches_oracle() {
        assert_eq!(
            store_phone_app_origin("").unwrap_err(),
            "origin is required"
        );
        assert_eq!(
            store_phone_app_origin("http://app.example").unwrap_err(),
            "origin must be an HTTPS origin"
        );
        assert_eq!(
            store_phone_app_origin("not a url").unwrap_err(),
            "origin must be an HTTPS origin"
        );
        assert_eq!(
            store_phone_app_origin("https://user@app.example").unwrap_err(),
            "origin must be an HTTPS origin"
        );
        assert_eq!(
            store_phone_app_origin("https://app.example/path").unwrap_err(),
            "origin must not contain a path, query, or fragment"
        );
        assert_eq!(
            store_phone_app_origin("https://app.example/?q=1").unwrap_err(),
            "origin must not contain a path, query, or fragment"
        );
        assert_eq!(
            store_phone_app_origin("https://app.example/#frag").unwrap_err(),
            "origin must not contain a path, query, or fragment"
        );
    }

    #[test]
    fn origin_persists_under_runtime_dir() {
        // The env-derived runtime dir is not injectable without env
        // manipulation; exercise the writer through a temp runtime dir by
        // pointing HERDR_PLUGIN_CONFIG_DIR at it — the env slot the
        // resolver honors anyway.
        let dir = tempfile::tempdir().unwrap();
        let key = "HERDR_PLUGIN_CONFIG_DIR";
        let saved = std::env::var(key).ok();
        std::env::set_var(key, dir.path());
        let result = store_phone_app_origin("https://app.example:8443");
        match saved {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        result.unwrap();
        let written = std::fs::read_to_string(dir.path().join("phone-app-origin")).unwrap();
        assert_eq!(written, "https://app.example:8443\n");
    }

    // ── conversation history / copy validation ───────────────────────

    #[test]
    fn copy_preflight_order() {
        assert_eq!(copy_preflight(None), Some("Agent pane not found"));
        let pane = AgentInfo {
            agent_status: lerdr_herdr::AgentStatus::Working,
            ..AgentInfo::default()
        };
        assert_eq!(
            copy_preflight(Some(&pane)),
            Some("Agent is still working; wait for the current turn to finish")
        );
        let pane = AgentInfo::default();
        assert_eq!(copy_preflight(Some(&pane)), None);
    }

    // ── slash catalog ────────────────────────────────────────────────

    #[test]
    fn provider_aliases_match_oracle() {
        assert_eq!(profile_id_for_agent_name("Claude Code"), "claude");
        assert_eq!(profile_id_for_agent_name("qodercli"), "qoder");
        assert_eq!(profile_id_for_agent_name("Pi-Coding-Agent"), "pi");
        assert_eq!(profile_id_for_agent_name("oh-my-pi"), "omp");
        assert_eq!(profile_id_for_agent_name("kimi-cli"), "kimi");
        assert_eq!(profile_id_for_agent_name("Open Code"), "opencode");
        assert_eq!(profile_id_for_agent_name("hermes-agent"), "hermes");
        assert_eq!(profile_id_for_agent_name("devin"), "");
    }

    #[test]
    fn builtins_cover_every_provider() {
        for id in [
            "claude", "codex", "qoder", "opencode", "hermes", "kimi", "pi", "omp",
        ] {
            let builtins = provider_builtins(id).expect(id);
            assert!(!builtins.is_empty(), "{id}");
            assert!(builtins.iter().all(|c| c.command.starts_with('/')));
            assert!(builtins.iter().all(|c| c.source == "builtin"));
        }
        // Counts match the oracle's builtin tables.
        assert_eq!(claude_builtins().len(), 51);
        assert_eq!(codex_builtins().len(), 50);
        assert_eq!(qoder_builtins().len(), 9);
        assert_eq!(opencode_builtins().len(), 17);
        assert_eq!(hermes_builtins().len(), 23);
        assert_eq!(kimi_builtins().len(), 39);
        assert_eq!(pi_builtins().len(), 22);
        assert_eq!(omp_builtins().len(), 68);
    }

    #[test]
    fn command_name_and_format_validation() {
        assert!(command_name_valid("review"));
        assert!(command_name_valid("ns:name.v2"));
        assert!(!command_name_valid(""));
        assert!(!command_name_valid("-lead"));
        assert!(!command_name_valid(&"x".repeat(121)));
        assert!(valid_command_format("skill:{name}"));
        assert!(valid_command_format("/{name}"));
        assert!(!valid_command_format("{name}{name}"));
        assert!(!valid_command_format("{other}"));
        assert!(!valid_command_format("{name}}"));
    }

    #[test]
    fn generic_skills_scan_dirs() {
        let root = tempfile::tempdir().unwrap();
        let personal = root.path().join("personal");
        let project = root.path().join("project");
        let skill_a = personal.join("alpha");
        let skill_b = project.join("beta");
        std::fs::create_dir_all(&skill_a).unwrap();
        std::fs::create_dir_all(&skill_b).unwrap();
        std::fs::write(
            skill_a.join("SKILL.md"),
            "---\nname: alpha\ndescription: Alpha skill\nargument-hint: [file]\n---\n",
        )
        .unwrap();
        std::fs::write(
            skill_b.join("SKILL.md"),
            "---\nname: beta\nuser-invocable: false\n---\n",
        )
        .unwrap();
        let dirs = vec![
            personal.to_string_lossy().into_owned(),
            project.to_string_lossy().into_owned(),
        ];
        let (commands, truncated) = discover_generic_skills(&dirs, "skill:{name}");
        assert!(!truncated);
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].command, "/skill:alpha");
        assert_eq!(commands[0].description, "Alpha skill");
        assert_eq!(commands[0].source, "personal");
        assert_eq!(commands[0].argument_hint, "[file]");
        // A missing {name} yields nothing.
        let (commands, _) = discover_generic_skills(&dirs, "run");
        assert!(commands.is_empty());
    }

    #[test]
    fn generic_skills_dedupe_and_budget() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("d");
        for name in ["one", "one-dup"] {
            let skill = dir.join(name);
            std::fs::create_dir_all(&skill).unwrap();
            // Both declare the same frontmatter name — first wins.
            std::fs::write(skill.join("SKILL.md"), "---\nname: one\n---\n").unwrap();
        }
        let dirs = vec![dir.to_string_lossy().into_owned()];
        let (commands, _) = discover_generic_skills(&dirs, "{name}");
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].command, "/one");
    }

    #[test]
    fn frontmatter_parses_scalars_and_blocks() {
        let parsed = parse_frontmatter(
            "---\nname: demo\ndescription: |\n  line one\n  line two\nuser-invocable: \"no\"\n---\nbody\n",
        );
        assert_eq!(parsed.get("name").unwrap(), "demo");
        assert_eq!(parsed.get("description").unwrap(), "line one\nline two");
        assert_eq!(parsed.get("user-invocable").unwrap(), "no");
        assert!(!user_invocable(&parsed));
        let plain = parse_frontmatter("no fence at all\n");
        assert!(plain.is_empty());
    }

    #[test]
    fn frontmatter_folded_block_joins_with_space() {
        let parsed = parse_frontmatter("---\ndescription: >\n  wrapped\n  text\n---\n");
        assert_eq!(parsed.get("description").unwrap(), "wrapped text");
    }

    #[test]
    fn catalog_truncates_at_max_entries() {
        let commands: Vec<SlashCommand> = (0..MAX_ENTRIES + 10)
            .map(|i| SlashCommand {
                command: format!("/c{i}"),
                description: String::new(),
                source: "builtin".to_owned(),
                argument_hint: String::new(),
            })
            .collect();
        let catalog = finalize_catalog(commands, false);
        assert!(catalog.truncated);
        assert_eq!(catalog.commands.len(), MAX_ENTRIES);
    }

    #[test]
    fn fit_catalog_binary_searches_under_cap() {
        let commands: Vec<SlashCommand> = (0..2000)
            .map(|i| SlashCommand {
                command: format!("/command-{i}"),
                // ~4 KB per entry → the full catalog exceeds the 4 MiB
                // cap and must binary-search a fitting prefix.
                description: "x".repeat(4096),
                source: "builtin".to_owned(),
                argument_hint: String::new(),
            })
            .collect();
        let catalog = finalize_catalog(commands, false);
        let fitted = fit_catalog(catalog, "req-large-catalog", "pane-1");
        assert!(fitted.truncated);
        assert!(fitted.commands.len() < 2000);
        let frame = crate::actions::local::command_result(
            "req-large-catalog",
            "list_slash_commands",
            true,
            "completed",
            "",
            "pane-1",
            serde_json::to_value(&fitted).ok(),
        );
        assert!(frame.encode().len() <= MAX_OUTBOUND_MESSAGE_BYTES);
    }

    #[test]
    fn catalog_unknown_provider_uses_generic_only() {
        let catalog = catalog_for_profile("", "devin", &[], "", false);
        assert!(catalog.commands.is_empty());
        assert!(!catalog.truncated);
    }

    #[test]
    fn catalog_claude_builtin_only_without_format() {
        let catalog = catalog_for_profile("claude", "Claude Code", &[], "", false);
        assert_eq!(catalog.commands.len(), 51);
        assert!(catalog.commands.iter().any(|c| c.command == "/copy"));
        assert!(catalog.commands.iter().all(|c| c.source == "builtin"));
    }

    #[test]
    fn catalog_format_escape_hatch() {
        let root = tempfile::tempdir().unwrap();
        let skill = root.path().join("s");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: mine\ndescription: Custom\n---\n",
        )
        .unwrap();
        let dirs = vec![root.path().to_string_lossy().into_owned()];
        // pi honors the INI format; builtins keep name collisions.
        let catalog = catalog_for_profile("pi", "pi", &dirs, "skill:{name}", false);
        assert_eq!(catalog.commands.len(), 23);
        assert!(catalog.commands.iter().any(|c| c.command == "/skill:mine"));
        // claude ignores the configured format (native discovery only in
        // the oracle) — builtins stay.
        let catalog = catalog_for_profile("claude", "claude", &dirs, "skill:{name}", false);
        assert_eq!(catalog.commands.len(), 51);
        // suppress_native strips customs everywhere.
        let catalog = catalog_for_profile("pi", "pi", &dirs, "skill:{name}", true);
        assert_eq!(catalog.commands.len(), 22);
    }

    #[test]
    fn command_discovery_reads_ini() {
        let dir = tempfile::tempdir().unwrap();
        let ini_dir = dir.path().join("herdr");
        std::fs::create_dir_all(&ini_dir).unwrap();
        std::fs::write(
            ini_dir.join("agent-profiles.ini"),
            "[skills]\npi = /skills/a:/skills/b\n[commands]\npi = skill:{name}\nclaude = off\n",
        )
        .unwrap();
        // command_discovery reads <config_home>/herdr/agent-profiles.ini;
        // inject the config home through XDG_CONFIG_HOME.
        let saved = std::env::var("XDG_CONFIG_HOME").ok();
        std::env::set_var("XDG_CONFIG_HOME", dir.path());
        let (dirs, format, suppressed) = command_discovery("pi");
        let off = command_discovery("claude");
        let missing = command_discovery("codex");
        match saved {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        assert_eq!(dirs, vec!["/skills/a", "/skills/b"]);
        assert_eq!(format, "skill:{name}");
        assert!(!suppressed);
        assert!(off.2);
        assert!(!missing.2 && missing.1.is_empty());
    }

    #[test]
    fn inbound_fields_parse() {
        let msg = inbound(serde_json::json!({
            "type": "install_update",
            "pane_id": "wE:p1",
            "expected_version": "1.2.3",
            "expected_revision": "abc",
            "origin": "https://app.example",
            "cursor": "c",
            "retry": true,
            "limit": 50,
        }));
        assert_eq!(msg.pane_id, "wE:p1");
        assert_eq!(msg.expected_version, "1.2.3");
        assert_eq!(msg.expected_revision, "abc");
        assert_eq!(msg.origin, "https://app.example");
        assert_eq!(msg.cursor, "c");
        assert!(msg.retry);
        assert_eq!(msg.limit, 50);
    }
}
