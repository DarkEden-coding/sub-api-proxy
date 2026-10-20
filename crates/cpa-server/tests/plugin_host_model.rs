//! v8.0.35 internal host.model callbacks through the production dispatcher and
//! native plugins, with private credentials and loopback-only upstreams.
#![cfg(unix)]

use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use cpa_core::config::Config;
use cpa_exec::Executors;
use cpa_plugin::callbacks::RequestScope;
use cpa_plugin::client::CallbackInstance;
use cpa_plugin::testing::{call_from_plugin, callback_context};
use serde_json::{Value, json};

const BODY: &str = r#"{ "model":"fast", "messages":[{"role":"user","content":"hello"}] }"#;
const REPLY: &str = r#"{ "id":"reply", "choices":[{"message":{"content":"ok"}}] }"#;
const CHUNK: &str = r#"{"id":"reply","choices":[{"delta":{"content":"ok"}}]}"#;

#[derive(Default)]
struct Seen(Mutex<Vec<(String, String, Bytes)>>);

async fn upstream(State(seen): State<Arc<Seen>>, req: Request) -> Response {
    let path = req.uri().to_string();
    let auth = req.headers()["authorization"].to_str().unwrap().to_owned();
    let body = axum::body::to_bytes(req.into_body(), usize::MAX).await.unwrap();
    let fields: Value = serde_json::from_slice(&body).unwrap();
    let stream = fields["stream"] == true;
    seen.0.lock().unwrap().push((path, auth, body));
    if fields["wait"] == true {
        return std::future::pending().await;
    }
    let mut response = if stream {
        ([("x-upstream", "stream")], format!("data: {CHUNK}\n\ndata: [DONE]\n\n")).into_response()
    } else {
        ([("x-upstream", "buffered")], REPLY).into_response()
    };
    for (name, value) in [
        ("set-cookie", "session=fake"),
        ("connection", "x-private"),
        ("x-private", "hop"),
        ("x-litellm-test", "gateway"),
        ("x-cpa-trace-id", "reserved"),
    ] {
        response.headers_mut().insert(name, value.parse().unwrap());
    }
    response
}

async fn serve(seen: Arc<Seen>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, axum::Router::new().fallback(upstream).with_state(seen))
            .await
            .unwrap();
    });
    url
}

fn b64(bytes: impl AsRef<[u8]>) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn decode(value: &Value) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(value.as_str().unwrap())
        .unwrap()
}

fn check_headers(headers: &Value, upstream: &str) {
    assert_eq!(headers["X-Upstream"], json!([upstream]));
    for name in [
        "Set-Cookie",
        "Connection",
        "X-Private",
        "X-Litellm-Test",
        "X-Cpa-Trace-Id",
    ] {
        assert!(headers.get(name).is_none(), "unsafe upstream header {name}");
    }
}

fn canned(records: &Path, label: &str, method: &str, result: Value) {
    let dir = records.join("respond").join(label);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{method}.json")),
        json!({"ok":true,"result":result}).to_string(),
    )
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn callbacks_execute_pin_and_stream_without_recursing_into_the_caller() {
    let tmp = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let Some(built) = cpa_plugin::testing::built_plugins(tmp, "plugin_host_model") else {
        return;
    };
    let dir = cpa_plugin::testing::scratch(tmp, "host-model");
    let auths = dir.join("auths");
    let records = dir.join("records");
    let plugins = dir.join("plugins");
    for d in [&auths, &records, &plugins] {
        std::fs::create_dir_all(d).unwrap();
    }
    let seen = Arc::new(Seen::default());
    let proxy_seen = Arc::new(Seen::default());
    let url = serve(seen.clone()).await;
    // A mock HTTP proxy answers requests itself; it never forwards traffic.
    let proxy = serve(proxy_seen.clone()).await;
    let yaml = format!(
        "auth-dir: {}\napi-keys:\n  openai-compatibility:\n    - name: Acme\n      base-url: {url}/v1\n      models: [{{name: up-model, alias: fast}}]\n      keys: [{{api-key: first}}, {{api-key: second}}]\n",
        auths.display()
    );
    let cfg = Config::parse(&yaml).unwrap();
    let credentials = cpa_core::config::credentials::load(&cfg);
    let pinned = credentials[1].id.clone();
    let executors = Executors {
        claude: cpa_exec::claude::ClaudeExecutor::new("http://127.0.0.1:1").unwrap(),
        codex: Default::default(),
        devices: Default::default(),
        openai: Default::default(),
        google: Default::default(),
    };
    let rt = Arc::new(cpa_server::testing::runtime(cfg, credentials, executors));
    cpa_server::plugins::start(&rt).await;
    let host = rt.plugins().clone();
    let instance = Arc::new(CallbackInstance::default());
    let guard = callback_context(&host, "origin", instance.clone(), RequestScope::default());
    let id = guard.id().to_owned();
    let (h, i, pin) = (host.clone(), instance.clone(), pinned.clone());
    tokio::task::spawn_blocking(move || {
        let call = |method, request: Value| -> Value {
            let bytes = call_from_plugin(&h, "origin", &i, method, &serde_json::to_vec(&request).unwrap()).unwrap();
            serde_json::from_slice::<Value>(&bytes).unwrap()["result"].clone()
        };
        let request = json!({"entry_protocol":"openai", "model":"fast", "auth_id":pin,
            "body":b64(BODY), "host_callback_id":id});
        let response = call("host.model.execute", request.clone());
        assert_eq!(response["status_code"], 200);
        assert_eq!(decode(&response["body"]), REPLY.as_bytes());
        check_headers(&response["headers"], "buffered");
        let mut overridden = request.clone();
        overridden["proxy_url"] = json!(proxy);
        assert_eq!(
            decode(&call("host.model.execute", overridden)["body"]),
            REPLY.as_bytes()
        );
        let mut stream = request;
        stream["stream"] = json!(true);
        stream["body"] = json!(b64(BODY.replace("\"model\":", "\"stream\":true,\"model\":")));
        let opened = call("host.model.execute_stream", stream);
        check_headers(&opened["headers"], "stream");
        let read = json!({"stream_id":opened["stream_id"]});
        let first = call("host.model.stream_read", read.clone());
        assert_eq!(
            decode(&first["payload"]),
            CHUNK.as_bytes(),
            "internal chunks are not SSE frames"
        );
        loop {
            if call("host.model.stream_read", read.clone())["done"] == true {
                break;
            }
        }
        call("host.model.stream_close", read);
    })
    .await
    .unwrap();
    assert_eq!(seen.0.lock().unwrap().len(), 2);
    assert_eq!(proxy_seen.0.lock().unwrap().len(), 1);
    for (_, auth, body) in seen.0.lock().unwrap().iter().chain(proxy_seen.0.lock().unwrap().iter()) {
        assert_eq!(auth, "Bearer second");
        assert_eq!(serde_json::from_slice::<Value>(body).unwrap()["model"], "up-model");
    }
    assert!(
        rt.store()
            .snapshot()
            .iter()
            .all(|c| !c.attributes.contains_key("proxy_url"))
    );

    for label in ["origin", "other"] {
        std::fs::copy(built.join("recorder.so"), plugins.join(format!("{label}.so"))).unwrap();
        for method in [
            "request.intercept_before",
            "request.intercept_after",
            "response.intercept_after",
            "response.intercept_stream_chunk",
            "request.complete",
        ] {
            canned(&records, label, method, json!({}));
        }
        canned(
            &records,
            label,
            "model.route",
            json!({"handled":true,"target_kind":"provider","target":"openai-compatible-acme"}),
        );
    }
    let mut plugin_yaml = format!(
        "{yaml}plugins:\n  enabled: true\n  dir: {}\n  configs:\n",
        plugins.display()
    );
    for label in ["origin", "other"] {
        plugin_yaml.push_str(&format!(
            "    {label}:\n      enabled: true\n      label: {label}\n      record: {}\n      caps: management_api,model_router,request_interceptor,response_interceptor,response_stream_interceptor,request_lifecycle_plugin\n",
            records.display()
        ));
    }
    let cfg = Config::parse(&plugin_yaml).unwrap();
    cpa_server::plugins::sync(&rt, Arc::new(cfg)).await;
    assert!(host.plugin_registered("origin") && host.plugin_registered("other"));
    let nested = json!({"calls":[{"method":"host.model.execute","request":{
        "entry_protocol":"openai","model":"fast","auth_id":pinned,"body":b64(BODY)}}]});
    let response = host
        .serve_management(cpa_plugin::management::Inbound {
            method: "POST".into(),
            path: "/v0/management/rec/origin/calls".into(),
            headers: [("X-Inherited".into(), vec!["outer".into()])].into(),
            query: [("q".into(), vec!["one".into(), "two".into()])].into(),
            body: Bytes::from(serde_json::to_vec(&nested).unwrap()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(response.status, 201);
    let response: Value = serde_json::from_slice(&response.body).unwrap();
    let nested = response["results"][0].as_str().unwrap().strip_prefix("0 ").unwrap();
    let nested: Value = serde_json::from_str(nested).unwrap();
    assert_eq!(decode(&nested["result"]["body"]), REPLY.as_bytes());
    let methods = |label: &str| -> Vec<String> {
        std::fs::read_to_string(records.join(format!("{label}.jsonl")))
            .unwrap()
            .lines()
            .map(|line| {
                serde_json::from_str::<Value>(line).unwrap()["method"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .filter(|method| {
                method.starts_with("request.") || method.starts_with("response.") || method == "model.route"
            })
            .collect()
    };
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !methods("other").iter().any(|method| method == "request.complete") {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(methods("origin").is_empty(), "the caller must not recurse into itself");
    assert_eq!(
        methods("other"),
        [
            "model.route",
            "request.intercept_before",
            "request.intercept_after",
            "response.intercept_after",
            "request.complete"
        ]
    );
    let records = std::fs::read_to_string(records.join("other.jsonl")).unwrap();
    let route: Value = records
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|record| record["method"] == "model.route")
        .unwrap();
    let route: Value = serde_json::from_str(route["request"].as_str().unwrap()).unwrap();
    assert_eq!(route["Headers"]["X-Inherited"], json!(["outer"]));
    assert_eq!(route["Query"]["q"], json!(["one", "two"]));
    let (h, i, id) = (host.clone(), instance, guard.id().to_owned());
    tokio::task::spawn_blocking(move || {
        let call = |method, request: Value| -> Value {
            let raw = call_from_plugin(&h, "origin", &i, method, &serde_json::to_vec(&request).unwrap()).unwrap();
            serde_json::from_slice::<Value>(&raw).unwrap()["result"].clone()
        };
        let opened = call(
            "host.model.execute_stream",
            json!({"entry_protocol":"openai","model":"fast",
            "stream":true,"body":b64(BODY.replace("\"model\":", "\"stream\":true,\"model\":")),"host_callback_id":id}),
        );
        let read = json!({"stream_id":opened["stream_id"]});
        assert_eq!(
            decode(&call("host.model.stream_read", read.clone())["payload"]),
            CHUNK.as_bytes()
        );
        while call("host.model.stream_read", read.clone())["done"] != true {}
    })
    .await
    .unwrap();
    let methods = |label: &str| -> Vec<String> {
        std::fs::read_to_string(dir.join("records").join(format!("{label}.jsonl")))
            .unwrap()
            .lines()
            .map(|line| {
                serde_json::from_str::<Value>(line).unwrap()["method"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect()
    };
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while methods("other")
            .iter()
            .filter(|method| *method == "request.complete")
            .count()
            != 2
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        methods("other")
            .iter()
            .any(|method| method == "response.intercept_stream_chunk")
    );
    assert!(
        !methods("origin")
            .iter()
            .any(|method| method.starts_with("request.") || method.starts_with("response.") || method == "model.route")
    );
    let count = seen.0.lock().unwrap().len();
    let h = host.clone();
    let blocked = tokio::spawn(async move {
        let request = json!({"calls":[{"method":"host.model.execute","request":{
            "entry_protocol":"openai","model":"fast","body":b64(BODY.replace("\"model\":", "\"wait\":true,\"model\":"))}}]});
        h.serve_management(cpa_plugin::management::Inbound {
            method: "POST".into(),
            path: "/v0/management/rec/origin/calls".into(),
            body: Bytes::from(serde_json::to_vec(&request).unwrap()),
            ..Default::default()
        })
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while seen.0.lock().unwrap().len() == count {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    blocked.abort();
    assert!(blocked.await.unwrap_err().is_cancelled());
    // Shutdown waits for active native calls, so this also proves cancellation
    // releases the plugin blocked in its synchronous host.model callback.
    tokio::time::timeout(std::time::Duration::from_secs(2), host.shutdown_all(None))
        .await
        .unwrap();
    drop(guard);
    std::fs::remove_dir_all(dir).unwrap();
}
