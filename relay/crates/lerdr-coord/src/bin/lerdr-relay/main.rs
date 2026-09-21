//! `lerdr-relay` binary — `cmd/lerdr` serve path: env/flag config, device
//! auth store, pairing bootstrap + SIGUSR1 re-arm, graceful shutdown on
//! SIGINT/SIGTERM.
//!
//! `lerdr-relay` with no subcommand is `serve`, matching the oracle's
//! `command := "serve"` default.

mod bootstrap;
mod config;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Args, Parser, Subcommand};
use lerdr_coord::{ClientSinkLookup, HerdRouterFactory, TopologyActor};
use lerdr_herdr::Client;
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

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let args = match cli.command {
        Some(Commands::Serve(args)) => args,
        None => ServeArgs::default(),
    };
    match run(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            error!(%error, "lerdr-relay failed");
            eprintln!("lerdr-relay: {error:#}");
            ExitCode::FAILURE
        }
    }
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
    let factory =
        HerdRouterFactory::new(topology.clone(), sink_of, shutdown.clone()).into_factory();
    let topology_for_snapshot = topology.clone();
    let relay = Relay::with_router_factory(auth, factory).with_session_config(SessionConfig {
        snapshot_fn: Some(SnapshotFn(Arc::new(move || {
            lerdr_coord::compose_snapshot(&topology_for_snapshot.topology.borrow())
        }))),
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

    let listener = TcpListener::bind((cfg.host.as_str(), cfg.port)).await?;
    info!(addr = %listener.local_addr()?, "lerdr-relay listening");
    relay.serve(listener).await?;
    info!("lerdr-relay stopped");
    Ok(())
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
