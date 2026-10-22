//! The seam between node state (workstream H) and the HTTP layer (workstream I).
//!
//! `node` implements [`Control`]; `api` serves `/sap/api/*`, the visualizer's
//! reset-aware routes and the dashboard on top of it, and tests itself against a fake.
//! Routes and JSON shapes are defined in `sap_core::api` and `sap_core::reset_aware`.

use std::future::Future;
use std::pin::Pin;

use sap_core::api::{
    AccountHistory, AccountPatch, AccountView, ApiError, EventView, FlowSnapshot, LoginRequest, LoginStart, NodeView,
    ThreadView,
};
use sap_core::ids::{AccountId, NodeId, ThreadKey};
use sap_core::reset_aware::{HistoryResponse, RequestsResponse, StatusResponse};
use sap_core::sync::Conflict;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// An error the API returns as JSON [`ApiError`] with `status`.
#[derive(Debug, Clone, PartialEq)]
pub struct ControlError {
    pub status: u16,
    /// Short machine code, such as `not_found` or `invalid_request`.
    pub error: &'static str,
    pub message: String,
}

impl ControlError {
    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: 404,
            error: "not_found",
            message: message.into(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self {
            status: 400,
            error: "invalid_request",
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            status: 500,
            error: "internal",
            message: message.into(),
        }
    }

    pub fn body(&self) -> ApiError {
        ApiError {
            error: self.error.to_owned(),
            message: self.message.clone(),
        }
    }
}

pub type ControlResult<T> = Result<T, ControlError>;

/// A change pushed to dashboard subscribers (`GET /sap/api/stream`).
#[derive(Debug, Clone)]
pub enum ControlEvent {
    /// Something in the flow view changed. Subscribers fetch [`Control::flow`]; the API
    /// coalesces these to at most four snapshots per second.
    Changed,
    /// A new event for the event list.
    Event(EventView),
}

/// Everything the management API needs from the running node. Methods are cheap reads
/// of in-memory state unless they return a future.
pub trait Control: Send + Sync + 'static {
    fn flow(&self) -> FlowSnapshot;
    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<ControlEvent>;

    fn accounts(&self) -> Vec<AccountView>;
    fn patch_account(&self, id: &AccountId, patch: AccountPatch) -> ControlResult<AccountView>;
    /// Starts a provider login on this node (adding an account, or re-logging one in).
    fn start_login(&self, request: LoginRequest) -> BoxFuture<'_, ControlResult<LoginStart>>;
    fn account_history(&self, id: &AccountId, hours: u32) -> ControlResult<AccountHistory>;

    /// Active threads only when `active_only`, otherwise every retained thread.
    fn threads(&self, active_only: bool) -> Vec<ThreadView>;
    fn assign_thread(&self, key: &ThreadKey, account: &AccountId) -> ControlResult<ThreadView>;

    fn nodes(&self) -> Vec<NodeView>;
    fn conflicts(&self) -> Vec<Conflict>;
    fn resolve_conflict(&self, id: &str, keep: &NodeId) -> ControlResult<Conflict>;
    fn events(&self, limit: usize) -> Vec<EventView>;

    /// The macOS visualizer's routes (`sap_core::reset_aware`).
    fn reset_aware_status(&self) -> StatusResponse;
    fn reset_aware_history(&self, hours: u32) -> HistoryResponse;
    fn reset_aware_requests(&self) -> RequestsResponse;
    /// Probes every account's usage endpoint now, then returns the status.
    fn reset_aware_refresh(&self) -> BoxFuture<'_, StatusResponse>;
}
