//! The dashboard and CLI API under `/sap/api`. JSON keys are snake_case; times are
//! RFC 3339. `ui/sap/src/api/types.ts` mirrors these types, and
//! `crates/sap-core/tests/fixtures/flow.json` is a sample both sides test against.
//!
//! | Method and path | Body | Response |
//! | --- | --- | --- |
//! | `GET /sap/api/flow` | | [`FlowSnapshot`] |
//! | `GET /sap/api/stream` | | Server-sent events: `snapshot` with a [`FlowSnapshot`] on every change (at most 4 per second), `event` with an [`EventView`] |
//! | `GET /sap/api/accounts` | | `Vec<AccountView>` |
//! | `PATCH /sap/api/accounts/{id}` | [`AccountPatch`] | [`AccountView`] |
//! | `POST /sap/api/accounts/login` | [`LoginRequest`] | [`LoginStart`] |
//! | `GET /sap/api/accounts/{id}/history?hours=N` | | [`AccountHistory`] |
//! | `GET /sap/api/threads?active=true` | | `Vec<ThreadView>` |
//! | `POST /sap/api/threads/{key}/assign` | [`AssignRequest`] | [`ThreadView`] |
//! | `GET /sap/api/nodes` | | `Vec<NodeView>` |
//! | `GET /sap/api/conflicts` | | `Vec<Conflict>` |
//! | `POST /sap/api/conflicts/{id}/resolve` | [`ResolveRequest`] | `Conflict` |
//! | `GET /sap/api/events?limit=N` | | `Vec<EventView>` |
//!
//! Every route requires `Authorization: Bearer <management key>`. Errors are
//! [`ApiError`] with a 4xx or 5xx status.

use serde::{Deserialize, Serialize};

use crate::account::CapacitySource;
use crate::health::{NodeRole, NodeState};
use crate::ids::{AccountId, NodeId, Provider, ThreadKey};
use crate::quota::{ObservationSource, WindowKind};
use crate::thread::{ClientKind, LineageKind};
use crate::{DateTime, Utc};

pub const API_PREFIX: &str = "/sap/api";

/// Everything the flow view shows. Only active threads are included.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlowSnapshot {
    pub now: DateTime<Utc>,
    /// The node that produced this snapshot.
    pub node: NodeId,
    pub nodes: Vec<NodeView>,
    pub accounts: Vec<AccountView>,
    pub threads: Vec<ThreadView>,
    /// Most recent first.
    pub events: Vec<EventView>,
    pub stats: StatsView,
    pub open_conflicts: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountState {
    /// Has active threads.
    Active,
    /// Usable, no active threads.
    Idle,
    /// Temporary rate limit or overload.
    Cooling,
    /// A window is used up; see `available_at`.
    Exhausted,
    Disabled,
    /// No working login on the serving node.
    LoginNeeded,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountView {
    pub id: AccountId,
    pub provider: Provider,
    pub label: String,
    pub email: Option<String>,
    pub plan: Option<String>,
    pub capacity_multiplier: f64,
    pub capacity_source: CapacitySource,
    pub priority: i32,
    pub state: AccountState,
    /// Share of new threads, 0 to 100 per provider.
    pub weight_percent: f64,
    pub windows: Vec<WindowView>,
    pub active_threads: u32,
    /// When an exhausted or cooling account becomes usable, if known.
    pub available_at: Option<DateTime<Utc>>,
    /// Nodes without a working login for this account.
    pub login_needed_on: Vec<NodeId>,
    /// Quota data is missing or stale.
    pub stale: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowView {
    pub kind: WindowKind,
    pub used_percent: f64,
    pub reset_at: Option<DateTime<Utc>>,
    pub observed_at: DateTime<Utc>,
    pub source: ObservationSource,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadView {
    pub key: ThreadKey,
    pub client: ClientKind,
    /// Client machine, from the local router. `None` when unknown.
    pub machine: Option<String>,
    pub title: Option<String>,
    pub lineage: LineageKind,
    pub parent: Option<ThreadKey>,
    /// Claude agent type or Codex agent path.
    pub agent_label: Option<String>,
    pub account: Option<AccountId>,
    /// Human label from `routing::Reason::label`.
    pub reason: Option<String>,
    pub serving_node: Option<NodeId>,
    pub streaming: bool,
    pub cache_hit_percent: Option<f64>,
    pub compactions: u32,
    pub requests: u64,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeView {
    pub id: NodeId,
    pub role: Option<NodeRole>,
    pub state: Option<NodeState>,
    /// Reachable from the node producing the snapshot.
    pub reachable: bool,
    pub version: Option<String>,
    pub requests_per_minute: Option<f64>,
    pub open_streams: Option<u32>,
    /// Median SAP selection callback duration in milliseconds, excluding upstream time.
    pub added_latency_p50_ms: Option<f64>,
    pub rss_bytes: Option<u64>,
    /// How far this node's replicated state trails the snapshot node, in seconds.
    pub sync_lag_secs: Option<f64>,
    pub last_failover_at: Option<DateTime<Utc>>,
    pub last_seen: Option<DateTime<Utc>>,
    /// Peer clock sample time and local receipt time; absent for older peers.
    #[serde(default)]
    pub telemetry_observed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub telemetry_received_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub telemetry_stale: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    ThreadAssigned,
    ThreadMoved,
    Compaction,
    Failover,
    Failback,
    SyncConflict,
    AccountState,
    LoginChange,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventView {
    pub id: String,
    pub at: DateTime<Utc>,
    pub kind: EventKind,
    /// One sentence, plain text.
    pub message: String,
    pub node: NodeId,
    pub thread: Option<ThreadKey>,
    pub account: Option<AccountId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatsView {
    /// Requests over the last three hours, all accounts.
    pub requests_3h: u64,
    pub cache_hit_percent_3h: Option<f64>,
    pub added_latency_p50_ms: Option<f64>,
    pub rss_bytes: Option<u64>,
    /// Fifteen-minute buckets over the last three hours, oldest first.
    pub series: Vec<SeriesPoint>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeriesPoint {
    pub start: DateTime<Utc>,
    pub requests: u64,
    pub cache_read_tokens: u64,
    pub input_tokens: u64,
}

/// Fields to change; absent fields stay as they are.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AccountPatch {
    pub label: Option<String>,
    pub disabled: Option<bool>,
    pub priority: Option<i32>,
    /// `Some(None)` clears a manual capacity override. Omitted from JSON when `None`,
    /// because a JSON `null` means "clear".
    #[serde(default, with = "double_option", skip_serializing_if = "Option::is_none")]
    pub capacity_override: Option<Option<f64>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoginRequest {
    pub provider: Provider,
    /// Re-login an existing logical account on this node; `None` adds a new account.
    pub account: Option<AccountId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoginStart {
    /// Open this URL in a browser to finish signing in.
    pub url: String,
    /// Poll `GET /sap/api/accounts` until an account with this login appears.
    pub login_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountHistory {
    pub account: AccountId,
    pub samples: Vec<HistoryPoint>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryPoint {
    pub t: DateTime<Utc>,
    pub five_hour_used_percent: Option<f64>,
    pub weekly_used_percent: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssignRequest {
    pub account: AccountId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolveRequest {
    /// Keep the value from this node.
    pub keep: NodeId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
    pub message: String,
}

mod double_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<Option<f64>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            None => s.serialize_none(),
            Some(inner) => inner.serialize(s),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<f64>>, D::Error> {
        Ok(Some(Option::<f64>::deserialize(d)?))
    }
}
