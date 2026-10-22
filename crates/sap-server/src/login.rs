use crate::views::internal;
use crate::{
    control::{ControlError, ControlResult},
    node::Node,
};
use sap_core::api::{LoginRequest, LoginStart};

impl Node {
    pub(crate) async fn login(&self, request: LoginRequest) -> ControlResult<LoginStart> {
        use tower::ServiceExt;
        let (management, password) = self
            .management
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(|(m, p)| m.upgrade().map(|m| (m, p.clone())))
            .ok_or_else(|| ControlError::internal("Management login is not attached"))?;
        if let Some(id) = &request.account
            && !self
                .store
                .accounts()
                .map_err(internal)?
                .iter()
                .any(|a| &a.id == id && a.provider == request.provider)
        {
            return Err(ControlError::invalid("Account does not match login provider"));
        }
        let mut req = http::Request::builder()
            .uri(format!(
                "/v8/management/oauth/auth-url?provider={}&is_webui=true",
                request.provider
            ))
            .header("authorization", format!("Bearer {password}"))
            .body(axum::body::Body::empty())
            .map_err(internal)?;
        req.extensions_mut().insert(axum::extract::ConnectInfo(
            "127.0.0.1:1".parse::<std::net::SocketAddr>().unwrap(),
        ));
        let response = cpa_server::management::router(management)
            .oneshot(req)
            .await
            .map_err(internal)?;
        if !response.status().is_success() {
            return Err(ControlError::internal("Management login could not start"));
        }
        let body = axum::body::to_bytes(response.into_body(), 16384)
            .await
            .map_err(internal)?;
        let value: serde_json::Value = serde_json::from_slice(&body).map_err(internal)?;
        Ok(LoginStart {
            url: value["url"]
                .as_str()
                .ok_or_else(|| ControlError::internal("Missing login URL"))?
                .into(),
            login_id: value["state"]
                .as_str()
                .ok_or_else(|| ControlError::internal("Missing login state"))?
                .into(),
        })
    }
}
