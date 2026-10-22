//! `GET /sap/health`: what a server reports to local routers and peers.

use serde::{Deserialize, Serialize};

use crate::ids::NodeId;
use crate::sync::SyncCursor;
use crate::{DateTime, Utc};

/// Path of the health endpoint on every server. No authentication; reveals no secrets.
pub const HEALTH_PATH: &str = "/sap/health";

/// Header the local router adds with the client machine's name. Servers record it and
/// strip it before forwarding upstream.
pub const CLIENT_HOST_HEADER: &str = "x-sap-client-host";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeRole {
    Primary,
    Fallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeState {
    /// Accepting and serving requests.
    Serving,
    /// Starting up or catching up with peers; routers should not send new requests yet.
    Starting,
    /// Running but cannot reach any upstream provider.
    Degraded,
    /// Shutting down; finish open streams elsewhere.
    Draining,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthReport {
    pub node: NodeId,
    pub role: NodeRole,
    pub state: NodeState,
    pub version: String,
    pub now: DateTime<Utc>,
    pub started_at: DateTime<Utc>,
    /// Whether each provider's upstream answered recently.
    pub upstream: UpstreamHealth,
    pub open_streams: u32,
    pub sync: SyncCursor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamHealth {
    pub claude: bool,
    pub codex: bool,
}

/// Authenticated peer-only process telemetry. Never added to public health.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PeerTelemetry {
    pub node: NodeId,
    pub role: NodeRole,
    pub state: NodeState,
    pub version: String,
    pub observed_at: DateTime<Utc>,
    /// Inference requests identified in the trailing 60 seconds, including failures.
    pub requests_per_minute: f64,
    pub open_streams: u32,
    /// Median SAP selection callback duration, excluding upstream time.
    pub added_latency_p50_ms: Option<f64>,
    pub rss_bytes: Option<u64>,
}
