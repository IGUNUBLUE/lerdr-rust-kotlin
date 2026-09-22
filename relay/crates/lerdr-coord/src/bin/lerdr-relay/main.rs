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

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Args, Parser, Subcommand};
use lerdr_coord::{ClientSinkLookup, HerdRouterFactory, TopologyActor};
use lerdr_herdr::Client;
use lerdr_relay::auth::BootstrapRearm;
use lerdr_relay::session::{SessionConfig, SnapshotFn};
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
            let version = env!("CARGO_PKG_VERSION");
            let revision = option_env!("LERDR_REVISION").unwrap_or("dev");
            if json {
                println!(
                    "{}",
                    serde_json::json!({"version": version, "revision": revision})
                );
            } else {
                println!("lerdr {version} ({revision})");
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
        Commands::Serve(_) => unreachable!("serve handled in main"),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(error),
    }
}

fn fail(error: BoxError) -> ExitCode {
    eprintln!("lerdr-relay: {error:#}");
    ExitCode::FAILURE
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

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
    let factory = HerdRouterFactory::new(
        topology.clone(),
        sink_of,
        shutdown.clone(),
        cfg.runtime_dir.join("uploads"),
    )
    .into_factory();
    let topology_for_snapshot = topology.clone();
    let relay = Relay::with_router_factory(auth, factory).with_session_config(SessionConfig {
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
        ..SessionConfig::default()
    });
    let _ = relay_cell.set(relay.clone());
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
    info!(addr = %listener.local_addr()?, "lerdr-relay listening");
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
                    topology.refresh().await;
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
    let state = serde_json::json!({
        "generated_at": chrono_free_timestamp(),
        "version": env!("CARGO_PKG_VERSION"),
        "revision": option_env!("LERDR_REVISION").unwrap_or("dev"),
        "protocol": 3,
        "readiness": "serving",
        "inventory": {},
        "components": {"relay": "rust"},
        "activity_failures": 0,
        "topology_retries": 0,
        "poll_failures": 0,
        "recent_errors": [],
    });
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
