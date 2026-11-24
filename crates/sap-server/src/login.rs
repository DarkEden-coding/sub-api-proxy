use crate::views::internal;
use crate::{
    control::{ControlError, ControlResult},
    node::Node,
};
use sap_core::{
    api::{LoginRequest, LoginStart},
    ids::NodeId,
};
use std::time::{Duration, Instant};

/// How long a login started on a peer waits for its callback here.
const DELEGATED_LOGIN_TTL: Duration = Duration::from_secs(600);

impl Node {
    pub(crate) async fn login(&self, mut request: LoginRequest) -> ControlResult<LoginStart> {
        if let Some(node) = request.node.take()
            && node != self.config.node
        {
            return self.login_on_peer(node, request).await;
        }
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
                "/v8/management/oauth/auth-url?provider={}&is_webui={}",
                request.provider, !request.callback_relay
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
            callback_relay: request.callback_relay,
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

impl Node {
    /// Starts the login on `node`, which keeps the PKCE verifier. The browser's callback
    /// still reaches this node (the dashboard's router pinned the state here), so the
    /// state is remembered and [`Self::forward_oauth_callback`] hands it on.
    async fn login_on_peer(&self, node: NodeId, request: LoginRequest) -> ControlResult<LoginStart> {
        let peer = self
            .peers
            .iter()
            .find(|peer| peer.node == node)
            .ok_or_else(|| ControlError::invalid(format!("Unknown server {node}")))?;
        let start = sap_sync::start_login(&self.config.node, peer, &request)
            .await
            .map_err(|error| ControlError::upstream(format!("Could not start sign-in on {node}: {error}")))?;
        let mut pending = self
            .delegated_logins
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        pending.retain(|_, (_, started)| now.duration_since(*started) < DELEGATED_LOGIN_TTL);
        pending.insert(start.login_id.clone(), (node, now));
        Ok(start)
    }

    /// Hands an OAuth callback to the peer this node started its login on, if any.
    pub(crate) async fn forward_oauth_callback(&self, path_and_query: &str) {
        let Some(state) = callback_state(path_and_query) else {
            return;
        };
        let node = {
            let mut pending = self
                .delegated_logins
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            pending.retain(|_, (_, started)| started.elapsed() < DELEGATED_LOGIN_TTL);
            pending.remove(state).map(|(node, _)| node)
        };
        let Some(peer) = node.and_then(|node| self.peers.iter().find(|peer| peer.node == node)) else {
            return;
        };
        match sap_sync::forward_callback(peer, path_and_query).await {
            Ok(()) => tracing::info!(node = %peer.node, "forwarded a sign-in callback to its server"),
            Err(error) => tracing::warn!(node = %peer.node, %error, "forwarding a sign-in callback failed"),
        }
    }
}

/// The `state` parameter of a callback. Login states are hex, so no decoding is needed.
fn callback_state(path_and_query: &str) -> Option<&str> {
    let (_, query) = path_and_query.split_once('?')?;
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix("state="))
        .filter(|state| !state.is_empty())
}

#[cfg(test)]
mod tests {
    #[test]
    fn callback_state_reads_the_state_parameter() {
        assert_eq!(
            super::callback_state("/anthropic/callback?code=a%2Bb&state=abc123"),
            Some("abc123")
        );
        assert_eq!(super::callback_state("/codex/callback?code=x"), None);
        assert_eq!(super::callback_state("/codex/callback"), None);
    }
}
