//! Hybrid logical clock for ordering replicated changes across nodes.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::ids::NodeId;

/// A timestamp that orders changes consistently on every node: wall time in
/// milliseconds, a counter for changes in the same millisecond or behind a peer's clock,
/// and the node as the final tie-break.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Hlc {
    pub wall_ms: i64,
    pub counter: u32,
    pub node: NodeId,
}

impl Ord for Hlc {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.wall_ms, self.counter, &self.node).cmp(&(other.wall_ms, other.counter, &other.node))
    }
}

impl PartialOrd for Hlc {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Hlc {
    pub fn zero(node: NodeId) -> Self {
        Self { wall_ms: 0, counter: 0, node }
    }

    /// The next local timestamp after `self`, given the current wall time.
    pub fn tick(&self, now_ms: i64) -> Self {
        if now_ms > self.wall_ms {
            Self { wall_ms: now_ms, counter: 0, node: self.node.clone() }
        } else {
            Self { wall_ms: self.wall_ms, counter: self.counter + 1, node: self.node.clone() }
        }
    }

    /// The local timestamp after receiving `remote`, so later local changes order after it.
    pub fn observe(&self, remote: &Hlc, now_ms: i64) -> Self {
        let wall = now_ms.max(self.wall_ms).max(remote.wall_ms);
        let counter = if wall == self.wall_ms && wall == remote.wall_ms {
            self.counter.max(remote.counter) + 1
        } else if wall == self.wall_ms {
            self.counter + 1
        } else if wall == remote.wall_ms {
            remote.counter + 1
        } else {
            0
        };
        Self { wall_ms: wall, counter, node: self.node.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(s: &str) -> NodeId {
        NodeId(s.into())
    }

    #[test]
    fn tick_and_observe_stay_monotonic() {
        let a = Hlc::zero(n("a")).tick(100);
        let a2 = a.tick(90);
        assert!(a2 > a);
        let remote = Hlc { wall_ms: 500, counter: 3, node: n("b") };
        let a3 = a2.observe(&remote, 120);
        assert!(a3 > remote && a3 > a2);
    }
}
