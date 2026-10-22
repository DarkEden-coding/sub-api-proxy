//! Thread identity as read from one request, and the registry's record of a thread.
//!
//! The extraction rules are documented in `docs/sub-api-proxy/validation.md`.

use serde::{Deserialize, Serialize};

use crate::DateTime;
use crate::Utc;
use crate::ids::{AccountId, NodeId, Provider, ThreadKey};
use crate::routing::Reason;

/// The coding client that sent a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    ClaudeCode,
    CodexCli,
    /// A request with no recognizable client markers.
    Other,
}

/// What a request does for its thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestClass {
    /// An ordinary model turn (Claude `main`, Codex `turn`).
    Turn,
    /// A turn of a subagent thread (Claude `subagent`; Codex turns on a subagent thread).
    SubagentTurn,
    /// A compaction request (Claude `compaction`, Codex `request_kind: compaction`).
    Compaction,
    /// A Codex connection warmup (`generate: false`). Uses the thread's account; no turn.
    Prewarm,
    /// A helper request such as a Claude session title. Uses the thread's account if the
    /// thread is known, and never creates a thread.
    Auxiliary,
}

/// How a thread relates to another thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineageKind {
    Root,
    Subagent,
    Fork,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionTrigger {
    Manual,
    Auto,
}

/// Whether the request carries the whole conversation or depends on state the provider
/// holds for an earlier request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Continuation {
    /// Full history in the request (Claude `thread: create` or no `thread` field; Codex
    /// `previous_response_id: null`). Any eligible account can serve it.
    FullHistory,
    /// Depends on provider-side state: Claude `thread: continue` with its
    /// `previous_message_id`, or Codex `previous_response_id`. Only the account (and
    /// possibly the login) that produced `previous_id` can serve it.
    Continue { previous_id: String },
}

/// Everything the proxy reads from one request about its thread. Produced by
/// `sap_threads::identify`; consumed by the registry, routing and the server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadIdentity {
    pub key: ThreadKey,
    pub client: ClientKind,
    pub class: RequestClass,
    pub lineage: LineageKind,
    /// The root session: Claude's session ID, Codex `session-id`. Equal to `key.id` for a
    /// root thread.
    pub root_session: String,
    /// The parent thread for a subagent (Claude: the session thread; Codex:
    /// `parent_thread_id`).
    pub parent: Option<ThreadKey>,
    /// The source thread of a fork when the request names it (Codex
    /// `forked_from_thread_id`). Claude forks are resolved by the registry from
    /// [`Self::previous_message_id`].
    pub forked_from: Option<ThreadKey>,
    /// Claude `diagnostics.previous_message_id`: the last assistant message the client
    /// saw. A new session whose value belongs to another thread is a fork of it.
    pub previous_message_id: Option<String>,
    pub continuation: Continuation,
    /// Set on compaction requests.
    pub compaction: Option<CompactionTrigger>,
    /// Codex window number from `x-codex-window-id` (`<thread>:<n>`); increments after
    /// each compaction. `None` for Claude, whose generation the registry counts.
    pub window_number: Option<u32>,
    /// Claude `x-claude-code-agent-type` or the Codex `agent_name` path (`/root/reader`).
    pub agent_label: Option<String>,
    /// Claude `x-claude-code-prompt-id` or Codex `turn_id`, when present.
    pub turn_id: Option<String>,
    pub model: Option<String>,
    /// The client machine, from the local router's `X-Sap-Client-Host` header.
    pub client_host: Option<String>,
}

/// The registry's record of a thread. Replicated between nodes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadRecord {
    pub key: ThreadKey,
    pub client: ClientKind,
    pub lineage: LineageKind,
    pub root_session: String,
    pub parent: Option<ThreadKey>,
    pub client_host: Option<String>,
    pub agent_label: Option<String>,
    /// A short human title when known (for example from a Claude title response).
    pub title: Option<String>,
    pub assignment: Option<Assignment>,
    /// Completed compactions.
    pub compaction_generation: u32,
    /// The last assistant message or response ID produced for this thread. Used to
    /// detect Claude forks and to check whether a continuation can be served.
    pub last_response_id: Option<String>,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    /// The node that served the latest request.
    pub serving_node: Option<NodeId>,
    /// Requests currently streaming for this thread on the local node.
    pub open_streams: u32,
    pub usage: ThreadUsage,
}

/// The account a thread is pinned to, and why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assignment {
    pub account: AccountId,
    pub reason: Reason,
    pub at: DateTime<Utc>,
}

/// Token counts for a thread since it was first seen (from upstream usage reports).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ThreadUsage {
    pub requests: u64,
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub output_tokens: u64,
}

impl ThreadUsage {
    /// Share of input served from the provider's prompt cache, if any input was sent.
    pub fn cache_hit_percent(&self) -> Option<f64> {
        let total = self.input_tokens + self.cache_read_tokens + self.cache_write_tokens;
        (total > 0).then(|| self.cache_read_tokens as f64 * 100.0 / total as f64)
    }
}

impl ThreadIdentity {
    pub fn provider(&self) -> Provider {
        self.key.provider
    }
}
