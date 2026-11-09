//! Browser-local OAuth callbacks stay pinned to the server that issued their state.
use crate::proxy::{Body, full};
use http::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{net::TcpListener, task::JoinHandle};

type Sessions = Mutex<HashMap<String, (String, Instant)>>;
#[derive(Default)]
struct Lifecycle {
    task: Option<JoinHandle<()>>,
}
#[derive(Default)]
struct ListenerRelay {
    sessions: Arc<Sessions>,
    lifecycle: Arc<Mutex<Lifecycle>>,
    changed: Arc<tokio::sync::Notify>,
}
impl Drop for ListenerRelay {
    fn drop(&mut self) {
        if let Some(task) = self
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .task
            .take()
        {
            task.abort();
        }
    }
}
#[derive(Default)]
pub(crate) struct Relay {
    claude: ListenerRelay,
    codex: ListenerRelay,
}

struct Provider {
    port: u16,
    local_path: &'static str,
    server_path: &'static str,
}
const CLAUDE: Provider = Provider {
    port: 54545,
    local_path: "/callback",
    server_path: "/anthropic/callback",
};
const CODEX: Provider = Provider {
    port: 1455,
    local_path: "/auth/callback",
    server_path: "/codex/callback",
};

fn validate_browser_url(raw: &str) -> anyhow::Result<(&'static Provider, String)> {
    let url = url::Url::parse(raw)?;
    anyhow::ensure!(
        url.scheme() == "https"
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
            && url.path() == "/oauth/authorize",
        "Unsupported sign-in URL"
    );
    let (provider, client) = match url.host_str() {
        Some("claude.ai") => (&CLAUDE, "9d1c250a-e61b-44d9-88ed-5944d1962f5e"),
        Some("auth.openai.com") => (&CODEX, "app_EMoamEEZ73f0CkXaXp7hrann"),
        _ => anyhow::bail!("Unsupported sign-in provider"),
    };
    let mut params = HashMap::new();
    for (key, value) in url.query_pairs() {
        anyhow::ensure!(
            params.insert(key.into_owned(), value.into_owned()).is_none(),
            "Duplicate sign-in parameters"
        );
    }
    let get = |key: &str| params.get(key).map(String::as_str).unwrap_or_default();
    anyhow::ensure!(
        get("client_id") == client
            && get("redirect_uri") == format!("http://localhost:{}{}", provider.port, provider.local_path)
            && get("response_type") == "code"
            && get("code_challenge_method") == "S256"
            && !get("code_challenge").is_empty()
            && !get("state").is_empty(),
        "Invalid sign-in parameters"
    );
    Ok((provider, get("state").to_owned()))
}

impl Relay {
    /// Explicit handoff from a remote dashboard. Accept only the registered provider
    /// clients and loopback redirects, never a browser-supplied callback destination.
    pub(crate) fn register_browser(&self, raw: &str, base: &str) -> anyhow::Result<()> {
        validate_browser_url(raw)?;
        self.register_at(
            serde_json::json!({"url":raw,"callback_relay":true})
                .to_string()
                .as_bytes(),
            base,
            None,
        )?;
        Ok(())
    }

    pub(crate) fn has_session(&self, raw: &str) -> bool {
        let Ok((provider, state)) = validate_browser_url(raw) else {
            return false;
        };
        let relay = if provider.port == CLAUDE.port {
            &self.claude
        } else {
            &self.codex
        };
        relay
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&state)
            .is_some_and(|(_, expires)| *expires > Instant::now())
    }

    pub(crate) async fn register(&self, bytes: &[u8], base: &str) -> anyhow::Result<()> {
        self.register_at(bytes, base, None).map(|_| ())
    }

    pub(super) fn register_at(
        &self,
        bytes: &[u8],
        base: &str,
        address: Option<std::net::SocketAddr>,
    ) -> anyhow::Result<Option<std::net::SocketAddr>> {
        let target = url::Url::parse(base)?;
        let value: serde_json::Value = serde_json::from_slice(bytes)?;
        if value["callback_relay"] != true
            && matches!(target.host_str(), Some("localhost" | "127.0.0.1" | "[::1]" | "::1"))
        {
            return Ok(None);
        }
        let Some(raw) = value["url"].as_str() else {
            return Ok(None);
        };
        let url = url::Url::parse(raw)?;
        if url.scheme() != "https" {
            return Ok(None);
        }
        let (relay, provider) = match (url.host_str(), url.path()) {
            (Some("claude.ai"), "/oauth/authorize") => (&self.claude, &CLAUDE),
            (Some("auth.openai.com"), "/oauth/authorize") => (&self.codex, &CODEX),
            _ => return Ok(None),
        };
        let params: HashMap<_, _> = url.query_pairs().into_owned().collect();
        if params.get("redirect_uri") != Some(&format!("http://localhost:{}{}", provider.port, provider.local_path)) {
            return Ok(None);
        }
        let state = params
            .get("state")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Missing OAuth state"))?;
        relay.register_at(
            state,
            base,
            provider,
            address.unwrap_or_else(|| ([127, 0, 0, 1], provider.port).into()),
        )
    }
}

impl ListenerRelay {
    fn register_at(
        &self,
        state: &str,
        base: &str,
        provider: &'static Provider,
        address: std::net::SocketAddr,
    ) -> anyhow::Result<Option<std::net::SocketAddr>> {
        // Registration and listener retirement take these locks in the same order.
        let mut lifecycle = self.lifecycle.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut sessions = self.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        sessions.retain(|_, (_, expires)| *expires > Instant::now());
        let target = format!("{}{}", base.trim_end_matches('/'), provider.server_path);
        if let Some((existing, _)) = sessions.get(state) {
            anyhow::ensure!(
                existing == &target,
                "OAuth state is already registered to another server"
            );
            return Ok(None);
        }
        anyhow::ensure!(sessions.len() < 128, "Too many pending OAuth callbacks");
        let mut bound = None;
        if lifecycle.task.is_none() {
            let socket = std::net::TcpListener::bind(address).map_err(|_| {
                anyhow::anyhow!(
                    "Cannot receive OAuth callback: localhost:{} is already in use",
                    provider.port
                )
            })?;
            socket.set_nonblocking(true)?;
            bound = Some(socket.local_addr()?);
            let listener = TcpListener::from_std(socket)?;
            let sessions = self.sessions.clone();
            let owner_lifecycle = Arc::downgrade(&self.lifecycle);
            let changed = self.changed.clone();
            let task = tokio::spawn(async move {
                let mut connections = tokio::task::JoinSet::new();
                let mut listener = Some(listener);
                loop {
                    tokio::select! {
                        _ = changed.notified() => {},
                        _ = tokio::time::sleep(Duration::from_secs(1)) => {},
                        accepted = listener.as_ref().unwrap().accept() => {
                            let Ok((stream, _)) = accepted else { break; };
                            let sessions = sessions.clone();
                            let changed = changed.clone();
                            connections.spawn(async move {
                                let service = hyper::service::service_fn(move |request: Request<hyper::body::Incoming>| {
                                    let response = callback(&sessions, request.method(), request.uri(), provider.local_path);
                                    changed.notify_one();
                                    async { Ok::<_, std::convert::Infallible>(response) }
                                });
                                let _ = tokio::time::timeout(Duration::from_secs(10), hyper::server::conn::http1::Builder::new()
                                    .serve_connection(TokioIo::new(stream), service)).await;
                            });
                        },
                        _ = connections.join_next(), if !connections.is_empty() => {},
                    }
                    let Some(owner) = owner_lifecycle.upgrade() else {
                        break;
                    };
                    let mut owner = owner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    let mut pending = sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    pending.retain(|_, (_, expires)| *expires > Instant::now());
                    if pending.is_empty() {
                        // Close the socket before publishing that a new listener may start.
                        drop(listener.take());
                        owner.task.take();
                        break;
                    }
                }
                while connections.join_next().await.is_some() {}
            });
            lifecycle.task = Some(task);
        }
        sessions.insert(state.to_owned(), (target, Instant::now() + Duration::from_secs(300)));
        Ok(bound)
    }
}
fn callback(sessions: &Sessions, method: &http::Method, uri: &http::Uri, path: &str) -> Response<Body> {
    let state = url::form_urlencoded::parse(uri.query().unwrap_or_default().as_bytes())
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.into_owned());
    let mut sessions = sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    sessions.retain(|_, (_, expires)| *expires > Instant::now());
    if method == http::Method::GET
        && uri.path() == path
        && let Some((target, _)) = state.and_then(|state| sessions.remove(&state))
    {
        return Response::builder()
            .status(StatusCode::FOUND)
            .header("location", format!("{target}?{}", uri.query().unwrap_or_default()))
            .header("cache-control", "no-store")
            .header("referrer-policy", "no-referrer")
            .body(full("Completing sign-in on your proxy server."))
            .unwrap();
    }
    Response::builder()
        .status(StatusCode::BAD_REQUEST)
        .header("cache-control", "no-store")
        .body(full(
            "Unknown or expired OAuth callback. Start sign-in from the local router dashboard.",
        ))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn loopback_socket_routes_concurrent_sessions_and_releases_port() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let relay = Relay::default();
        let login = |state: &str| {
            serde_json::json!({"url": format!("https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A54545%2Fcallback&state={state}")}).to_string()
        };
        let address = relay
            .register_at(
                login("first").as_bytes(),
                "http://100.64.0.1:8318",
                Some("127.0.0.1:0".parse().unwrap()),
            )
            .unwrap()
            .unwrap();
        relay
            .register_at(login("second").as_bytes(), "http://100.64.0.2:8318", Some(address))
            .unwrap();
        async fn get(address: std::net::SocketAddr, state: &str) -> String {
            let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
            stream.write_all(format!("GET /callback?code=test%2Bcode&state={state} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
            let mut response = String::new();
            tokio::time::timeout(Duration::from_secs(2), stream.read_to_string(&mut response))
                .await
                .unwrap()
                .unwrap();
            response
        }
        let first = get(address, "first").await;
        assert!(first.contains("http://100.64.0.1:8318/anthropic/callback?code=test%2Bcode&state=first"));
        assert!(std::net::TcpListener::bind(address).is_err());
        let second = get(address, "second").await;
        assert!(second.contains("http://100.64.0.2:8318/anthropic/callback?code=test%2Bcode&state=second"));
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(listener) = std::net::TcpListener::bind(address) {
                    drop(listener);
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        // Registration after retirement creates another listener, and expiry closes it.
        relay
            .register_at(login("expired").as_bytes(), "http://100.64.0.1:8318", Some(address))
            .unwrap();
        relay.claude.sessions.lock().unwrap().get_mut("expired").unwrap().1 = Instant::now() - Duration::from_secs(1);
        relay.claude.changed.notify_one();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(listener) = std::net::TcpListener::bind(address) {
                    drop(listener);
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn codex_and_claude_use_separate_listeners_and_callback_paths() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let relay = Relay::default();
        for (host, provider, base) in [
            ("claude.ai", &CLAUDE, "http://100.64.0.1:8318"),
            ("auth.openai.com", &CODEX, "http://100.64.0.2:8318"),
        ] {
            let mut url = url::Url::parse(&format!("https://{host}/oauth/authorize")).unwrap();
            url.query_pairs_mut()
                .append_pair(
                    "redirect_uri",
                    &format!("http://localhost:{}{}", provider.port, provider.local_path),
                )
                .append_pair("state", "same-state");
            let login = serde_json::json!({"url": url.as_str()}).to_string();
            let address = relay
                .register_at(login.as_bytes(), base, Some("127.0.0.1:0".parse().unwrap()))
                .unwrap()
                .unwrap();
            let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
            stream.write_all(format!("GET {}?code=test%2Bcode&state=same-state HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n", provider.local_path).as_bytes()).await.unwrap();
            let mut response = String::new();
            tokio::time::timeout(Duration::from_secs(2), stream.read_to_string(&mut response))
                .await
                .unwrap()
                .unwrap();
            assert!(response.contains(&format!(
                "{base}{}?code=test%2Bcode&state=same-state",
                provider.server_path
            )));
        }
    }

    #[test]
    fn post_does_not_consume_a_browser_callback() {
        let sessions = Mutex::new(HashMap::from([(
            "pending".into(),
            (
                "http://100.64.0.1:8318/codex/callback".into(),
                Instant::now() + Duration::from_secs(60),
            ),
        )]));
        let uri = "/auth/callback?state=pending&code=test".parse().unwrap();
        assert_eq!(
            callback(&sessions, &http::Method::POST, &uri, "/auth/callback").status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            callback(&sessions, &http::Method::GET, &uri, "/auth/callback").status(),
            StatusCode::FOUND
        );
    }

    #[test]
    fn callback_is_pinned_and_preserves_query_without_replay() {
        let sessions = Mutex::new(HashMap::from([
            (
                "first".into(),
                (
                    "http://100.64.0.1:8318/anthropic/callback".into(),
                    Instant::now() + Duration::from_secs(60),
                ),
            ),
            (
                "second".into(),
                (
                    "http://100.64.0.2:8318/anthropic/callback".into(),
                    Instant::now() + Duration::from_secs(60),
                ),
            ),
        ]));
        let uri = "/callback?code=secret%2Bcode&state=first&extra=%26".parse().unwrap();
        let response = callback(&sessions, &http::Method::GET, &uri, "/callback");
        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(
            response.headers()["location"],
            "http://100.64.0.1:8318/anthropic/callback?code=secret%2Bcode&state=first&extra=%26"
        );
        assert_eq!(response.headers()["referrer-policy"], "no-referrer");
        assert_eq!(
            callback(&sessions, &http::Method::GET, &uri, "/callback").status(),
            StatusCode::BAD_REQUEST
        );
        assert!(sessions.lock().unwrap().contains_key("second"));
    }

    #[test]
    fn unknown_expired_and_wrong_path_callbacks_do_not_redirect() {
        let sessions = Mutex::new(HashMap::from([
            (
                "expired".into(),
                (
                    "http://100.64.0.1/anthropic/callback".into(),
                    Instant::now() - Duration::from_secs(1),
                ),
            ),
            (
                "valid".into(),
                (
                    "http://100.64.0.1/anthropic/callback".into(),
                    Instant::now() + Duration::from_secs(60),
                ),
            ),
        ]));
        for path in [
            "/callback?state=expired",
            "/callback?state=unknown",
            "/other?state=valid",
            "/callback",
        ] {
            assert_eq!(
                callback(&sessions, &http::Method::GET, &path.parse().unwrap(), "/callback").status(),
                StatusCode::BAD_REQUEST
            );
        }
        assert!(sessions.lock().unwrap().contains_key("valid"));
    }
}
