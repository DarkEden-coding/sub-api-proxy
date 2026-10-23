use cpa_core::config::Config;
use cpa_server::{
    Runtime,
    sap_hooks::{AttemptResult, Observation, SapCandidate, SapHooks, SapPick},
};
use sap_core::{
    ids::{AccountId, Provider},
    thread::ThreadUsage,
};
use sap_server::{Node, control::Control};
use std::future::IntoFuture;
use std::{path::PathBuf, sync::Arc};

struct Fixture {
    dir: PathBuf,
    rt: Arc<Runtime>,
    node: Arc<Node>,
    candidates: Vec<SapCandidate>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
impl Fixture {
    fn new(provider: Provider) -> Self {
        Self::new_at(provider, "http://127.0.0.1:9")
    }
    fn new_at(provider: Provider, upstream: &str) -> Self {
        Self::with_auth(provider, upstream, false)
    }
    fn with_auth(provider: Provider, upstream: &str, oauth: bool) -> Self {
        Self::build(provider, upstream, oauth, false)
    }
    /// Codex credentials that talk to `upstream` over WebSocket, as signed-in Codex does.
    fn new_websocket(upstream: &str) -> Self {
        Self::build(Provider::Codex, upstream, false, true)
    }
    fn build(provider: Provider, upstream: &str, oauth: bool, websockets: bool) -> Self {
        let ws = if websockets { "    websockets: true\n" } else { "" };
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "sap-node-{}-{}-{}",
            std::process::id(),
            sap_core::Utc::now().timestamp_nanos_opt().unwrap(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(dir.join("auth")).unwrap();
        let mut config = Config::parse(&format!("{provider}-api-key:\n  - api-key: fake-a\n    base-url: {upstream}\n{ws}    models:\n      - name: test\n  - api-key: fake-b\n    base-url: {upstream}\n{ws}    models:\n      - name: test\n")).unwrap();
        config.auth_dir = dir.join("auth");
        let mut credentials = cpa_core::config::credentials::load(&config);
        for (i, c) in credentials.iter_mut().enumerate() {
            if oauth {
                c.attributes.remove("api_key");
                c.metadata
                    .insert("access_token".into(), serde_json::json!(format!("private-test-{i}")));
            }
            c.metadata.insert(
                if provider == Provider::Claude {
                    "account_uuid"
                } else {
                    "account_id"
                }
                .into(),
                serde_json::json!(format!("account-{i}")),
            );
        }
        let candidates = credentials
            .iter()
            .map(|c| SapCandidate {
                credential: c.id.clone(),
                provider: provider.to_string(),
                priority: 0,
            })
            .collect();
        let rt = Arc::new(cpa_server::testing::runtime(
            config,
            credentials,
            cpa_exec::Executors {
                claude: cpa_exec::claude::ClaudeExecutor::new(upstream).unwrap(),
                codex: cpa_exec::codex::CodexExecutor::default().with_alpha_base_url(upstream),
                devices: Default::default(),
                openai: Default::default(),
                google: Default::default(),
            },
        ));
        std::fs::write(
            dir.join("sap.yaml"),
            "node: local\nrole: primary\nstore_path: state.sqlite\n",
        )
        .unwrap();
        let node = sap_server::install(&rt, &dir.join("sap.yaml")).unwrap();
        Self {
            dir,
            rt,
            node,
            candidates,
        }
    }
    fn request(
        &self,
        session: &str,
        continuation: bool,
        child: bool,
        ws: bool,
    ) -> Arc<cpa_server::sap_hooks::SapRequest> {
        let mut headers = http::HeaderMap::new();
        let provider = if self.candidates[0].provider == "claude" {
            Provider::Claude
        } else {
            Provider::Codex
        };
        let path = if provider == Provider::Claude {
            headers.insert("x-claude-code-session-id", session.parse().unwrap());
            headers.insert(
                "x-claude-code-request-class",
                if child { "subagent" } else { "main" }.parse().unwrap(),
            );
            if child {
                headers.insert("x-claude-code-agent-id", "child".parse().unwrap());
            }
            "/v1/messages"
        } else {
            headers.insert("thread-id", session.parse().unwrap());
            headers.insert("session-id", session.parse().unwrap());
            "/backend-api/codex/responses"
        };
        if ws {
            headers.insert("upgrade", "websocket".parse().unwrap());
        }
        let body = if continuation {
            if provider == Provider::Claude {
                br#"{"model":"test","thread":{"type":"continue","previous_message_id":"msg_a"}}"#.as_slice()
            } else {
                br#"{"model":"test","previous_response_id":"resp_a"}"#.as_slice()
            }
        } else {
            br#"{ "model":"test", "messages":[], "input":[] }"#.as_slice()
        };
        self.node.identify(path, &headers, body).unwrap()
    }
    fn pick(&self, req: &cpa_server::sap_hooks::SapRequest) -> String {
        match self.node.select(req, &self.candidates) {
            SapPick::Credential(c) => c,
            _ => panic!("expected credential"),
        }
    }
}

#[tokio::test]
async fn node_affinity_children_usage_and_reopen() {
    let f = Fixture::new(Provider::Claude);
    let req = f.request("parent", false, false, false);
    let credential = f.pick(&req);
    f.node
        .on_observation(&req, &credential, Observation::ResponseId("msg_a".into()));
    for tokens in [10, 10, 15] {
        f.node.on_observation(
            &req,
            &credential,
            Observation::Usage(ThreadUsage {
                requests: 1,
                input_tokens: tokens,
                output_tokens: 2,
                ..Default::default()
            }),
        );
    }
    // Streams repeat the cumulative report on every event: no new op-log rows.
    let log = sap_store::Store::open(
        f.dir.join("state.sqlite"),
        sap_core::ids::NodeId("local".into()),
        Default::default(),
    )
    .unwrap();
    let before = sap_core::sync::ChangeLog::cursor(&log).unwrap();
    for _ in 0..3 {
        f.node.on_observation(
            &req,
            &credential,
            Observation::Usage(ThreadUsage {
                requests: 1,
                input_tokens: 15,
                output_tokens: 2,
                ..Default::default()
            }),
        );
    }
    assert_eq!(sap_core::sync::ChangeLog::cursor(&log).unwrap(), before);
    assert_eq!(f.pick(&f.request("parent", true, false, false)), credential);
    assert_eq!(f.pick(&f.request("parent", false, true, false)), credential);
    let parent = f
        .node
        .threads(false)
        .into_iter()
        .find(|t| t.key.id == "parent")
        .unwrap();
    assert_eq!(parent.requests, 1);
    f.node.on_outcome(
        &req,
        &credential,
        &AttemptResult {
            status: Some(200),
            error: None,
        },
    );
    assert_eq!(f.node.flow().stats.requests_3h, 1);
    let restored = sap_server::install(&f.rt, &f.dir.join("sap.yaml")).unwrap();
    assert_eq!(
        restored
            .threads(false)
            .into_iter()
            .find(|t| t.key.id == "parent")
            .unwrap()
            .account,
        parent.account
    );
    assert_eq!(restored.accounts().len(), 2);
}

#[tokio::test]
async fn node_exhaustion_requires_full_history_until_resent() {
    let f = Fixture::new(Provider::Claude);
    let req = f.request("parent", false, false, false);
    let first = f.pick(&req);
    f.node.on_outcome(
        &req,
        &first,
        &AttemptResult {
            status: Some(429),
            error: Some(cpa_server::sap_hooks::AttemptError::RateLimited),
        },
    );
    f.node.on_observation(
        &req,
        &first,
        Observation::Quota {
            observed_at: std::time::SystemTime::now(),
            signals: [
                ("Anthropic-Ratelimit-Unified-Status".into(), "rejected".into()),
                (
                    "Anthropic-Ratelimit-Unified-Representative-Claim".into(),
                    "five_hour".into(),
                ),
                ("Anthropic-Ratelimit-Unified-5h-Utilization".into(), "1".into()),
            ]
            .into(),
        },
    );
    // Quota arriving before a later generic 429 must also retain exhaustion.
    f.node.on_outcome(
        &req,
        &first,
        &AttemptResult {
            status: Some(429),
            error: Some(cpa_server::sap_hooks::AttemptError::RateLimited),
        },
    );
    for _ in 0..2 {
        match f.node.select(&f.request("parent", true, false, false), &f.candidates) {
            SapPick::Reply { status, body, .. } => {
                assert_eq!(status, 400);
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&body).unwrap()["error"]["type"],
                    "invalid_request_error"
                );
            }
            _ => panic!("expected forced resend"),
        }
    }
    let restored = sap_server::install(&f.rt, &f.dir.join("sap.yaml")).unwrap();
    assert!(matches!(
        restored.select(&f.request("parent", true, false, false), &f.candidates),
        SapPick::Reply { status: 400, .. }
    ));
    let second = f.pick(&f.request("parent", false, false, false));
    assert_ne!(second, first);
    f.node.on_observation(
        &f.request("parent", false, false, false),
        &second,
        Observation::ResponseId("msg_a".into()),
    );
    assert_eq!(f.pick(&f.request("parent", true, false, false)), second);
    assert_eq!(
        f.node
            .accounts()
            .iter()
            .filter(|a| a.state == sap_core::api::AccountState::Exhausted)
            .count(),
        1
    );
}

#[tokio::test]
async fn node_codex_confirmed_quota_moves_within_full_history_request() {
    for websocket in [false, true] {
        let f = Fixture::new(Provider::Codex);
        let req = f.request("thread", false, false, websocket);
        let first = f.pick(&req);
        f.node.on_observation(
            &req,
            &first,
            Observation::Quota {
                observed_at: std::time::SystemTime::now(),
                signals: [
                    ("X-Codex-Limit-Reached".into(), "true".into()),
                    ("X-Codex-Primary-Used-Percent".into(), "100".into()),
                    ("X-Codex-Primary-Window-Minutes".into(), "300".into()),
                    ("X-Codex-Primary-Reset-After-Seconds".into(), "3600".into()),
                ]
                .into(),
            },
        );
        f.node.on_outcome(
            &req,
            &first,
            &AttemptResult {
                status: Some(429),
                error: Some(cpa_server::sap_hooks::AttemptError::RateLimited),
            },
        );
        // Exhaustion moves immediately; a generic rate limit would keep/retry A.
        assert_ne!(f.pick(&req), first);
    }
}

#[tokio::test]
async fn node_codex_move_closes_websocket_and_http_rejects() {
    let f = Fixture::new(Provider::Codex);
    let req = f.request("thread", false, false, true);
    let first = f.pick(&req);
    f.node.on_outcome(
        &req,
        &first,
        &AttemptResult {
            status: Some(401),
            error: Some(cpa_server::sap_hooks::AttemptError::Unauthorized),
        },
    );
    assert!(matches!(
        f.node.select(&f.request("thread", true, false, true), &f.candidates),
        SapPick::CloseSocket
    ));
    assert!(matches!(
        f.node.select(&f.request("thread", true, false, false), &f.candidates),
        SapPick::Reply { status: 400, .. }
    ));
    assert_ne!(f.pick(&f.request("thread", false, false, true)), first);
}

#[tokio::test]
async fn node_manual_assignment_and_unknown_continuation() {
    let f = Fixture::new(Provider::Claude);
    assert!(matches!(
        f.node.select(&f.request("unknown", true, false, false), &f.candidates),
        SapPick::Reply { status: 400, .. }
    ));
    let req = f.request("thread", false, false, false);
    f.pick(&req);
    let key = req.identity.key.clone();
    f.node
        .assign_thread(&key, &AccountId("claude:account-1".into()))
        .unwrap();
    assert!(f.node.assign_thread(&key, &AccountId("codex:missing".into())).is_err());
    assert!(
        f.node
            .patch_account(
                &AccountId("claude:account-1".into()),
                sap_core::api::AccountPatch {
                    capacity_override: Some(Some(-1.0)),
                    ..Default::default()
                }
            )
            .is_err()
    );
}

#[tokio::test]
async fn node_two_loopback_nodes_replicate_assignment() {
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        let a = Fixture::new(Provider::Claude); let b = Fixture::new(Provider::Claude);
        let la = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let lb = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        for (f,id,peer,address) in [(&a,"a","b",lb.local_addr().unwrap()),(&b,"b","a",la.local_addr().unwrap())] {
            std::fs::write(f.dir.join("peer.secret"),b"loopback-test-secret").unwrap();
            std::fs::write(f.dir.join("sync.yaml"),format!("node: {id}\nrole: primary\nstore_path: sync.sqlite\nallow_loopback_peers: true\npeers:\n  - id: {peer}\n    url: http://{address}\n    secret_file: peer.secret\n")).unwrap();
        }
        let na = sap_server::install(&a.rt,&a.dir.join("sync.yaml")).unwrap();
        let nb = sap_server::install(&b.rt,&b.dir.join("sync.yaml")).unwrap();
        let ta = tokio::spawn(axum::serve(la,cpa_server::router(a.rt.clone()).into_make_service_with_connect_info::<std::net::SocketAddr>()).into_future());
        let tb = tokio::spawn(axum::serve(lb,cpa_server::router(b.rt.clone()).into_make_service_with_connect_info::<std::net::SocketAddr>()).into_future());
        struct Stop(tokio::task::JoinHandle<std::io::Result<()>>,tokio::task::JoinHandle<std::io::Result<()>>);
        impl Drop for Stop {fn drop(&mut self) {self.0.abort();self.1.abort();}}
        let _stop = Stop(ta,tb);
        let mut headers = http::HeaderMap::new(); headers.insert("x-claude-code-session-id","replicated".parse().unwrap()); headers.insert("x-claude-code-request-class","main".parse().unwrap());
        let hooks = a.rt.sap_hooks().unwrap();
        let req = hooks.identify("/v1/messages",&headers,br#"{"model":"test"}"#).unwrap();
        assert!(matches!(hooks.select(&req,&a.candidates),SapPick::Credential(_)));
        hooks.on_stream(&req, true);
        let expected = na.threads(false).into_iter().find(|t|t.key.id == "replicated").unwrap().account;
        loop { if nb.threads(false).iter().any(|t|t.key.id == "replicated" && t.account == expected) {break;} tokio::time::sleep(std::time::Duration::from_millis(20)).await; }
        loop {
            let remote = nb.nodes().into_iter().find(|n| n.id.0 == "a").unwrap();
            if remote.open_streams == Some(1) && remote.requests_per_minute == Some(1.0) {
                assert_eq!(remote.role, Some(sap_core::health::NodeRole::Primary));
                assert_eq!(remote.state, Some(sap_core::health::NodeState::Serving));
                assert!(remote.added_latency_p50_ms.unwrap() > 0.0);
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                assert!(remote.rss_bytes.unwrap() > 0);
                assert!(remote.telemetry_observed_at.is_some());
                assert!(remote.telemetry_received_at.is_some());
                assert!(!remote.telemetry_stale);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        hooks.on_stream(&req, false);
    }).await.unwrap();
}

#[tokio::test]
async fn node_http_hooks_pin_and_observe_loopback_upstream() {
    use tower::ServiceExt;
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let capture = seen.clone();
        let upstream = axum::Router::new().fallback(move |request: axum::extract::Request| {
            let capture = capture.clone();
            async move {
                let (parts,body) = request.into_parts();
                let bytes = axum::body::to_bytes(body,16384).await.unwrap();
                capture.lock().unwrap().push((parts.headers,bytes));
                ([("content-type","application/json")],r#"{"id":"msg_a","type":"message","role":"assistant","content":[],"usage":{"input_tokens":12,"output_tokens":2}}"#)
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let f = Fixture::new_at(Provider::Claude,&format!("http://{}",listener.local_addr().unwrap()));
        let task = tokio::spawn(axum::serve(listener,upstream).into_future());
        struct Stop(tokio::task::JoinHandle<std::io::Result<()>>); impl Drop for Stop {fn drop(&mut self) {self.0.abort();}}
        let _stop = Stop(task);
        let app = cpa_server::router(f.rt.clone());
        for body in [r#"{ "model":"test", "max_tokens":10, "messages":[] }"#,r#"{"model":"test","max_tokens":10,"messages":[],"thread":{"type":"continue","previous_message_id":"msg_a"}}"#] {
            let req = http::Request::builder().method("POST").uri("/v1/messages")
                .header("content-type","application/json").header("x-claude-code-session-id","http-thread")
                .header("x-claude-code-request-class","main").header("x-sap-client-host","loopback-test")
                .body(axum::body::Body::from(body)).unwrap();
            let response = app.clone().oneshot(req).await.unwrap();
            assert_eq!(response.status(),200);
            let _ = axum::body::to_bytes(response.into_body(),16384).await.unwrap();
        }
        let seen = seen.lock().unwrap(); assert_eq!(seen.len(),2);
        assert_eq!(seen[0].0["authorization"],seen[1].0["authorization"]);
        assert!(seen.iter().all(|(h,_)| !h.contains_key("x-sap-client-host")));
        assert_eq!(f.node.threads(false).iter().find(|t| t.key.id == "http-thread").unwrap().requests,2);
    }).await.unwrap();
}

#[tokio::test]
async fn node_temporary_failures_keep_assignment_and_deadline_rejects() {
    let f = Fixture::new(Provider::Claude);
    let req = f.request("temporary", false, false, false);
    let first = f.pick(&req);
    for (status, error) in [
        (429, cpa_server::sap_hooks::AttemptError::RateLimited),
        (529, cpa_server::sap_hooks::AttemptError::Overloaded),
    ] {
        f.node.on_outcome(
            &req,
            &first,
            &AttemptResult {
                status: Some(status),
                error: Some(error),
            },
        );
        assert_eq!(f.pick(&req), first);
    }
    std::fs::write(
        f.dir.join("deadline.yaml"),
        "node: local\nrole: primary\nstore_path: state.sqlite\nrouting:\n  retry_deadline_secs: 0\n",
    )
    .unwrap();
    let node = sap_server::install(&f.rt, &f.dir.join("deadline.yaml")).unwrap();
    node.on_outcome(
        &req,
        &first,
        &AttemptResult {
            status: Some(429),
            error: Some(cpa_server::sap_hooks::AttemptError::RateLimited),
        },
    );
    assert!(matches!(
        node.select(&req, &f.candidates),
        SapPick::Reply { status: 429, .. }
    ));
    assert_eq!(
        node.threads(false)
            .iter()
            .find(|t| t.key.id == "temporary")
            .unwrap()
            .account,
        f.node
            .threads(false)
            .iter()
            .find(|t| t.key.id == "temporary")
            .unwrap()
            .account
    );
}

#[tokio::test]
async fn node_successful_compaction_is_counted_once_and_capacity_is_bounded() {
    let f = Fixture::new(Provider::Claude);
    let mut headers = http::HeaderMap::new();
    headers.insert("x-claude-code-session-id", "compact".parse().unwrap());
    headers.insert("x-claude-code-request-class", "compaction".parse().unwrap());
    let req = f
        .node
        .identify("/v1/messages", &headers, br#"{"model":"test","messages":[]}"#)
        .unwrap();
    let credential = f.pick(&req);
    for _ in 0..2 {
        f.node.on_outcome(
            &req,
            &credential,
            &AttemptResult {
                status: Some(200),
                error: None,
            },
        );
    }
    assert_eq!(
        f.node
            .threads(false)
            .iter()
            .find(|t| t.key.id == "compact")
            .unwrap()
            .compactions,
        1
    );
    std::fs::write(
        f.dir.join("bounded.yaml"),
        "node: bounded\nrole: primary\nstore_path: bounded.sqlite\nregistry_max: 1\n",
    )
    .unwrap();
    let node = sap_server::install(&f.rt, &f.dir.join("bounded.yaml")).unwrap();
    let first = node.identify("/v1/messages", &headers, br#"{"model":"test"}"#).unwrap();
    assert!(matches!(node.select(&first, &f.candidates), SapPick::Credential(_)));
    headers.insert("x-claude-code-session-id", "next".parse().unwrap());
    let next = node.identify("/v1/messages", &headers, br#"{"model":"test"}"#).unwrap();
    assert!(matches!(
        node.select(&next, &f.candidates),
        SapPick::Reply { status: 429, .. }
    ));
    drop(first);
    drop(next);
    let next = node.identify("/v1/messages", &headers, br#"{"model":"test"}"#).unwrap();
    assert!(matches!(node.select(&next, &f.candidates), SapPick::Credential(_)));
}

/// One HTTP/1.1 request over a fresh loopback connection; returns status and body.
async fn http_get(addr: std::net::SocketAddr, path: &str, bearer: Option<&str>) -> (u16, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let auth = bearer
        .map(|b| format!("Authorization: Bearer {b}\r\n"))
        .unwrap_or_default();
    let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n{auth}Connection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let raw = String::from_utf8_lossy(&raw).into_owned();
    let status = raw.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let body = raw
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_owned())
        .unwrap_or_default();
    (status, body)
}

#[tokio::test]
async fn installed_node_serves_management_api_and_dashboard() {
    let f = Fixture::new(Provider::Claude);
    std::fs::write(f.dir.join("config.yaml"), "").unwrap();
    let options = cpa_server::management::Options {
        local_password: "local-test-key".into(),
        standalone: true,
        ..Default::default()
    };
    let management = cpa_server::management::Management::with_options(f.rt.clone(), f.dir.join("config.yaml"), options);
    f.node.attach_management(&management, "local-test-key".into());
    let app = cpa_server::app(f.rt.clone(), cpa_server::management::router(management.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .into_future(),
    );

    let (status, _) = http_get(addr, "/sap/api/flow", None).await;
    assert_eq!(status, 401, "the API requires the management key");
    let (status, body) = http_get(addr, "/sap/api/flow", Some("local-test-key")).await;
    assert_eq!(status, 200, "flow with the management key: {body}");
    assert!(body.contains("\"node\":\"local\""), "flow names this node: {body}");
    let (status, body) = http_get(
        addr,
        "/v8/management/routing/reset-aware/status",
        Some("local-test-key"),
    )
    .await;
    assert_eq!(status, 200, "visualizer status: {body}");
    assert!(body.contains("thread-affinity"));
    let (status, body) = http_get(addr, "/sap/", None).await;
    assert_eq!(status, 200);
    assert!(body.to_ascii_lowercase().contains("<html"), "dashboard index is served");
    let (status, _) = http_get(addr, sap_core::health::HEALTH_PATH, None).await;
    assert_eq!(status, 200);
    server.abort();
}

#[tokio::test]
async fn active_refresh_is_singleflight_and_reports_probe_sources_and_sanitized_errors() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let status = Arc::new(AtomicUsize::new(200));
    let app = axum::Router::new().route("/api/oauth/usage", axum::routing::get({
        let calls = calls.clone();
        let status = status.clone();
        move |headers: http::HeaderMap| {
            let calls = calls.clone();
            let status = status.clone();
            async move {
                assert!(headers["authorization"].to_str().unwrap().starts_with("Bearer private-test-"));
                assert_eq!(headers["anthropic-beta"], "oauth-2025-04-20");
                calls.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                (http::StatusCode::from_u16(status.load(Ordering::SeqCst) as u16).unwrap(),
                    axum::Json(serde_json::json!({"five_hour":{"utilization":25,"resets_at":"2030-01-01T00:00:00Z"},"seven_day":{"utilization":45},"error":"secret upstream body"})))
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(axum::serve(listener, app).into_future());
    let f = Fixture::with_auth(Provider::Claude, &format!("http://{address}"), true);
    let first = tokio::spawn({
        let node = f.node.clone();
        async move { node.reset_aware_refresh().await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while calls.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(f.node.reset_aware_status().polling);
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    let first = f.node.reset_aware_refresh().await;
    assert!(!first.polling);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    for account in &first.accounts {
        assert_eq!(account.five_hour.as_ref().unwrap().source.as_deref(), Some("probe"));
        assert_eq!(account.five_hour.as_ref().unwrap().used_percent, 25.0);
        assert!(account.last_poll_at.is_some());
        assert!(account.last_poll_error.is_none());
    }
    assert_eq!(f.node.reset_aware_history(1).accounts[0].samples.len(), 1);
    status.store(403, Ordering::SeqCst);
    let failed = f.node.reset_aware_refresh().await;
    for account in &failed.accounts {
        assert_eq!(
            account.last_poll_error.as_deref(),
            Some("Usage endpoint returned HTTP 403")
        );
        assert_eq!(account.five_hour.as_ref().unwrap().used_percent, 25.0);
    }
    status.store(200, Ordering::SeqCst);
    assert!(
        f.node
            .reset_aware_refresh()
            .await
            .accounts
            .iter()
            .all(|a| a.last_poll_error.is_none())
    );
    server.abort();
}

#[tokio::test]
async fn model_entitlement_moves_thread_without_revoking_account() {
    let f = Fixture::new(Provider::Codex);
    let request = f.request("entitlement", false, false, false);
    let first = f.pick(&request);
    f.node.on_outcome(
        &request,
        &first,
        &AttemptResult {
            status: Some(403),
            error: Some(cpa_server::sap_hooks::AttemptError::ModelIneligible),
        },
    );
    assert_ne!(f.pick(&request), first);
    assert!(
        f.node
            .accounts()
            .iter()
            .all(|a| a.state != sap_core::api::AccountState::LoginNeeded)
    );
}

#[tokio::test]
async fn active_refresh_codex_exhaustion_recovers_from_usage_endpoint() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let exhausted = Arc::new(AtomicBool::new(true));
    let app = axum::Router::new().route(
        "/backend-api/wham/usage",
        axum::routing::get({
            let exhausted = exhausted.clone();
            move || {
                let exhausted = exhausted.load(Ordering::SeqCst);
                async move {
                    axum::Json(serde_json::json!({
                        "rate_limit": {"allowed":!exhausted,"limit_reached":exhausted,
                            "primary_window":{"used_percent":if exhausted {100} else {10},
                                "limit_window_seconds":18000,"reset_at":1900000000},
                            "secondary_window":{"used_percent":20,"limit_window_seconds":604800}}
                    }))
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(axum::serve(listener, app).into_future());
    let f = Fixture::with_auth(Provider::Codex, &format!("http://{address}"), true);
    let request = f.request("refresh-recovery", false, false, false);
    let first = f.pick(&request);
    f.node.on_observation(
        &request,
        &first,
        Observation::Quota {
            observed_at: std::time::SystemTime::now(),
            signals: [("x-codex-limit-reached".into(), "true".into())].into(),
        },
    );
    let guarded = f.node.reset_aware_refresh().await;
    assert!(guarded.accounts.iter().all(|a| a.state == "guarded"));
    exhausted.store(false, Ordering::SeqCst);
    let recovered = f.node.reset_aware_refresh().await;
    assert!(
        recovered
            .accounts
            .iter()
            .all(|a| a.state == "normal" && a.last_poll_error.is_none())
    );
    assert_eq!(
        f.pick(&request),
        first,
        "fresh allowed usage supersedes the thread's old exhaustion"
    );
    // Passive observations cannot claim a successful usage poll.
    let keys = Fixture::new(Provider::Claude);
    let unsupported = keys.node.reset_aware_refresh().await;
    assert!(
        unsupported
            .accounts
            .iter()
            .all(|a| a.last_poll_error.as_deref() == Some("Usage probe requires a subscription OAuth credential"))
    );
    server.abort();
}

/// A Codex WebSocket turn shows as streaming while upstream events flow, every frame
/// refreshes the thread's `last_seen`, and the stream closes with the turn.
#[tokio::test]
async fn node_codex_websocket_turns_stream_and_refresh_activity() {
    use futures_util::StreamExt;
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        async fn upstream_turns(mut socket: axum::extract::ws::WebSocket) {
            use axum::extract::ws::Message;
            while socket.recv().await.is_some() {
                for event in [
                    r#"{"type":"response.created","response":{"id":"resp_a","output":[]}}"#,
                    r#"{"type":"response.completed","response":{"id":"resp_a","output":[],"usage":{"input_tokens":3,"output_tokens":1}}}"#,
                ] {
                    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                    let _ = socket.send(Message::Text(event.into())).await;
                }
            }
        }
        let upstream = axum::Router::new()
            .fallback(|ws: axum::extract::ws::WebSocketUpgrade| async move { ws.on_upgrade(upstream_turns) });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let f = Fixture::new_websocket(&format!("http://{}", listener.local_addr().unwrap()));
        let _upstream = tokio::spawn(axum::serve(listener, upstream).into_future());
        let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy.local_addr().unwrap();
        let _proxy = tokio::spawn(axum::serve(proxy, cpa_server::router(f.rt.clone())).into_future());
        let mut socket = wreq::Client::new()
            .websocket(format!("ws://{proxy_addr}/backend-api/codex/responses"))
            .header("thread-id", "ws-thread")
            .header("session-id", "ws-thread")
            .send()
            .await
            .unwrap()
            .into_websocket()
            .await
            .unwrap();
        let thread = || f.node.threads(false).into_iter().find(|t| t.key.id == "ws-thread");
        let frames = [
            r#"{"type":"response.create","model":"test","generate":false,"input":[]}"#,
            r#"{"type":"response.create","model":"test","previous_response_id":"resp_a","input":[]}"#,
            r#"{"type":"response.create","model":"test","previous_response_id":"resp_a","input":[]}"#,
        ];
        let mut seen = Vec::new();
        for (turn, frame) in frames.into_iter().enumerate() {
            socket.send(wreq::ws::message::Message::text(frame)).await.unwrap();
            loop {
                let wreq::ws::message::Message::Text(text) = socket.next().await.unwrap().unwrap() else {
                    continue;
                };
                if text.contains("response.created") {
                    let t = thread().unwrap();
                    assert!(t.streaming, "turn {turn} streams after response.created: {t:?}");
                    seen.push(t.last_seen);
                }
                if text.contains("response.completed") {
                    break;
                }
            }
        }
        assert!(seen.windows(2).all(|w| w[1] > w[0]), "every frame refreshed last_seen: {seen:?}");
        drop(socket);
        for _ in 0..100 {
            if !thread().unwrap().streaming {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("stream stayed open after the socket closed");
    })
    .await
    .unwrap();
}
