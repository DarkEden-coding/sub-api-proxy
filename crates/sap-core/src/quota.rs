//! Usage windows reported by providers.

use serde::{Deserialize, Serialize};

use crate::ids::{AccountId, NodeId};
use crate::{DateTime, Utc};

/// A limiting window. Claude and Codex both report a five-hour and a weekly window;
/// anything else is kept by name so routing can still respect it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowKind {
    FiveHour,
    Weekly,
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationSource {
    /// Read from response headers or stream events of ordinary traffic
    /// (Claude `anthropic-ratelimit-unified-*`, Codex `codex.rate_limits`).
    Passive,
    /// Read from a usage endpoint (Claude `/api/oauth/usage`, Codex `wham/usage`).
    Probe,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaWindow {
    pub kind: WindowKind,
    /// 0 to 100.
    pub used_percent: f64,
    /// `None` when the provider did not say. Never invented.
    pub reset_at: Option<DateTime<Utc>>,
    pub window_minutes: Option<u32>,
    pub observed_at: DateTime<Utc>,
    pub source: ObservationSource,
}

/// One observation of an account's windows. Append-only and replicated; `(node, seq)`
/// identifies it so merges never double count.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaObservation {
    pub account: AccountId,
    pub node: NodeId,
    pub seq: u64,
    pub windows: Vec<QuotaWindow>,
    /// Plan name if this observation carried one (Codex `plan_type`, Claude rate tier).
    pub plan: Option<String>,
    /// Upstream says the account can be used now (Codex `rate_limits.allowed`).
    pub allowed: Option<bool>,
}
