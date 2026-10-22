//! Subscription usage endpoints only. No inference, token refresh or arbitrary URLs.
use std::{collections::BTreeMap, time::Duration};

use cpa_core::credential::Credential;
use futures_util::StreamExt;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeError {
    Unsupported,
    Transport,
    Timeout,
    Status(u16),
    InvalidResponse,
}
impl std::error::Error for ProbeError {}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => f.write_str("Usage probe requires a subscription OAuth credential"),
            Self::Transport => f.write_str("Usage endpoint transport failed"),
            Self::Timeout => f.write_str("Usage endpoint timed out"),
            Self::Status(status) => write!(f, "Usage endpoint returned HTTP {status}"),
            Self::InvalidResponse => f.write_str("Usage endpoint returned an invalid response"),
        }
    }
}

pub(crate) async fn fetch(
    client: wreq::Client,
    base: &str,
    credential: &Credential,
    claude: bool,
) -> Result<BTreeMap<String, String>, ProbeError> {
    // Only executor-owned origins are accepted. Credential base_url and quota_probe
    // metadata must never redirect subscription tokens to another service.
    let base = url::Url::parse(base).map_err(|_| ProbeError::Unsupported)?;
    let first_party =
        base.scheme() == "https" && base.host_str() == Some(if claude { "api.anthropic.com" } else { "chatgpt.com" });
    // Existing executor constructors support private loopback mock upstreams.
    let loopback = base
        .host_str()
        .and_then(|s| s.parse::<std::net::IpAddr>().ok())
        .is_some_and(|ip| ip.is_loopback());
    if (!first_party && !loopback) || base.username() != "" || base.password().is_some() {
        return Err(ProbeError::Unsupported);
    }
    if credential.provider != if claude { "claude" } else { "codex" }
        || credential.attributes.get("api_key").is_some_and(|k| !k.is_empty())
    {
        return Err(ProbeError::Unsupported);
    }
    // ponytail: use the stored token and report auth failures; integrate the
    // runtime's prepare/publish path if manual probes need OAuth refresh as well.
    let token = credential
        .str("access_token")
        .filter(|s| !s.is_empty())
        .ok_or(ProbeError::Unsupported)?;
    let url = base
        .join(if claude {
            "/api/oauth/usage"
        } else {
            "/backend-api/wham/usage"
        })
        .map_err(|_| ProbeError::Unsupported)?;
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut request = client
            .get(url.as_str())
            .redirect(wreq::redirect::Policy::none())
            .bearer_auth(token);
        if claude {
            request = request.header("anthropic-beta", "oauth-2025-04-20");
        } else if let Some(account) = credential.str("account_id").filter(|s| !s.is_empty()) {
            request = request.header("chatgpt-account-id", account);
        }
        // These executor transports disable redirects. Never follow a usage redirect.
        let response = request.send().await.map_err(|_| ProbeError::Transport)?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(ProbeError::Status(status));
        }
        let mut stream = response.bytes_stream();
        let mut raw = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| ProbeError::Transport)?;
            if raw.len() + chunk.len() > 1024 * 1024 {
                return Err(ProbeError::InvalidResponse);
            }
            raw.extend_from_slice(&chunk);
        }
        signals(&raw, claude)
    })
    .await
    .map_err(|_| ProbeError::Timeout)?
}

fn signals(raw: &[u8], claude: bool) -> Result<BTreeMap<String, String>, ProbeError> {
    let value: Value = serde_json::from_slice(raw).map_err(|_| ProbeError::InvalidResponse)?;
    let mut out = BTreeMap::new();
    if claude {
        for (key, name) in [("five_hour", "5h"), ("seven_day", "7d")] {
            let window = &value[key];
            if let Some(used) = window["utilization"]
                .as_f64()
                .filter(|n| n.is_finite() && (0.0..=100.0).contains(n))
            {
                let prefix = format!("anthropic-ratelimit-unified-{name}");
                out.insert(format!("{prefix}-utilization"), (used / 100.0).to_string());
                out.insert(
                    format!("{prefix}-status"),
                    if used >= 100.0 { "rejected" } else { "allowed" }.into(),
                );
                if let Some(reset) = window["resets_at"].as_str() {
                    out.insert(format!("{prefix}-reset"), reset.into());
                }
            }
        }
        if out.values().any(|v| v == "rejected") {
            out.insert("anthropic-ratelimit-unified-status".into(), "rejected".into());
        } else if out.contains_key("anthropic-ratelimit-unified-5h-utilization")
            && out.contains_key("anthropic-ratelimit-unified-7d-utilization")
        {
            out.insert("anthropic-ratelimit-unified-status".into(), "allowed".into());
        }
    } else {
        for (key, name) in [("primary_window", "primary"), ("secondary_window", "secondary")] {
            let window = &value["rate_limit"][key];
            if let Some(used) = window["used_percent"]
                .as_f64()
                .filter(|n| n.is_finite() && (0.0..=100.0).contains(n))
            {
                let prefix = format!("x-codex-{name}");
                out.insert(format!("{prefix}-used-percent"), used.to_string());
                if let Some(seconds) = window["limit_window_seconds"].as_u64().filter(|n| *n % 60 == 0) {
                    out.insert(format!("{prefix}-window-minutes"), (seconds / 60).to_string());
                }
                if let Some(reset) = window["reset_at"].as_i64() {
                    out.insert(format!("{prefix}-reset-at"), reset.to_string());
                }
                if let Some(seconds) = window["reset_after_seconds"].as_i64().filter(|n| *n >= 0) {
                    out.insert(format!("{prefix}-reset-after-seconds"), seconds.to_string());
                }
            }
        }
        for (key, signal) in [
            ("allowed", "x-codex-allowed"),
            ("limit_reached", "x-codex-limit-reached"),
        ] {
            if let Some(v) = value["rate_limit"][key].as_bool() {
                out.insert(signal.into(), v.to_string());
            }
        }
        if let Some(plan) = value["plan_type"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
        {
            // Provider-controlled strings must not reach errors or logs.
            out.insert("x-codex-plan-type".into(), plan.into());
        }
    }
    if !out
        .keys()
        .any(|k| k.ends_with("-utilization") || k.ends_with("-used-percent"))
    {
        return Err(ProbeError::InvalidResponse);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, routing::get};
    use std::{future::IntoFuture, sync::Arc};

    fn credential() -> Credential {
        let mut credential = Credential::relay_session("test");
        credential.provider = "codex".into();
        credential
            .metadata
            .insert("access_token".into(), "private-test-token".into());
        credential
            .metadata
            .insert("account_id".into(), "private-test-account".into());
        credential
    }

    #[tokio::test]
    async fn usage_probe_codex_wire_redirects_and_errors_are_bounded() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let app = Router::new().route("/backend-api/wham/usage", get({
            let calls = calls.clone();
            move |headers: http::HeaderMap| {
                let attempt = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async move {
                    assert_eq!(headers["authorization"], "Bearer private-test-token");
                    assert_eq!(headers["chatgpt-account-id"], "private-test-account");
                    if attempt == 0 {
                        (http::StatusCode::OK, [("location", "/must-not-follow")],
                         r#"{"plan_type":"plus","rate_limit":{"allowed":false,"limit_reached":true,"primary_window":{"used_percent":100,"limit_window_seconds":18000,"reset_at":1900000000},"secondary_window":{"used_percent":20,"limit_window_seconds":604800}}}"#)
                    } else {
                        (http::StatusCode::FOUND, [("location", "/must-not-follow")], "private-test-token")
                    }
                }
            }
        })).route("/must-not-follow", get(|| async { (http::StatusCode::INTERNAL_SERVER_ERROR, "redirect followed") }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(axum::serve(listener, app).into_future());
        let client = crate::proxy::default_client();
        let result = fetch(client.clone(), &base, &credential(), false).await.unwrap();
        assert_eq!(result["x-codex-primary-window-minutes"], "300");
        assert_eq!(result["x-codex-secondary-window-minutes"], "10080");
        assert_eq!(result["x-codex-limit-reached"], "true");
        assert_eq!(result["x-codex-primary-reset-at"], "1900000000");
        assert_eq!(
            fetch(client.clone(), &base, &credential(), false).await,
            Err(ProbeError::Status(302))
        );
        assert_eq!(
            fetch(client, "https://untrusted.invalid", &credential(), false).await,
            Err(ProbeError::Unsupported)
        );
        assert_eq!(
            fetch(crate::proxy::default_client(), &base, &credential(), true).await,
            Err(ProbeError::Unsupported)
        );
        server.abort();
    }

    #[tokio::test]
    async fn usage_probe_timeout_includes_response_body() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0; 4096];
            assert!(socket.read(&mut buf).await.unwrap() > 0);
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 999\r\n\r\n{")
                .await
                .unwrap();
            std::future::pending::<()>().await;
        });
        assert_eq!(
            fetch(crate::proxy::default_client(), &base, &credential(), false).await,
            Err(ProbeError::Timeout)
        );
        server.abort();
    }

    #[test]
    fn usage_probe_partial_claude_response_does_not_clear_unseen_exhaustion() {
        let result = signals(br#"{"five_hour":{"utilization":5}}"#, true).unwrap();
        assert!(!result.contains_key("anthropic-ratelimit-unified-status"));
        assert_eq!(
            signals(br#"{"rate_limit":{"allowed":true}}"#, false),
            Err(ProbeError::InvalidResponse)
        );
        assert_eq!(
            signals(br#"{"five_hour":{"utilization":101}}"#, true),
            Err(ProbeError::InvalidResponse)
        );
    }
}
