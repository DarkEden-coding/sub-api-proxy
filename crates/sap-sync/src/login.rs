//! Sign in on a peer: start its login over the authenticated peer channel, then hand it
//! the provider's OAuth callback, which reaches the node that answered the dashboard.
use crate::{Peer, agent::HttpClient};
use bytes::Bytes;
use http::{Method, Request, Uri};
use http_body_util::Full;
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::TokioExecutor,
};
use sap_core::{
    api::{LoginRequest, LoginStart},
    ids::NodeId,
};
use std::time::Duration;

pub(crate) const LOGIN_PATH: &str = "/sap/sync/login";

fn client() -> HttpClient {
    let mut connector = HttpConnector::new();
    connector.set_connect_timeout(Some(Duration::from_secs(3)));
    Client::builder(TokioExecutor::new()).build(connector)
}

/// Starts a login on `peer`, which keeps its PKCE verifier and waits for the callback.
pub async fn start_login(local: &NodeId, peer: &Peer, login: &LoginRequest) -> Result<LoginStart, String> {
    let body = serde_json::to_vec(login).map_err(|_| "login encoding failed")?;
    let answer = crate::agent::request(&client(), local, peer, Method::POST, LOGIN_PATH, body).await?;
    serde_json::from_slice(&answer).map_err(|_| "invalid peer login answer".into())
}

/// Delivers an OAuth callback (`/anthropic/callback?code=…&state=…`) to the peer that
/// started its login. Peers serve provider callbacks on their public routes, so this is
/// the request the browser would have made.
pub async fn forward_callback(peer: &Peer, path_and_query: &str) -> Result<(), String> {
    let uri: Uri = format!("{}{path_and_query}", peer.base_url)
        .parse()
        .map_err(|_| "invalid callback URL")?;
    let request = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Full::new(Bytes::new()))
        .map_err(|_| "invalid callback request")?;
    let response = tokio::time::timeout(Duration::from_secs(15), client().request(request))
        .await
        .map_err(|_| "peer callback timed out")?
        .map_err(|_| "peer connection failed")?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("peer returned HTTP {}", response.status().as_u16()))
    }
}
