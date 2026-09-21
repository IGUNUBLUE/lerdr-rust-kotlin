//! `lerdr-relay` binary — minimal wiring: bind address + device-store dir on
//! argv, tracing to stderr, graceful shutdown on SIGINT/SIGTERM.
//!
//! CLI parity with `cmd/lerdr` (clap, full flag surface) lands with the
//! real subsystems; this slice needs only `--bind` and `--data-dir`.

use std::net::SocketAddr;
use std::sync::Arc;

use lerdr_relay::store::FileAuthStore;
use lerdr_relay::Relay;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "lerdr_relay=info,tower_http=info".into()),
        )
        .init();

    let mut bind: SocketAddr = "127.0.0.1:8443".parse().expect("default bind");
    let mut data_dir = std::path::PathBuf::from("./lerdr-data");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--bind" => {
                bind = args
                    .next()
                    .ok_or("--bind requires an address")?
                    .parse()
                    .map_err(|_| "--bind must be host:port")?;
            }
            "--data-dir" => {
                data_dir = args
                    .next()
                    .map(std::path::PathBuf::from)
                    .ok_or("--data-dir requires a path")?;
            }
            other => return Err(format!("unknown flag {other}").into()),
        }
    }

    let store = FileAuthStore::open(&data_dir)?;
    let relay = Relay::new(Arc::new(store));
    let shutdown = relay.shutdown();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        shutdown.cancel();
    });
    #[cfg(unix)]
    {
        let shutdown = relay.shutdown();
        tokio::spawn(async move {
            if let Ok(mut term) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            {
                term.recv().await;
                shutdown.cancel();
            }
        });
    }

    let listener = TcpListener::bind(bind).await?;
    tracing::info!(%bind, "lerdr-relay listening");
    relay.serve(listener).await?;
    Ok(())
}
