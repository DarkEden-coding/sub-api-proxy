//! Same-origin browser bridge. Only loopback clients and configured issuers may register.
use crate::{
    config::Config,
    oauth::Relay,
    proxy::{Body, full},
};
use http::{Request, Response, StatusCode};
use http_body_util::BodyExt;

fn reply(status: StatusCode, content_type: &str, body: impl Into<bytes::Bytes>) -> Response<Body> {
    Response::builder().status(status)
        .header("content-type", content_type)
        .header("cache-control", "no-store")
        .header("referrer-policy", "no-referrer")
        .header("x-frame-options", "DENY")
        .header("content-security-policy", "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'")
        .body(full(body)).unwrap()
}

fn error(status: StatusCode, message: &str) -> Response<Body> {
    reply(
        status,
        "application/json",
        serde_json::json!({"message":message}).to_string(),
    )
}

fn local_origin(request: &Request<Body>) -> Option<String> {
    if !request.extensions().get::<std::net::SocketAddr>()?.ip().is_loopback() {
        return None;
    }
    let host = request.headers().get("host")?.to_str().ok()?;
    let url = url::Url::parse(&format!("http://{host}")).ok()?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let local = url.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    local.then(|| url.origin().ascii_serialization())
}

fn issuer<'a>(config: &'a Config, origin: &str) -> Option<&'a str> {
    let url = url::Url::parse(origin).ok()?;
    if url.origin().ascii_serialization() != origin {
        return None;
    }
    config
        .servers
        .iter()
        .find(|server| {
            std::iter::once(&server.base_url)
                .chain(&server.oauth_origins)
                .any(|raw| url::Url::parse(raw).is_ok_and(|url| url.origin().ascii_serialization() == origin))
        })
        .map(|server| server.base_url.as_str())
}

pub(crate) async fn serve(
    config: std::sync::Arc<Config>,
    relay: std::sync::Arc<Relay>,
    request: Request<Body>,
) -> Response<Body> {
    let Some(origin) = local_origin(&request) else {
        return error(
            StatusCode::FORBIDDEN,
            "Open the sign-in helper on this device's loopback address.",
        );
    };
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    match (method.as_str(), path.as_str()) {
        ("GET", "/sap/oauth/start") => reply(
            StatusCode::OK,
            "text/html; charset=utf-8",
            include_str!("oauth_helper.html"),
        ),
        ("POST", "/sap/oauth/register") => {
            if request.headers().get("origin").and_then(|v| v.to_str().ok()) != Some(origin.as_str()) {
                return error(
                    StatusCode::FORBIDDEN,
                    "Sign-in registration must come from the local helper page.",
                );
            }
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Registration {
                server: String,
                url: String,
            }
            // Read at most 16 KiB without buffering an unbounded request.
            let mut body = request.into_body();
            let mut bytes = Vec::new();
            while let Some(frame) = body.frame().await {
                let Ok(frame) = frame else {
                    return error(StatusCode::BAD_REQUEST, "Invalid sign-in registration.");
                };
                if let Ok(data) = frame.into_data() {
                    if bytes.len() + data.len() > 16384 {
                        return error(StatusCode::PAYLOAD_TOO_LARGE, "Sign-in registration is too large.");
                    }
                    bytes.extend_from_slice(&data);
                }
            }
            let Ok(login) = serde_json::from_slice::<Registration>(&bytes) else {
                return error(StatusCode::BAD_REQUEST, "Invalid sign-in registration.");
            };
            if login.server == origin && relay.has_session(&login.url) {
                return reply(StatusCode::OK, "application/json", "{\"status\":\"ok\"}");
            }
            let Some(base) = issuer(&config, &login.server) else {
                return error(
                    StatusCode::FORBIDDEN,
                    "This dashboard is not configured in the local helper's router.yaml. Add its server or oauth_origins entry, then restart sap-router.",
                );
            };
            match relay.register_browser(&login.url, base) {
                Ok(()) => reply(StatusCode::OK, "application/json", "{\"status\":\"ok\"}"),
                Err(problem) => error(StatusCode::BAD_REQUEST, &problem.to_string()),
            }
        }
        _ => error(StatusCode::NOT_FOUND, "Unknown sign-in helper route."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Server;
    use std::sync::Arc;

    fn config() -> Arc<Config> {
        Arc::new(Config {
            servers: vec![Server {
                name: "local-server".into(),
                base_url: "http://127.0.0.1:8318".into(),
                oauth_origins: vec!["https://proxy.example.test".into()],
            }],
            ..Config::default()
        })
    }

    fn login() -> String {
        let mut url = url::Url::parse("https://auth.openai.com/oauth/authorize").unwrap();
        url.query_pairs_mut().extend_pairs([
            ("client_id", "app_EMoamEEZ73f0CkXaXp7hrann"),
            ("response_type", "code"),
            ("code_challenge_method", "S256"),
            ("code_challenge", "fake-challenge"),
            ("state", "fake-state"),
            ("redirect_uri", "http://localhost:1455/auth/callback"),
        ]);
        url.into()
    }

    fn request(server: &str, url: &str) -> Request<Body> {
        let mut request = Request::builder()
            .method("POST")
            .uri("/sap/oauth/register")
            .header("host", "127.0.0.1:8317")
            .header("origin", "http://127.0.0.1:8317")
            .body(full(serde_json::json!({"server":server,"url":url}).to_string()))
            .unwrap();
        request
            .extensions_mut()
            .insert("127.0.0.1:1234".parse::<std::net::SocketAddr>().unwrap());
        request
    }

    #[tokio::test]
    async fn helper_accepts_configured_dashboard_alias_without_a_management_key() {
        let relay = Arc::new(Relay::default());
        relay
            .register_at(
                serde_json::json!({"url":login(),"callback_relay":true})
                    .to_string()
                    .as_bytes(),
                "http://127.0.0.1:8318",
                Some("127.0.0.1:0".parse().unwrap()),
            )
            .unwrap();
        let response = serve(config(), relay, request("https://proxy.example.test", &login())).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["referrer-policy"], "no-referrer");
        assert_eq!(response.headers()["cache-control"], "no-store");
    }

    #[tokio::test]
    async fn helper_rejects_untrusted_origins_hosts_peers_and_provider_urls() {
        let mut cross_origin = request("https://proxy.example.test", &login());
        cross_origin
            .headers_mut()
            .insert("origin", "https://evil.example.test".parse().unwrap());
        let mut rebound = request("https://proxy.example.test", &login());
        rebound
            .headers_mut()
            .insert("host", "evil.example.test:8317".parse().unwrap());
        let mut remote = request("https://proxy.example.test", &login());
        remote
            .extensions_mut()
            .insert("192.0.2.1:1234".parse::<std::net::SocketAddr>().unwrap());
        for request in [
            cross_origin,
            rebound,
            remote,
            request("https://evil.example.test", &login()),
        ] {
            assert_eq!(
                serve(config(), Arc::default(), request).await.status(),
                StatusCode::FORBIDDEN
            );
        }
        for url in [
            "https://evil.example.test/oauth/authorize".to_owned(),
            login().replace("localhost%3A1455", "evil.example.test%3A1455"),
            format!("{}&state=duplicate", login()),
            login().replace("app_EMoamEEZ73f0CkXaXp7hrann", "wrong-client"),
        ] {
            assert_eq!(
                serve(config(), Arc::default(), request("https://proxy.example.test", &url))
                    .await
                    .status(),
                StatusCode::BAD_REQUEST
            );
        }
    }

    #[test]
    fn dashboard_alias_is_exact_and_pinned_to_the_configured_transport() {
        assert_eq!(
            issuer(&config(), "https://proxy.example.test"),
            Some("http://127.0.0.1:8318")
        );
        for origin in [
            "https://proxy.example.test/",
            "https://proxy.example.test/path",
            "https://proxy.example.test.evil.test",
            "https://user@proxy.example.test",
        ] {
            assert_eq!(issuer(&config(), origin), None);
        }
    }
}
