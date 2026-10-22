//! Reuse cpa-server's management guard, including bcrypt, bans and remote policy.
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::handler::Handler;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodRouter, get_service};
use axum::{Json, http::StatusCode};
use cpa_server::management::Management;
use sap_core::api::ApiError;

#[derive(Clone)]
pub struct ManagementAuth {
    verifier: MethodRouter,
}

impl ManagementAuth {
    /// Share the running proxy's Management so bans, configuration reloads and
    /// trusted-proxy policy are identical to its existing management endpoints.
    pub fn new(management: Arc<Management>) -> Self {
        Self {
            verifier: get_service(cpa_server::management::router(management)),
        }
    }
}

pub(super) async fn guard(State(auth): State<ManagementAuth>, req: Request, next: Next) -> Response {
    // ponytail: the upstream guard is crate-private. A read-only scalar route is
    // our in-process verification seam until cpa-server exports its guard directly.
    // No socket, credentials listing, config serialization or upstream call occurs.
    let mut probe = Request::builder()
        .uri("/v0/management/debug")
        .body(Body::empty())
        .unwrap();
    *probe.headers_mut() = req.headers().clone();
    if let Some(peer) = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
    {
        probe.extensions_mut().insert(*peer);
    }
    let checked = auth.verifier.call(probe, ()).await;
    if checked.status().is_success() {
        return next.run(req).await;
    }
    let status = checked.status();
    let body = axum::body::to_bytes(checked.into_body(), 16 * 1024).await.ok();
    let message = body
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v.get("error").and_then(|v| v.as_str()).map(str::to_owned))
        .unwrap_or_else(|| "management API unavailable".into());
    (
        status,
        Json(ApiError {
            error: if status == StatusCode::UNAUTHORIZED {
                "unauthorized"
            } else {
                "forbidden"
            }
            .into(),
            message,
        }),
    )
        .into_response()
}

#[cfg(test)]
impl ManagementAuth {
    pub(super) fn test_only() -> Self {
        // HTTP tests isolate the Control contract and exercise the same guard
        // plumbing; upstream owns cryptographic and management-policy tests.
        Self {
            verifier: axum::routing::get(|headers: axum::http::HeaderMap| async move {
                if headers
                    .get("authorization")
                    .is_some_and(|h| h == "Bearer fake-management")
                {
                    StatusCode::OK.into_response()
                } else {
                    (
                        StatusCode::UNAUTHORIZED,
                        Json(serde_json::json!({"error":"invalid management key"})),
                    )
                        .into_response()
                }
            }),
        }
    }
}
