//! Routing decisions: the per-thread rules in `docs/sub-api-proxy/discovery.md`.

use serde::{Deserialize, Serialize};

use crate::account::AccountRuntime;
use crate::ids::{AccountId, Provider};
use crate::quota::WindowKind;
use crate::thread::Assignment;
use crate::{DateTime, Utc};

/// Why a thread is on its account. Shown in the dashboard and the visualizer, so each
/// variant has a short label.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "code")]
pub enum Reason {
    /// New independent thread: best score.
    BestScore { detail: ScoreDetail },
    /// Child or fork started on its parent's account.
    ParentAffinity,
    /// Child or fork could not use its parent's account and was scored instead.
    ParentUnavailable { detail: ScoreDetail },
    /// Ordinary continuation.
    Kept,
    /// Re-scored after compaction and stayed.
    KeptAfterCompaction,
    /// Re-scored after compaction and moved to a clearly better account.
    MovedAfterCompaction { from: AccountId },
    /// Moved because the previous account's window is exhausted.
    MovedExhausted { from: AccountId, window: WindowKind },
    /// Moved because the previous account was revoked, disabled or cannot serve the model.
    MovedIneligible { from: AccountId },
    /// Set by the user.
    Manual,
}

/// Which factor decided a score, for the explanation text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoreDetail {
    /// Most allowance that resets soon and would otherwise go unused.
    ResetSoon,
    /// Most headroom in its tightest window.
    MostHeadroom,
    /// Least loaded among similar accounts.
    LeastLoaded,
    /// Only eligible account.
    OnlyChoice,
}

impl Reason {
    /// Short label for the UI, such as "parent affinity" or "moved: weekly limit".
    pub fn label(&self) -> String {
        let detail = |d: &ScoreDetail| match d {
            ScoreDetail::ResetSoon => "resets soonest",
            ScoreDetail::MostHeadroom => "most headroom",
            ScoreDetail::LeastLoaded => "least loaded",
            ScoreDetail::OnlyChoice => "only eligible account",
        };
        let window = |w: &WindowKind| match w {
            WindowKind::FiveHour => "5-hour limit".to_owned(),
            WindowKind::Weekly => "weekly limit".to_owned(),
            WindowKind::Other(n) => format!("{n} limit"),
        };
        match self {
            Reason::BestScore { detail: d } => detail(d).to_owned(),
            Reason::ParentAffinity => "parent affinity".to_owned(),
            Reason::ParentUnavailable { detail: d } => format!("parent account unavailable · {}", detail(d)),
            Reason::Kept => "kept".to_owned(),
            Reason::KeptAfterCompaction => "kept after compaction".to_owned(),
            Reason::MovedAfterCompaction { .. } => "moved after compaction".to_owned(),
            Reason::MovedExhausted { window: w, .. } => format!("moved: {}", window(w)),
            Reason::MovedIneligible { .. } => "moved: account ineligible".to_owned(),
            Reason::Manual => "set manually".to_owned(),
        }
    }
}

/// What the upstream said about the last attempt, classified by the executors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Outcome {
    Success,
    /// Temporary rate limit (429 without exhaustion).
    RateLimited { retry_after: Option<DateTime<Utc>> },
    /// Upstream overloaded (Claude 529, 503).
    Overloaded { retry_after: Option<DateTime<Utc>> },
    /// A five-hour or weekly window is confirmed used up.
    Exhausted { window: WindowKind, reset_at: Option<DateTime<Utc>> },
    /// Login revoked or account disabled upstream.
    Revoked,
    /// The account's plan cannot serve the requested model.
    ModelIneligible,
}

/// The situation a decision is made for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum RoutingEvent {
    /// First request of an independent thread.
    NewThread,
    /// First request of a subagent or fork; prefer the parent's current account.
    ChildStart { parent_account: Option<AccountId> },
    /// A later request of a known thread.
    Continue { current: Assignment },
    /// The previous attempt on the thread's account failed with this outcome.
    AfterOutcome { current: Assignment, outcome: Outcome, first_failure_at: DateTime<Utc> },
    /// The thread just completed a compaction; reconsider its account.
    CompactionCompleted { current: Assignment },
}

/// Everything a decision needs. Built by the server; the policy is a pure function of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingInput {
    pub now: DateTime<Utc>,
    pub provider: Provider,
    pub model: Option<String>,
    pub event: RoutingEvent,
    /// Accounts of `provider`, including ineligible ones (the policy filters them).
    pub accounts: Vec<AccountRuntime>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Decision {
    /// Send the request on this account. When `account` differs from the thread's
    /// current account, the thread moves and the server forces a full-history request.
    Use { account: AccountId, reason: Reason },
    /// Temporary limit: retry the same account after `at` (before the deadline).
    RetrySame { account: AccountId, at: DateTime<Utc> },
    /// Nothing can serve the request now. `retryable` errors carry the earliest known
    /// usable time; it is `None` when unknown and is never invented.
    Reject { retryable: bool, earliest_usable_at: Option<DateTime<Utc>>, message: String },
}

/// One account's score, for explanations and the visualizer's weights.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountScore {
    pub account: AccountId,
    pub eligible: bool,
    /// Why the account is not eligible, if it is not.
    pub ineligible_reason: Option<String>,
    pub score: f64,
    /// Share of new threads this account would receive, 0 to 100, summing to 100 over
    /// eligible accounts of a provider.
    pub weight_percent: f64,
    /// Windows with no observation or an observation older than `RoutingConfig::stale_after_secs`.
    pub stale: bool,
}

/// Implemented by `sap-routing`.
pub trait RoutingPolicy: Send + Sync {
    fn decide(&self, input: &RoutingInput) -> Decision;
    /// Scores for every account in `input.accounts` (the event is ignored).
    fn scores(&self, input: &RoutingInput) -> Vec<AccountScore>;
}

/// Tunables. Defaults are starting points to be set from replayed usage history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RoutingConfig {
    pub urgency_weight: f64,
    pub headroom_weight: f64,
    pub load_weight: f64,
    /// Lower bound for time-to-reset in the urgency term, in seconds.
    pub reset_floor_secs: u64,
    /// Observations older than this count as stale.
    pub stale_after_secs: u64,
    /// Score multiplier for accounts with stale or missing windows (0 to 1).
    pub stale_penalty: f64,
    /// How long a temporary limit is retried on the same account before giving up.
    pub retry_deadline_secs: u64,
    /// After compaction, move only if another account's score is this much higher
    /// (relative, 0.25 = 25 percent).
    pub compaction_move_margin: f64,
    /// Capacity multiplier by plan name; lookups are case-insensitive substrings.
    pub plan_capacity: Vec<(String, f64)>,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            urgency_weight: 1.0,
            headroom_weight: 1.0,
            load_weight: 0.5,
            reset_floor_secs: 600,
            stale_after_secs: 1800,
            stale_penalty: 0.7,
            retry_deadline_secs: 60,
            compaction_move_margin: 0.25,
            plan_capacity: vec![
                ("max_20x".into(), 20.0),
                ("max_5x".into(), 5.0),
                ("max".into(), 5.0),
                ("team".into(), 1.5),
                ("pro".into(), 1.0),
                ("plus".into(), 1.0),
                ("free".into(), 0.2),
            ],
        }
    }
}
