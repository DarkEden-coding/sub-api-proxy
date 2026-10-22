//! Sub-Api-Proxy inside cliproxy: implements `cpa_server::sap_hooks::SapHooks` with the
//! thread registry, routing policy, store and peer sync, and serves the management API.
//!
//! Modules: `control` is the seam between node state (`node`, workstream H) and the
//! HTTP layer (`api`, workstream I). See `docs/sub-api-proxy/parallel-work.md`.

pub mod control;

pub mod api;
