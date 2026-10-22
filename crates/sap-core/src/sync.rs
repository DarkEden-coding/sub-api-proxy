//! Replicated changes between nodes. `sap-store` owns the op log and merge rules
//! (implementing [`ChangeLog`]); `sap-sync` moves batches between peers.
//!
//! Merge rules (`docs/sub-api-proxy/implementation-plan.md`):
//! - [`Table::Accounts`], [`Table::Settings`], [`Table::ClientAccess`]: last writer wins
//!   per field by HLC; concurrent edits from different nodes during a split create a
//!   [`Conflict`].
//! - [`Table::Threads`]: last writer wins by HLC; divergence is recorded as an event.
//! - [`Table::QuotaObservations`], [`Table::UsageBuckets`], [`Table::Events`]: append-only,
//!   deduplicated by `(origin, seq)`.
//! - Node credentials are never replicated.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::hlc::Hlc;
use crate::ids::NodeId;
use crate::{DateTime, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Table {
    Accounts,
    Settings,
    ClientAccess,
    Threads,
    QuotaObservations,
    UsageBuckets,
    Events,
    Conflicts,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "op")]
pub enum ChangeOp {
    /// Set one field of a record (field-level LWW tables) or the whole record when
    /// `field` is `None`.
    Set {
        field: Option<String>,
        value: serde_json::Value,
    },
    /// Delete a record (a tombstone that wins over older sets).
    Delete,
    /// Append an immutable entry (append-only tables).
    Append { value: serde_json::Value },
}

/// One replicated change. `(origin, seq)` is unique; `seq` increases by one per change
/// made on `origin`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Change {
    pub origin: NodeId,
    pub seq: u64,
    pub hlc: Hlc,
    pub table: Table,
    pub key: String,
    pub op: ChangeOp,
}

/// The highest `seq` applied from each origin. A peer sends everything above it.
pub type SyncCursor = BTreeMap<NodeId, u64>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangeBatch {
    pub from: NodeId,
    pub changes: Vec<Change>,
    /// More changes are available after this batch.
    pub more: bool,
}

/// Concurrent edits to the same field that need a choice from the user.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Conflict {
    pub id: String,
    pub table: Table,
    pub key: String,
    pub field: String,
    /// The value currently applied (the higher HLC) and the one it replaced.
    pub applied: ConflictSide,
    pub other: ConflictSide,
    pub detected_at: DateTime<Utc>,
    pub resolved: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConflictSide {
    pub node: NodeId,
    pub hlc: Hlc,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ApplyReport {
    pub applied: usize,
    /// Already present (same origin and seq) or superseded.
    pub ignored: usize,
    pub conflicts: Vec<Conflict>,
}

/// The op log of one node. Implemented by `sap-store`.
pub trait ChangeLog: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    fn node(&self) -> &NodeId;
    /// What this node has applied, per origin (including itself).
    fn cursor(&self) -> Result<SyncCursor, Self::Error>;
    /// Changes this node holds that are newer than `since`, oldest first, at most `limit`.
    fn changes_since(&self, since: &SyncCursor, limit: usize) -> Result<ChangeBatch, Self::Error>;
    /// Merge changes received from a peer. Idempotent.
    fn apply_remote(&self, changes: &[Change]) -> Result<ApplyReport, Self::Error>;
}
