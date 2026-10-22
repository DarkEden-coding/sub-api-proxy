//! Logical accounts and their live state.

use serde::{Deserialize, Serialize};

use crate::ids::{AccountId, NodeId, Provider};
use crate::quota::QuotaWindow;
use crate::{DateTime, Utc};

/// Where an account's capacity multiplier came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapacitySource {
    /// No plan known yet; multiplier 1.
    Default,
    /// From the plan table in config.
    Plan,
    /// Refined from observed tokens per percentage point of usage.
    Learned,
    /// Set by the user.
    Override,
}

/// How much one percentage point of this account's windows is worth relative to a
/// baseline plan (Claude Pro or ChatGPT Plus = 1). Different plans reporting the same
/// percentage are not equal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Capacity {
    pub multiplier: f64,
    pub source: CapacitySource,
}

impl Default for Capacity {
    fn default() -> Self {
        Self {
            multiplier: 1.0,
            source: CapacitySource::Default,
        }
    }
}

/// A logical account. Replicated; each field merges by HLC (see `sap-store`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Account {
    pub id: AccountId,
    pub provider: Provider,
    pub label: String,
    pub email: Option<String>,
    /// Plan name as the provider reports it (`plus`, `pro`, `default_claude_max_20x`, ...).
    pub plan: Option<String>,
    pub capacity: Capacity,
    /// Higher is preferred when scores tie. Default 0.
    pub priority: i32,
    pub disabled: bool,
    /// Models this account may serve. `None` means every model of its provider.
    pub models: Option<Vec<String>>,
    pub created_at: DateTime<Utc>,
}

impl Account {
    pub fn serves_model(&self, model: &str) -> bool {
        self.models.as_ref().is_none_or(|m| m.iter().any(|x| x == model))
    }
}

/// An account plus what the serving node knows about it right now. Built by the server
/// for each routing decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountRuntime {
    pub account: Account,
    /// Latest window per kind. Empty when nothing has been observed.
    pub windows: Vec<QuotaWindow>,
    /// Streams currently open on this account across the local node.
    pub active_streams: u32,
    /// Active threads assigned to this account (all nodes, from the registry).
    pub active_threads: u32,
    /// Confirmed exhaustion: unusable until this time (the reset of the exhausted window).
    /// `Some(None)` means exhausted with an unknown reset.
    pub exhausted_until: Option<Option<DateTime<Utc>>>,
    /// Temporary rate limit or overload: retry after this time.
    pub cooling_until: Option<DateTime<Utc>>,
    /// The serving node has a working login for this account.
    pub credential_usable: bool,
    /// Revoked or disabled upstream.
    pub revoked: bool,
}

/// A node-owned login for a logical account. Never replicated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeCredential {
    pub account: AccountId,
    pub node: NodeId,
    /// Path of the cliproxy credential file on that node.
    pub credential_file: String,
    pub last_refresh: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}
