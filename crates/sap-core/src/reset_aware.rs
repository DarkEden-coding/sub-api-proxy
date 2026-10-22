//! Response shapes for the macOS usage visualizer (`CLI-Proxy-Usage-Indicator`,
//! `Sources/UsageCore/Models.swift`). Keys are snake_case and dates RFC 3339.
//!
//! Routes (all `GET` except `refresh`, under `/v8/management/routing/reset-aware/`):
//! `status` → [`StatusResponse`], `history?hours=N` → [`HistoryResponse`],
//! `requests` → [`RequestsResponse`], `POST refresh` → [`StatusResponse`].

use serde::{Deserialize, Serialize};

use crate::{DateTime, Utc};

pub const BASE_PATH: &str = "/v8/management/routing/reset-aware";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusResponse {
    pub now: DateTime<Utc>,
    pub polling: bool,
    pub settings: RoutingSettings,
    pub accounts: Vec<AccountStatus>,
}

/// The visualizer requires these fields. Sub-Api-Proxy reports `strategy:
/// "thread-affinity"` and fills the legacy tunables with zeros; the app shows server
/// reasons instead of recomputing ranks from them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingSettings {
    pub strategy: String,
    pub active: bool,
    pub drain_window_seconds: f64,
    pub drain_floor_percent: f64,
    pub protect_after_seconds: f64,
    pub protect_used_percent: f64,
    pub five_hour_guard_percent: f64,
    pub reset_weight_exponent: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowStatus {
    pub used_percent: f64,
    pub reset_at: Option<DateTime<Utc>>,
    pub source: Option<String>,
    pub observed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountStatus {
    /// Stable per logical account (the visualizer's identity).
    pub auth_index: String,
    pub auth_id: String,
    /// `claude` or `codex`.
    pub provider: String,
    pub label: String,
    pub email: Option<String>,
    pub priority: i32,
    pub disabled: bool,
    pub unavailable: bool,
    /// `oauth` for subscription logins.
    pub auth_kind: String,
    /// `normal`, `draining`, `protected`, `guarded` or `unknown`. Sub-Api-Proxy uses
    /// `normal`, `unknown` (no quota data) and `guarded` (exhausted or cooling).
    pub state: String,
    pub weight: f64,
    pub share_percent: f64,
    pub weekly: Option<WindowStatus>,
    pub five_hour: Option<WindowStatus>,
    pub last_poll_at: Option<DateTime<Utc>>,
    pub last_poll_error: Option<String>,
    /// Sub-Api-Proxy addition, ignored by older app builds: the routing explanation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryResponse {
    pub now: DateTime<Utc>,
    pub hours: u32,
    pub accounts: Vec<HistoryAccount>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryAccount {
    pub auth_index: String,
    pub provider: String,
    pub samples: Vec<HistorySample>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistorySample {
    pub t: DateTime<Utc>,
    pub weekly_used_percent: Option<f64>,
    pub weekly_reset_at: Option<DateTime<Utc>>,
    pub five_hour_used_percent: Option<f64>,
    pub five_hour_reset_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestsResponse {
    pub now: DateTime<Utc>,
    pub bucket_seconds: u32,
    pub groups: Vec<RequestGroup>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestGroup {
    pub provider: String,
    pub accounts: Vec<RequestAccount>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestAccount {
    pub auth_index: String,
    pub buckets: Vec<RequestBucket>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestBucket {
    pub start: DateTime<Utc>,
    pub success: u64,
    pub failed: u64,
}
