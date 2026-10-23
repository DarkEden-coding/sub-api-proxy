use sap_core::{health::NodeRole, ids::NodeId, routing::RoutingConfig};
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub node: NodeId,
    pub role: NodeRole,
    pub store_path: PathBuf,
    /// Development only: accept authenticated peers over loopback.
    #[serde(default)]
    pub allow_loopback_peers: bool,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub peers: Vec<PeerConfig>,
    #[serde(default)]
    pub routing: RoutingConfig,
    #[serde(default = "idle")]
    pub idle_window_secs: u64,
    #[serde(default = "maximum")]
    pub registry_max: usize,
}
fn idle() -> u64 {
    20
}
fn maximum() -> usize {
    10000
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerConfig {
    pub id: NodeId,
    pub url: String,
    pub secret_file: PathBuf,
}
#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    pub quota_max_age_secs: u64,
    pub usage_max_age_secs: u64,
    pub event_max_age_secs: u64,
    pub op_log_max_age_secs: u64,
    pub thread_max_age_secs: u64,
    pub max_bytes: u64,
}
impl Default for Limits {
    fn default() -> Self {
        let l = sap_store::Limits::default();
        Self {
            quota_max_age_secs: l.quota_max_age.as_secs(),
            usage_max_age_secs: l.usage_max_age.as_secs(),
            event_max_age_secs: l.event_max_age.as_secs(),
            op_log_max_age_secs: l.op_log_max_age.as_secs(),
            thread_max_age_secs: l.thread_max_age.as_secs(),
            max_bytes: l.max_bytes,
        }
    }
}
impl Limits {
    pub(crate) fn store(&self) -> sap_store::Limits {
        sap_store::Limits {
            quota_max_age: Duration::from_secs(self.quota_max_age_secs),
            usage_max_age: Duration::from_secs(self.usage_max_age_secs),
            event_max_age: Duration::from_secs(self.event_max_age_secs),
            op_log_max_age: Duration::from_secs(self.op_log_max_age_secs),
            thread_max_age: Duration::from_secs(self.thread_max_age_secs),
            max_bytes: self.max_bytes,
        }
    }
}
impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let mut c: Self = serde_yaml_ng::from_slice(&std::fs::read(path)?)?;
        anyhow::ensure!(
            !c.node.0.trim().is_empty() && c.registry_max > 0,
            "node and registry_max must be nonempty"
        );
        let base = path.parent().unwrap_or(Path::new("."));
        c.store_path = base.join(&c.store_path);
        let mut ids = std::collections::BTreeSet::new();
        for peer in &mut c.peers {
            anyhow::ensure!(
                peer.id != c.node && ids.insert(peer.id.clone()),
                "duplicate or self peer"
            );
            peer.secret_file = base.join(&peer.secret_file);
        }
        Ok(c)
    }
}
