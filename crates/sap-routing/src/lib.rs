//! Pure thread-affinity routing and plan capacity lookup.
//!
//! Import [`sap_core::routing::RoutingPolicy`] to call the policy's `decide` and
//! `scores` methods. No clock, provider client, or assignment state is kept here.

mod capacity;
mod policy;
mod scoring;

pub use capacity::plan_capacity;
pub use policy::ThreadAffinityPolicy;
