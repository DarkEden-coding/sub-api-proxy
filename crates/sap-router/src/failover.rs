use crate::proxy::{Body, full};
use http::{Method, Response, StatusCode};

/// The 503 a client sees when its request cannot be completed. The path is logged
/// without its query, which may carry credentials.
pub fn overloaded(method: &Method, path: &str, reason: &str) -> Response<Body> {
    tracing::warn!("{method} {path}: 503 to client: {reason}");
    Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header("content-type", "application/json")
        .body(full(r#"{"type":"error","error":{"type":"overloaded_error","message":"Server unavailable; retry through the local router"}}"#))
        .unwrap()
}
