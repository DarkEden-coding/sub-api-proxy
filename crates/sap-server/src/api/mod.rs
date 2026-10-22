//! Management HTTP routes and the embedded `/sap/` dashboard.
mod assets;
mod auth;
mod routes;
mod stream;

pub use auth::ManagementAuth;

use crate::control::Control;
use axum::{
    Router, middleware,
    routing::{any, get, patch, post},
};
use std::sync::Arc;

/// Mount on the proxy alongside the inference and peer-sync routers.
pub fn router(control: Arc<dyn Control>, auth: ManagementAuth) -> Router {
    let api = Router::new()
        .route("/sap/api/flow", get(routes::flow))
        .route("/sap/api/stream", get(stream::subscribe))
        .route("/sap/api/accounts", get(routes::accounts))
        .route("/sap/api/accounts/{id}", patch(routes::patch_account))
        .route("/sap/api/accounts/login", post(routes::login))
        .route("/sap/api/accounts/{id}/history", get(routes::account_history))
        .route("/sap/api/threads", get(routes::threads))
        .route("/sap/api/threads/{key}/assign", post(routes::assign))
        .route("/sap/api/nodes", get(routes::nodes))
        .route("/sap/api/conflicts", get(routes::conflicts))
        .route("/sap/api/conflicts/{id}/resolve", post(routes::resolve))
        .route("/sap/api/events", get(routes::events))
        .route("/sap/api", any(routes::not_found))
        .route("/sap/api/{*path}", any(routes::not_found));
    let base = sap_core::reset_aware::BASE_PATH;
    api.route(&format!("{base}/status"), get(routes::status))
        .route(&format!("{base}/history"), get(routes::history))
        .route(&format!("{base}/requests"), get(routes::requests))
        .route(&format!("{base}/refresh"), post(routes::refresh))
        .method_not_allowed_fallback(routes::method_not_allowed)
        .route_layer(middleware::from_fn_with_state(auth, auth::guard))
        .with_state(control)
        .route("/sap", get(assets::redirect))
        .route("/sap/", get(assets::serve))
        .route("/sap/{*path}", get(assets::serve))
}

#[cfg(test)]
mod tests;
