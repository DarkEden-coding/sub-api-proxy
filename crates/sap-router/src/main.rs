use clap::Parser;
use sap_router::config::{Cli, Config};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // launchd and systemd send stderr to the router log; RUST_LOG overrides the level.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .init();
    let config = Config::load(Cli::parse().config)?;
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    tracing::info!(
        listen = %config.listen,
        servers = ?config.servers.iter().map(|s| &s.name).collect::<Vec<_>>(),
        "sap-router {} started",
        env!("CARGO_PKG_VERSION")
    );
    sap_router::serve(config, listener, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
}
