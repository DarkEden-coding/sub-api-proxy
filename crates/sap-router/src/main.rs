use clap::Parser;
use sap_router::config::{Cli, Config};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::load(Cli::parse().config)?;
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    sap_router::serve(config, listener, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
}
