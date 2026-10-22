//! Authenticated, bounded HTTP replication over Tailscale.
mod agent;
mod auth;
mod server;

pub use agent::{Peer, PeerStatus, SyncAgent};
pub use server::{ServerState, router};

use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ponytail: fixed wire caps keep transport bounded; add negotiated/configured
// limits when deployments need records or batches beyond these ceilings.
pub(crate) const MAX_BODY: usize = 8 * 1024 * 1024;
pub(crate) const MAX_BATCH: usize = 1000;
pub(crate) const MAX_WAIT_MS: u64 = 25_000;

#[cfg(test)]
mod tests;
