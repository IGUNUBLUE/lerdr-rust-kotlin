//! Speech/TTS actions — the [`Speech`] handle plus the `speak_text`,
//! `cancel_speech`, `speech_voices_list`, `speech_voice_install`, and
//! `speech_voice_remove` handlers.
//!
//! Port of `internal/speech` (`speech.go` — engine detection, synthesis,
//! WAV canonicalization; `voices.go` — the pinned voice catalog,
//! install/remove, runtime download) and the `server.go` speech arms
//! (`speakText`, `cancelSpeech`, the `speech_voices_list` case,
//! `changeSpeechVoice`, `speechVoicePayload`, and the `speechRequests`
//! in-flight map).
//!
//! Wire contract summary (all byte-shapes the oracle emits):
//! - `speak_text` answers `command_result` — `data:{"format":"wav",
//!   "audio":"<base64>"}` on success; the three oracle failure texts are
//!   reproduced verbatim. Synthesis is tracked per client +
//!   `speech_request_id`; a same-key request cancels its predecessor and a
//!   prior `cancel_speech` pre-cancels the next request (tombstones, capped
//!   at 128 like `speechRequestCancelCap`).
//! - `cancel_speech` mutates only that map and emits no `command_result`;
//!   the relay answers `confirmed` (its sibling `unwatch_pane` does the
//!   same — a fire-and-forget cleanup the relay still receipts).
//! - `speech_voices_list` answers `command_result` carrying
//!   `speechVoicePayload`'s `data` object.
//! - `speech_voice_install`/`speech_voice_remove` answer
//!   `[speech_voices broadcast, command_result]` on success (the oracle's
//!   broadcast-before-result order; the router's hub delivers the
//!   broadcast to this client — `lerdr-coord` has no cross-session fanout
//!   seam on `ActionContext`) and `command_result` carrying the refreshed
//!   payload on failure.
//!
//! The engine itself lives behind [`SpeechEngine`] so tests inject a fake
//! and hosts without any engine binary get the oracle's no-engine path.
//! `SystemEngine` ports `engines()`/`selectEngine` (piper preferred, then
//! `say` on macOS, `espeak-ng`, `espeak`, `flite`), the 15s synthesis
//! bound, the 400-rune text cap, the ~900KB WAV budget (via the oracle's
//! halve-until-fits decimation), and the SHA256-pinned voice catalog
//! (downloaded with `curl`, the runtime with `tar`).

use std::collections::HashMap;
use std::ffi::OsStr;
use std::future::Future;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(target_os = "macos")]
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine as _;
use lerdr_core::json::MaybeNull;
use lerdr_core::protocol::{
    action_receipt_response, error_codes, ActionReceipt, ActionReceiptPhase, Inbound, Outbound,
    SpeechVoice, SpeechVoicesMessage,
};
use tokio_util::sync::CancellationToken;

use super::local::home_dir;
use super::{api_error_plain, ActionContext, Outcome};

/// `speech.Offered` — the five languages the app reads aloud.
const OFFERED: [&str; 5] = ["en", "fr", "de", "es", "zh"];

/// `speech.MaxTextRunes` — `speak_text` rejects longer scripts.
const MAX_TEXT_RUNES: usize = 400;

/// `speech.maxWAVBytes` — the transport frame budget for inline audio.
const MAX_WAV_BYTES: usize = 900 << 10;

/// The oracle's 15s `context.WithTimeout` around `Synthesize`.
const SYNTH_TIMEOUT: Duration = Duration::from_secs(15);

/// `InstallTimeout` — the oracle's 5-minute bound on engine + voice
/// downloads. Downloads are cancelled with the client's context there;
/// here the deadline alone bounds them (`ActionContext` exposes no
/// client-lifetime token — see the module's limitation notes).
const INSTALL_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// `runtimeStartTimeout` — `runtimeReady`'s `piper --help` probe budget.
const RUNTIME_START_TIMEOUT: Duration = Duration::from_secs(5);

/// `speechRequestCancelCap` — `cancel_speech` tombstones for requests that
/// never arrive are bounded so the map can't grow without limit.
const MAX_IN_FLIGHT: usize = 128;

/// How often a spawned engine child is polled for exit/cancel/deadline
/// (the oracle selects on `ctx.Done()`; the blocking thread polls).
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// `speechVoiceCatalog` — the pinned Hugging Face voice set (name is the
/// on-disk file stem, path the URL prefix, both digests SHA256 hex).
struct VoiceEntry {
    path: &'static str,
    name: &'static str,
    model_sha: &'static str,
    config_sha: &'static str,
    total_bytes: i64,
}

const VOICE_CATALOG: [(&str, VoiceEntry); 5] = [
    (
        "en",
        VoiceEntry {
            path: "en/en_US/lessac/medium",
            name: "en_US-lessac-medium",
            model_sha: "5efe09e69902187827af646e1a6e9d269dee769f9877d17b16b1b46eeaaf019f",
            config_sha: "efe19c417bed055f2d69908248c6ba650fa135bc868b0e6abb3da181dab690a0",
            total_bytes: 63_206_179,
        },
    ),
    (
        "fr",
        VoiceEntry {
            path: "fr/fr_FR/siwis/medium",
            name: "fr_FR-siwis-medium",
            model_sha: "641d1ab097da2b81128c076810edb052b385decc8be3381814802a64a73baf99",
            config_sha: "39479916c2db192b5ac9764daddd0c744d83e023ad890c6976c0633ae4df8959",
            total_bytes: 63_206_169,
        },
    ),
    (
        "de",
        VoiceEntry {
            path: "de/de_DE/thorsten/medium",
            name: "de_DE-thorsten-medium",
            model_sha: "7e64762d8e5118bb578f2eea6207e1a35a8e0c30595010b666f983fc87bb7819",
            config_sha: "974adee790533adb273a1ac88f49027d2a1b8f0f2cf4905954a4791e79264e85",
            total_bytes: 63_206_113,
        },
    ),
    (
        "es",
        VoiceEntry {
            path: "es/es_MX/claude-high/medium",
            name: "es_MX-claude-high-medium",
            model_sha: "6658b03b1a6c316ee4c265a9896abc1393353c2d9e1bca7d66c2c442e222a917",
            config_sha: "0e0dda87c732f6f38771ff274a6380d9252f327dca77aa2963d5fbdf9ec54842",
            total_bytes: 63_206_111,
        },
    ),
    (
        "zh",
        VoiceEntry {
            path: "zh/zh_CN/huayan/medium",
            name: "zh_CN-huayan-medium",
            model_sha: "9929917bf8cabb26fd528ea44d3a6699c11e87317a14765312420be230be0f3d",
            config_sha: "d521dc45504a8ccc99e325822b35946dd701840bfb07e3dbb31a40929ed6a82b",
            total_bytes: 63_206_116,
        },
    ),
];

/// `runtimeAssets` — piper release tarballs the relay can self-install,
/// keyed by GOOS/GOARCH spelling (no darwin/arm64 release is published).
fn runtime_asset() -> Option<(&'static str, &'static str)> {
    match (goos(), goarch()) {
        ("linux", "amd64") => Some((
            "piper_linux_x86_64.tar.gz",
            "a50cb45f355b7af1f6d758c1b360717877ba0a398cc8cbe6d2a7a3a26e225992",
        )),
        ("linux", "arm64") => Some((
            "piper_linux_aarch64.tar.gz",
            "fea0fd2d87c54dbc7078d0f878289f404bd4d6eea6e7444a77835d1537ab88eb",
        )),
        ("darwin", "amd64") => Some((
            "piper_macos_x64.tar.gz",
            "ced85c0a3df13945b1e623b878a48fdc2854d5c485b4b67f62857cf551deaf8b",
        )),
        _ => None,
    }
}

/// `runtime.GOOS` spelled the Rust way (`env::consts::OS` says "macos").
fn goos() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    }
}

/// `runtime.GOARCH` spelled the Rust way.
fn goarch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "x86" | "i586" | "i686" => "386",
        other => other,
    }
}

/// `speechEnv` — `LERDR_<key>` takes precedence over the legacy
/// `HERDR_<key>`; empty values read as unset.
fn speech_env(key: &str) -> Option<String> {
    std::env::var(format!("LERDR_{key}"))
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::env::var(format!("HERDR_{key}"))
                .ok()
                .filter(|value| !value.is_empty())
        })
}

/// `voiceBaseURL`.
fn voice_base_url() -> String {
    speech_env("PIPER_VOICE_BASE_URL")
        .unwrap_or_else(|| "https://huggingface.co/rhasspy/piper-voices/resolve/v1.0.0".to_owned())
}

/// `runtimeBaseURL`.
fn runtime_base_url() -> String {
    speech_env("PIPER_RUNTIME_BASE_URL").unwrap_or_else(|| {
        "https://github.com/rhasspy/piper/releases/download/2023.11.14-2".to_owned()
    })
}

/// `speechCache` — `XDG_CACHE_HOME/lerdr/speech`, keeping the legacy
/// `herdr-mobile-relay` dir when it already holds voices.
fn speech_cache(home: &Path) -> PathBuf {
    speech_cache_in(cache_base(home), home)
}

fn cache_base(home: &Path) -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| home.join(".cache"))
}

/// `speechCache` with the resolved base dir — a free function so tests
/// don't touch process env.
fn speech_cache_in(base: PathBuf, _home: &Path) -> PathBuf {
    let current = base.join("lerdr").join("speech");
    if current.exists() {
        return current;
    }
    let legacy = base.join("herdr-mobile-relay").join("speech");
    if legacy.exists() {
        return legacy;
    }
    current
}

/// `languageLabel` — the display names baked into failure messages.
fn language_label(language: &str) -> &str {
    match language {
        "en" => "English",
        "fr" => "French",
        "de" => "German",
        "es" => "Spanish",
        "zh" => "Chinese",
        _ => language,
    }
}

/// `speechCatalog`'s entry lookup (the oracle stores the same table in a
/// map; unknown languages are rejected before it's consulted).
fn voice_entry(language: &str) -> Option<&'static VoiceEntry> {
    VOICE_CATALOG
        .iter()
        .find(|(key, _)| *key == language)
        .map(|(_, entry)| entry)
}

/// `speech.Offered` membership.
fn offered(language: &str) -> bool {
    OFFERED.contains(&language)
}

// ── shared speech state (server.go's speechRequests/speechLanguages) ────

/// One `speechRequest` map entry: `seq` disambiguates same-key
/// replacements at removal time, `cancelled` is the oracle's flag, and
/// `cancel` kills the engine process while the request is in flight.
struct InFlight {
    seq: u64,
    cancelled: bool,
    cancel: Option<CancellationToken>,
}

/// `server.speechRequests` + `server.speechLanguages` under one lock
/// (`speechMu`). `next_seq` is the replacement-stamp the oracle gets from
/// pointer identity on the map value.
#[derive(Default)]
struct SpeechState {
    requests: HashMap<String, InFlight>,
    languages: Vec<String>,
    next_seq: u64,
}

/// A live `speak_text` registration — the map key, the sequence stamp,
/// and the token the engine waits on. Returned to the handler so
/// `finish` only evicts the entry that created it.
pub(crate) struct Registration {
    key: String,
    seq: u64,
    token: CancellationToken,
}

struct SpeechInner {
    engine: Arc<dyn SpeechEngine>,
    state: Mutex<SpeechState>,
}

/// The speech subsystem one relay session hands to every action context.
/// `Clone` copies share `inner` — the oracle's server-level
/// `speechRequests` map and cached `speechLanguages` — so a `cancel_speech`
/// from any session reaches the in-flight synthesis it names, and voice
/// installs serialize process-wide via the engine.
#[derive(Clone)]
pub(crate) struct Speech {
    inner: Arc<SpeechInner>,
}

impl Speech {
    /// `Speech::default()` uses the real engine detection; tests inject a
    /// fake through `with_engine`.
    pub(crate) fn with_engine(engine: Arc<dyn SpeechEngine>) -> Self {
        Self {
            inner: Arc::new(SpeechInner {
                engine,
                state: Mutex::new(SpeechState::default()),
            }),
        }
    }

    /// `s.speechStatus` + `rememberSpeechLanguages` — refresh the cached
    /// speakable-language list while answering with the catalog. Runs off
    /// the executor: detection probes binaries and scans the voice cache.
    async fn catalog(&self) -> Catalog {
        let engine = self.inner.engine.clone();
        let catalog = match tokio::task::spawn_blocking(move || engine.catalog()).await {
            Ok(catalog) => catalog,
            Err(err) => {
                tracing::warn!(error = %err, "speech engine catalog probe panicked");
                Catalog::default()
            }
        };
        self.inner
            .state
            .lock()
            .expect("speech state poisoned")
            .languages = catalog.languages.clone();
        catalog
    }

    /// `s.speakableLanguages`.
    async fn speakable_languages(&self) -> Vec<String> {
        self.catalog().await.languages
    }

    /// `speakText`'s map insert: a same-key predecessor gets cancelled
    /// (unless it was already cancelled, in which case the new request
    /// inherits the flag — tombstone pre-cancellation included).
    fn register(&self, client_id: &str, speech_request_id: &str) -> Registration {
        let key = format!("{client_id}\x00{speech_request_id}");
        let token = CancellationToken::new();
        let mut state = self.inner.state.lock().expect("speech state poisoned");
        state.next_seq += 1;
        let seq = state.next_seq;
        let mut request = InFlight {
            seq,
            cancelled: false,
            cancel: Some(token.clone()),
        };
        if let Some(previous) = state.requests.get(&key) {
            if previous.cancelled {
                request.cancelled = true;
            } else if let Some(cancel) = &previous.cancel {
                cancel.cancel();
            }
        }
        if request.cancelled {
            token.cancel();
        }
        state.requests.insert(key.clone(), request);
        Registration { key, seq, token }
    }

    /// `s.Synthesize(ctx, ...)` — the 15s bound rides along as the
    /// deadline the engine enforces while its child runs.
    async fn synthesize(
        &self,
        registration: &Registration,
        text: &str,
        language: &str,
    ) -> Result<Vec<u8>, String> {
        self.inner
            .engine
            .synthesize(
                registration.token.clone(),
                Instant::now() + SYNTH_TIMEOUT,
                text,
                language,
            )
            .await
    }

    /// The oracle's `defer` — cancel + remove — except removal is
    /// stamped so a newer same-key registration survives.
    fn finish(&self, registration: &Registration) {
        registration.token.cancel();
        let mut state = self.inner.state.lock().expect("speech state poisoned");
        if state
            .requests
            .get(&registration.key)
            .is_some_and(|request| request.seq == registration.seq)
        {
            state.requests.remove(&registration.key);
        }
    }

    /// `cancelSpeech`: empty ids do nothing; a live request is flagged and
    /// cancelled; an absent one becomes a tombstone (evicting stale
    /// tombstones first, bounded at 128).
    fn cancel(&self, client_id: &str, speech_request_id: &str) {
        if client_id.is_empty() || speech_request_id.is_empty() {
            return;
        }
        let key = format!("{client_id}\x00{speech_request_id}");
        let mut state = self.inner.state.lock().expect("speech state poisoned");
        if let Some(request) = state.requests.get_mut(&key) {
            request.cancelled = true;
            if let Some(cancel) = &request.cancel {
                cancel.cancel();
            }
            return;
        }
        if state.requests.len() >= MAX_IN_FLIGHT {
            state
                .requests
                .retain(|_, request| !(request.cancelled && request.cancel.is_none()));
            if state.requests.len() >= MAX_IN_FLIGHT {
                return;
            }
        }
        state.requests.insert(
            key,
            InFlight {
                seq: 0,
                cancelled: true,
                cancel: None,
            },
        );
    }

    /// `s.Install` — bounded by `InstallTimeout`; the cancel token is
    /// fresh because the oracle binds this to the client's lifetime,
    /// which `ActionContext` doesn't expose.
    async fn install(&self, language: &str) -> Result<(), String> {
        self.inner
            .engine
            .install(
                CancellationToken::new(),
                Instant::now() + INSTALL_TIMEOUT,
                language,
            )
            .await
    }

    /// `s.Remove`.
    async fn remove(&self, language: &str) -> Result<(), String> {
        self.inner.engine.remove(language).await
    }
}

impl Default for Speech {
    fn default() -> Self {
        Self::with_engine(Arc::new(SystemEngine::new()))
    }
}

// ── engine abstraction ──────────────────────────────────────────────────

/// `speech.Status` — the voice catalog answer.
#[derive(Clone, Debug, Default)]
pub(crate) struct Catalog {
    pub cache_dir: String,
    pub engine_installed: bool,
    pub management_supported: bool,
    /// `Status.Languages` — offered languages an installed engine can
    /// speak. Serializes as `null` when empty (Go's nil slice).
    pub languages: Vec<String>,
    pub voices: Vec<VoiceStatus>,
}

/// `speech.VoiceStatus`.
#[derive(Clone, Debug, Default)]
pub(crate) struct VoiceStatus {
    pub language: String,
    pub name: String,
    pub installed: bool,
    pub bytes: i64,
    pub engine: String,
}

/// The `internal/speech` engine surface the action layer uses. Detection
/// is synchronous (filesystem probes and short `* --help`/`say -v ?`
/// execs); synthesis and installs are futures so a cancellation token can
/// interrupt them — the oracle's `ctx` split into `cancel` + `deadline`.
pub(crate) trait SpeechEngine: Send + Sync {
    /// `speech.Status` — `engineInstalled`, `managementSupported`, the
    /// speakable `languages`, and one `VoiceStatus` per offered language.
    fn catalog(&self) -> Catalog;

    /// `speech.Synthesize` — mono 16-bit PCM WAV within the frame budget,
    /// or a server-side detail string (never the wire error).
    fn synthesize<'a>(
        &'a self,
        cancel: CancellationToken,
        deadline: Instant,
        text: &'a str,
        language: &'a str,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send + 'a>>;

    /// `speech.Install` — ensure an engine, then download the pinned
    /// voice pair for `language`.
    fn install<'a>(
        &'a self,
        cancel: CancellationToken,
        deadline: Instant,
        language: &'a str,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

    /// `speech.Remove` — delete the voice pair; missing files are fine.
    fn remove<'a>(
        &'a self,
        language: &'a str,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;
}

// ── SystemEngine: the `internal/speech` port ────────────────────────────

/// `runtimeProbe` — a cached `piper --help` verdict keyed by stat stamp.
#[derive(Clone)]
struct RuntimeProbe {
    size: u64,
    mod_time: i64,
    mode: u32,
    healthy: bool,
}

/// `engines()` candidates — binary name, non-PATH fallbacks, whether the
/// script rides an argv slot (`flite -t`) instead of stdin, and how the
/// per-language voice name is found.
struct Candidate {
    binary: &'static str,
    fallback: Vec<PathBuf>,
    text_arg: bool,
    kind: Kind,
}

enum Kind {
    Piper {
        models: HashMap<String, String>,
    },
    #[cfg(target_os = "macos")]
    Say {
        voices: Arc<HashMap<String, String>>,
    },
    Espeak,
    Flite,
}

impl Candidate {
    /// The candidate's `voice` closure: piper resolves a scanned model
    /// path, `say` a voice name, espeak a language code, flite accepts
    /// English only.
    fn voice(&self, language: &str) -> Option<String> {
        match &self.kind {
            Kind::Piper { models } => models.get(language).cloned(),
            #[cfg(target_os = "macos")]
            Kind::Say { voices } => voices.get(language).cloned(),
            Kind::Espeak => {
                if !offered(language) {
                    None
                } else if language == "zh" {
                    // Mandarin on espeak engines.
                    Some("cmn".to_owned())
                } else {
                    Some(language.to_owned())
                }
            }
            Kind::Flite => (language == "en").then(String::new),
        }
    }

    /// The candidate's `argv` closure — `out` is the temp WAV path.
    fn argv(&self, voice: &str, out: &Path) -> Vec<String> {
        let out = out.to_string_lossy().into_owned();
        match &self.kind {
            Kind::Piper { .. } => vec![
                "--model".to_owned(),
                voice.to_owned(),
                "--output_file".to_owned(),
                out,
            ],
            #[cfg(target_os = "macos")]
            Kind::Say { .. } => vec![
                "-v".to_owned(),
                voice.to_owned(),
                "-o".to_owned(),
                out,
                "--file-format=WAVE".to_owned(),
                "--data-format=LEI16@22050".to_owned(),
            ],
            Kind::Espeak => vec![
                "-v".to_owned(),
                voice.to_owned(),
                "-s".to_owned(),
                "175".to_owned(),
                "-w".to_owned(),
                out,
                "--stdin".to_owned(),
            ],
            Kind::Flite => vec!["-o".to_owned(), out],
        }
    }
}

/// The winning candidate — `selectEngine`'s `engineSelection`.
struct Selection<'a> {
    candidate: &'a Candidate,
    binary: PathBuf,
    voice: String,
}

/// `internal/speech`'s process-side state: the captured home dir,
/// `installMu`, `runtimeProbeState`, and `sayVoiceOnce`. Clone shares the
/// mutexes and caches so spawned blocking work sees one engine.
#[derive(Clone)]
pub(crate) struct SystemEngine {
    home: PathBuf,
    /// Tests pin the cache explicitly — `speechCache` otherwise reads
    /// `XDG_CACHE_HOME`, which leaks the host's real voice dir.
    cache_override: Option<PathBuf>,
    install_mu: Arc<Mutex<()>>,
    probes: Arc<Mutex<HashMap<PathBuf, RuntimeProbe>>>,
    #[cfg(target_os = "macos")]
    say_voices: Arc<OnceLock<Arc<HashMap<String, String>>>>,
}

impl SystemEngine {
    pub(crate) fn new() -> Self {
        Self::with_home(home_dir().unwrap_or_default())
    }

    /// `with_home` keeps tests off the process env — `os.UserHomeDir` is
    /// the only piece `engines()` takes from the environment.
    fn with_home(home: PathBuf) -> Self {
        Self {
            home,
            cache_override: None,
            install_mu: Arc::new(Mutex::new(())),
            probes: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(target_os = "macos")]
            say_voices: Arc::new(OnceLock::new()),
        }
    }

    /// `with_home` plus an explicit cache dir — full isolation from the
    /// host's `XDG_CACHE_HOME`.
    #[cfg(test)]
    fn with_home_and_cache(home: PathBuf, cache: PathBuf) -> Self {
        let mut engine = Self::with_home(home);
        engine.cache_override = Some(cache);
        engine
    }

    fn cache_dir(&self) -> PathBuf {
        self.cache_override
            .clone()
            .unwrap_or_else(|| speech_cache(&self.home))
    }

    /// `voiceDir`.
    fn voice_dir(&self) -> PathBuf {
        self.cache_dir().join("voices")
    }

    /// `runtimeBinary`.
    fn runtime_binary(&self) -> PathBuf {
        self.cache_dir().join("runtime").join("piper").join("piper")
    }

    /// `piperVoices` — scan the configured + conventional model dirs for
    /// `<lang>_*.onnx` pairs (sidecar `.json` required); the first
    /// directory wins per language.
    fn piper_voices(&self) -> HashMap<String, String> {
        let mut dirs = Vec::new();
        if let Some(configured) = speech_env("PIPER_VOICES") {
            dirs.push(PathBuf::from(configured));
        }
        dirs.push(self.cache_dir().join("voices"));
        dirs.push(self.home.join(".local/share/piper-voices"));
        dirs.push(PathBuf::from("/usr/local/share/piper-voices"));
        dirs.push(PathBuf::from("/usr/share/piper-voices"));
        piper_voices_in(&dirs)
    }

    /// `sayVoices` — `say -v ?` parsed once (macOS only, like
    /// `runtime.GOOS == "darwin"` in `engines()`).
    #[cfg(target_os = "macos")]
    fn say_voices(&self) -> Arc<HashMap<String, String>> {
        self.say_voices
            .get_or_init(|| {
                let listing = Command::new("say")
                    .args(["-v", "?"])
                    .output()
                    .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
                    .unwrap_or_default();
                Arc::new(parse_say_voices(&listing))
            })
            .clone()
    }

    /// `engines()` — the candidate list: piper first (with the model scan
    /// baked in), `say` on macOS, then the espeak pair and flite.
    fn candidates(&self) -> Vec<Candidate> {
        let mut candidates = vec![Candidate {
            binary: "piper",
            fallback: vec![
                self.runtime_binary(),
                self.home.join(".local/bin/piper"),
                PathBuf::from("/usr/local/bin/piper"),
                PathBuf::from("/opt/piper/piper"),
            ],
            text_arg: false,
            kind: Kind::Piper {
                models: self.piper_voices(),
            },
        }];
        #[cfg(target_os = "macos")]
        candidates.push(Candidate {
            binary: "say",
            fallback: Vec::new(),
            text_arg: false,
            kind: Kind::Say {
                voices: self.say_voices(),
            },
        });
        for binary in ["espeak-ng", "espeak"] {
            candidates.push(Candidate {
                binary,
                fallback: Vec::new(),
                text_arg: false,
                kind: Kind::Espeak,
            });
        }
        candidates.push(Candidate {
            binary: "flite",
            fallback: Vec::new(),
            text_arg: true,
            kind: Kind::Flite,
        });
        candidates
    }

    /// `lookup` — `exec.LookPath` then the fallbacks (any existing
    /// non-directory, matching `os.Stat` + `!IsDir`).
    fn lookup(candidate: &Candidate) -> Option<PathBuf> {
        if let Some(path) = look_path(candidate.binary) {
            return Some(path);
        }
        candidate
            .fallback
            .iter()
            .find(|path| std::fs::metadata(path).is_ok_and(|m| !m.is_dir()))
            .cloned()
    }

    /// `selectEngine` — first candidate whose voice exists and whose
    /// binary resolves; a runtime-path piper must also pass the health
    /// probe.
    fn select_engine<'a>(
        &self,
        candidates: &'a [Candidate],
        language: &str,
    ) -> Option<Selection<'a>> {
        for candidate in candidates {
            let Some(voice) = candidate.voice(language) else {
                continue;
            };
            let Some(binary) = Self::lookup(candidate) else {
                continue;
            };
            if candidate.binary == "piper"
                && clean(&binary) == clean(&self.runtime_binary())
                && !self.runtime_ready(&binary)
            {
                continue;
            }
            return Some(Selection {
                candidate,
                binary,
                voice,
            });
        }
        None
    }

    /// `Languages` — the offered languages an installed engine can speak.
    fn languages_in(&self, candidates: &[Candidate]) -> Vec<String> {
        OFFERED
            .iter()
            .filter(|language| self.select_engine(candidates, language).is_some())
            .map(|language| (*language).to_owned())
            .collect()
    }

    /// `piperInstalled` — a found piper binary counts; the downloaded
    /// runtime counts only when the probe says healthy.
    fn piper_installed(&self, candidates: &[Candidate]) -> bool {
        for candidate in candidates {
            if candidate.binary != "piper" {
                continue;
            }
            let Some(path) = Self::lookup(candidate) else {
                return false;
            };
            if clean(&path) == clean(&self.runtime_binary()) {
                return self.runtime_ready(&path);
            }
            return true;
        }
        false
    }

    /// `Status` — the catalog the handlers serialize.
    fn status(&self) -> Catalog {
        let candidates = self.candidates();
        let engine_installed = self.piper_installed(&candidates);
        let management_supported = engine_installed || runtime_asset().is_some();
        let voice_dir = self.voice_dir();
        let mut voices = Vec::with_capacity(OFFERED.len());
        for language in OFFERED {
            let entry = voice_entry(language).expect("offered language is catalogued");
            let mut current = VoiceStatus {
                language: language.to_owned(),
                name: entry.name.to_owned(),
                installed: false,
                bytes: entry.total_bytes,
                engine: String::new(),
            };
            if let Some(selection) = self.select_engine(&candidates, language) {
                current.engine = selection.candidate.binary.to_owned();
            }
            let model = voice_dir.join(format!("{}.onnx", entry.name));
            if let Ok(model_info) = std::fs::metadata(&model) {
                if let Ok(config_info) = std::fs::metadata(format!("{}.json", model.display())) {
                    current.installed = true;
                    current.bytes = (model_info.len() + config_info.len()) as i64;
                }
            }
            voices.push(current);
        }
        Catalog {
            cache_dir: self.cache_dir().to_string_lossy().into_owned(),
            engine_installed,
            management_supported,
            languages: self.languages_in(&candidates),
            voices,
        }
    }

    /// `runtimeReady` — cache `path`'s health by stat stamp so the probe
    /// runs once per installed binary.
    fn runtime_ready(&self, path: &Path) -> bool {
        let Ok(info) = std::fs::metadata(path) else {
            return false;
        };
        if info.is_dir() {
            return false;
        }
        let stamp = (
            info.len(),
            info.modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(0),
            mode_bits(&info),
        );
        {
            let probes = self.probes.lock().expect("runtime probes poisoned");
            if let Some(probe) = probes.get(path) {
                if (probe.size, probe.mod_time, probe.mode) == stamp {
                    return probe.healthy;
                }
            }
        }
        let healthy = self
            .start_runtime(
                path,
                &CancellationToken::new(),
                Instant::now() + RUNTIME_START_TIMEOUT,
            )
            .is_ok();
        self.probes.lock().expect("runtime probes poisoned").insert(
            path.to_path_buf(),
            RuntimeProbe {
                size: stamp.0,
                mod_time: stamp.1,
                mode: stamp.2,
                healthy,
            },
        );
        healthy
    }

    /// `forgetRuntimeProbe` — a swapped binary must be re-probed.
    fn forget_runtime_probe(&self, path: &Path) {
        self.probes
            .lock()
            .expect("runtime probes poisoned")
            .remove(path);
    }

    /// `startRuntime` — `binary --help` within the start budget; the
    /// parent's `deadline` bounds it too (`context.WithTimeout` inside
    /// the install ctx).
    fn start_runtime(
        &self,
        binary: &Path,
        cancel: &CancellationToken,
        deadline: Instant,
    ) -> Result<(), String> {
        let budget = deadline.min(Instant::now() + RUNTIME_START_TIMEOUT);
        let mut child = Command::new(binary)
            .arg("--help")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| format!("speech engine failed to start: {err}"))?;
        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());
        let status = poll_child(&mut child, cancel, budget);
        let detail = drain_text(stdout.join(), Some(stderr.join()));
        match status {
            Ok(exit) if exit.success() => Ok(()),
            Ok(exit) => {
                if detail.is_empty() {
                    Err(format!("speech engine failed to start: {exit}"))
                } else {
                    Err(format!("speech engine failed to start: {detail}"))
                }
            }
            Err(PollStop::Deadline) if Instant::now() < deadline => Err(format!(
                "speech engine did not start within {}s",
                RUNTIME_START_TIMEOUT.as_secs()
            )),
            Err(stop) => Err(format!("speech engine failed to start: {stop}")),
        }
    }

    /// `Synthesize` — the blocking body (spawned via `spawn_blocking` by
    /// the trait method so executor threads stay free).
    fn synthesize_blocking(
        &self,
        cancel: &CancellationToken,
        deadline: Instant,
        text: &str,
        language: &str,
    ) -> Result<Vec<u8>, String> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err("text is required".to_owned());
        }
        if trimmed.chars().count() > MAX_TEXT_RUNES {
            return Err(format!("text exceeds {MAX_TEXT_RUNES} characters"));
        }
        let candidates = self.candidates();
        let selected = self
            .select_engine(&candidates, language)
            .ok_or_else(|| format!("no installed engine speaks {language}"))?;
        let dir = TempDir::new_in(&std::env::temp_dir(), "lerdr-speech-")
            .map_err(|err| format!("create temp dir: {err}"))?;
        let out_path = dir.path().join("speech.wav");
        let mut args = selected.candidate.argv(&selected.voice, &out_path);
        if selected.candidate.text_arg {
            args.push("-t".to_owned());
            args.push(trimmed.to_owned());
        }
        let mut command = Command::new(&selected.binary);
        command
            .args(&args)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if selected.candidate.text_arg {
            command.stdin(Stdio::null());
        } else {
            command.stdin(Stdio::piped());
        }
        let mut child = command
            .spawn()
            .map_err(|err| format!("{}: {err}", selected.candidate.binary))?;
        if !selected.candidate.text_arg {
            if let Some(mut stdin) = child.stdin.take() {
                // `cmd.Stdin = strings.NewReader(trimmed)` — the engines
                // read the script from stdin; dropping the handle gives
                // them EOF.
                let _ = stdin.write_all(trimmed.as_bytes());
                let _ = stdin.flush();
            }
        }
        let stderr = drain(child.stderr.take());
        let status = poll_child(&mut child, cancel, deadline);
        let detail = drain_text(stderr.join(), None);
        let status = status.map_err(|stop| format!("{}: {stop}", selected.candidate.binary))?;
        if !status.success() {
            return Err(if detail.is_empty() {
                format!("{}: {status}", selected.candidate.binary)
            } else {
                format!("{}: {detail}", selected.candidate.binary)
            });
        }
        let raw = std::fs::read(&out_path)
            .map_err(|err| format!("read {} output: {err}", selected.candidate.binary))?;
        let (mut rate, mut samples) = parse_pcm16_mono(&raw)?;
        while 44 + samples.len() * 2 > MAX_WAV_BYTES && rate > 8000 {
            samples = decimate(&samples);
            rate /= 2;
        }
        if 44 + samples.len() * 2 > MAX_WAV_BYTES {
            return Err("synthesized audio exceeds the transport frame budget".to_owned());
        }
        Ok(encode_wav(rate, &samples))
    }

    /// `Install` — engine first (runtime download when nothing's
    /// installed), then the pinned voice pair. `installMu` serializes.
    fn install_blocking(
        &self,
        cancel: &CancellationToken,
        deadline: Instant,
        language: &str,
    ) -> Result<(), String> {
        let _guard = self.install_mu.lock().expect("speech install poisoned");
        let entry =
            voice_entry(language).ok_or_else(|| format!("unknown speech language {language:?}"))?;
        let candidates = self.candidates();
        if !self.piper_installed(&candidates) {
            self.install_runtime(cancel, deadline)?;
        }
        std::fs::create_dir_all(self.voice_dir())
            .map_err(|err| format!("create {}: {err}", self.voice_dir().display()))?;
        let model = self.voice_dir().join(format!("{}.onnx", entry.name));
        let base = format!("{}/{}/{}", voice_base_url(), entry.path, entry.name);
        download(
            cancel,
            deadline,
            &format!("{base}.onnx"),
            &model,
            entry.model_sha,
        )?;
        download(
            cancel,
            deadline,
            &format!("{base}.onnx.json"),
            &PathBuf::from(format!("{}.json", model.display())),
            entry.config_sha,
        )
    }

    /// `installRuntime` — download the pinned platform tarball into a
    /// staging dir, extract, probe, then atomically swap `piper` dirs.
    fn install_runtime(&self, cancel: &CancellationToken, deadline: Instant) -> Result<(), String> {
        let Some((asset_name, digest)) = runtime_asset() else {
            return Err(format!(
                "no speech engine is published for {}/{}",
                goos(),
                goarch()
            ));
        };
        let engine_dir = self
            .runtime_binary()
            .parent()
            .and_then(Path::parent)
            .expect("runtime binary has a grandparent")
            .to_path_buf();
        std::fs::create_dir_all(&engine_dir)
            .map_err(|err| format!("create {}: {err}", engine_dir.display()))?;
        let work = TempDir::new_in(&engine_dir, ".install-")
            .map_err(|err| format!("create staging dir: {err}"))?;
        let archive = work.path().join(asset_name);
        download(
            cancel,
            deadline,
            &format!("{}/{}", runtime_base_url(), asset_name),
            &archive,
            digest,
        )?;
        extract_tar_gz(&archive, work.path())?;
        self.start_runtime(&work.path().join("piper").join("piper"), cancel, deadline)
            .map_err(|err| format!("validate the speech engine: {err}"))?;
        let replaced = engine_dir.join("piper.replaced");
        let _ = std::fs::remove_dir_all(&replaced);
        let current = engine_dir.join("piper");
        if current.exists() {
            std::fs::rename(&current, &replaced)
                .map_err(|err| format!("replace the installed engine: {err}"))?;
        }
        std::fs::rename(work.path().join("piper"), &current)
            .map_err(|err| format!("install the speech engine: {err}"))?;
        // `forgetRuntimeProbe` — the oracle passes `current` (the dir);
        // probes are keyed by the binary path inside it, so the intent is
        // invalidating the swapped-in binary. The stat stamp would catch
        // the swap either way.
        self.forget_runtime_probe(&current.join("piper"));
        let _ = std::fs::remove_dir_all(&replaced);
        Ok(())
    }

    /// `Remove` — the voice pair goes; absent files are already done.
    fn remove_blocking(&self, language: &str) -> Result<(), String> {
        let _guard = self.install_mu.lock().expect("speech install poisoned");
        let entry =
            voice_entry(language).ok_or_else(|| format!("unknown speech language {language:?}"))?;
        let model = self.voice_dir().join(format!("{}.onnx", entry.name));
        for path in [
            model.clone(),
            PathBuf::from(format!("{}.json", model.display())),
        ] {
            match std::fs::remove_file(&path) {
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(format!(
                        "remove {}: {err}",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    ));
                }
                Ok(()) => {}
            }
        }
        Ok(())
    }
}

impl SpeechEngine for SystemEngine {
    fn catalog(&self) -> Catalog {
        self.status()
    }

    fn synthesize<'a>(
        &'a self,
        cancel: CancellationToken,
        deadline: Instant,
        text: &'a str,
        language: &'a str,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send + 'a>> {
        let engine = self.clone();
        let text = text.to_owned();
        let language = language.to_owned();
        Box::pin(async move {
            match tokio::task::spawn_blocking(move || {
                engine.synthesize_blocking(&cancel, deadline, &text, &language)
            })
            .await
            {
                Ok(result) => result,
                Err(err) => Err(format!("synthesis task failed: {err}")),
            }
        })
    }

    fn install<'a>(
        &'a self,
        cancel: CancellationToken,
        deadline: Instant,
        language: &'a str,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        let engine = self.clone();
        let language = language.to_owned();
        Box::pin(async move {
            match tokio::task::spawn_blocking(move || {
                engine.install_blocking(&cancel, deadline, &language)
            })
            .await
            {
                Ok(result) => result,
                Err(err) => Err(format!("install task failed: {err}")),
            }
        })
    }

    fn remove<'a>(
        &'a self,
        language: &'a str,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        let engine = self.clone();
        let language = language.to_owned();
        Box::pin(async move {
            match tokio::task::spawn_blocking(move || engine.remove_blocking(&language)).await {
                Ok(result) => result,
                Err(err) => Err(format!("remove task failed: {err}")),
            }
        })
    }
}

// ── process + filesystem helpers ────────────────────────────────────────

/// `poll_child` — wait on a spawned engine while honoring the
/// cancellation token and deadline; both kill the child, matching the
/// oracle's `cmd.Wait` under `ctx.Done()`.
enum PollStop {
    Cancelled,
    Deadline,
    Io(std::io::Error),
}

impl std::fmt::Display for PollStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str("cancelled"),
            Self::Deadline => f.write_str("timed out"),
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

fn poll_child(
    child: &mut Child,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<ExitStatus, PollStop> {
    loop {
        if cancel.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(PollStop::Cancelled);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(PollStop::Deadline);
        }
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => thread::sleep(POLL_INTERVAL),
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(PollStop::Io(err));
            }
        }
    }
}

/// `drain` — read a child's pipe to EOF on a helper thread so a chatty
/// engine can't deadlock on a full pipe buffer while we poll.
fn drain(pipe: Option<impl Read + Send + 'static>) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    })
}

fn drain_text(a: thread::Result<Vec<u8>>, b: Option<thread::Result<Vec<u8>>>) -> String {
    let mut text = String::new();
    if let Ok(bytes) = a {
        text.push_str(&String::from_utf8_lossy(&bytes));
    }
    if let Some(Ok(bytes)) = b {
        text.push_str(&String::from_utf8_lossy(&bytes));
    }
    text.trim().to_owned()
}

/// `exec.LookPath` — `dir/name` for each PATH entry; a regular file with
/// any exec bit counts (`findExecutable`).
fn look_path(binary: &str) -> Option<PathBuf> {
    if binary.contains(std::path::is_separator) {
        return is_executable(Path::new(binary)).then(|| PathBuf::from(binary));
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(binary);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|info| info.is_file() && info.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(unix)]
fn mode_bits(info: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    info.permissions().mode()
}

#[cfg(not(unix))]
fn mode_bits(info: &std::fs::Metadata) -> u32 {
    u32::from(info.readonly())
}

/// `filepath.Clean` — lexical normalization for the runtime-path compare.
fn clean(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// `piperVoices`' directory scan, split out so tests control the dir set.
fn piper_voices_in(dirs: &[PathBuf]) -> HashMap<String, String> {
    let mut found = HashMap::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut matches: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|path| path.extension() == Some(OsStr::new("onnx")))
            .collect();
        matches.sort();
        for model in matches {
            let Some(name) = model.file_name().and_then(OsStr::to_str) else {
                continue;
            };
            let language = name.split('_').next().unwrap_or("");
            if !offered(language) || found.contains_key(language) {
                continue;
            }
            let sidecar = PathBuf::from(format!("{}.json", model.display()));
            match std::fs::metadata(&sidecar) {
                Ok(info) if !info.is_dir() => {}
                _ => continue,
            }
            found.insert(language.to_owned(), model.to_string_lossy().into_owned());
        }
    }
    found
}

/// `preferredSayVoices` — the `say` voice shortlist per language (first
/// is best). Pure data + parser so it stays testable off macOS.
#[cfg(any(target_os = "macos", test))]
fn preferred_say_voices(language: &str) -> &'static [&'static str] {
    match language {
        "en" => &["Samantha", "Daniel", "Karen", "Moira", "Tessa"],
        "fr" => &["Thomas", "Amélie", "Amelie", "Audrey"],
        "de" => &["Anna", "Markus", "Petra", "Yannick"],
        "es" => &["Mónica", "Monica", "Paulina", "Jorge"],
        "zh" => &["Tingting", "Tian-Tian", "Meijia"],
        _ => &[],
    }
}

/// `parseSayVoices` — `say -v ?` lines are `Name  locale  # description`;
/// the voice must sit on the preferred list and the best rank wins.
#[cfg(any(target_os = "macos", test))]
fn parse_say_voices(listing: &str) -> HashMap<String, String> {
    let mut found: HashMap<String, String> = HashMap::new();
    let mut ranks: HashMap<String, usize> = HashMap::new();
    for line in listing.lines() {
        let description = line.split('#').next().unwrap_or("");
        let fields: Vec<&str> = description.split_whitespace().collect();
        if fields.len() < 2 {
            continue;
        }
        let name = fields[..fields.len() - 1].join(" ");
        let language = fields[fields.len() - 1]
            .split('_')
            .next()
            .unwrap_or("")
            .to_owned();
        let base_name = name.split(" (").next().unwrap_or("");
        let Some(rank) = preferred_say_voices(&language)
            .iter()
            .position(|preferred| base_name == *preferred)
        else {
            continue;
        };
        if ranks.get(&language).is_none_or(|current| rank < *current) {
            found.insert(language.clone(), name);
            ranks.insert(language, rank);
        }
    }
    found
}

/// `download` — skip when the pinned digest already sits at `dest`,
/// otherwise `curl` to a sibling `.part-` file, verify, rename. Mirrors
/// the oracle's `http.Get` + temp + `os.Rename`.
fn download(
    cancel: &CancellationToken,
    deadline: Instant,
    url: &str,
    destination: &Path,
    digest: &str,
) -> Result<(), String> {
    if file_digest(destination) == digest {
        return Ok(());
    }
    let name = destination
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".to_owned());
    let dir = destination.parent().unwrap_or_else(|| Path::new("."));
    let partial = dir.join(format!(".{name}.part-{}", unique_suffix()));
    let cleanup = |partial: &Path| {
        let _ = std::fs::remove_file(partial);
    };
    let mut child = Command::new("curl")
        .arg("--fail")
        .arg("--silent")
        .arg("--show-error")
        .arg("--location")
        .arg("--output")
        .arg(&partial)
        .arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("download {name}: {err}"))?;
    let stderr = drain(child.stderr.take());
    match poll_child(&mut child, cancel, deadline) {
        Err(stop) => {
            cleanup(&partial);
            Err(format!("download {name}: {stop}"))
        }
        Ok(status) if !status.success() => {
            cleanup(&partial);
            let detail = drain_text(stderr.join(), None);
            Err(if detail.is_empty() {
                format!("download {name}: {status}")
            } else {
                format!("download {name}: {detail}")
            })
        }
        Ok(_) => {
            if file_digest(&partial) != digest {
                cleanup(&partial);
                return Err(format!("{name} does not match its published checksum"));
            }
            std::fs::rename(&partial, destination).map_err(|err| {
                cleanup(&partial);
                format!("install {name}: {err}")
            })
        }
    }
}

/// `fileDigest` — SHA256 hex of the file, or "" when it can't be read.
fn file_digest(path: &Path) -> String {
    use sha2::Digest as _;
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(read) => hasher.update(&buf[..read]),
            Err(_) => return String::new(),
        }
    }
    hex::encode(hasher.finalize())
}

/// `extract` — `tar -xzf` into `dest` with the oracle's member-name and
/// symlink-target checks (`archivePath`/`safeLinkTarget`).
fn extract_tar_gz(archive: &Path, dest: &Path) -> Result<(), String> {
    let base = archive
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "archive".to_owned());
    let listing = Command::new("tar")
        .arg("-t")
        .arg("-z")
        .arg("-f")
        .arg(archive)
        .output()
        .map_err(|err| format!("read {base}: {err}"))?;
    if !listing.status.success() {
        return Err(format!(
            "read {base}: {}",
            String::from_utf8_lossy(&listing.stderr).trim()
        ));
    }
    for name in String::from_utf8_lossy(&listing.stdout).lines() {
        if unsafe_member_name(name) {
            return Err(format!("{base} contains unsafe path {name:?}"));
        }
    }
    let output = Command::new("tar")
        .arg("-x")
        .arg("-z")
        .arg("-f")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| format!("extract {base}: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "extract {base}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    check_symlinks(dest, dest)
}

/// `archivePath` — `filepath.Clean` the slash-separated member name, then
/// reject what the oracle rejects: empty/`.`/absolute names and any path
/// still leading with `..` after cleaning (`a/../b` cleans to `b` and is
/// fine).
fn unsafe_member_name(name: &str) -> bool {
    if name.starts_with('/') {
        return true;
    }
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
    stack.is_empty() || stack.first() == Some(&"..")
}

/// `safeLinkTarget` — post-extract check that no symlink target is
/// absolute or escapes via `..` (member-dir + target joined lexically).
fn check_symlinks(root: &Path, dir: &Path) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|err| format!("scan {}: {err}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|err| format!("stat {}: {err}", path.display()))?;
        if file_type.is_symlink() {
            let target = std::fs::read_link(&path)
                .map_err(|err| format!("read {}: {err}", path.display()))?;
            if target.as_os_str().is_empty() || target.is_absolute() {
                return Err(format!("archive link escapes {}", root.display()));
            }
            let resolved = path.parent().unwrap_or(dir).join(&target);
            if resolved
                .components()
                .any(|component| matches!(component, Component::ParentDir))
            {
                return Err(format!("archive link escapes {}", root.display()));
            }
        } else if file_type.is_dir() {
            check_symlinks(root, &path)?;
        }
    }
    Ok(())
}

/// `os.MkdirTemp` + the oracle's deferred `RemoveAll`, as a guard.
struct TempDir(PathBuf);

impl TempDir {
    fn new_in(parent: &Path, prefix: &str) -> std::io::Result<Self> {
        for _ in 0..64 {
            let candidate = parent.join(format!("{prefix}{}", unique_suffix()));
            match std::fs::create_dir(&candidate) {
                Ok(()) => return Ok(Self(candidate)),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(err),
            }
        }
        Err(std::io::Error::other("could not create temp dir"))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `os.CreateTemp`'s random suffix — pid + nanos + a counter.
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
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

// ── WAV canonicalization (speech.go parsePCM16Mono/decimate/encodeWAV) ──

/// `parsePCM16Mono` — RIFF/WAVE with fmt=PCM, mono, 16-bit only.
fn parse_pcm16_mono(raw: &[u8]) -> Result<(u32, Vec<i16>), String> {
    if raw.len() < 44 || &raw[0..4] != b"RIFF" || &raw[8..12] != b"WAVE" {
        return Err("not a RIFF WAVE file".to_owned());
    }
    let mut sample_rate = 0u32;
    let mut data: &[u8] = &[];
    let mut offset = 12usize;
    while offset + 8 <= raw.len() {
        let id = &raw[offset..offset + 4];
        let mut size = u32::from_le_bytes(raw[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let body = offset + 8;
        if body + size > raw.len() {
            size = raw.len() - body;
        }
        match id {
            b"fmt " => {
                if size < 16 {
                    return Err("truncated fmt chunk".to_owned());
                }
                let format = u16::from_le_bytes(raw[body..body + 2].try_into().unwrap());
                let channels = u16::from_le_bytes(raw[body + 2..body + 4].try_into().unwrap());
                let bits = u16::from_le_bytes(raw[body + 14..body + 16].try_into().unwrap());
                if format != 1 || channels != 1 || bits != 16 {
                    return Err(format!("unsupported format {format}/{channels}/{bits}"));
                }
                sample_rate = u32::from_le_bytes(raw[body + 4..body + 8].try_into().unwrap());
            }
            b"data" => data = &raw[body..body + size],
            _ => {}
        }
        offset = body + size + (size & 1);
    }
    if sample_rate == 0 || data.len() < 2 {
        return Err("missing audio data".to_owned());
    }
    let (pairs, _) = data.as_chunks::<2>();
    let samples = pairs
        .iter()
        .map(|chunk| i16::from_le_bytes(*chunk))
        .collect();
    Ok((sample_rate, samples))
}

/// `decimate` — keep every other sample to halve the sample rate.
fn decimate(samples: &[i16]) -> Vec<i16> {
    samples.iter().step_by(2).copied().collect()
}

/// `encodeWAV` — canonical 44-byte header + little-endian i16 payload.
fn encode_wav(rate: u32, samples: &[i16]) -> Vec<u8> {
    let data_size = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_size as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_size).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_size.to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

// ── wire frames ─────────────────────────────────────────────────────────

/// `s.speechVoicePayload(status)` — the `data` object embedded in
/// `command_result`s: `languages` is `null` when nothing is speakable
/// (Go's nil slice), and every voice row carries all five keys.
fn voice_payload(catalog: &Catalog) -> serde_json::Value {
    serde_json::json!({
        "cache_dir": catalog.cache_dir,
        "engine_installed": catalog.engine_installed,
        "languages": if catalog.languages.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::json!(catalog.languages)
        },
        "management_supported": catalog.management_supported,
        "voices": catalog
            .voices
            .iter()
            .map(|voice| serde_json::json!({
                "bytes": voice.bytes,
                "engine": voice.engine,
                "installed": voice.installed,
                "language": voice.language,
                "name": voice.name,
            }))
            .collect::<Vec<_>>(),
    })
}

/// The standalone `speech_voices` broadcast frame (`s.broadcastToAll`'s
/// payload) — same catalog, typed `SpeechVoicesMessage` encoding.
fn speech_voices_frame(catalog: &Catalog) -> Outbound {
    Outbound::SpeechVoices(SpeechVoicesMessage {
        cache_dir: Some(catalog.cache_dir.clone()),
        engine_installed: Some(catalog.engine_installed),
        languages: Some(if catalog.languages.is_empty() {
            MaybeNull::Null
        } else {
            MaybeNull::Value(catalog.languages.clone())
        }),
        management_supported: Some(catalog.management_supported),
        r#type: "speech_voices".to_owned(),
        voices: Some(MaybeNull::Value(
            catalog
                .voices
                .iter()
                .map(|voice| SpeechVoice {
                    bytes: voice.bytes,
                    engine: voice.engine.clone(),
                    installed: voice.installed,
                    language: voice.language.clone(),
                    name: voice.name.clone(),
                })
                .collect(),
        )),
    })
}

/// `sendCommandResult(…, false, "failed", error, "", data)` — the
/// oracle's speech failure shape: `command_result` phase `failed`, and
/// the dispatch receipt at `failed_before_dispatch`/`invalid_request`
/// (the codebase's uniform refusal classification, `Outcome::failed`
/// plus a data slot).
fn speech_failed(error: &str, data: Option<serde_json::Value>) -> Outcome {
    Outcome {
        ok: false,
        phase: "failed",
        error: error.to_owned(),
        pane_id: String::new(),
        data,
        receipt_phase: ActionReceiptPhase::FAILED_BEFORE_DISPATCH,
        receipt_error: Some(api_error_plain(error_codes::INVALID_REQUEST, error)),
    }
}

// ── handlers ────────────────────────────────────────────────────────────

/// `speak_text` — the oracle's `speakText`: engine and language gates
/// first, then register the request and synthesize with the 15s bound.
/// Answers `command_result` (+ terminal receipt); the audio is inline
/// base64 WAV under `data`.
pub(crate) async fn speak_text(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    let speakable = ctx.speech.speakable_languages().await;
    if speakable.is_empty() {
        return speech_failed("No speech engine is installed on this computer", None).frames(
            request_id,
            "speak_text",
            action_id,
        );
    }
    let language = message.raw_str("language").unwrap_or("");
    if !speakable.iter().any(|candidate| candidate == language) {
        return speech_failed("This computer has no voice for that language", None).frames(
            request_id,
            "speak_text",
            action_id,
        );
    }
    let speech_request_id = message.raw_str("speech_request_id").unwrap_or("");
    let registration = ctx.speech.register(&ctx.client_id, speech_request_id);
    let wav = ctx
        .speech
        .synthesize(&registration, &message.text, language)
        .await;
    ctx.speech.finish(&registration);
    match wav {
        Ok(wav) => Outcome::completed(
            "",
            Some(serde_json::json!({
                "format": "wav",
                "audio": base64::engine::general_purpose::STANDARD.encode(wav),
            })),
        )
        .frames(request_id, "speak_text", action_id),
        Err(err) => {
            tracing::warn!(error = %err, language, "speech synthesis failed");
            speech_failed("Speech synthesis failed on this computer", None).frames(
                request_id,
                "speak_text",
                action_id,
            )
        }
    }
}

/// `cancel_speech` — the oracle's `cancelSpeech`: scope the request key
/// by client, flag+cancel in-flight work, or drop a tombstone so the next
/// same-key `speak_text` starts cancelled. The oracle emits no frame; the
/// relay answers `confirmed` like its `unwatch_pane` sibling.
pub(crate) async fn cancel_speech(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    let speech_request_id = message.raw_str("speech_request_id").unwrap_or("");
    ctx.speech.cancel(&ctx.client_id, speech_request_id);
    vec![Outbound::ActionReceipt(action_receipt_response(
        request_id,
        ActionReceipt {
            action_id: action_id.to_owned(),
            phase: ActionReceiptPhase::from(ActionReceiptPhase::CONFIRMED),
            error: None,
        },
    ))]
}

/// `speech_voices_list` — `command_result` carrying `speechVoicePayload`.
pub(crate) async fn voices_list(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    _message: &Inbound,
) -> Vec<Outbound> {
    let catalog = ctx.speech.catalog().await;
    Outcome::completed("", Some(voice_payload(&catalog))).frames(
        request_id,
        "speech_voices_list",
        action_id,
    )
}

/// `speech_voice_install` — `changeSpeechVoice` with `install = true`.
pub(crate) async fn voice_install(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    change_speech_voice(ctx, request_id, action_id, "speech_voice_install", message).await
}

/// `speech_voice_remove` — `changeSpeechVoice` with `install = false`.
pub(crate) async fn voice_remove(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    change_speech_voice(ctx, request_id, action_id, "speech_voice_remove", message).await
}

/// `changeSpeechVoice` — offered-language gate, management-support gate
/// (installs only), run the engine change, then broadcast the refreshed
/// catalog and answer the caller.
async fn change_speech_voice(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    action: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    let language = message.raw_str("language").unwrap_or("");
    if !offered(language) {
        return speech_failed("That language is not one this app reads aloud", None)
            .frames(request_id, action, action_id);
    }
    let status = ctx.speech.catalog().await;
    if action != "speech_voice_remove" && !status.management_supported {
        return speech_failed(
            "Voice downloads are not supported on this computer",
            Some(voice_payload(&status)),
        )
        .frames(request_id, action, action_id);
    }
    let result = if action == "speech_voice_remove" {
        ctx.speech.remove(language).await
    } else {
        ctx.speech.install(language).await
    };
    let status = ctx.speech.catalog().await;
    let payload = voice_payload(&status);
    if let Err(err) = result {
        tracing::warn!(error = %err, language, action, "speech voice change failed");
        let verb = if action == "speech_voice_remove" {
            "Removing"
        } else {
            "Downloading"
        };
        let label = language_label(language);
        return speech_failed(
            &format!("{verb} the {label} voice failed on this computer"),
            Some(payload),
        )
        .frames(request_id, action, action_id);
    }
    let voices = speech_voices_frame(&status);
    // `broadcastToAll` — every session learns the catalog changed; the
    // requester is excluded from the fanout because the frame below is
    // already its first response (preserving broadcast-before-result).
    ctx.notices.send(voices.clone(), ctx.client_id.clone());
    let mut frames = vec![voices];
    frames.extend(Outcome::completed("", Some(payload)).frames(request_id, action, action_id));
    frames
}

#[cfg(test)]
mod tests {
    use lerdr_core::protocol::CommandResultMessage;
    use lerdr_herdr::{Client, SessionSnapshot};

    use super::*;
    use crate::actions::{leases::Leases, profiles::Resolver};
    use crate::topology::Topology;
    use crate::TopologyActor;

    // ── fake engine ─────────────────────────────────────────────────

    #[derive(Clone)]
    enum Synth {
        Bytes(Vec<u8>),
        Fail(String),
        /// Block until the request's token cancels (the oracle's
        /// `<-ctx.Done()` fake).
        UntilCancel,
    }

    struct FakeEngine {
        catalog: Mutex<Catalog>,
        synth: Mutex<Synth>,
        synth_calls: Mutex<Vec<(String, String, bool)>>,
        install_result: Mutex<Result<(), String>>,
        remove_result: Mutex<Result<(), String>>,
        installed: Mutex<Vec<String>>,
        removed: Mutex<Vec<String>>,
    }

    impl FakeEngine {
        fn new() -> Self {
            Self {
                catalog: Mutex::new(Catalog {
                    cache_dir: "/cache".to_owned(),
                    engine_installed: true,
                    management_supported: true,
                    languages: vec!["en".to_owned(), "fr".to_owned()],
                    voices: OFFERED
                        .iter()
                        .map(|language| VoiceStatus {
                            language: (*language).to_owned(),
                            name: format!("{language}-voice"),
                            installed: *language == "en",
                            bytes: 63_206_179,
                            engine: "piper".to_owned(),
                        })
                        .collect(),
                }),
                synth: Mutex::new(Synth::Bytes(b"RIFFfake".to_vec())),
                synth_calls: Mutex::new(Vec::new()),
                install_result: Mutex::new(Ok(())),
                remove_result: Mutex::new(Ok(())),
                installed: Mutex::new(Vec::new()),
                removed: Mutex::new(Vec::new()),
            }
        }
    }

    impl SpeechEngine for FakeEngine {
        fn catalog(&self) -> Catalog {
            self.catalog.lock().unwrap().clone()
        }

        fn synthesize<'a>(
            &'a self,
            cancel: CancellationToken,
            _deadline: Instant,
            text: &'a str,
            language: &'a str,
        ) -> std::pin::Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send + 'a>> {
            Box::pin(async move {
                self.synth_calls.lock().unwrap().push((
                    text.to_owned(),
                    language.to_owned(),
                    cancel.is_cancelled(),
                ));
                // Clone the script out — a MutexGuard can't live across
                // the `UntilCancel` await (the future must stay `Send`).
                let script = self.synth.lock().unwrap().clone();
                match script {
                    Synth::Bytes(bytes) => {
                        if cancel.is_cancelled() {
                            Err("cancelled".to_owned())
                        } else {
                            Ok(bytes)
                        }
                    }
                    Synth::Fail(err) => Err(err),
                    Synth::UntilCancel => {
                        cancel.cancelled().await;
                        Err("cancelled".to_owned())
                    }
                }
            })
        }

        fn install<'a>(
            &'a self,
            _cancel: CancellationToken,
            _deadline: Instant,
            language: &'a str,
        ) -> std::pin::Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
            self.installed.lock().unwrap().push(language.to_owned());
            let result = self.install_result.lock().unwrap().clone();
            Box::pin(async move { result })
        }

        fn remove<'a>(
            &'a self,
            language: &'a str,
        ) -> std::pin::Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
            self.removed.lock().unwrap().push(language.to_owned());
            let result = self.remove_result.lock().unwrap().clone();
            Box::pin(async move { result })
        }
    }

    // ── test scaffolding ────────────────────────────────────────────

    /// `ActionContext` over a dead Herdr socket — speech handlers never
    /// dial it; the rest of the context just has to construct.
    fn test_context(speech: Speech, client_id: &str) -> ActionContext {
        let client = Client::unix("/nonexistent/lerdr-speech-test.sock");
        let mut topology = Topology::default();
        topology.accept(SessionSnapshot::default());
        ActionContext {
            handle: TopologyActor::spawn(client.clone(), CancellationToken::new()),
            leases: Leases::new(client.clone()),
            profiles: Resolver::with_config_home(tempfile::tempdir().expect("tempdir").keep()),
            questions: crate::actions::questions::Questions::default(),
            uploads: crate::actions::uploads::Uploads::new(
                tempfile::tempdir().expect("tempdir").keep(),
            ),
            activities: crate::actions::activity::Journal::default(),
            push: crate::actions::push::Push::default(),
            speech,
            notices: crate::actions::Notices::default(),
            audit: None,
            device_id: "test-device".to_owned(),
            client,
            topology: Arc::new(topology),
            client_id: client_id.to_owned(),
        }
    }

    fn message(fields: serde_json::Value) -> Inbound {
        let mut map = serde_json::Map::new();
        map.insert("type".to_owned(), serde_json::json!("speak_text"));
        if let serde_json::Value::Object(extra) = fields {
            map.extend(extra);
        }
        Inbound::decode_map(&map).expect("inbound decodes")
    }

    fn command_result(outbound: &Outbound) -> CommandResultMessage {
        let Outbound::CommandResult(result) = outbound else {
            panic!("expected command_result, got {outbound:?}");
        };
        result.clone()
    }

    fn receipt(outbound: &Outbound) -> ActionReceipt {
        let Outbound::ActionReceipt(frame) = outbound else {
            panic!("expected action_receipt, got {outbound:?}");
        };
        frame.receipt.clone().expect("receipt present")
    }

    fn data(result: &CommandResultMessage) -> serde_json::Value {
        result
            .data
            .clone()
            .and_then(|value| value.into_value())
            .map(|raw| serde_json::from_str(raw.get()).unwrap())
            .expect("command_result carries data")
    }

    fn speech_with(engine: FakeEngine) -> Speech {
        Speech::with_engine(Arc::new(engine))
    }

    // ── speak_text ──────────────────────────────────────────────────

    #[tokio::test]
    async fn speak_text_without_engine_fails() {
        let engine = FakeEngine::new();
        engine.catalog.lock().unwrap().languages = Vec::new();
        let ctx = test_context(speech_with(engine), "client-1");
        let frames = speak_text(
            ctx,
            "req-1",
            "act-1",
            &message(serde_json::json!({
                "language": "en",
                "speech_request_id": "s1",
                "text": "hello",
            })),
        )
        .await;
        assert_eq!(frames.len(), 2);
        let result = command_result(&frames[0]);
        assert_eq!(result.ok, Some(false));
        assert_eq!(result.phase.as_deref(), Some("failed"));
        assert_eq!(
            result.error.as_deref(),
            Some("No speech engine is installed on this computer")
        );
        assert_eq!(result.action.as_deref(), Some("speak_text"));
        assert!(result.data.is_none());
        let receipt = receipt(&frames[1]);
        assert_eq!(
            receipt.phase.as_str(),
            ActionReceiptPhase::FAILED_BEFORE_DISPATCH
        );
        assert_eq!(receipt.action_id, "act-1");
    }

    #[tokio::test]
    async fn speak_text_without_voice_fails() {
        let ctx = test_context(speech_with(FakeEngine::new()), "client-1");
        let frames = speak_text(
            ctx,
            "req-1",
            "act-1",
            &message(serde_json::json!({
                "language": "de",
                "speech_request_id": "s1",
                "text": "hello",
            })),
        )
        .await;
        let result = command_result(&frames[0]);
        assert_eq!(result.ok, Some(false));
        assert_eq!(
            result.error.as_deref(),
            Some("This computer has no voice for that language")
        );
        assert!(result.data.is_none());
    }

    #[tokio::test]
    async fn speak_text_returns_inline_wav() {
        let shared = Arc::new(FakeEngine::new());
        let ctx = test_context(Speech::with_engine(shared.clone()), "client-1");
        let frames = speak_text(
            ctx,
            "req-9",
            "act-9",
            &message(serde_json::json!({
                "language": "fr",
                "speech_request_id": "s9",
                "text": "  bonjour  ",
            })),
        )
        .await;
        assert_eq!(frames.len(), 2);
        let result = command_result(&frames[0]);
        assert_eq!(result.ok, Some(true));
        assert_eq!(result.phase.as_deref(), Some("completed"));
        let body = data(&result);
        assert_eq!(body["format"], "wav");
        assert_eq!(
            body["audio"].as_str().unwrap(),
            base64::engine::general_purpose::STANDARD.encode(b"RIFFfake")
        );
        let calls = shared.synth_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "  bonjour  ");
        assert_eq!(calls[0].1, "fr");
        assert!(!calls[0].2, "token arrives uncancelled");
        let receipt = receipt(&frames[1]);
        assert_eq!(receipt.phase.as_str(), ActionReceiptPhase::CONFIRMED);
    }

    #[tokio::test]
    async fn speak_text_engine_error_is_generic() {
        let engine = FakeEngine::new();
        *engine.synth.lock().unwrap() = Synth::Fail("piper exploded".to_owned());
        let ctx = test_context(speech_with(engine), "client-1");
        let frames = speak_text(
            ctx,
            "req-1",
            "act-1",
            &message(serde_json::json!({
                "language": "en",
                "speech_request_id": "s1",
                "text": "hello",
            })),
        )
        .await;
        let result = command_result(&frames[0]);
        assert_eq!(result.ok, Some(false));
        assert_eq!(
            result.error.as_deref(),
            Some("Speech synthesis failed on this computer")
        );
        assert!(result.data.is_none());
    }

    // ── cancellation ────────────────────────────────────────────────

    #[tokio::test]
    async fn cancel_speech_tombstone_precancels() {
        let engine = Arc::new(FakeEngine::new());
        let speech = Speech::with_engine(engine.clone());
        let ctx = test_context(speech.clone(), "client-1");
        speech.cancel("client-1", "s1");
        let frames = speak_text(
            ctx,
            "req-1",
            "act-1",
            &message(serde_json::json!({
                "language": "en",
                "speech_request_id": "s1",
                "text": "hello",
            })),
        )
        .await;
        let result = command_result(&frames[0]);
        assert_eq!(result.ok, Some(false));
        assert_eq!(
            result.error.as_deref(),
            Some("Speech synthesis failed on this computer")
        );
        assert!(
            engine.synth_calls.lock().unwrap()[0].2,
            "synthesis saw a pre-cancelled token"
        );
    }

    #[tokio::test]
    async fn cancel_speech_stops_inflight() {
        let engine = Arc::new(FakeEngine::new());
        *engine.synth.lock().unwrap() = Synth::UntilCancel;
        let speech = Speech::with_engine(engine);
        let ctx = test_context(speech.clone(), "client-1");
        let task = tokio::spawn({
            let ctx = ctx.clone();
            async move {
                speak_text(
                    ctx,
                    "req-1",
                    "act-1",
                    &message(serde_json::json!({
                        "language": "en",
                        "speech_request_id": "s1",
                        "text": "hello",
                    })),
                )
                .await
            }
        });
        // Let the speak request register before cancelling.
        tokio::task::yield_now().await;
        let frames = cancel_speech(
            ctx,
            "req-2",
            "act-2",
            &message(serde_json::json!({"speech_request_id": "s1"})),
        )
        .await;
        assert_eq!(frames.len(), 1);
        assert_eq!(
            receipt(&frames[0]).phase.as_str(),
            ActionReceiptPhase::CONFIRMED
        );
        let result = command_result(&task.await.unwrap()[0]);
        assert_eq!(result.ok, Some(false));
        assert_eq!(
            result.error.as_deref(),
            Some("Speech synthesis failed on this computer")
        );
    }

    #[tokio::test]
    async fn cancel_speech_is_scoped_by_client() {
        let engine = Arc::new(FakeEngine::new());
        let speech = Speech::with_engine(engine);
        // A tombstone for client-1 must not pre-cancel client-2's request.
        speech.cancel("client-1", "s1");
        let ctx = test_context(speech, "client-2");
        let frames = speak_text(
            ctx,
            "req-1",
            "act-1",
            &message(serde_json::json!({
                "language": "en",
                "speech_request_id": "s1",
                "text": "hello",
            })),
        )
        .await;
        assert_eq!(command_result(&frames[0]).ok, Some(true));
    }

    #[tokio::test]
    async fn cancel_speech_empty_ids_do_nothing() {
        let engine = Arc::new(FakeEngine::new());
        let speech = Speech::with_engine(engine);
        speech.cancel("", "s1");
        speech.cancel("client-1", "");
        let state = speech.inner.state.lock().unwrap();
        assert!(state.requests.is_empty());
    }

    #[tokio::test]
    async fn second_same_key_speak_cancels_first() {
        let engine = Arc::new(FakeEngine::new());
        *engine.synth.lock().unwrap() = Synth::UntilCancel;
        let speech = Speech::with_engine(engine.clone());
        let first = speech.register("client-1", "s1");
        let second = speech.register("client-1", "s1");
        assert!(first.token.is_cancelled(), "first request was cancelled");
        assert!(!second.token.is_cancelled());
        speech.finish(&first);
        // first's finish must not evict the newer same-key entry.
        let state = speech.inner.state.lock().unwrap();
        assert_eq!(state.requests["client-1\x00s1"].seq, second.seq);
        drop(state);
        speech.finish(&second);
        assert!(speech.inner.state.lock().unwrap().requests.is_empty());
    }

    #[tokio::test]
    async fn cancel_tombstones_are_capped() {
        let speech = Speech::with_engine(Arc::new(FakeEngine::new()));
        for index in 0..MAX_IN_FLIGHT + 4 {
            speech.cancel("client-1", &format!("s{index}"));
        }
        // The 129th cancel evicts every stale tombstone before inserting
        // its own — the map refills rather than growing past the cap.
        let state = speech.inner.state.lock().unwrap();
        assert_eq!(state.requests.len(), 4);
        assert!(state.requests.len() <= MAX_IN_FLIGHT);
    }

    // ── voices ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn voices_list_returns_catalog() {
        let ctx = test_context(speech_with(FakeEngine::new()), "client-1");
        let frames = voices_list(ctx, "req-1", "act-1", &message(serde_json::json!({}))).await;
        assert_eq!(frames.len(), 2);
        let result = command_result(&frames[0]);
        assert_eq!(result.ok, Some(true));
        assert_eq!(result.action.as_deref(), Some("speech_voices_list"));
        let body = data(&result);
        assert_eq!(body["cache_dir"], "/cache");
        assert_eq!(body["engine_installed"], true);
        assert_eq!(body["management_supported"], true);
        assert_eq!(body["languages"], serde_json::json!(["en", "fr"]));
        let voices = body["voices"].as_array().unwrap();
        assert_eq!(voices.len(), OFFERED.len());
        let english = &voices[0];
        assert_eq!(english["language"], "en");
        assert_eq!(english["name"], "en-voice");
        assert_eq!(english["installed"], true);
        assert_eq!(english["bytes"], 63_206_179);
        assert_eq!(english["engine"], "piper");
        // All five keys exist on every row — even when false/zero.
        let french = &voices[1];
        assert!(french.get("engine").is_some());
        assert!(french.get("bytes").is_some());
        assert!(french.get("installed").is_some());
        let receipt = receipt(&frames[1]);
        assert_eq!(receipt.phase.as_str(), ActionReceiptPhase::CONFIRMED);
    }

    #[tokio::test]
    async fn voices_list_null_languages_when_empty() {
        let engine = FakeEngine::new();
        engine.catalog.lock().unwrap().languages = Vec::new();
        let ctx = test_context(speech_with(engine), "client-1");
        let frames = voices_list(ctx, "req-1", "act-1", &message(serde_json::json!({}))).await;
        let body = data(&command_result(&frames[0]));
        assert!(body["languages"].is_null(), "nil Go slice -> null");
    }

    #[tokio::test]
    async fn voice_install_rejects_unoffered_language() {
        let engine = Arc::new(FakeEngine::new());
        let ctx = test_context(Speech::with_engine(engine.clone()), "client-1");
        let frames = voice_install(
            ctx,
            "req-1",
            "act-1",
            &message(serde_json::json!({"language": "ja"})),
        )
        .await;
        let result = command_result(&frames[0]);
        assert_eq!(result.ok, Some(false));
        assert_eq!(result.action.as_deref(), Some("speech_voice_install"));
        assert_eq!(
            result.error.as_deref(),
            Some("That language is not one this app reads aloud")
        );
        assert!(result.data.is_none());
        assert!(engine.installed.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn voice_install_unsupported_keeps_payload() {
        let engine = FakeEngine::new();
        engine.catalog.lock().unwrap().management_supported = false;
        let ctx = test_context(speech_with(engine), "client-1");
        let frames = voice_install(
            ctx,
            "req-1",
            "act-1",
            &message(serde_json::json!({"language": "fr"})),
        )
        .await;
        let result = command_result(&frames[0]);
        assert_eq!(result.ok, Some(false));
        assert_eq!(
            result.error.as_deref(),
            Some("Voice downloads are not supported on this computer")
        );
        let body = data(&result);
        assert_eq!(body["management_supported"], false);
        assert!(body["voices"].is_array());
    }

    #[tokio::test]
    async fn voice_install_broadcasts_then_results() {
        let engine = Arc::new(FakeEngine::new());
        let ctx = test_context(Speech::with_engine(engine.clone()), "client-1");
        let frames = voice_install(
            ctx,
            "req-1",
            "act-1",
            &message(serde_json::json!({"language": "fr"})),
        )
        .await;
        assert_eq!(frames.len(), 3);
        let Outbound::SpeechVoices(broadcast) = &frames[0] else {
            panic!("first frame must be the speech_voices broadcast");
        };
        assert_eq!(broadcast.r#type, "speech_voices");
        assert_eq!(broadcast.management_supported, Some(true));
        let voices = broadcast
            .voices
            .clone()
            .and_then(|value| value.into_value())
            .unwrap();
        assert_eq!(voices.len(), OFFERED.len());
        let result = command_result(&frames[1]);
        assert_eq!(result.ok, Some(true));
        assert_eq!(result.phase.as_deref(), Some("completed"));
        assert!(data(&result)["voices"].is_array());
        assert_eq!(
            receipt(&frames[2]).phase.as_str(),
            ActionReceiptPhase::CONFIRMED
        );
        assert_eq!(engine.installed.lock().unwrap().as_slice(), ["fr"]);
    }

    #[tokio::test]
    async fn voice_install_fans_out_to_peer_sessions() {
        // `broadcastToAll` — the catalog frame also leaves via the
        // notices channel, addressed to every session but the requester.
        let engine = Arc::new(FakeEngine::new());
        let ctx = test_context(Speech::with_engine(engine), "client-1");
        let mut rx = ctx.notices.subscribe();
        let frames = voice_install(
            ctx,
            "req-1",
            "act-1",
            &message(serde_json::json!({"language": "fr"})),
        )
        .await;
        assert_eq!(frames.len(), 3);
        let notice = rx.try_recv().expect("notice for peer sessions");
        assert_eq!(notice.exclude_client, "client-1");
        assert!(matches!(notice.frame, Outbound::SpeechVoices(_)));
    }

    #[tokio::test]
    async fn voice_install_failure_sends_no_fanout() {
        let engine = Arc::new(FakeEngine::new());
        *engine.install_result.lock().unwrap() = Err("curl exploded".to_owned());
        let ctx = test_context(Speech::with_engine(engine), "client-1");
        let mut rx = ctx.notices.subscribe();
        let frames = voice_install(
            ctx,
            "req-1",
            "act-1",
            &message(serde_json::json!({"language": "fr"})),
        )
        .await;
        assert_eq!(frames.len(), 2);
        assert!(rx.try_recv().is_err(), "no fanout on failure");
    }

    #[tokio::test]
    async fn voice_install_failure_keeps_payload() {
        let engine = FakeEngine::new();
        *engine.install_result.lock().unwrap() = Err("curl exploded".to_owned());
        let ctx = test_context(speech_with(engine), "client-1");
        let frames = voice_install(
            ctx,
            "req-1",
            "act-1",
            &message(serde_json::json!({"language": "fr"})),
        )
        .await;
        assert_eq!(frames.len(), 2, "no broadcast on failure");
        let result = command_result(&frames[0]);
        assert_eq!(result.ok, Some(false));
        assert_eq!(
            result.error.as_deref(),
            Some("Downloading the French voice failed on this computer")
        );
        assert!(data(&result)["voices"].is_array());
    }

    #[tokio::test]
    async fn voice_remove_skips_support_gate() {
        let engine = FakeEngine::new();
        // `speech_voice_remove` ignores managementSupported entirely.
        engine.catalog.lock().unwrap().management_supported = false;
        let shared = Arc::new(engine);
        let ctx = test_context(Speech::with_engine(shared.clone()), "client-1");
        let frames = voice_remove(
            ctx,
            "req-1",
            "act-1",
            &message(serde_json::json!({"language": "en"})),
        )
        .await;
        assert_eq!(frames.len(), 3);
        assert!(matches!(frames[0], Outbound::SpeechVoices(_)));
        assert_eq!(command_result(&frames[1]).ok, Some(true));
        assert_eq!(shared.removed.lock().unwrap().as_slice(), ["en"]);
    }

    #[tokio::test]
    async fn voice_remove_failure_keeps_payload() {
        let engine = FakeEngine::new();
        *engine.remove_result.lock().unwrap() = Err("disk exploded".to_owned());
        let ctx = test_context(speech_with(engine), "client-1");
        let frames = voice_remove(
            ctx,
            "req-1",
            "act-1",
            &message(serde_json::json!({"language": "de"})),
        )
        .await;
        let result = command_result(&frames[0]);
        assert_eq!(
            result.error.as_deref(),
            Some("Removing the German voice failed on this computer")
        );
        assert!(data(&result)["voices"].is_array());
    }

    // ── engine internals (pure pieces) ──────────────────────────────

    #[test]
    fn wav_round_trip() {
        let samples = vec![0i16, 100, -100, i16::MAX, i16::MIN];
        let wav = encode_wav(22050, &samples);
        let (rate, parsed) = parse_pcm16_mono(&wav).unwrap();
        assert_eq!(rate, 22050);
        assert_eq!(parsed, samples);
    }

    #[test]
    fn wav_rejects_non_pcm16_mono() {
        let mut wav = encode_wav(22050, &[0i16; 4]);
        // fmt chunk body starts at 20; channels at +22, bits at +34.
        wav[22] = 2; // stereo
        assert!(parse_pcm16_mono(&wav)
            .unwrap_err()
            .contains("unsupported format"));
        wav[22] = 1;
        wav[34] = 8; // 8-bit
        assert!(parse_pcm16_mono(&wav)
            .unwrap_err()
            .contains("unsupported format"));
        assert!(parse_pcm16_mono(b"not a wave").is_err());
    }

    #[test]
    fn decimate_halves() {
        assert_eq!(decimate(&[1, 2, 3, 4, 5]), vec![1, 3, 5]);
    }

    #[test]
    fn piper_voices_scan_rules() {
        let dir = tempfile::tempdir().unwrap();
        let voices = dir.path().join("voices");
        std::fs::create_dir_all(&voices).unwrap();
        // A model without its sidecar is skipped.
        std::fs::write(voices.join("de_DE-x-medium.onnx"), b"m").unwrap();
        // The first model per language wins (sorted order).
        std::fs::write(voices.join("en_US-aaa-medium.onnx"), b"m").unwrap();
        std::fs::write(voices.join("en_US-aaa-medium.onnx.json"), b"c").unwrap();
        std::fs::write(voices.join("en_US-zzz-medium.onnx"), b"m").unwrap();
        std::fs::write(voices.join("en_US-zzz-medium.onnx.json"), b"c").unwrap();
        // Non-offered languages ignored.
        std::fs::write(voices.join("ja_JP-x.onnx"), b"m").unwrap();
        std::fs::write(voices.join("ja_JP-x.onnx.json"), b"c").unwrap();
        let found = piper_voices_in(&[voices]);
        assert_eq!(found.len(), 1);
        assert!(found["en"].ends_with("en_US-aaa-medium.onnx"));
    }

    #[test]
    fn speech_cache_prefers_current_then_legacy() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("cache");
        let home = PathBuf::from("/nonexistent-home");
        assert_eq!(
            speech_cache_in(base.clone(), &home),
            base.join("lerdr").join("speech")
        );
        let legacy = base.join("herdr-mobile-relay").join("speech");
        std::fs::create_dir_all(&legacy).unwrap();
        assert_eq!(speech_cache_in(base.clone(), &home), legacy);
        let current = base.join("lerdr").join("speech");
        std::fs::create_dir_all(&current).unwrap();
        assert_eq!(speech_cache_in(base.clone(), &home), current);
    }

    #[test]
    fn language_labels_match_oracle() {
        assert_eq!(language_label("fr"), "French");
        assert_eq!(language_label("zh"), "Chinese");
        assert_eq!(language_label("xx"), "xx");
    }

    #[test]
    fn say_voice_parsing() {
        let listing = "Samantha             en_US    # Hello\n\
                       Daniel               en_GB    # Hi\n\
                       Amélie               fr_CA    # Salut\n\
                       Thomas               fr_FR    # Salut\n";
        let voices = parse_say_voices(listing);
        assert_eq!(voices["en"], "Samantha");
        // Thomas outranks Amélie for fr.
        assert_eq!(voices["fr"], "Thomas");
    }

    #[test]
    fn unsafe_member_names_rejected() {
        assert!(unsafe_member_name("/etc/passwd"));
        assert!(unsafe_member_name("../escape"));
        assert!(unsafe_member_name("a/../../escape"));
        assert!(unsafe_member_name("."));
        assert!(!unsafe_member_name("piper/piper"));
        assert!(!unsafe_member_name("piper/espeak-ng-data/phonemes"));
    }

    #[test]
    fn no_engine_system_catalog_is_empty() {
        // A host without piper/espeak/flite binaries reports the oracle's
        // empty-languages catalog (drives "No speech engine is installed").
        // The cache is pinned so the host's real voice dir can't leak in.
        let dir = tempfile::tempdir().unwrap();
        let engine =
            SystemEngine::with_home_and_cache(dir.path().join("home"), dir.path().join("cache"));
        let catalog = engine.catalog();
        for voice in &catalog.voices {
            assert!(!voice.installed);
            assert_eq!(
                voice.bytes,
                voice_entry(&voice.language).unwrap().total_bytes
            );
            assert!(OFFERED.contains(&voice.language.as_str()));
        }
        assert_eq!(catalog.voices.len(), OFFERED.len());
    }
}
