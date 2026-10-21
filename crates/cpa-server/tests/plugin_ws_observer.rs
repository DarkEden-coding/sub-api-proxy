//! Go `Options.WebSocketResponseObserver` end to end: a native plugin declaring
//! `websocket_response_observer` receives every upstream Codex WebSocket frame of a
//! request (Go `EmitWebSocketResponseEvent` beside `AppendCodexAPIWebsocketResponse`),
//! with fake credentials and a loopback upstream.
#![cfg(unix)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use cpa_core::config::Config;
use cpa_exec::Executors;
use cpa_server::router;
use futures_util::StreamExt;
use serde_json::Value;
use wreq::ws::message::Message;

const RATE_LIMITS: &str = r#"{"type":"codex.rate_limits","rate_limits":{"primary":{"used_percent":42}}}"#;
const CREATED: &str = r#"{"type":"response.created","response":{"id":"r1","status":"in_progress","output":[]}}"#;
const DONE: &str = r#"{"type":"response.completed","response":{"id":"r1","status":"completed","output":[],"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}"#;

/// Answers every turn with a rate-limit frame, then the response frames.
async fn serve_codex() -> String {
    use axum::extract::ws::{Message, WebSocketUpgrade};
    let handler = |ws: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
                   req: axum::http::Request<axum::body::Body>| async move {
        let Ok(ws) = ws else {
            panic!("expected a WebSocket upgrade, got {} {}", req.method(), req.uri());
        };
        ws.on_upgrade(|mut socket| async move {
            while let Some(Ok(message)) = socket.recv().await {
                if matches!(message, Message::Text(_)) {
                    for frame in [RATE_LIMITS, CREATED, DONE] {
                        let _ = socket.send(Message::Text(frame.into())).await;
                    }
                }
            }
        })
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, axum::Router::new().fallback(handler))
            .await
            .unwrap()
    });
    url
}

/// The observer events recorded so far.
fn events(log: &Path) -> Vec<Value> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|record| record["method"] == "websocket.response_event")
        .map(|record| serde_json::from_str(record["request"].as_str().unwrap()).unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn observer_plugins_receive_every_upstream_websocket_frame() {
    let tmp = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let Some(built) = cpa_plugin::testing::built_plugins(tmp, "plugin_ws_observer") else {
        return;
    };
    let dir = cpa_plugin::testing::scratch(tmp, "plugin-ws-observer");
    let (auths, plugins, records) = (dir.join("auths"), dir.join("plugins"), dir.join("records"));
    for d in [&auths, &plugins, &records] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::copy(built.join("recorder.so"), plugins.join("watcher.so")).unwrap();
    let codex = serve_codex().await;
    let yaml = format!(
        "auth-dir: {}\naccess:\n  api-keys: [client-key-1]\napi-keys:\n  codex:\n    - base-url: {codex}\n      models: [{{name: gpt-fixture}}]\n      keys: [{{api-key: sk-fake-codex, websockets: true}}]\nplugins:\n  enabled: true\n  dir: {}\n  configs:\n    watcher:\n      enabled: true\n      label: watcher\n      record: {}\n      caps: websocket_response_observer\n",
        auths.display(),
        plugins.display(),
        records.display()
    );
    let cfg = Config::parse(&yaml).unwrap();
    let credentials = cpa_core::config::credentials::load(&cfg);
    let auth_id = credentials[0].id.clone();
    let executors = Executors {
        claude: cpa_exec::claude::ClaudeExecutor::new("http://127.0.0.1:1").unwrap(),
        codex: Default::default(),
        devices: Default::default(),
        openai: Default::default(),
        google: Default::default(),
    };
    let rt = Arc::new(cpa_server::testing::runtime(cfg, credentials, executors));
    cpa_server::install_registry(&rt);
    cpa_server::plugins::start(&rt).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router(rt)).await.unwrap() });

    let log = records.join("watcher.jsonl");
    // Go's Codex auto executor takes the upstream WebSocket for a downstream WebSocket.
    let mut socket = wreq::Client::new()
        .websocket(format!("{}/v1/responses", base.replacen("http", "ws", 1)))
        .header("Authorization", "Bearer client-key-1")
        .send()
        .await
        .unwrap()
        .into_websocket()
        .await
        .unwrap();
    for turn in 0..2 {
        let create = r#"{"type":"response.create","model":"gpt-fixture","input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}]}"#;
        socket.send(Message::text(create)).await.unwrap();
        loop {
            let next = tokio::time::timeout(Duration::from_secs(5), socket.next())
                .await
                .unwrap();
            let Some(Ok(Message::Text(text))) = next else {
                panic!("turn {turn}: {next:?}");
            };
            if text.as_str().contains("\"response.completed\"") {
                break;
            }
        }

        // Delivery runs beside the turn; wait for its three frames.
        let want = 3 * (turn + 1);
        let mut seen = events(&log);
        for _ in 0..100 {
            if seen.len() >= want {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            seen = events(&log);
        }
        assert_eq!(seen.len(), want, "turn {turn}: {seen:?}");
        let turn_events = &seen[want - 3..];
        let types: Vec<&str> = turn_events.iter().map(|e| e["EventType"].as_str().unwrap()).collect();
        assert_eq!(
            types,
            ["codex.rate_limits", "response.created", "response.completed"],
            "turn {turn}"
        );
        let request_id = turn_events[0]["RequestID"].as_str().unwrap();
        assert!(!request_id.is_empty());
        for (event, frame) in turn_events.iter().zip([RATE_LIMITS, CREATED, DONE]) {
            let payload = base64::engine::general_purpose::STANDARD
                .decode(event["Payload"].as_str().unwrap())
                .unwrap();
            assert_eq!(payload, frame.as_bytes(), "turn {turn}");
            assert_eq!(event["RequestID"], request_id, "one request ID per turn");
            assert!(!event["TraceID"].as_str().unwrap().is_empty());
            assert_eq!(event["SourceFormat"], "openai-response");
            assert_eq!(event["Provider"], "codex");
            assert_eq!(event["Model"], "gpt-fixture");
            assert_eq!(event["RequestedModel"], "gpt-fixture");
            assert_eq!(event["AuthID"], auth_id.as_str());
            assert_eq!(event["AuthType"], "api_key");
            assert_eq!(event["Metadata"]["selected_auth_id"], auth_id.as_str());
        }
        if turn == 1 {
            assert_ne!(seen[0]["RequestID"], seen[3]["RequestID"], "each turn has its own ID");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
