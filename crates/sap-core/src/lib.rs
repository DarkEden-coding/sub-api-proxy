//! Shared contracts for Sub-Api-Proxy.
//!
//! Every type that crosses a crate boundary lives here, so the thread registry, routing
//! policy, store, sync, local router, server wiring and dashboard can be built in
//! parallel against one definition. This crate has no I/O.
//!
//! Ownership of the behavior behind these types:
//! - [`thread`]: `sap-threads` produces [`thread::ThreadIdentity`] and keeps
//!   [`thread::ThreadRecord`]s.
//! - [`routing`]: `sap-routing` implements [`routing::RoutingPolicy`].
//! - [`sync`]: `sap-store` implements [`sync::ChangeLog`]; `sap-sync` moves changes between peers.
//! - [`health`]: `sap-server` serves [`health::HealthReport`]; `sap-router` consumes it.
//! - [`api`]: `sap-server` serves these JSON shapes; the dashboard in `ui/sap` reads them.
//! - [`reset_aware`]: the routes the macOS usage visualizer reads.

pub mod account;
pub mod api;
pub mod health;
pub mod hlc;
pub mod ids;
pub mod quota;
pub mod reset_aware;
pub mod routing;
pub mod sync;
pub mod thread;

pub use chrono::{DateTime, Utc};

/// Time source, so tests can control the clock.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

/// The wall clock.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}
