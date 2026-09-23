//! `lerdr-relay` binary — `cmd/lerdr` serve path: env/flag config, device
//! auth store, pairing bootstrap + SIGUSR1 re-arm, graceful shutdown on
//! SIGINT/SIGTERM.
//!
//! `lerdr-relay` with no subcommand is `serve`, matching the oracle's
//! `command := "serve"` default. The helper subcommands (`event-hook`,
//! `startup-hook`, `setup-fragment`, `normalize-origin`, `qr`, `support`,
//! `version`) are the surface `plugin/scripts/*` invokes.

mod bootstrap;
mod config;
mod hooks;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Args, Parser, Subcommand};
use lerdr_coord::{release, ClientSinkLookup, HerdRouterFactory, TopologyActor};
use lerdr_core::audit;
use lerdr_herdr::Client;
use lerdr_relay::auth::BootstrapRearm;
use lerdr_relay::server::{HealthProbe, InventoryProbeFn};
use lerdr_relay::session::{
    AttributionFn, AuditHook, ClientsChangedHook, SessionConfig, SnapshotFn,
};
use lerdr_relay::store::FileAuthStore;
use lerdr_relay::Relay;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

#[derive(Parser)]
#[command(
    name = "lerdr-relay",
    about = "Lerdr relay — the computer-side companion Herdr talks to",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the relay: /ws + /healthz on the resolved address.
    ///
    /// Flags override the environment; LERDR_* variables override the
    /// legacy HERDR_* spellings (see config.rs for the full list).
    Serve(ServeArgs),
    /// Print version (optionally as JSON).
    Version {
        /// Emit `{"version":…,"revision":…}` JSON.
        #[arg(long)]
        json: bool,
    },
    /// Herdr [[events]] hook: forward HERDR_PLUGIN_EVENT_JSON to the running
    /// relay over UDP (fire-and-forget; invoked by herdr, never by hand).
    EventHook,
    /// Herdr [[startup]] hook: poke the running relay to re-assert its
    /// socket/subscription/view state after session restore or
    /// server.live_handoff.
    StartupHook,
    /// Build a setup-link fragment: setup-fragment TOKEN LABEL [RELAY].
    SetupFragment {
        token: String,
        label: String,
        relay: Option<String>,
    },
    /// Normalize a relay origin URL (https by default; http only for
    /// loopback with --allow-loopback-http).
    NormalizeOrigin {
        /// Allow http:// for localhost/loopback origins.
        #[arg(long)]
        allow_loopback_http: bool,
        origin: String,
    },
    /// Render a value as a terminal QR (half-block rows, EC level M).
    Qr {
        /// Maximum terminal columns; refuses when the code won't fit.
        #[arg(long, default_value_t = 80)]
        columns: usize,
        value: String,
    },
    /// Print the running relay's support-state.json diagnostics.
    Support {
        /// Runtime directory override [default: resolved like serve].
        #[arg(long)]
        runtime_dir: Option<PathBuf>,
    },
    /// Verify a release directory against its release-manifest.json and
    /// print the manifest. `internal/release.Verify` + the identity checks
    /// from cmd/lerdr.
    VerifyRelease {
        /// Expected os/architecture [default: this binary's target].
        #[arg(long)]
        target: Option<String>,
        /// Expected release version.
        #[arg(long)]
        version: Option<String>,
        /// Expected release revision.
        #[arg(long)]
        revision: Option<String>,
        /// Allow a build-host tool to verify another target's bundle.
        #[arg(long)]
        allow_cross_target: bool,
        /// Release directory [default: the running binary's directory].
        #[arg(value_name = "DIRECTORY")]
        directory: Option<PathBuf>,
    },
    /// Write release-manifest.json for a staged release tree and print it.
    /// `internal/release.Build` — `release-manifest DIRECTORY VERSION
    /// REVISION os/arch`.
    ReleaseManifest {
        /// Staged release tree to manifest.
        #[arg(value_name = "DIRECTORY")]
        directory: PathBuf,
        /// Release version stamp.
        #[arg(value_name = "VERSION")]
        version: String,
        /// Release revision stamp.
        #[arg(value_name = "REVISION")]
        revision: String,
        /// Target os/arch (e.g. linux/amd64).
        #[arg(value_name = "OS/ARCH")]
        target: String,
    },
    /// Point RELEASE_ROOT/current at a verified RELEASE_DIRECTORY
    /// (temp symlink + rename — `internal/update.Activate`).
    ActivateRelease {
        /// Install root holding `releases/` and the `current` link.
        #[arg(value_name = "RELEASE_ROOT")]
        release_root: PathBuf,
        /// The release directory to activate (must verify first).
        #[arg(value_name = "RELEASE_DIRECTORY")]
        release_directory: PathBuf,
    },
    /// Verify a release tree, then strip write permission across it so the
    /// installed bundle stays immutable (`internal/release.Seal`).
    SealRelease {
        /// Release directory to seal.
        #[arg(value_name = "RELEASE_DIRECTORY")]
        release_directory: PathBuf,
    },
    /// Remove stale verified releases under RELEASE_ROOT/releases, keeping
    /// CURRENT_RELEASE (and PREVIOUS_RELEASE for rollback) —
    /// `internal/update.PruneOldReleases`.
    PruneReleases {
        /// Install root holding `releases/`.
        #[arg(value_name = "RELEASE_ROOT")]
        release_root: PathBuf,
        /// The active release directory to keep.
        #[arg(value_name = "CURRENT_RELEASE")]
        current_release: PathBuf,
        /// The rollback release directory to keep.
        #[arg(value_name = "PREVIOUS_RELEASE")]
        previous_release: Option<PathBuf>,
    },
    /// Run one staged update job to completion — `internal/update`'s
    /// `Worker.Run`. Invoked by `install_update` via systemd-run/launchctl
    /// as a detached transient unit; never run by hand.
    UpdateWorker {
        /// The `update-job-*.json` payload path.
        #[arg(value_name = "JOB.json")]
        job_path: PathBuf,
    },
    /// Manage the cached speech voices — `internal/speech`'s `Run`:
    /// `list|missing|install|reinstall-runtime|remove` plus a repeated or
    /// comma-separated `--languages`. Flags are parsed inside the command
    /// so the Go `flag` usage contract (exit 2) survives.
    SpeechVoices {
        /// Raw argv passed through to the speech command.
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            value_name = "ARGS"
        )]
        args: Vec<String>,
    },
    /// Herdr `plugin.pane.*` passthrough — open/focus/close plugin panes.
    /// Flags mirror `herdr plugin pane …` (plugin/scripts/* invokes that
    /// CLI; this is the same surface through our typed socket client).
    PluginPane {
        /// Herdr API socket [env: HERDR_SOCKET_PATH, default
        /// ~/.config/herdr/herdr.sock].
        #[arg(long, global = true)]
        socket_path: Option<PathBuf>,
        #[command(subcommand)]
        command: PluginPaneCommand,
    },
    /// Herdr `server.*` passthrough — config reload + agent-manifest
    /// reload/status (`herdr server reload-config` & friends).
    HerdrReload {
        /// Herdr API socket [env: HERDR_SOCKET_PATH, default
        /// ~/.config/herdr/herdr.sock].
        #[arg(long, global = true)]
        socket_path: Option<PathBuf>,
        #[command(subcommand)]
        command: HerdrReloadCommand,
    },
    /// Herdr `integration.{install,uninstall}` passthrough.
    Integration {
        /// Herdr API socket [env: HERDR_SOCKET_PATH, default
        /// ~/.config/herdr/herdr.sock].
        #[arg(long, global = true)]
        socket_path: Option<PathBuf>,
        #[command(subcommand)]
        command: IntegrationCommand,
    },
}

#[derive(Subcommand)]
enum PluginPaneCommand {
    /// `plugin.pane.open` — flags mirror `herdr plugin pane open`.
    Open(Box<PluginPaneOpenArgs>),
    /// `plugin.pane.focus <PANE_ID>`.
    Focus {
        /// The plugin pane to focus.
        pane_id: String,
    },
    /// `plugin.pane.close <PANE_ID>`.
    Close {
        /// The plugin pane to close.
        pane_id: String,
    },
}

#[derive(Args)]
struct PluginPaneOpenArgs {
    /// Plugin id (`--plugin`).
    #[arg(long = "plugin", value_name = "ID")]
    plugin_id: String,
    /// Manifest entrypoint id (`--entrypoint`).
    #[arg(long, value_name = "ID")]
    entrypoint: String,
    /// `overlay|popup|split|tab|zoomed`.
    #[arg(long, value_name = "PLACEMENT")]
    placement: Option<String>,
    /// Workspace to open into (`--workspace`).
    #[arg(long, value_name = "ID")]
    workspace: Option<String>,
    /// Pane to split/replace (`--target-pane`).
    #[arg(long = "target-pane", value_name = "PANE")]
    target_pane: Option<String>,
    /// `right|down` — split direction.
    #[arg(long, value_name = "DIRECTION")]
    direction: Option<String>,
    /// Working directory for the pane process.
    #[arg(long, value_name = "PATH")]
    cwd: Option<String>,
    /// `KEY=VALUE` — repeatable env for the launched process.
    #[arg(long = "env", value_name = "KEY=VALUE")]
    env: Vec<String>,
    /// Popup width — cells or `N%`.
    #[arg(long, value_name = "SIZE")]
    width: Option<String>,
    /// Popup height — cells or `N%`.
    #[arg(long, value_name = "SIZE")]
    height: Option<String>,
    /// Focus the new pane.
    #[arg(long, overrides_with = "no_focus")]
    focus: bool,
    /// Do not focus the new pane.
    #[arg(long)]
    no_focus: bool,
}

#[derive(Subcommand)]
enum HerdrReloadCommand {
    /// `server.reload_config` (`herdr server reload-config`).
    Config,
    /// `server.agent_manifests` — active manifest status.
    AgentManifests,
    /// `server.reload_agent_manifests`
    /// (`herdr server reload-agent-manifests`).
    ReloadAgentManifests,
}

#[derive(Subcommand)]
enum IntegrationCommand {
    /// `integration.install <TARGET>` (`herdr integration install`).
    Install {
        /// The integration target (claude, codex, devin, …).
        target: String,
    },
    /// `integration.uninstall <TARGET>`.
    Uninstall {
        /// The integration target.
        target: String,
    },
}

#[derive(Args, Debug, Default)]
struct ServeArgs {
    /// Bind host [env: LERDR_RELAY_HOST / HERDR_RELAY_HOST, default 127.0.0.1]
    #[arg(long)]
    host: Option<String>,
    /// Bind port [env: LERDR_RELAY_PORT / HERDR_RELAY_PORT, default 8375]
    #[arg(long)]
    port: Option<u16>,
    /// 32-byte relay key — enables pairing and non-loopback binds
    /// [env: LERDR_RELAY_TOKEN / HERDR_RELAY_TOKEN]
    #[arg(long)]
    token: Option<String>,
    /// Herdr API socket [env: HERDR_SOCKET_PATH]
    #[arg(long)]
    socket_path: Option<PathBuf>,
    /// Runtime directory (pid file, relay.env) [env: LERDR_RELAY_ENV dir,
    /// HERDR_PLUGIN_CONFIG_DIR, or ~/.config/lerdr with legacy adoption]
    #[arg(long)]
    runtime_dir: Option<PathBuf>,
    /// Device-auth store directory [default: <runtime-dir>/device-auth]
    #[arg(long)]
    device_auth_dir: Option<PathBuf>,
    /// ws(s):// origin printed inside pairing links when it differs from the
    /// bind address (Tailscale serve, port forwards) [env: LERDR_RELAY_URL]
    #[arg(long)]
    advertised_url: Option<String>,
    /// Wipe enrolled devices and mint a fresh bootstrap invitation at startup
    /// [env: LERDR_RELAY_REARM_BOOTSTRAP]
    #[arg(long)]
    rearm_bootstrap: bool,
}

impl From<&ServeArgs> for config::Overrides {
    fn from(args: &ServeArgs) -> Self {
        config::Overrides {
            host: args.host.clone(),
            port: args.port,
            token: args.token.clone(),
            socket_path: args.socket_path.clone(),
            runtime_dir: args.runtime_dir.clone(),
            device_auth_dir: args.device_auth_dir.clone(),
            advertised_url: args.advertised_url.clone(),
            rearm_bootstrap: args.rearm_bootstrap,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Commands::Serve(ServeArgs::default())) {
        Commands::Serve(args) => {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(error) => {
                    eprintln!("lerdr-relay: {error:#}");
                    return ExitCode::FAILURE;
                }
            };
            match runtime.block_on(run(args)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    error!(%error, "lerdr-relay failed");
                    eprintln!("lerdr-relay: {error:#}");
                    ExitCode::FAILURE
                }
            }
        }
        command => run_hook(command),
    }
}

/// Sync subcommands — no tokio runtime, matching the oracle's dispatch.
fn run_hook(command: Commands) -> ExitCode {
    let result: Result<(), BoxError> = match command {
        Commands::Version { json } => {
            let stamp = lerdr_coord::release::binary_stamp();
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "version": stamp.version,
                        "revision": stamp.revision,
                        "target": stamp.target,
                    })
                );
            } else {
                println!("lerdr {} ({})", stamp.version, stamp.revision);
            }
            Ok(())
        }
        Commands::EventHook => hooks::event_hook().map_err(Into::into),
        Commands::StartupHook => hooks::startup_hook().map_err(Into::into),
        Commands::SetupFragment {
            token,
            label,
            relay,
        } => {
            println!(
                "{}",
                hooks::setup_fragment(&token, &label, relay.as_deref())
            );
            Ok(())
        }
        Commands::NormalizeOrigin {
            allow_loopback_http,
            origin,
        } => hooks::normalize_origin(&origin, allow_loopback_http)
            .map(|o| println!("{o}"))
            .map_err(Into::into),
        Commands::Qr { columns, value } => hooks::terminal_qr(&value, columns)
            .map(|r| println!("{r}"))
            .map_err(Into::into),
        Commands::Support { runtime_dir } => {
            let dir = match runtime_dir {
                Some(dir) => dir,
                None => match config::resolve(
                    &|key| std::env::var(key).ok(),
                    &|path| path.is_dir(),
                    &config::Overrides::default(),
                ) {
                    Ok(cfg) => cfg.runtime_dir,
                    Err(error) => return fail(Box::new(error)),
                },
            };
            hooks::support(&dir).map_err(Into::into)
        }
        Commands::VerifyRelease {
            target,
            version,
            revision,
            allow_cross_target,
            directory,
        } => verify_release(directory, target, version, revision, allow_cross_target),
        Commands::ReleaseManifest {
            directory,
            version,
            revision,
            target,
        } => {
            let manifest = match release::build(&directory, &version, &revision, &target) {
                Ok(manifest) => manifest,
                Err(error) => return fail(Box::new(error)),
            };
            match serde_json::to_string(&manifest) {
                Ok(encoded) => println!("{encoded}"),
                Err(error) => return fail(Box::new(error)),
            }
            Ok(())
        }
        Commands::ActivateRelease {
            release_root,
            release_directory,
        } => {
            if let Err(error) = release::verify(&release_directory, &release::current_target()) {
                return fail(format!("refusing to activate invalid release: {error}").into());
            }
            release::activate(&release_root, &release_directory).map_err(Into::into)
        }
        Commands::SealRelease { release_directory } => {
            release::seal(&release_directory).map_err(Into::into)
        }
        Commands::PruneReleases {
            release_root,
            current_release,
            previous_release,
        } => {
            let mut keep = vec![current_release];
            keep.extend(previous_release);
            release::prune_old_releases(&release_root, &keep).map_err(Into::into)
        }
        Commands::UpdateWorker { job_path } => {
            match lerdr_coord::update_worker::run(&job_path) {
                // `errors.Is(err, update.ErrConcurrent)` → exit 3.
                Err(error) if error.is_concurrent() => {
                    eprintln!("lerdr-relay: {error}");
                    return ExitCode::from(3);
                }
                other => other.map_err(|e| -> BoxError { e.into() }),
            }
        }
        Commands::SpeechVoices { args } => {
            let mut stdout = std::io::stdout().lock();
            let mut stderr = std::io::stderr().lock();
            lerdr_coord::speech_voices_cli(&args, &mut stdout, &mut stderr).map_err(|error| {
                // `speech.ErrUsage` → exit 2 like the oracle's run().
                if error.is_usage() {
                    UsageError(error.to_string()).into()
                } else {
                    error.into()
                }
            })
        }
        Commands::PluginPane {
            socket_path,
            command,
        } => {
            let socket_path = herdr_socket_path(socket_path);
            block_on_herdr(async move { plugin_pane(socket_path, command).await })
        }
        Commands::HerdrReload {
            socket_path,
            command,
        } => {
            let socket_path = herdr_socket_path(socket_path);
            block_on_herdr(async move { herdr_reload(socket_path, command).await })
        }
        Commands::Integration {
            socket_path,
            command,
        } => {
            let socket_path = herdr_socket_path(socket_path);
            block_on_herdr(async move { integration(socket_path, command).await })
        }
        Commands::Serve(_) => unreachable!("serve handled in main"),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(error),
    }
}

/// `verify-release` — `release.Verify` then the oracle's
/// `verifyReleaseIdentity`: the expected-* flags are candidate checks on
/// top of the binary's own stamp (which is always authoritative).
fn verify_release(
    directory: Option<PathBuf>,
    target: Option<String>,
    version: Option<String>,
    revision: Option<String>,
    allow_cross_target: bool,
) -> Result<(), BoxError> {
    if allow_cross_target && (version.is_some() || revision.is_some()) {
        return Err(Box::new(UsageError(
            "--allow-cross-target cannot be combined with --version or --revision candidate checks"
                .to_string(),
        )));
    }
    let root = match directory {
        Some(directory) => directory,
        None => std::env::current_exe()?
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default(),
    };
    let expected_target = target.unwrap_or_else(release::current_target);
    let manifest = release::verify(&root, &expected_target)?;
    release::verify_identity(
        &manifest,
        version.as_deref().unwrap_or_default(),
        revision.as_deref().unwrap_or_default(),
        &expected_target,
        allow_cross_target,
        &release::binary_stamp(),
    )?;
    println!("{}", serde_json::to_string(&manifest)?);
    Ok(())
}

/// Usage-class failure — exit 2, matching the oracle's `flag.ContinueOnError`
/// contract for bad flag combinations (clap already exits 2 on shape errors).
#[derive(Debug)]
struct UsageError(String);

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UsageError {}

fn fail(error: BoxError) -> ExitCode {
    eprintln!("lerdr-relay: {error:#}");
    if error.is::<UsageError>() {
        return ExitCode::from(2);
    }
    ExitCode::FAILURE
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The Herdr socket a passthrough command talks to — `--socket-path`,
/// then `HERDR_SOCKET_PATH`, then the default
/// `$XDG_CONFIG_HOME/herdr/herdr.sock` (config.rs's resolution minus the
/// relay-only fields).
fn herdr_socket_path(override_path: Option<PathBuf>) -> PathBuf {
    override_path
        .or_else(|| std::env::var("HERDR_SOCKET_PATH").ok().map(PathBuf::from))
        .unwrap_or_else(|| {
            let config_home = std::env::var("XDG_CONFIG_HOME")
                .ok()
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    std::env::var("HOME")
                        .ok()
                        .map(PathBuf::from)
                        .unwrap_or_default()
                        .join(".config")
                });
            config_home.join("herdr").join("herdr.sock")
        })
}

/// The passthrough commands are one-shot socket calls — a current-thread
/// runtime is plenty (no serve loop).
fn block_on_herdr(
    f: impl std::future::Future<Output = Result<(), BoxError>>,
) -> Result<(), BoxError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(f)
}

/// `herdr plugin pane …` — one socket call per subcommand; the response
/// body prints as JSON for scripts.
async fn plugin_pane(socket_path: PathBuf, command: PluginPaneCommand) -> Result<(), BoxError> {
    use lerdr_herdr::{PluginPanePlacement, PopupSize, SplitDirection};
    let client = Client::unix(socket_path);
    match command {
        PluginPaneCommand::Open(args) => {
            let PluginPaneOpenArgs {
                plugin_id,
                entrypoint,
                placement,
                workspace,
                target_pane,
                direction,
                cwd,
                env,
                width,
                height,
                focus,
                no_focus,
            } = *args;
            let placement = placement
                .map(|raw| {
                    PluginPanePlacement::parse(&raw).ok_or_else(|| {
                        usage(format!(
                            "invalid --placement {raw:?} (overlay|popup|split|tab|zoomed)"
                        ))
                    })
                })
                .transpose()?;
            let direction = direction
                .map(|raw| {
                    Ok::<SplitDirection, BoxError>(match raw.as_str() {
                        "right" => SplitDirection::Right,
                        "down" => SplitDirection::Down,
                        _ => {
                            return Err(usage(format!("invalid --direction {raw:?} (right|down)")))
                        }
                    })
                })
                .transpose()?;
            let parse_size = |flag: &str, raw: String| -> Result<PopupSize, BoxError> {
                PopupSize::parse(&raw)
                    .ok_or_else(|| usage(format!("invalid {flag} {raw:?} (cells or N%)")))
            };
            let width = width.map(|raw| parse_size("--width", raw)).transpose()?;
            let height = height.map(|raw| parse_size("--height", raw)).transpose()?;
            let mut env_map = std::collections::BTreeMap::new();
            for kv in env {
                let Some((key, value)) = kv.split_once('=') else {
                    return Err(usage(format!("invalid --env {kv:?} (KEY=VALUE)")));
                };
                env_map.insert(key.to_owned(), value.to_owned());
            }
            let focus = if focus {
                Some(true)
            } else if no_focus {
                Some(false)
            } else {
                None
            };
            let pane = client
                .plugin_pane_open(&lerdr_herdr::PluginPaneOpenParams {
                    plugin_id,
                    entrypoint,
                    workspace_id: workspace,
                    target_pane_id: target_pane,
                    cwd,
                    env: env_map,
                    direction,
                    placement,
                    width,
                    height,
                    focus,
                })
                .await?;
            println!("{}", serde_json::to_string(&pane)?);
        }
        PluginPaneCommand::Focus { pane_id } => {
            let pane = client.plugin_pane_focus(&pane_id).await?;
            println!("{}", serde_json::to_string(&pane)?);
        }
        PluginPaneCommand::Close { pane_id } => {
            let closed = client.plugin_pane_close(&pane_id).await?;
            println!("{}", serde_json::json!({"pane_id": closed}));
        }
    }
    Ok(())
}

/// `herdr server …` — config reload, manifest status, manifest reload;
/// each response prints as JSON.
async fn herdr_reload(socket_path: PathBuf, command: HerdrReloadCommand) -> Result<(), BoxError> {
    let client = Client::unix(socket_path);
    match command {
        HerdrReloadCommand::Config => {
            let outcome = client.server_reload_config().await?;
            println!("{}", serde_json::to_string(&outcome)?);
        }
        HerdrReloadCommand::AgentManifests => {
            let status = client.server_agent_manifests().await?;
            println!("{}", serde_json::to_string(&status)?);
        }
        HerdrReloadCommand::ReloadAgentManifests => {
            let manifests = client.server_reload_agent_manifests().await?;
            println!("{}", serde_json::to_string(&manifests)?);
        }
    }
    Ok(())
}

/// `herdr integration {install,uninstall} <TARGET>` — the typed outcome
/// prints as JSON.
async fn integration(socket_path: PathBuf, command: IntegrationCommand) -> Result<(), BoxError> {
    use lerdr_herdr::IntegrationTarget;
    let client = Client::unix(socket_path);
    let (target_name, install) = match command {
        IntegrationCommand::Install { target } => (target, true),
        IntegrationCommand::Uninstall { target } => (target, false),
    };
    let target = IntegrationTarget::parse(&target_name)
        .ok_or_else(|| usage(format!("unknown integration target {target_name:?}")))?;
    if install {
        let outcome = client.integration_install(target).await?;
        println!("{}", serde_json::to_string(&outcome)?);
    } else {
        let outcome = client.integration_uninstall(target).await?;
        println!("{}", serde_json::to_string(&outcome)?);
    }
    Ok(())
}

fn usage(message: String) -> BoxError {
    Box::new(UsageError(message))
}

async fn run(args: ServeArgs) -> Result<(), BoxError> {
    let cfg = config::resolve(
        &|key| std::env::var(key).ok(),
        &|path| path.is_dir(),
        &(&args).into(),
    )?;
    init_tracing(cfg.log_level.as_deref());

    if cfg.rearm_bootstrap {
        bootstrap::reset_device_store(&cfg.device_auth_dir)?;
    }
    let store = Arc::new(FileAuthStore::open(&cfg.device_auth_dir)?);

    let label = bootstrap::host_label();
    let socket_url = cfg.socket_url();
    if let Some(offer) = bootstrap::ensure_pairing(
        &store,
        &label,
        &socket_url,
        cfg.token.as_deref(),
        &mut bootstrap::os_fill,
    )? {
        info!(
            invitation_id = %offer.invitation.invitation_id,
            expires_at_ms = offer.invitation.expires_at_ms,
            "pairing invitation armed"
        );
        bootstrap::print_setup_link(&offer.deep_link());
    }

    if let Err(error) = bootstrap::write_pid_file(&cfg.runtime_dir) {
        warn!(%error, dir = %cfg.runtime_dir.display(), "pid file not written");
    }

    let auth = Arc::new(bootstrap::EventedAuthStore::new(Arc::clone(&store)));

    // lerdr-coord: the topology actor drives Herdr's event stream; each
    // session's HerdRouter dispatches actions to the socket and owns the
    // pane watches. When Herdr is unreachable the supervisor keeps
    // retrying and actions answer failed_before_dispatch — the honest
    // dispatch-boundary state, so wiring is unconditional.
    let herdr = Client::unix(cfg.socket_path.clone());
    info!(socket = %cfg.socket_path.display(), herdr = %herdr.describe(), "herdr client configured");

    // The sink lookup is two-phase: routers resolve their ClientSink lazily
    // (sinks register after the handshake), so a OnceCell indirection lets
    // the factory capture the lookup before the Relay exists.
    let relay_cell = Arc::new(std::sync::OnceLock::new());
    let sink_of: ClientSinkLookup = {
        let cell = Arc::clone(&relay_cell);
        Arc::new(move |id: &str| cell.get().and_then(|r: &Relay| r.client_sink(id)))
    };
    let shutdown = CancellationToken::new();
    let topology = TopologyActor::spawn(herdr, shutdown.clone());
    // `audit.Open(cfg.CacheDir)` — one process-wide append-only log shared
    // by the session layer (attempt + admin rows) and the router's spawned
    // handlers (result rows). A failed open degrades to the no-op logger
    // like the oracle's `s.auditLog == nil`.
    let audit = Arc::new(
        audit::AuditLog::open(&cfg.runtime_dir).unwrap_or_else(|error| {
            warn!(%error, "remote write audit unavailable");
            audit::AuditLog::noop()
        }),
    );
    // `d.state.Agent(paneID)` — audit attribution off the live topology.
    let attribution: Arc<AttributionFn> = {
        let topology = topology.clone();
        Arc::new(move |pane_id: &str| {
            let snapshot = topology.topology.borrow();
            let Some(agent) = snapshot.pane_of(pane_id) else {
                return audit::Attribution::default();
            };
            audit::Attribution {
                agent: agent
                    .agent
                    .clone()
                    .or_else(|| agent.agent_session.as_ref().map(|s| s.agent.clone()))
                    .unwrap_or_default(),
                project: String::new(),
                session: agent
                    .agent_session
                    .as_ref()
                    .map(|s| s.value.clone())
                    .unwrap_or_default(),
                host: String::new(),
            }
        })
    };
    // `lerdr.devices` workspace annotation — the live controller count
    // resolves through the same OnceCell (the Relay is built below).
    let devices_of: lerdr_coord::ClientCountLookup = {
        let cell = Arc::clone(&relay_cell);
        Arc::new(move || {
            cell.get()
                .map(|r: &Relay| r.connected_clients())
                .unwrap_or(0)
        })
    };
    let router_factory = HerdRouterFactory::new(
        topology.clone(),
        sink_of,
        shutdown.clone(),
        cfg.runtime_dir.clone(),
        Some(audit.clone()),
        devices_of,
    );
    let factory = router_factory.clone().into_factory();
    let topology_for_snapshot = topology.clone();
    let topology_for_health = topology.clone();
    // `s.cfg.InstanceID` + `s.state.InventoryStatus` — the probe reads the
    // topology watch cell directly, so /healthz and /readyz reflect live
    // committed inventory state without the relay calling back into the
    // coordinator.
    let health = HealthProbe {
        instance_id: cfg.instance_id.clone(),
        inventory: Some(InventoryProbeFn(Arc::new(move || {
            lerdr_coord::inventory_status(&topology_for_health.topology.borrow())
        }))),
        ..HealthProbe::default()
    };
    let relay = Relay::with_router_factory(auth, factory)
        .with_health(health)
        .with_session_config(SessionConfig {
            snapshot_fn: Some(SnapshotFn(Arc::new(move || {
                lerdr_coord::compose_snapshot(&topology_for_snapshot.topology.borrow())
            }))),
            // `ResetWithBootstrap` — a configured relay key re-arms the
            // bootstrap invitation after `reset_devices` wipes the store, so
            // the printed setup link keeps pairing (the oracle feeds
            // `[]byte(cfg.Token)`; the type requires exactly 32 bytes).
            reset_bootstrap: cfg.token.as_deref().and_then(|token| {
                token
                    .as_bytes()
                    .try_into()
                    .ok()
                    .map(|secret| BootstrapRearm {
                        secret,
                        name: label.clone(),
                    })
            }),
            audit: Some(AuditHook {
                log: audit.clone(),
                attribution: Some(attribution),
            }),
            // `client.window_title` — "lerdr: N device(s)" while
            // controllers are connected; cleared when the last leaves.
            // The async call hops onto the runtime from the registry's
            // sync hook.
            clients_changed: Some(ClientsChangedHook(Arc::new({
                let client = topology.client.clone();
                move |count| {
                    let client = client.clone();
                    tokio::spawn(async move {
                        lerdr_coord::update_window_title(&client, count).await;
                    });
                }
            }))),
            ..SessionConfig::default()
        });
    let _ = relay_cell.set(relay.clone());
    // `d.broadcast` — journal events (`activity` rows, `activity_history`
    // clears) fan out to every connected client, matching the oracle's
    // live activity pushes.
    router_factory.spawn_activity_broadcast(
        {
            let relay = relay.clone();
            move |message| relay.broadcast(message)
        },
        relay.shutdown(),
    );
    // `broadcastToAll`/`hub.Broadcast` — voice-catalog changes,
    // `update_status`, and any other relay-wide notice fan out to every
    // session except the requesting one (it already has the frame).
    router_factory.spawn_notice_broadcast(
        {
            let relay = relay.clone();
            move |message, exclude| relay.broadcast_except(message, exclude)
        },
        relay.shutdown(),
    );
    // Web Push delivery — VAPID key load-or-generate failures are fatal
    // at startup (corrupt/mismatched key files), matching the oracle's
    // `push.NewManager` error path. Dormant when no subscriptions exist.
    router_factory
        .spawn_push_worker(relay.shutdown())
        .map_err(|e| -> BoxError { format!("initialize push manager: {e}").into() })?;
    // Tie the topology actor's token to the relay's real shutdown token.
    {
        let relay_shutdown = relay.shutdown();
        tokio::spawn(async move {
            relay_shutdown.cancelled().await;
            shutdown.cancel();
        });
    }

    spawn_signal_handlers(&relay, store, label, socket_url, cfg.token.clone());
    spawn_udp_ingress(&topology, &cfg, relay.shutdown());
    spawn_support_writer(&cfg, relay.shutdown());

    let listener = TcpListener::bind((cfg.host.as_str(), cfg.port)).await?;
    // The oracle logs `instance` with the listen line.
    info!(addr = %listener.local_addr()?, instance = %cfg.instance_id, "lerdr-relay listening");
    relay.serve(listener).await?;
    info!("lerdr-relay stopped");
    Ok(())
}

/// UDP event ingress — `internal/coordinator/udp.go`. `event-hook` and
/// `startup-hook` deliver datagrams here; a valid one pokes the topology
/// actor into a fresh `session.snapshot` read (invalidation semantics —
/// payloads are never applied directly).
fn spawn_udp_ingress(
    topology: &lerdr_coord::TopologyHandle,
    cfg: &config::Config,
    shutdown: CancellationToken,
) {
    let port = std::env::var("LERDR_RELAY_PLUGIN_PORT")
        .ok()
        .or_else(|| std::env::var("HERDR_RELAY_PLUGIN_PORT").ok())
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(8376);
    let expected_socket = cfg
        .socket_path
        .canonicalize()
        .unwrap_or_else(|_| cfg.socket_path.clone());
    let topology = topology.clone();

    tokio::spawn(async move {
        let Ok(socket) = tokio::net::UdpSocket::bind(("127.0.0.1", port)).await else {
            warn!(port, "udp plugin port unavailable — event hooks disabled");
            return;
        };
        info!(port, "udp event ingress listening");
        let mut buf = vec![0u8; 65536];
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                received = socket.recv_from(&mut buf) => {
                    let Ok((n, _from)) = received else { continue };
                    let Ok(event) = serde_json::from_slice::<serde_json::Value>(&buf[..n])
                        else { continue };
                    let kind = event.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    if kind != "agent_event" && kind != "startup" {
                        continue;
                    }
                    if let Some(socket_path) =
                        event.get("socket_path").and_then(|p| p.as_str())
                    {
                        let offered = std::path::Path::new(socket_path)
                            .canonicalize()
                            .unwrap_or_else(|_| PathBuf::from(socket_path));
                        if offered != expected_socket {
                            continue;
                        }
                    }
                    if kind == "startup" {
                        // Session restore / server.live_handoff — re-assert
                        // the transient agent view + capability evidence,
                        // not just the snapshot.
                        topology.startup_hook().await;
                    } else {
                        topology.refresh().await;
                    }
                }
            }
        }
    });
}

/// `support-state.json` writer — the `support` subcommand reads this back.
/// Refreshed on a slow interval; fields follow `internal/support.Snapshot`
/// where the Rust relay has an equivalent value.
fn spawn_support_writer(cfg: &config::Config, shutdown: CancellationToken) {
    let runtime_dir = cfg.runtime_dir.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = interval.tick() => write_support_state(&runtime_dir),
            }
        }
    });
}

fn write_support_state(runtime_dir: &std::path::Path) {
    use std::io::Write;
    let stamp = release::binary_stamp();
    // `filepath.Dir(os.Executable())` — the directory the running binary
    // was executed from (canonical on Linux via /proc/self/exe).
    let release_directory = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut state = serde_json::json!({
        "generated_at": chrono_free_timestamp(),
        "version": stamp.version,
        "revision": stamp.revision,
        "protocol": 3,
        "readiness": "serving",
        "inventory": {},
        "components": {"relay": "rust"},
        "activity_failures": 0,
        "topology_retries": 0,
        "poll_failures": 0,
        "recent_errors": [],
    });
    // `release_directory,omitempty` — omitted when the executable path is
    // unknowable, matching the oracle's support snapshot.
    if !release_directory.is_empty() {
        state["release_directory"] = serde_json::Value::String(release_directory);
    }
    let Ok(data) = serde_json::to_string_pretty(&state) else {
        return;
    };
    if std::fs::create_dir_all(runtime_dir).is_err() {
        return;
    }
    let path = runtime_dir.join("support-state.json");
    let Ok(mut temp) = std::fs::File::create(runtime_dir.join(".support-state.tmp")) else {
        return;
    };
    let _ = temp.write_all(data.as_bytes());
    let _ = temp.sync_all();
    drop(temp);
    let _ = std::fs::rename(runtime_dir.join(".support-state.tmp"), path);
}

/// RFC3339 UTC without pulling in chrono for one timestamp.
fn chrono_free_timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86400;
    let rem = secs % 86400;
    let (h, m, s) = (rem / 3600, rem % 3600 / 60, rem % 60);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// `RUST_LOG` wins; `LERDR_RELAY_LOG_LEVEL` maps onto a global level for
/// oracle parity; the default keeps the relay at info.
fn init_tracing(log_level: Option<&str>) {
    let filter = std::env::var("RUST_LOG")
        .ok()
        .and_then(|v| tracing_subscriber::EnvFilter::try_new(v).ok())
        .or_else(|| log_level.map(tracing_subscriber::EnvFilter::new))
        .unwrap_or_else(|| tracing_subscriber::EnvFilter::new("lerdr_relay=info"));
    tracing_subscriber::fmt()
        .compact()
        .with_env_filter(filter)
        .init();
}

/// SIGINT/SIGTERM → graceful shutdown; SIGUSR1 → re-arm the one-use pairing
/// invitation (`armBootstrapLoop` — the setup scripts signal before they
/// print a link, and standalone use gets the fresh link printed here).
fn spawn_signal_handlers(
    relay: &Relay,
    store: Arc<FileAuthStore>,
    label: String,
    socket_url: String,
    token: Option<String>,
) {
    let shutdown = relay.shutdown();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        shutdown.cancel();
    });

    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let shutdown = relay.shutdown();
        tokio::spawn(async move {
            if let Ok(mut term) = signal(SignalKind::terminate()) {
                term.recv().await;
                shutdown.cancel();
            }
        });

        tokio::spawn(async move {
            let Ok(mut usr1) = signal(SignalKind::user_defined1()) else {
                return;
            };
            while usr1.recv().await.is_some() {
                match bootstrap::arm_invitation(
                    &store,
                    &label,
                    token.as_deref(),
                    &mut bootstrap::os_fill,
                ) {
                    Ok(invitation) => {
                        info!(
                            outcome = "armed for one more device",
                            "bootstrap invitation re-arm requested"
                        );
                        if let Some(offer) =
                            bootstrap::offer_for_invitation(&invitation, &label, &socket_url)
                        {
                            bootstrap::print_setup_link(&offer.deep_link());
                        }
                    }
                    Err(error) => {
                        warn!(%error, "bootstrap invitation re-arm failed");
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `speech-voices` collects its whole argv — flags included — for the
    /// Go `flag`-compatible parser inside the command (exit-2 contract).
    #[test]
    fn speech_voices_collects_trailing_args() {
        let cli = Cli::try_parse_from([
            "lerdr-relay",
            "speech-voices",
            "list",
            "--languages",
            "en,fr",
        ])
        .unwrap();
        let Some(Commands::SpeechVoices { args }) = cli.command else {
            panic!("expected speech-voices");
        };
        assert_eq!(args, vec!["list", "--languages", "en,fr"]);
    }

    /// `update-worker` binds exactly one JOB.json path.
    #[test]
    fn update_worker_takes_one_job_path() {
        let cli = Cli::try_parse_from(["lerdr-relay", "update-worker", "/tmp/update-job-7.json"])
            .unwrap();
        let Some(Commands::UpdateWorker { job_path }) = cli.command else {
            panic!("expected update-worker");
        };
        assert_eq!(job_path, Path::new("/tmp/update-job-7.json"));
        assert!(Cli::try_parse_from(["lerdr-relay", "update-worker"]).is_err());
        assert!(Cli::try_parse_from(["lerdr-relay", "update-worker", "a", "b"]).is_err());
    }
}
