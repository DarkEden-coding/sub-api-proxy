//! Go 35af74f9 (sdk/cliproxy/auth/conductor_issue6643_test.go
//! TestManager_Issue6643_InFlightSuccessesKeepFreshQuotaCooldown) through the real router:
//! requests already in flight on one Claude credential, a stream and a buffered one,
//! succeed only after a later request was refused with a ten-minute 429. Their successes
//! predate the cooldown, so the credential stays cooling and the next request never
//! reaches the upstream. Loopback upstream, fake keys and a private `auth-dir` only.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use cpa_core::config::Config;
use cpa_core::credential::Credential;
use cpa_exec::Executors;
use cpa_exec::claude::ClaudeExecutor;
use serde_json::json;

const REPLY: &str = r#"{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"m","stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}"#;
const STREAM: &str = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"m\",\"stop_reason\":null,\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";

/// The first two requests wait for the test's release and then succeed; the third is
/// refused for ten minutes (inside the default `max-trusted-cooldown`, so Go's plain
/// cooldown); any later one would succeed.
struct Up {
    calls: AtomicUsize,
    release: tokio::sync::Semaphore,
}

async fn upstream(State(up): State<Arc<Up>>, req: axum::extract::Request) -> Response {
    let body = axum::body::to_bytes(req.into_body(), usize::MAX).await.unwrap();
    let stream = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["stream"] == true;
    let call = up.calls.fetch_add(1, Ordering::SeqCst);
    if call == 2 {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [("content-type", "application/json"), ("retry-after", "600")],
            r#"{"type":"error","error":{"type":"rate_limit_error","message":"rate limited"}}"#,
        )
            .into_response();
    }
    if call < 2 {
        up.release.acquire().await.unwrap().forget();
    }
    if stream {
        ([("content-type", "text/event-stream")], STREAM).into_response()
    } else {
        ([("content-type", "application/json")], REPLY).into_response()
    }
}

async fn serve(app: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

async fn message(url: &str, stream: bool) -> (u16, String) {
    let body = json!({"model": "claude-opus-4-6", "max_tokens": 8, "stream": stream,
        "messages": [{"role": "user", "content": "hi"}]});
    let res = wreq::Client::new()
        .post(format!("{url}/v1/messages"))
        .header("x-api-key", "client-key")
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    (res.status().as_u16(), res.text().await.unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_flight_successes_keep_a_fresh_quota_cooldown() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("cooldown-races-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let up = Arc::new(Up {
        calls: AtomicUsize::new(0),
        release: tokio::sync::Semaphore::new(0),
    });
    let upstream_url = serve(axum::Router::new().fallback(upstream).with_state(up.clone())).await;
    let yaml = format!("api-keys: [client-key]\nauth-dir: {}\n", dir.display());
    let meta = json!({"type": "claude", "api_key": "sk-fake-claude", "base_url": upstream_url});
    std::fs::write(dir.join("claude.json"), meta.to_string()).unwrap();
    let mut credential =
        Credential::from_file(&dir, &dir.join("claude.json"), meta.as_object().unwrap().clone()).unwrap();
    credential.attributes.insert("api_key".into(), "sk-fake-claude".into());
    credential.attributes.insert("base_url".into(), upstream_url);
    let rt = Arc::new(cpa_server::testing::runtime(
        Config::parse(&yaml).unwrap(),
        vec![credential],
        Executors {
            claude: ClaudeExecutor::new("http://127.0.0.1:1").unwrap(),
            codex: Default::default(),
            devices: Default::default(),
            openai: Default::default(),
            google: Default::default(),
        },
    ));
    let url = serve(cpa_server::app(rt, axum::Router::new())).await;

    let streamed = tokio::spawn({
        let url = url.clone();
        async move { message(&url, true).await }
    });
    let buffered = tokio::spawn({
        let url = url.clone();
        async move { message(&url, false).await }
    });
    while up.calls.load(Ordering::SeqCst) < 2 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let (status, body) = message(&url, false).await;
    assert_eq!(status, 429, "{body}");

    up.release.add_permits(2);
    let (status, body) = streamed.await.unwrap();
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("message_stop"), "{body}");
    let (status, body) = buffered.await.unwrap();
    assert_eq!(status, 200, "{body}");
    // The stream's success is recorded as its body ends.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let (status, body) = message(&url, false).await;
    assert_eq!(status, 429, "still cooling: {body}");
    assert_eq!(
        up.calls.load(Ordering::SeqCst),
        3,
        "the cooling credential was not tried"
    );
}
