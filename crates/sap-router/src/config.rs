use std::{net::SocketAddr, path::PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;
use http::{HeaderValue, Uri};
use serde::Deserialize;

#[derive(Parser)]
#[command(version, about = "Local streaming proxy with probe-driven server failover")]
pub struct Cli {
    /// YAML configuration; defaults to ~/.config/sub-api-proxy/router.yaml.
    #[arg(long)]
    pub config: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub listen: SocketAddr,
    pub client_host: String,
    pub servers: Vec<Server>,
    pub probe_interval_ms: u64,
    pub probe_timeout_ms: u64,
    pub down_after_failures: u32,
    pub stable_secs: u64,
    pub connect_timeout_ms: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    pub name: String,
    pub base_url: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:8317".parse().unwrap(),
            client_host: hostname(),
            servers: Vec::new(),
            probe_interval_ms: 1000,
            probe_timeout_ms: 1500,
            down_after_failures: 2,
            stable_secs: 30,
            connect_timeout_ms: 1000,
        }
    }
}

fn hostname() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "localhost".into())
}

impl Config {
    pub fn load(path: Option<PathBuf>) -> Result<Self> {
        let path = match path {
            Some(path) => path,
            None => PathBuf::from(std::env::var_os("HOME").context("HOME is unset; use --config")?)
                .join(".config/sub-api-proxy/router.yaml"),
        };
        let config: Self =
            serde_yaml_ng::from_slice(&std::fs::read(&path).with_context(|| format!("read {}", path.display()))?)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        HeaderValue::from_str(&self.client_host).context("invalid client_host header")?;
        if self.servers.is_empty() {
            bail!("servers must not be empty");
        }
        if self.probe_interval_ms == 0
            || self.probe_timeout_ms == 0
            || self.connect_timeout_ms == 0
            || self.down_after_failures == 0
        {
            bail!("probe/connect durations and down_after_failures must be positive");
        }
        let mut names = std::collections::HashSet::new();
        for server in &self.servers {
            let uri: Uri = server.base_url.parse().context("invalid server base_url")?;
            // ponytail: private-network HTTP servers only; add TLS with certificate verification
            // before accepting https URLs. Never silently downgrade a configured HTTPS endpoint.
            if uri.scheme_str() != Some("http")
                || uri.host().is_none()
                || uri.authority().is_some_and(|a| a.as_str().contains('@'))
                || uri.path_and_query().is_some_and(|p| p.as_str() != "/")
            {
                bail!("base_url must be an http:// authority without credentials, path or query");
            }
            if server.name.is_empty() || !names.insert(&server.name) {
                bail!("server names must be nonempty and unique");
            }
        }
        Ok(())
    }
}
