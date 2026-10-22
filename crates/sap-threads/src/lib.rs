//! Request identity extraction and a bounded, caller-synchronized thread registry.
mod identify;
mod registry;

pub use identify::{identify, identify_claude, identify_codex};
pub use registry::{Observation, Registry};
