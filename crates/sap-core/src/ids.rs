//! Identifiers shared by every component.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The upstream provider family. Claude Code uses Claude accounts; Codex CLI uses OpenAI
/// (ChatGPT) accounts. Serialized names match the visualizer contract (`claude`, `codex`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Claude,
    Codex,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A server node, such as `power-server` or `macbook`. Stable across restarts.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub String);

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A logical subscription account, the same on every node. Node-owned login credentials
/// refer to it; they are never replicated.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AccountId(pub String);

impl fmt::Display for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A conversation thread as the provider sees it.
///
/// - Claude Code: `X-Claude-Code-Session-Id`, or `<session>/<agent-id>` for a subagent.
/// - Codex: the `thread-id` header (a subagent has its own thread ID).
///
/// Serialized as `"<provider>:<id>"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ThreadKey {
    pub provider: Provider,
    pub id: String,
}

impl ThreadKey {
    pub fn new(provider: Provider, id: impl Into<String>) -> Self {
        Self { provider, id: id.into() }
    }

    /// Parses `"<provider>:<id>"`.
    pub fn parse(s: &str) -> Option<Self> {
        let (p, id) = s.split_once(':')?;
        let provider = match p {
            "claude" => Provider::Claude,
            "codex" => Provider::Codex,
            _ => return None,
        };
        (!id.is_empty()).then(|| Self::new(provider, id))
    }
}

impl fmt::Display for ThreadKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.provider, self.id)
    }
}

impl Serialize for ThreadKey {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ThreadKey {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        ThreadKey::parse(&raw).ok_or_else(|| serde::de::Error::custom(format!("invalid thread key {raw:?}")))
    }
}
