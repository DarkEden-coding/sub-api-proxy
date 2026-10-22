//! Go v8.0.35 plugin-owned providers without a model router: a catalog-only plugin's
//! models bind to the credentials of their provider (with the credential's excluded
//! models applied), and a plugin executor serves those credentials through the
//! ordinary selection loop. Plugin schedulers choose among built-in credentials.
//! Private auth-dir, native test plugins, loopback-only upstreams.
#![cfg(unix)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use cpa_core::config::Config;
use cpa_exec::Executors;
use serde_json::{Value, json};

fn canned(records: &Path, label: &str, method: &str, result: Value) {
    let dir = records.join("respond").join(label);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{method}.json")),
        json!({"ok":true,"result":result}).to_string(),
    )
    .unwrap();
}

fn calls(records: &Path, label: &str, method: &str) -> Vec<Value> {
    std::fs::read_to_string(records.join(format!("{label}.jsonl")))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|r| r["method"] == method)
        .map(|r| serde_json::from_str(r["request"].as_str().unwrap()).unwrap())
        .collect()
}

/// A runtime with recorder plugins `(label, caps)` and auth files, serving on a local
/// port. `prepare` runs on the records dir before the plugins start.
struct Env {
    /// The plugin catalog is process-wide; tests in this binary take turns.
    _serial: tokio::sync::MutexGuard<'static, ()>,
    rt: Arc<cpa_server::Runtime>,
    dir: PathBuf,
    records: PathBuf,
    plugins: PathBuf,
    yaml: String,
    proxy: SocketAddr,
}

async fn env(
    name: &str,
    plugins: &[(&str, &str)],
    extra_yaml: &str,
    auths: &[(&str, &str)],
    prepare: impl FnOnce(&Path),
) -> Option<Env> {
    env_with(name, plugins, extra_yaml, auths, prepare, Default::default()).await
}

/// [`env`] with a Codex executor (for Codex live endpoints on a local mock).
async fn env_with(
    name: &str,
    plugins: &[(&str, &str)],
    extra_yaml: &str,
    auths: &[(&str, &str)],
    prepare: impl FnOnce(&Path),
    codex: cpa_exec::codex::CodexExecutor,
) -> Option<Env> {
    static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let serial = SERIAL.lock().await;
    let tmp = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let built = cpa_plugin::testing::built_plugins(tmp, "plugin_owned")?;
    let dir = cpa_plugin::testing::scratch(tmp, name);
    let (auth_dir, records, plugin_dir) = (dir.join("auths"), dir.join("records"), dir.join("plugins"));
    for d in [&auth_dir, &records, &plugin_dir] {
        std::fs::create_dir_all(d).unwrap();
    }
    let mut yaml = format!(
        "auth-dir: {}\n{extra_yaml}plugins:\n  enabled: true\n  dir: {}\n  configs:\n",
        auth_dir.display(),
        plugin_dir.display()
    );
    for (label, caps) in plugins {
        std::fs::copy(built.join("recorder.so"), plugin_dir.join(format!("{label}.so"))).unwrap();
        yaml.push_str(&format!(
            "    {label}:\n      enabled: true\n      label: {label}\n      record: {}\n      caps: {caps}\n",
            records.display()
        ));
    }
    for (file, body) in auths {
        std::fs::write(auth_dir.join(file), body).unwrap();
    }
    prepare(&records);
    let cfg = Config::parse(&yaml).unwrap();
    let credentials = cpa_core::config::credentials::load(&cfg);
    let rt = Arc::new(cpa_server::testing::runtime(
        cfg,
        credentials,
        Executors {
            claude: cpa_exec::claude::ClaudeExecutor::new("http://127.0.0.1:1").unwrap(),
            codex,
            devices: Default::default(),
            openai: Default::default(),
            google: Default::default(),
        },
    ));
    cpa_server::plugins::start(&rt).await;
    let proxy = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = cpa_server::router(rt.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        addr
    };
    Some(Env {
        _serial: serial,
        rt,
        dir,
        records,
        plugins: plugin_dir,
        yaml,
        proxy,
    })
}

impl Env {
    async fn chat(&self, model: &str) -> (u16, String) {
        self.post(format!(
            r#"{{"model":"{model}","messages":[{{"role":"user","content":"hi"}}]}}"#
        ))
        .await
    }

    async fn chat_stream(&self, model: &str) -> (u16, String) {
        self.post(format!(
            r#"{{"model":"{model}","stream":true,"messages":[{{"role":"user","content":"hi"}}]}}"#
        ))
        .await
    }

    async fn post(&self, body: String) -> (u16, String) {
        let response = wreq::Client::new()
            .post(format!("http://{}/v1/chat/completions", self.proxy))
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap();
        (response.status().as_u16(), response.text().await.unwrap())
    }

    async fn finish(self) {
        self.rt.plugins().shutdown_all(None).await;
        std::fs::remove_dir_all(self.dir).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_catalog_models_bind_to_credentials_served_by_a_plugin_executor() {
    // `catalog` lists models for provider `acme`; `acme` executes them.
    let Some(env) = env(
        "plugin-owned",
        &[("catalog", "model_provider"), ("acme", "executor")],
        "",
        &[(
            "acme.json",
            r#"{"type":"acme","token":"t-1","excluded_models":["acme-hidden"]}"#,
        )],
        |records| {
            canned(
                records,
                "catalog",
                "model.static",
                json!({"Provider":"ACME","Models":[{"ID":" acme-1 ","OwnedBy":"acme"},{"ID":"acme-hidden"}]}),
            )
        },
    )
    .await
    else {
        return;
    };
    let registry = env.rt.registry();
    assert!(registry.client_supports("acme.json", "acme-1"));
    assert!(
        !registry.client_supports("acme.json", "acme-hidden"),
        "excluded by the credential"
    );
    assert_eq!(
        registry.providers("acme-hidden"),
        Vec::<String>::new(),
        "no credential publishes it"
    );
    let client = wreq::Client::new();
    let models: Value = client
        .get(format!("http://{}/v1/models", env.proxy))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids: Vec<&str> = models["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["id"].as_str())
        .collect();
    assert_eq!(ids, ["acme-1"], "{models}");
    let (status, body) = env.chat("acme-1").await;
    assert_eq!(status, 200);
    // The recorder echoes the payload it was given.
    assert_eq!(
        body,
        r#"{"model":"acme-1","messages":[{"role":"user","content":"hi"}]}"#
    );
    let executed = calls(&env.records, "acme", "executor.execute");
    assert_eq!(executed.len(), 1);
    let request = &executed[0];
    assert_eq!(request["AuthID"], "acme.json", "relative to the auth dir");
    assert_eq!(request["AuthProvider"], "acme");
    assert_eq!(request["Model"], "acme-1");
    assert_eq!(request["AuthMetadata"]["token"], "t-1");
    let storage = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        request["StorageJSON"].as_str().unwrap(),
    )
    .unwrap();
    let storage: Value = serde_json::from_slice(&storage).unwrap();
    assert_eq!(storage["token"], "t-1");
    // A model no credential carries is not routed to the executor.
    assert_ne!(env.chat("acme-hidden").await.0, 200);
    assert_eq!(calls(&env.records, "acme", "executor.execute").len(), 1);
    env.finish().await;
}

/// Go `registerModelsForAuth` appends plugin catalogs to OpenAI-compatible credentials
/// under their provider key, with no exclusions, whether or not models are configured.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn openai_compatible_credentials_list_their_provider_catalog() {
    let compat = "openai-compatibility:\n  - name: empty\n    base-url: http://127.0.0.1:1/v1\n    api-key-entries: [{api-key: k1}]\n  - name: full\n    base-url: http://127.0.0.1:1/v1\n    api-key-entries: [{api-key: k2}]\n    models: [{name: up-model}]\n";
    let Some(env) = env(
        "plugin-owned-compat",
        &[("cat-empty", "model_provider"), ("cat-full", "model_provider")],
        compat,
        &[],
        |records| {
            canned(
                records,
                "cat-empty",
                "model.static",
                json!({"Provider":"openai-compatible-empty","Models":[{"ID":"plug-a"}]}),
            );
            canned(
                records,
                "cat-full",
                "model.static",
                json!({"Provider":"openai-compatible-full","Models":[{"ID":"plug-b"}]}),
            );
        },
    )
    .await
    else {
        return;
    };
    let registry = env.rt.registry();
    let credential = |name: &str| {
        env.rt
            .store()
            .snapshot()
            .into_iter()
            .find(|c| c.attributes.get("compat_name").map(String::as_str) == Some(name))
            .unwrap()
            .id
            .clone()
    };
    let (empty, full) = (credential("empty"), credential("full"));
    assert!(registry.client_supports(&empty, "plug-a"), "no configured models");
    assert!(
        !registry.client_supports(&empty, "plug-b"),
        "another provider's catalog"
    );
    assert!(registry.client_supports(&full, "up-model"));
    assert!(registry.client_supports(&full, "plug-b"), "configured models");
    env.finish().await;
}

/// Go reserves a catalog's models only for a provider with a bound built-in executor
/// (`manager.Executor`); an OpenAI-compatible name no credential uses has none, so a
/// plugin executor offering the same model still claims it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_catalog_for_an_unbound_builtin_provider_reserves_nothing() {
    let Some(env) = env(
        "plugin-owned-unbound",
        &[("catalog", "model_provider"), ("acme", "model_provider,executor")],
        "",
        &[("acme.json", r#"{"type":"acme"}"#)],
        |records| {
            canned(
                records,
                "catalog",
                "model.static",
                json!({"Provider":"openai-compatible-unused","Models":[{"ID":"shared"}]}),
            );
            canned(
                records,
                "acme",
                "model.static",
                json!({"Provider":"acme","Models":[{"ID":"shared"}]}),
            );
        },
    )
    .await
    else {
        return;
    };
    assert!(env.rt.registry().client_supports("acme.json", "shared"));
    let (status, _) = env.chat("shared").await;
    assert_eq!(status, 200);
    assert_eq!(calls(&env.records, "acme", "executor.execute").len(), 1);
    env.finish().await;
}

/// A failed credential-bound plugin execution is an attempt on that credential. Go's
/// `rpcError` and plain errors are neither request- nor credential-scoped, so the auth
/// manager classifies them by status and body: each case below fails over to the other
/// credential and cools the failed one. A real transport fault cools nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failing_plugin_credential_fails_over_to_the_next() {
    let dead = "openai-compatibility:\n  - name: dead\n    base-url: http://127.0.0.1:1/v1\n    api-key-entries: [{api-key: k1}]\n    models: [{name: dead-model}]\n";
    let Some(env) = env(
        "plugin-owned-failover",
        &[("catalog", "model_provider"), ("acme", "executor")],
        dead,
        &[
            ("acme-a.json", r#"{"type":"acme"}"#),
            ("acme-b.json", r#"{"type":"acme"}"#),
        ],
        |records| {
            canned(
                records,
                "catalog",
                "model.static",
                json!({"Provider":"acme","Models":[{"ID":"acme-1"}]}),
            )
        },
    )
    .await
    else {
        return;
    };
    let not_found = r#"{"error":{"code":"model_not_found","message":"no such model"}}"#;
    // (status, message, cooldown of the failed credential in seconds, None for none).
    const HOUR: u64 = 3600;
    let cases = [
        (Some(429), "slow down", Some(1)),
        (Some(500), "boom", Some(60)),
        (Some(401), "expired", Some(HOUR / 2)),
        (Some(404), "not found", Some(12 * HOUR)),
        (Some(404), not_found, Some(12 * HOUR)),
        (Some(400), "requested model is not supported", Some(12 * HOUR)),
        // An application error without a status, like an unavailable executor.
        (None, "plugin broke", Some(60)),
        // Statusless lifecycle and transport text: Go fails over without a cooldown.
        (None, "broken pipe", None),
        (None, "unexpected EOF", None),
        (None, "context deadline exceeded", None),
        (None, "read tcp 10.0.0.1:443: connection reset by peer", None),
        // With a status the text is not consulted.
        (Some(500), "broken pipe", Some(60)),
        // A statusless model-not-found still skips the cooldown on lifecycle or transport
        // text, but the result's transient check ignores a TLS handshake timeout.
        (
            None,
            r#"{"error":{"code":"model_not_found","message":"broken pipe"}}"#,
            None,
        ),
        (
            None,
            r#"{"error":{"code":"model_not_found","message":"unexpected EOF"}}"#,
            None,
        ),
        (
            None,
            r#"{"error":{"code":"model_not_found","message":"tls handshake timeout"}}"#,
            Some(12 * HOUR),
        ),
    ];
    for (status, message, cool) in cases {
        let case = format!("{status:?} {message}");
        let dir = env.records.join("respond").join("acme");
        std::fs::create_dir_all(&dir).unwrap();
        let mut error = json!({"code":"x","message":message});
        if let Some(status) = status {
            error["http_status"] = json!(status);
        }
        std::fs::write(
            dir.join("executor.execute.once.json"),
            json!({"ok":false,"error":error}).to_string(),
        )
        .unwrap();
        let _ = std::fs::remove_file(env.records.join("acme.jsonl"));
        let (got, body) = env.chat("acme-1").await;
        assert_eq!(got, 200, "{case}: {body}");
        let ids: Vec<String> = calls(&env.records, "acme", "executor.execute")
            .into_iter()
            .map(|r| r["AuthID"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(ids.len(), 2, "{case}: {ids:?}");
        assert_ne!(ids[0], ids[1], "{case}: the second attempt uses the other credential");
        let cooling = env.rt.store().cooldowns(&ids[0]);
        match cool {
            None => assert!(cooling.is_empty(), "{case}: no cooldown: {cooling:?}"),
            Some(secs) => {
                assert_eq!(cooling.len(), 1, "{case}: the failed credential cools: {cooling:?}");
                assert_eq!(
                    (cooling[0].model.as_str(), cooling[0].status),
                    ("acme-1", status.unwrap_or(0)),
                    "{case}"
                );
                let left = cooling[0].remaining;
                assert!(
                    left <= Duration::from_secs(secs) && left > Duration::from_secs(secs.saturating_sub(5)),
                    "{case}: cools for {secs}s, {left:?} left"
                );
            }
        }
        assert!(env.rt.store().cooldowns(&ids[1]).is_empty(), "{case}");
        for id in ["acme-a.json", "acme-b.json"] {
            env.rt.store().reset_cooldown(id);
        }
    }
    // A request fault wins over transport text in the same statusless error: Go's
    // resultErrorFromError codes it request-scoped before the lifecycle and transient
    // checks, so the request stops on the first credential and nothing cools.
    for message in [
        r#"{"error":{"type":"invalid_request_error","message":"tls handshake timeout"}}"#,
        r#"{"error":{"type":"invalid_request_error","message":"broken pipe"}}"#,
    ] {
        let dir = env.records.join("respond").join("acme");
        std::fs::write(
            dir.join("executor.execute.once.json"),
            json!({"ok":false,"error":{"code":"x","message":message}}).to_string(),
        )
        .unwrap();
        let _ = std::fs::remove_file(env.records.join("acme.jsonl"));
        let (got, body) = env.chat("acme-1").await;
        assert_ne!(got, 200, "{message}: {body}");
        let ids: Vec<String> = calls(&env.records, "acme", "executor.execute")
            .into_iter()
            .map(|r| r["AuthID"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(ids.len(), 1, "{message}: no failover: {ids:?}");
        assert!(env.rt.store().cooldowns(&ids[0]).is_empty(), "{message}: no cooldown");
    }
    // A connection refused by a built-in executor's upstream is transport: no cooldown.
    let dead_id = env
        .rt
        .store()
        .snapshot()
        .into_iter()
        .find(|c| c.attributes.get("compat_name").map(String::as_str) == Some("dead"))
        .unwrap()
        .id
        .clone();
    assert_ne!(env.chat("dead-model").await.0, 200);
    assert!(
        env.rt.store().cooldowns(&dead_id).is_empty(),
        "transport faults do not cool"
    );
    env.finish().await;
}

/// Go 3b69068e: when translating the client body into the plugin executor's format
/// refuses it (an attachment the target cannot carry, leaving the turn empty), the route
/// answers 400 naming the part and the plugin is never called (`prepareExecutorCall`
/// returns the envelope's error).
// ponytail: no translator on master refuses a body yet; #75 ports the Claude refusals.
// Enable this when it merges.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the translator refusals from #75"]
async fn a_refused_translation_answers_400_without_calling_the_routed_executor() {
    // `rt` routes and executes in Claude format; `exo` executes in chat-completions.
    let Some(env) = env(
        "plugin-owned-refused-translation",
        &[
            ("rt", "model_router,executor\n      inputs: claude"),
            ("exo", "executor"),
        ],
        "",
        &[],
        |_| {},
    )
    .await
    else {
        return;
    };
    let route = |target_kind: &str, target: &str| {
        let dir = env.records.join("respond").join("rt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("model.route.once.json"),
            json!({"ok":true,"result":{"Handled":true,"TargetKind":target_kind,"Target":target,"Reason":"test"}})
                .to_string(),
        )
        .unwrap();
    };
    let cases = [
        (
            "/v1/chat/completions",
            ("self", "rt"),
            r#"{"model":"m1","messages":[{"role":"user","content":[{"type":"file","file":{"file_id":"file-absent"}}]}]}"#,
            "file",
        ),
        (
            "/v1/responses",
            ("self", "rt"),
            r#"{"model":"m1","input":[{"role":"user","content":[{"type":"input_file","file_id":"file-absent"}]}]}"#,
            "input_file",
        ),
        (
            "/v1/messages",
            ("executor", "exo"),
            r#"{"model":"m1","max_tokens":5,"messages":[{"role":"user","content":[{"type":"container_upload","file_id":"file-absent"}]}]}"#,
            "container_upload",
        ),
    ];
    for (path, (kind, target), body, part) in cases {
        route(kind, target);
        for label in ["rt", "exo"] {
            let _ = std::fs::remove_file(env.records.join(format!("{label}.jsonl")));
        }
        let response = wreq::Client::new()
            .post(format!("http://{}{path}", env.proxy))
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        let text = response.text().await.unwrap();
        assert_eq!(status, 400, "{path}: {text}");
        assert!(
            text.contains(&format!("unsupported content part: {part}")),
            "{path}: {text}"
        );
        assert_eq!(calls(&env.records, "rt", "model.route").len(), 1, "{path}: routed");
        assert!(
            calls(&env.records, target, "executor.execute").is_empty(),
            "{path}: not executed"
        );
    }
    env.finish().await;
}

/// A loopback OpenAI-compatible upstream that records each request's bearer key and
/// fails with 500 for the key in `fail`.
async fn keyed_upstream(seen: Arc<std::sync::Mutex<Vec<String>>>, fail: Arc<std::sync::Mutex<String>>) -> String {
    let handler = move |headers: axum::http::HeaderMap| {
        let (seen, fail) = (seen.clone(), fail.clone());
        async move {
            let key = headers["authorization"]
                .to_str()
                .unwrap()
                .trim_start_matches("Bearer ")
                .to_owned();
            seen.lock().unwrap().push(key.clone());
            if *fail.lock().unwrap() == key {
                return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "down".to_owned());
            }
            (
                axum::http::StatusCode::OK,
                r#"{"id":"r","object":"chat.completion","created":1,"model":"up-model","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#.to_owned(),
            )
        }
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

/// Go `pickViaPluginScheduler` in `pickNextMixedLegacy`: the plugin scheduler sees the
/// ready candidates of the highest priority tier (all tiers when it asks), without
/// secret attributes, and picks one, rejects, or delegates to a built-in strategy;
/// an unusable answer leaves the choice to the configured selector.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plugin_scheduler_picks_rejects_or_delegates() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let fail = Arc::new(std::sync::Mutex::new(String::new()));
    let upstream = keyed_upstream(seen.clone(), fail.clone()).await;
    let yaml = format!(
        "openai-compatibility:\n  - name: up\n    base-url: {upstream}/v1\n    priority: 5\n    api-key-entries: [{{api-key: k-a}}, {{api-key: k-b}}]\n    models: [{{name: up-model}}]\n  - name: low\n    base-url: {upstream}/v1\n    api-key-entries: [{{api-key: k-low}}]\n    models: [{{name: up-model}}]\n"
    );
    // Codex live calls answer 201 with the `Thread-Id` as the call ID.
    let live_upstream = {
        let handler = |headers: axum::http::HeaderMap| async move {
            let id = headers["thread-id"].to_str().unwrap().to_owned();
            (
                axum::http::StatusCode::CREATED,
                [
                    ("location", format!("/v1/live/{id}")),
                    ("content-type", "application/sdp".to_owned()),
                ],
                "v=0",
            )
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, axum::Router::new().fallback(handler))
                .await
                .unwrap()
        });
        url
    };
    for across in [false, true] {
        // The second run also turns session affinity on.
        let (caps, yaml) = if across {
            (
                "scheduler,scheduler_across_priorities",
                format!("{yaml}routing:\n  session-affinity: true\n"),
            )
        } else {
            ("scheduler", yaml.clone())
        };
        let codex = cpa_exec::codex::CodexExecutor::with_client(
            wreq::Client::new(),
            cpa_exec::codex_oauth::CodexOAuth::new(wreq::Client::new()),
        )
        .with_live_endpoints(
            format!("{live_upstream}/backend-api/codex/realtime/calls"),
            format!("ws{}/v1", live_upstream.trim_start_matches("http")),
        );
        let Some(env) = env_with(
            &format!("plugin-scheduler-{across}"),
            &[("sched", caps)],
            &yaml,
            &[(
                "codex.json",
                r#"{"type":"codex","access_token":"at-FAKE","account_id":"acct-FAKE"}"#,
            )],
            |_| {},
            codex,
        )
        .await
        else {
            return;
        };
        let key_of = |id: &str| env.rt.store().get(id).unwrap().attributes["api_key"].clone();
        let respond = |method: &str, once: bool, envelope: Value| {
            let dir = env.records.join("respond").join("sched");
            std::fs::create_dir_all(&dir).unwrap();
            let file = if once {
                format!("{method}.once.json")
            } else {
                format!("{method}.json")
            };
            std::fs::write(dir.join(file), envelope.to_string()).unwrap();
        };
        let picks = || calls(&env.records, "sched", "scheduler.pick");
        // Alpha Search selects one provider (Go `pickNextLegacy`), which the request
        // names, and returns a reject unenriched with Go's 503 fallback status.
        let alpha = || async {
            respond(
                "scheduler.pick",
                true,
                json!({"ok":true,"result":{"handled":true,"reject":true,"reject_code":"quota_hold","reject_reason":"wait"}}),
            );
            let _ = std::fs::remove_file(env.records.join("sched.jsonl"));
            let alpha = wreq::Client::new()
                .post(format!("http://{}/backend-api/codex/alpha/search", env.proxy))
                .header("x-custom", "v")
                .body(r#"{"id":"s-1","model":"gpt-5.5","query":"q"}"#)
                .send()
                .await
                .unwrap();
            assert_eq!(alpha.status().as_u16(), 503);
            assert_eq!(alpha.text().await.unwrap(), r#"{"error":"quota_hold: wait"}"#);
            let pick = picks().pop().expect("the scheduler was asked");
            assert_eq!(pick["Provider"], "codex");
            assert_eq!(pick["Providers"], json!(["codex"]));
            assert_eq!(pick["Options"]["Headers"]["X-Custom"], json!(["v"]));
            // The synthesized session header (Go `Header.Set` casing).
            assert_eq!(pick["Options"]["Headers"]["X-Session-Id"], json!(["s-1"]));
            assert_eq!(pick["Options"]["Metadata"]["session_affinity_provider"], "codex");
            assert!(
                pick["Options"]["Metadata"].get("canonical_session_id").is_none(),
                "no Enrich"
            );
            pick
        };
        let send_full = || async {
            let _ = std::fs::remove_file(env.records.join("sched.jsonl"));
            seen.lock().unwrap().clear();
            let response = wreq::Client::new()
                .post(format!("http://{}/v1/chat/completions", env.proxy))
                .header("content-type", "application/json")
                .header("x-custom", "v")
                .body(r#"{"model":"up-model","messages":[{"role":"user","content":"hi"}]}"#)
                .send()
                .await
                .unwrap();
            let headers = response.headers().clone();
            (response.status().as_u16(), headers, response.text().await.unwrap())
        };
        let send = || async {
            let (status, _, body) = send_full().await;
            (status, body)
        };

        // The recorder picks the last candidate.
        let (status, body) = send().await;
        assert_eq!(status, 200, "{body}");
        let pick = picks().pop().expect("the scheduler was asked");
        let candidates = pick["Candidates"].as_array().unwrap();
        let keys: Vec<String> = candidates.iter().map(|c| key_of(c["ID"].as_str().unwrap())).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        let want: &[&str] = if across {
            &["k-a", "k-b", "k-low"]
        } else {
            &["k-a", "k-b"]
        };
        assert_eq!(sorted, want, "across={across}: the candidates");
        for c in candidates {
            let priority = if key_of(c["ID"].as_str().unwrap()) == "k-low" {
                0
            } else {
                5
            };
            assert_eq!(c["Priority"], priority);
            assert_eq!(c["Status"], "active");
            assert!(
                c["Attributes"].get("api_key").is_none(),
                "secret attributes stay out: {c}"
            );
            assert!(c["Attributes"].get("base_url").is_some(), "{c}");
        }
        let ids: Vec<&str> = candidates.iter().map(|c| c["ID"].as_str().unwrap()).collect();
        let mut by_id = ids.clone();
        by_id.sort();
        assert_eq!(ids, by_id, "candidates are ordered by ID");
        assert_eq!(pick["Provider"], "");
        assert_eq!(pick["Model"], "up-model");
        assert_eq!(pick["Stream"], false);
        assert_eq!(pick["Options"]["Headers"]["X-Custom"], json!(["v"]));
        let meta = &pick["Options"]["Metadata"];
        assert_eq!(meta["session_affinity_provider"], "mixed");
        assert_eq!(meta["session_affinity_model"], if across { "up-model" } else { "" });
        assert_eq!(meta["request_path"], "/v1/chat/completions");
        // Go `session.Enrich`: without an explicit session only the derived identity.
        assert!(
            meta["derived_session_id"].as_str().is_some_and(|d| !d.is_empty()),
            "{meta}"
        );
        assert!(meta.get("canonical_session_id").is_none(), "{meta}");
        assert_eq!(
            *seen.lock().unwrap(),
            [keys.last().unwrap().clone()],
            "the picked credential serves"
        );
        if across {
            // Go's affinity lookup cannot observe bindings a plugin scheduler made.
            let host = env.rt.plugins().clone();
            let instance = Arc::new(cpa_plugin::client::CallbackInstance::default());
            let guard = cpa_plugin::testing::callback_context(
                &host,
                "sched",
                instance.clone(),
                cpa_plugin::callbacks::RequestScope::default(),
            );
            let lookup = tokio::task::spawn_blocking(move || {
                cpa_plugin::testing::call_from_plugin(
                    &host,
                    "sched",
                    &instance,
                    "host.affinity.lookup",
                    br#"{"provider":"up","model":"up-model","session_id":"s1"}"#,
                )
                .unwrap()
            })
            .await
            .unwrap();
            drop(guard);
            let lookup: Value = serde_json::from_slice(&lookup).unwrap();
            assert_eq!(lookup["result"]["status"], "unsupported", "{lookup}");
            // Under session affinity Alpha's selector argument is the route model.
            assert_eq!(
                alpha().await["Options"]["Metadata"]["session_affinity_model"],
                "gpt-5.5"
            );
            env.finish().await;
            continue;
        }

        // An unknown ID leaves the choice to the configured selector.
        respond(
            "scheduler.pick",
            true,
            json!({"ok":true,"result":{"handled":true,"auth_id":"nope"}}),
        );
        assert_eq!(send().await.0, 200);
        assert_eq!(seen.lock().unwrap().len(), 1);

        // Rejects: Go's auth codes are enriched, another code keeps its text.
        respond(
            "scheduler.pick",
            true,
            json!({"ok":true,"result":{"handled":true,"reject":true}}),
        );
        let (status, headers, body) = send_full().await;
        assert_eq!(status, 503, "{body}");
        assert_eq!(
            body,
            r#"{"error":{"message":"auth_unavailable: scheduler rejected candidate selection (providers=openai-compatible-up,openai-compatible-low, model=up-model)","type":"server_error","code":"internal_server_error"}}"#
        );
        assert!(headers.get("retry-after").is_none(), "{headers:?}");
        for c in env.rt.store().snapshot() {
            assert!(
                env.rt.store().cooldowns(&c.id).is_empty(),
                "a reject cools nothing down"
            );
        }
        respond(
            "scheduler.pick",
            true,
            json!({"ok":true,"result":{"handled":true,"reject":true,"reject_code":"quota_hold","reject_reason":"wait"}}),
        );
        let (status, body) = send().await;
        assert_eq!(status, 500, "{body}");
        assert!(body.contains("quota_hold: wait"), "{body}");
        assert!(seen.lock().unwrap().is_empty(), "a reject sends nothing");

        assert_eq!(alpha().await["Options"]["Metadata"]["session_affinity_model"], "");

        // So does realtime, with Go's empty local model; any reject is a 503.
        respond(
            "scheduler.pick",
            true,
            json!({"ok":false,"error":{"code":"busy","message":"scheduler busy"}}),
        );
        let _ = std::fs::remove_file(env.records.join("sched.jsonl"));
        let live = wreq::Client::new()
            .post(format!("http://{}/v1/live", env.proxy))
            .header("content-type", "application/json")
            .header("x-custom", "v")
            .body(r#"{"sdp":"v=0"}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(live.status().as_u16(), 503, "a statusless scheduler error");
        assert!(live.text().await.unwrap().contains("scheduler busy"));
        let pick = picks().pop().expect("the scheduler was asked");
        assert_eq!(pick["Provider"], "codex");
        assert_eq!(pick["Model"], "");
        assert_eq!(pick["Options"]["Headers"]["X-Custom"], json!(["v"]));
        assert_eq!(pick["Options"]["Metadata"]["session_affinity_provider"], "codex");
        assert_eq!(pick["Options"]["Metadata"]["session_affinity_model"], "");
        assert!(pick["Options"]["Metadata"].get("pinned_auth_id").is_none());

        // An existing call's selection (here a hangup) names its credential and call.
        let codex_id = env
            .rt
            .store()
            .snapshot()
            .into_iter()
            .find(|c| c.provider == "codex")
            .unwrap()
            .id
            .clone();
        respond(
            "scheduler.pick",
            true,
            json!({"ok":true,"result":{"handled":true,"auth_id":codex_id}}),
        );
        let created = wreq::Client::new()
            .post(format!("http://{}/v1/live", env.proxy))
            .header("content-type", "application/json")
            .header("thread-id", "call-1")
            .body(r#"{"sdp":"v=0"}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(created.status().as_u16(), 201, "{}", created.text().await.unwrap());
        respond(
            "scheduler.pick",
            true,
            json!({"ok":true,"result":{"handled":true,"reject":true,"reject_code":"quota_hold","reject_reason":"wait"}}),
        );
        let _ = std::fs::remove_file(env.records.join("sched.jsonl"));
        let hangup = wreq::Client::new()
            .post(format!("http://{}/v1/realtime/calls/call-1/hangup", env.proxy))
            .send()
            .await
            .unwrap();
        assert_eq!(hangup.status().as_u16(), 503);
        let meta = picks().pop().expect("the scheduler was asked")["Options"]["Metadata"].clone();
        assert_eq!(meta["pinned_auth_id"], codex_id.as_str(), "{meta}");
        assert_eq!(meta["execution_session_id"], "call-1", "{meta}");
        assert_eq!(meta["session_affinity_model"], "", "{meta}");

        // A session Go's `ExtractSessionInfo` sees but `hasExplicitSession` does not
        // (`metadata.sessionID`) leaves only the derived identity, and a prompt cache key
        // is canonical without its conversation alias as a parent.
        for (body, canonical) in [
            (
                r#"{"model":"up-model","metadata":{"sessionID":"x"},"messages":[{"role":"user","content":"hi"}]}"#,
                None,
            ),
            (
                r#"{"model":"up-model","prompt_cache_key":"p1","conversation":{"id":"c1"},"messages":[{"role":"user","content":"hi"}]}"#,
                Some("pck:p1"),
            ),
        ] {
            let _ = std::fs::remove_file(env.records.join("sched.jsonl"));
            let response = wreq::Client::new()
                .post(format!("http://{}/v1/chat/completions", env.proxy))
                .header("content-type", "application/json")
                .body(body)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), 200);
            let meta = picks().pop().expect("the scheduler was asked")["Options"]["Metadata"].clone();
            match canonical {
                None => {
                    assert!(meta.get("canonical_session_id").is_none(), "{meta}");
                    assert!(
                        meta["derived_session_id"].as_str().is_some_and(|d| !d.is_empty()),
                        "{meta}"
                    );
                }
                Some(canonical) => {
                    assert_eq!(meta["canonical_session_id"], canonical, "{meta}");
                    assert!(meta.get("parent_session_id").is_none(), "{meta}");
                    assert!(meta.get("derived_session_id").is_none(), "{meta}");
                }
            }
        }

        // An explicit session is canonical, with its parent; no derived identity.
        let _ = std::fs::remove_file(env.records.join("sched.jsonl"));
        let explicit = wreq::Client::new()
            .post(format!("http://{}/v1/chat/completions", env.proxy))
            .header("content-type", "application/json")
            .header("x-session-id", "child")
            .body(r#"{"model":"up-model","parent_session_id":"parent","messages":[{"role":"user","content":"hi"}]}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(explicit.status().as_u16(), 200);
        let meta = picks().pop().expect("the scheduler was asked")["Options"]["Metadata"].clone();
        assert_eq!(meta["canonical_session_id"], "header:child", "{meta}");
        assert_eq!(meta["parent_session_id"], "header:parent", "{meta}");
        assert!(meta.get("derived_session_id").is_none(), "{meta}");
        seen.lock().unwrap().clear();

        // The scheduler's own failure answers with its status.
        respond(
            "scheduler.pick",
            true,
            json!({"ok":false,"error":{"code":"busy","message":"scheduler busy","http_status":429}}),
        );
        let (status, body) = send().await;
        assert_eq!(status, 429, "{body}");

        // Delegation to the built-in strategies, within the top tier.
        respond(
            "scheduler.pick",
            false,
            json!({"ok":true,"result":{"handled":true,"delegate_builtin":"fill-first"}}),
        );
        let mut served = Vec::new();
        for _ in 0..3 {
            assert_eq!(send().await.0, 200);
            served.extend(seen.lock().unwrap().clone());
        }
        // Fill-first takes the top tier's first credential by ID.
        let first_by_id = candidates
            .iter()
            .map(|c| c["ID"].as_str().unwrap())
            .min()
            .map(key_of)
            .unwrap();
        assert_eq!(served, [first_by_id.as_str(); 3], "fill-first stays on one credential");
        respond(
            "scheduler.pick",
            false,
            json!({"ok":true,"result":{"handled":true,"delegate_builtin":"round-robin"}}),
        );
        let mut served = Vec::new();
        for _ in 0..4 {
            assert_eq!(send().await.0, 200);
            served.extend(seen.lock().unwrap().clone());
        }
        served.sort();
        assert_eq!(served, ["k-a", "k-a", "k-b", "k-b"], "round-robin alternates");

        // A failed attempt is not offered again: the retry's candidates exclude it.
        std::fs::remove_file(env.records.join("respond").join("sched").join("scheduler.pick.json")).unwrap();
        let first = keys.last().unwrap().clone();
        *fail.lock().unwrap() = first.clone();
        let (status, body) = send().await;
        assert_eq!(status, 200, "{body}");
        let picks = picks();
        assert_eq!(picks.len(), 2, "one pick per attempt");
        let retry: Vec<String> = picks[1]["Candidates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| key_of(c["ID"].as_str().unwrap()))
            .collect();
        assert!(!retry.contains(&first), "{retry:?}");
        fail.lock().unwrap().clear();
        env.finish().await;
    }
}

/// Go's plugin availability pass does not filter by weight: under weighted round-robin
/// a ready weight-0 credential is offered and can be picked, and with every weight at
/// zero the scheduler is still asked. A picked credential that stopped being ready
/// while the plugin answered falls back to the configured selector (Go serves it from
/// its snapshot; docs/DIFFERENCES-FROM-GO.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plugin_scheduler_sees_zero_weights_and_a_stale_pick_falls_back() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let fail = Arc::new(std::sync::Mutex::new(String::new()));
    let upstream = keyed_upstream(seen.clone(), fail).await;
    let yaml = format!(
        "routing:\n  strategy: weighted-round-robin\nopenai-compatibility:\n  - name: up\n    base-url: {upstream}/v1\n    api-key-entries: [{{api-key: k-a, weight: 0}}, {{api-key: k-b, weight: 1}}]\n    models: [{{name: up-model}}]\n"
    );
    let Some(env) = env(
        "plugin-scheduler-weights",
        &[("sched", "scheduler")],
        &yaml,
        &[],
        |_| {},
    )
    .await
    else {
        return;
    };
    let credential = |key: &str| {
        env.rt
            .store()
            .snapshot()
            .into_iter()
            .find(|c| c.attributes.get("api_key").map(String::as_str) == Some(key))
            .unwrap()
    };
    let dir = env.records.join("respond").join("sched");
    std::fs::create_dir_all(&dir).unwrap();
    let pick_once = |key: &str| {
        let envelope = json!({"ok":true,"result":{"handled":true,"auth_id":credential(key).id}});
        std::fs::write(dir.join("scheduler.pick.once.json"), envelope.to_string()).unwrap();
    };
    let picks = || calls(&env.records, "sched", "scheduler.pick");
    let send = || async {
        seen.lock().unwrap().clear();
        let response = wreq::Client::new()
            .post(format!("http://{}/v1/chat/completions", env.proxy))
            .header("content-type", "application/json")
            .body(r#"{"model":"up-model","messages":[{"role":"user","content":"hi"}]}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 200, "{}", response.text().await.unwrap());
        seen.lock().unwrap().clone()
    };

    // The weight-0 credential is offered and serves when picked.
    pick_once("k-a");
    assert_eq!(send().await, ["k-a"]);
    let offered: Vec<Value> = picks().pop().unwrap()["Candidates"].as_array().unwrap().clone();
    assert_eq!(offered.len(), 2, "{offered:?}");

    // k-a stops being ready while the plugin answers: weighted selection serves k-b.
    pick_once("k-a");
    let hold = dir.join("scheduler.pick.hold");
    std::fs::write(&hold, "").unwrap();
    let before = picks().len();
    let disable = async {
        while picks().len() == before {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let mut off = (*credential("k-a")).clone();
        off.disabled = true;
        env.rt.store().upsert(off);
        std::fs::remove_file(&hold).unwrap();
    };
    let (served, ()) = tokio::join!(send(), disable);
    assert_eq!(served, ["k-b"]);

    // Every weight at zero: the scheduler is still asked, and its pick serves.
    let mut zero = (*credential("k-b")).clone();
    zero.attributes.insert("weight".into(), "0".into());
    env.rt.store().upsert(zero);
    pick_once("k-b");
    let before = picks().len();
    assert_eq!(send().await, ["k-b"]);
    assert_eq!(picks().len(), before + 1);
    env.finish().await;
}

/// Go keeps the cooldown skip and the retry round apart: a statusless lifecycle text
/// (`context deadline exceeded`) skips the cooldown but earns no request-retry round,
/// while transient transport text (`broken pipe`) earns one (`isRequestRetryRoundError`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn statusless_plugin_errors_earn_retry_rounds_only_for_transport_text() {
    for retry in [0, 1] {
        let Some(env) = env(
            &format!("plugin-owned-retry-{retry}"),
            &[("catalog", "model_provider"), ("acme", "executor")],
            &format!("request-retry: {retry}\n"),
            &[
                ("acme-a.json", r#"{"type":"acme"}"#),
                ("acme-b.json", r#"{"type":"acme"}"#),
            ],
            |records| {
                canned(
                    records,
                    "catalog",
                    "model.static",
                    json!({"Provider":"acme","Models":[{"ID":"acme-1"}]}),
                )
            },
        )
        .await
        else {
            return;
        };
        for (message, rounds) in [("context deadline exceeded", 1), ("broken pipe", 1 + retry)] {
            let case = format!("request-retry {retry}, {message}");
            let dir = env.records.join("respond").join("acme");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("executor.execute.json"),
                json!({"ok":false,"error":{"code":"x","message":message}}).to_string(),
            )
            .unwrap();
            let _ = std::fs::remove_file(env.records.join("acme.jsonl"));
            let (got, body) = env.chat("acme-1").await;
            assert_ne!(got, 200, "{case}: {body}");
            let attempts = calls(&env.records, "acme", "executor.execute").len();
            assert_eq!(attempts, 2 * rounds, "{case}: both credentials once per round");
            for id in ["acme-a.json", "acme-b.json"] {
                assert!(env.rt.store().cooldowns(id).is_empty(), "{case}: {id} does not cool");
            }
        }
        env.finish().await;
    }
}

/// Go hands a plugin stream's chunk error to the auth manager's stream bootstrap, which
/// classifies it like an execution error: an error before the first payload fails over
/// (conductor_stream.go:345-460); one after payload has been sent ends the stream.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plugin_stream_failing_before_its_first_chunk_fails_over() {
    let Some(env) = env(
        "plugin-owned-stream-failover",
        &[("catalog", "model_provider"), ("acme", "executor")],
        "",
        &[
            ("acme-a.json", r#"{"type":"acme"}"#),
            ("acme-b.json", r#"{"type":"acme"}"#),
        ],
        |records| {
            canned(
                records,
                "catalog",
                "model.static",
                json!({"Provider":"acme","Models":[{"ID":"acme-1"}]}),
            )
        },
    )
    .await
    else {
        return;
    };
    let script = |chunks: Value, error: &str| {
        let dir = env.records.join("respond").join("acme");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("executor.execute_stream.script.once.json"),
            json!({"chunks":chunks,"error":error}).to_string(),
        )
        .unwrap();
        let _ = std::fs::remove_file(env.records.join("acme.jsonl"));
    };
    let attempts = || -> Vec<String> {
        calls(&env.records, "acme", "executor.execute_stream")
            .into_iter()
            .map(|r| r["AuthID"].as_str().unwrap().to_owned())
            .collect()
    };
    // (first-chunk error, cooldown of the failed credential in seconds).
    for (error, cool) in [("plugin stream broke", Some(60)), ("broken pipe", None)] {
        script(json!([]), error);
        let (got, body) = env.chat_stream("acme-1").await;
        assert_eq!(got, 200, "{error}: {body}");
        assert!(body.contains("one") && body.contains("three"), "{error}: {body}");
        let ids = attempts();
        assert_eq!(ids.len(), 2, "{error}: {ids:?}");
        assert_ne!(ids[0], ids[1], "{error}: the second attempt uses the other credential");
        let cooling = env.rt.store().cooldowns(&ids[0]);
        match cool {
            None => assert!(cooling.is_empty(), "{error}: {cooling:?}"),
            Some(secs) => {
                assert_eq!(cooling.len(), 1, "{error}: {cooling:?}");
                assert_eq!((cooling[0].model.as_str(), cooling[0].status), ("acme-1", 0), "{error}");
                let left = cooling[0].remaining;
                assert!(
                    left <= Duration::from_secs(secs) && left > Duration::from_secs(secs - 5),
                    "{error}: {left:?}"
                );
            }
        }
        for id in ["acme-a.json", "acme-b.json"] {
            env.rt.store().reset_cooldown(id);
        }
    }
    // After payload has reached the client the stream ends; there is no second attempt.
    script(json!(["data: early\n\n"]), "plugin stream broke late");
    let (got, body) = env.chat_stream("acme-1").await;
    assert_eq!(got, 200, "{body}");
    assert!(body.contains("early") && !body.contains("three"), "{body}");
    assert_eq!(attempts().len(), 1);
    env.finish().await;
}

/// A plugin executor may claim a built-in provider while no credential of it exists, but
/// Go rebinds the built-in executor as soon as such a credential arrives
/// (`registerExecutorForAuth`), before any plugin sync. Credentials reach the store
/// without a sync both from a management publish (the store reconciles after the sync is
/// signalled) and from a plugin login (`upsert`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_builtin_credential_never_reaches_a_plugin_executor_for_its_provider() {
    let Some(env) = env(
        "plugin-owned-builtin-rebind",
        &[("catalog", "model_provider"), ("claude", "executor")],
        "",
        &[],
        |records| {
            canned(
                records,
                "catalog",
                "model.static",
                json!({"Provider":"claude","Models":[{"ID":"claude-plug-1"}]}),
            )
        },
    )
    .await
    else {
        return;
    };
    let cfg = env.rt.config();
    let credential = |file: &str| {
        let path = cfg.auth_dir.join(file);
        let body = br#"{"type":"claude","access_token":"fake"}"#;
        std::fs::write(&path, body).unwrap();
        cpa_core::config::credentials::from_file(&cfg, &cfg.auth_dir, &path, body)
            .unwrap()
            .unwrap()
    };
    let check = |stage: &str, (got, body): (u16, String)| {
        assert_ne!(got, 200, "{stage}: the built-in executor's dead upstream: {body}");
        assert!(
            calls(&env.records, "claude", "executor.execute").is_empty(),
            "{stage}: the plugin executor served a built-in credential"
        );
    };
    // A management publish: the store reconciles with the new file, no sync follows.
    let mut all: Vec<_> = env.rt.store().snapshot().iter().map(|c| (**c).clone()).collect();
    all.push(credential("claude-a.json"));
    env.rt.store().reconcile(all);
    check("publish", env.chat("claude-plug-1").await);
    // A plugin login: the credential is upserted, no sync follows.
    let rest: Vec<_> = env
        .rt
        .store()
        .snapshot()
        .iter()
        .filter(|c| c.provider != "claude")
        .map(|c| (**c).clone())
        .collect();
    env.rt.store().reconcile(rest);
    env.rt.store().upsert(credential("claude-b.json"));
    check("plugin login", env.chat("claude-plug-1").await);
    env.finish().await;
}

/// The executor table keeps the plugin instance it registered (Go `executorAdapter`
/// with path and version). A hot replacement that identifies another provider must
/// not receive the old provider's credentials before its own registration commits.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_replaced_executor_plugin_does_not_serve_its_predecessors_credentials() {
    let Some(env) = env(
        "plugin-owned-replace",
        &[("catalog", "model_provider"), ("acme", "executor")],
        "",
        &[("acme.json", r#"{"type":"acme","token":"t-1"}"#)],
        |records| {
            canned(
                records,
                "catalog",
                "model.static",
                json!({"Provider":"acme","Models":[{"ID":"acme-1"}]}),
            )
        },
    )
    .await
    else {
        return;
    };
    assert_eq!(env.chat("acme-1").await.0, 200);
    // v2 identifies as `acme2`. Apply it on the host alone: the plugin sync that would
    // re-register executors has not run yet.
    std::fs::copy(env.plugins.join("acme.so"), env.plugins.join("acme-v2.so")).unwrap();
    let yaml = env.yaml.replace("label: acme\n", "label: acme2\n");
    env.rt
        .plugins()
        .apply_config(Arc::new(Config::parse(&yaml).unwrap()))
        .await;
    let active = env.rt.plugins().active_records();
    assert!(active.iter().any(|r| r.id == "acme" && r.version == "2"), "v2 is live");
    let (status, _) = env.chat("acme-1").await;
    assert_ne!(status, 200);
    assert!(
        calls(&env.records, "acme2", "executor.execute").is_empty(),
        "v2 received a credential registered for v1"
    );
    env.finish().await;
}

/// Removing a built-in provider's catalog frees its models for a plugin executor in
/// the same sync, although ownership was first decided against the old catalog.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removing_a_builtin_catalog_hands_its_model_to_a_plugin_executor() {
    let codex = "api-keys:\n  codex:\n    - base-url: http://127.0.0.1:1\n      models: [{name: gpt-fixture}]\n      keys: [{api-key: sk-fake}]\n";
    let Some(env) = env(
        "plugin-owned-handover",
        &[("catalog", "model_provider"), ("acme", "model_provider,executor")],
        codex,
        &[("acme.json", r#"{"type":"acme"}"#)],
        |records| {
            canned(
                records,
                "catalog",
                "model.static",
                json!({"Provider":"codex","Models":[{"ID":"shared"}]}),
            );
            canned(
                records,
                "acme",
                "model.static",
                json!({"Provider":"acme","Models":[{"ID":"shared"}]}),
            );
        },
    )
    .await
    else {
        return;
    };
    assert!(
        !env.rt.registry().client_supports("acme.json", "shared"),
        "reserved for codex"
    );
    let yaml = env.yaml.replace(
        "    catalog:\n      enabled: true\n",
        "    catalog:\n      enabled: false\n",
    );
    assert_ne!(yaml, env.yaml);
    cpa_server::plugins::sync(&env.rt, Arc::new(Config::parse(&yaml).unwrap())).await;
    assert!(env.rt.registry().client_supports("acme.json", "shared"));
    let (status, body) = env.chat("shared").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(calls(&env.records, "acme", "executor.execute").len(), 1);
    env.finish().await;
}

/// Go binds a built-in executor to an unknown provider (the OpenAI-compatible one)
/// only when no plugin executor is a candidate for it or a native compatibility config
/// names it. An auth file typed `openai-compatible-acme` without config goes to the
/// plugin; a configured entry, with keys or without, keeps the built-in executor.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plugin_executor_takes_an_unconfigured_compatible_provider() {
    let catalog = |records: &Path| {
        canned(
            records,
            "acme",
            "model.static",
            json!({"Provider":"openai-compatible-acme","Models":[{"ID":"acme-m"}]}),
        )
    };
    let plugins = [("acme", "model_provider,executor")];
    let Some(unconfigured) = env(
        "plugin-owned-compat-candidate",
        &plugins,
        "",
        &[("acme.json", r#"{"type":"openai-compatible-acme"}"#)],
        catalog,
    )
    .await
    else {
        return;
    };
    let (status, body) = unconfigured.chat("acme-m").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(calls(&unconfigured.records, "acme", "executor.execute").len(), 1);
    unconfigured.finish().await;
    for keys in ["    api-key-entries: [{api-key: k1}]\n", ""] {
        let config = format!("openai-compatibility:\n  - name: acme\n    base-url: http://127.0.0.1:1/v1\n{keys}");
        let Some(configured) = env("plugin-owned-compat-native", &plugins, &config, &[], catalog).await else {
            return;
        };
        let env = configured;
        let case = if keys.is_empty() { "keyless" } else { "keyed" };
        let credential = env
            .rt
            .store()
            .snapshot()
            .into_iter()
            .find(|c| c.attributes.get("compat_name").map(String::as_str) == Some("acme"))
            .unwrap_or_else(|| panic!("{case}: a configured credential"));
        assert!(
            env.rt.registry().client_supports(&credential.id, "acme-m"),
            "{case}: the catalog binds to the configured credential"
        );
        assert_ne!(env.chat("acme-m").await.0, 200, "{case}: the dead native upstream");
        assert!(
            calls(&env.records, "acme", "executor.execute").is_empty(),
            "{case}: the built-in executor keeps the provider"
        );
        env.finish().await;
    }
}

/// The next `count` usage records, as JSON; a stream's record comes as it ends.
async fn usage_records(rt: &cpa_server::Runtime, count: usize) -> Vec<Value> {
    let mut records = Vec::new();
    for _ in 0..200 {
        records.extend(rt.usage_queue().pop_oldest(count - records.len()));
        if records.len() == count {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(records.len(), count);
    records.iter().map(|r| serde_json::from_slice(r).unwrap()).collect()
}

const USAGE_ON: &str = "observability:\n  usage:\n    usage-statistics-enabled: true\n";

/// Go `TestExecutorAdapterExecuteAttributesResponsesUsageToSelectedAuth`: a
/// credential-bound plugin executor's buffered answer publishes its usage, read from
/// the plugin's Responses output (an event's `response.usage`, else the completed
/// object's top-level usage), with the reasoning effort of the payload it was sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plugin_executor_publishes_the_usage_of_its_own_answer() {
    let Some(env) = env(
        "plugin-owned-usage",
        &[
            ("catalog", "model_provider"),
            ("acme", "executor\n      inputs: responses\n      outputs: responses"),
        ],
        USAGE_ON,
        &[("acme.json", r#"{"type":"acme"}"#)],
        |records| {
            canned(
                records,
                "catalog",
                "model.static",
                json!({"Provider":"acme","Models":[{"ID":"acme-1"}]}),
            )
        },
    )
    .await
    else {
        return;
    };
    env.rt.usage_queue().configure(true, &env.rt.config());
    let answers = [
        // A completed Responses object: top-level usage.
        (
            r#"{"id":"resp_1","object":"response","service_tier":"default","usage":{"input_tokens":34,"output_tokens":499,"total_tokens":533}}"#,
            (34, 499, 533),
        ),
        // A terminal event: `response.usage`, which the client-format parser misses.
        (
            r#"{"type":"response.completed","response":{"id":"resp_2","usage":{"input_tokens":5,"output_tokens":7,"total_tokens":12}}}"#,
            (5, 7, 12),
        ),
    ];
    for (answer, expected) in answers {
        let payload = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, answer);
        canned(&env.records, "acme", "executor.execute", json!({"Payload": payload}));
        let response = wreq::Client::new()
            .post(format!("http://{}/v1/responses", env.proxy))
            .header("content-type", "application/json")
            .body(r#"{"model":"acme-1","input":"Write me a poem","reasoning":{"effort":"high"}}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 200, "{answer}");
        let record = &usage_records(&env.rt, 1).await[0];
        assert_eq!(record["provider"], "acme", "{record}");
        assert_eq!(record["executor_type"], "executorAdapter", "{record}");
        assert_eq!(record["stream"], false, "{record}");
        assert_eq!(record["failed"], false, "{record}");
        assert_eq!(record["reasoning_effort"], "high", "{record}");
        let tokens = &record["tokens"];
        let got = (
            tokens["input_tokens"].as_i64().unwrap(),
            tokens["output_tokens"].as_i64().unwrap(),
            tokens["total_tokens"].as_i64().unwrap(),
        );
        assert_eq!(got, expected, "{record}");
    }
    env.finish().await;
}

/// Go `TestExecutorAdapterExecuteStreamSplitAcrossBraceBoundary`,
/// `…TwoCompleteChunksWithoutNewline` and `…ErrorChunk`: a plugin stream's usage is
/// read from its own lines, however its chunks split them, and a stream that fails
/// after its usage line publishes the failure with that usage
/// (`StreamUsageBuffer.PublishFailure`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plugin_executor_stream_publishes_its_usage_even_when_it_fails() {
    let Some(env) = env(
        "plugin-owned-stream-usage",
        &[("catalog", "model_provider"), ("acme", "executor")],
        USAGE_ON,
        &[("acme.json", r#"{"type":"acme"}"#)],
        |records| {
            canned(
                records,
                "catalog",
                "model.static",
                json!({"Provider":"acme","Models":[{"ID":"acme-1"}]}),
            )
        },
    )
    .await
    else {
        return;
    };
    env.rt.usage_queue().configure(true, &env.rt.config());
    let script = |chunks: Value, error: &str| {
        let dir = env.records.join("respond").join("acme");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("executor.execute_stream.script.once.json"),
            json!({"chunks":chunks,"error":error}).to_string(),
        )
        .unwrap();
    };
    let tokens = |record: &Value| {
        let t = &record["tokens"];
        (
            t["input_tokens"].as_i64().unwrap(),
            t["output_tokens"].as_i64().unwrap(),
            t["total_tokens"].as_i64().unwrap(),
        )
    };
    // Split just before the usage object's opening brace.
    script(
        json!([
            "data: {\"choices\":[],\"usage\":",
            "{\"prompt_tokens\":18,\"completion_tokens\":22,\"total_tokens\":40}}\n\n",
            "data: [DONE]\n\n"
        ]),
        "",
    );
    let (got, body) = env.chat_stream("acme-1").await;
    assert_eq!(got, 200, "{body}");
    let record = &usage_records(&env.rt, 1).await[0];
    assert_eq!(
        (record["failed"].clone(), tokens(record)),
        (json!(false), (18, 22, 40)),
        "{record}"
    );
    // Complete payloads without newlines, then a failure.
    script(
        json!([
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":22,\"total_tokens\":33}}"
        ]),
        "mid-stream chunk failure",
    );
    let (got, body) = env.chat_stream("acme-1").await;
    assert_eq!(got, 200, "{body}");
    let record = &usage_records(&env.rt, 1).await[0];
    assert_eq!(
        (record["failed"].clone(), tokens(record)),
        (json!(true), (11, 22, 33)),
        "{record}"
    );
    assert_eq!(record["fail"]["body"], "mid-stream chunk failure", "{record}");
    env.finish().await;
}

/// Go `NewExecutorUsageReporter(ctx, a, …)` for a credential-bound plugin attempt whose
/// formats differ from the client's: exactly one record per attempt, typed as the
/// host's `executorAdapter` for the plugin's provider, with usage read in the plugin's
/// Claude output (the client's chat format misses its cache reads) and the reasoning effort
/// of the translated payload (`max` becomes a 128000-token budget, which reads back as
/// `xhigh`). A stream the client drops publishes one 499 "context canceled" record
/// keeping the usage seen so far. Counting tokens and the model router's credential-less
/// attempts create no reporter, so they publish nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plugin_executor_records_each_attempt_once_in_its_own_format() {
    let Some(env) = env(
        "plugin-owned-usage-formats",
        &[
            ("catalog", "model_provider"),
            ("acme", "executor\n      inputs: claude\n      outputs: claude"),
            ("rt", "model_router,executor"),
        ],
        USAGE_ON,
        &[("acme.json", r#"{"type":"acme"}"#)],
        |records| {
            canned(
                records,
                "catalog",
                "model.static",
                json!({"Provider":"acme","Models":[{"ID":"acme-1"}]}),
            )
        },
    )
    .await
    else {
        return;
    };
    env.rt.usage_queue().configure(true, &env.rt.config());
    let no_more_records = || async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let extra = env.rt.usage_queue().pop_oldest(10);
        assert!(
            extra.is_empty(),
            "{:?}",
            extra.iter().map(|r| String::from_utf8_lossy(r)).collect::<Vec<_>>()
        );
    };
    let encode = |s: &str| base64::Engine::encode(&base64::engine::general_purpose::STANDARD, s);
    let tokens = |record: &Value| {
        let t = &record["tokens"];
        (
            t["input_tokens"].as_i64().unwrap(),
            t["output_tokens"].as_i64().unwrap(),
            t["cache_read_tokens"].as_i64().unwrap(),
        )
    };

    // Buffered.
    canned(
        &env.records,
        "acme",
        "executor.execute",
        json!({"Payload": encode(
            r#"{"id":"msg_1","type":"message","role":"assistant","model":"acme-1","content":[{"type":"text","text":"hi"}],"stop_reason":"end_turn","usage":{"input_tokens":34,"output_tokens":499,"cache_read_input_tokens":6}}"#
        )}),
    );
    let (got, body) = env
        .post(r#"{"model":"acme-1","reasoning_effort":"max","messages":[{"role":"user","content":"hi"}]}"#.into())
        .await;
    assert_eq!(got, 200, "{body}");
    let sent = calls(&env.records, "acme", "executor.execute");
    let payload = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        sent[0]["Payload"].as_str().unwrap(),
    )
    .unwrap();
    let payload: Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(payload["thinking"]["budget_tokens"], 128000, "{payload}");
    let record = &usage_records(&env.rt, 1).await[0];
    no_more_records().await;
    assert_eq!(
        (
            &record["provider"],
            &record["executor_type"],
            &record["reasoning_effort"]
        ),
        (&json!("acme"), &json!("executorAdapter"), &json!("xhigh")),
        "{record}"
    );
    assert_eq!(
        (record["failed"].clone(), tokens(record)),
        (json!(false), (34, 499, 6)),
        "{record}"
    );

    // A stream the client drops while the plugin holds it open. Each chunk is one
    // `data:` line, as Go's Claude translator reads a chunk.
    let dir = env.records.join("respond").join("acme");
    std::fs::write(dir.join("executor.execute_stream.close.hold"), "").unwrap();
    std::fs::write(
        dir.join("executor.execute_stream.script.once.json"),
        json!({"chunks": [
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_2\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"acme-1\",\"content\":[],\"usage\":{\"input_tokens\":21,\"output_tokens\":0,\"cache_read_input_tokens\":4}}}\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":null},\"usage\":{\"input_tokens\":21,\"output_tokens\":9,\"cache_read_input_tokens\":4}}\n"
        ], "error": ""})
        .to_string(),
    )
    .unwrap();
    {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let body = r#"{"model":"acme-1","stream":true,"messages":[{"role":"user","content":"hi"}]}"#;
        let mut conn = tokio::net::TcpStream::connect(env.proxy).await.unwrap();
        let head = format!(
            "POST /v1/chat/completions HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
            env.proxy,
            body.len()
        );
        conn.write_all(head.as_bytes()).await.unwrap();
        let mut seen = Vec::new();
        let mut buf = [0u8; 4096];
        while !String::from_utf8_lossy(&seen).contains("data:") {
            let n = tokio::time::timeout(Duration::from_secs(5), conn.read(&mut buf))
                .await
                .unwrap_or_else(|_| panic!("no first event: {}", String::from_utf8_lossy(&seen)))
                .unwrap();
            assert!(n > 0, "stream ended early: {}", String::from_utf8_lossy(&seen));
            seen.extend_from_slice(&buf[..n]);
        }
        // Both chunks are emitted before the hold; give the second one time to land.
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let record = &usage_records(&env.rt, 1).await[0];
    std::fs::remove_file(dir.join("executor.execute_stream.close.hold")).unwrap();
    no_more_records().await;
    assert_eq!(
        (
            record["failed"].clone(),
            record["fail"]["status_code"].clone(),
            record["fail"]["body"].clone(),
            tokens(record)
        ),
        (json!(true), json!(499), json!("context canceled"), (21, 9, 4)),
        "{record}"
    );
    assert_eq!(record["executor_type"], "executorAdapter", "{record}");

    // Counting tokens.
    canned(
        &env.records,
        "acme",
        "executor.count_tokens",
        json!({"Payload": encode(r#"{"input_tokens":5}"#)}),
    );
    let response = wreq::Client::new()
        .post(format!("http://{}/v1/messages/count_tokens", env.proxy))
        .header("content-type", "application/json")
        .body(r#"{"model":"acme-1","messages":[{"role":"user","content":"hi"}]}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200, "{}", response.text().await.unwrap());
    assert_eq!(calls(&env.records, "acme", "executor.count_tokens").len(), 1);
    no_more_records().await;

    // The model router's own executor, without a credential.
    std::fs::create_dir_all(env.records.join("respond").join("rt")).unwrap();
    std::fs::write(
        env.records.join("respond").join("rt").join("model.route.once.json"),
        json!({"ok":true,"result":{"Handled":true,"TargetKind":"self","Target":"rt","Reason":"test"}}).to_string(),
    )
    .unwrap();
    canned(
        &env.records,
        "rt",
        "executor.execute",
        json!({"Payload": encode(
            r#"{"id":"c1","object":"chat.completion","choices":[],"usage":{"prompt_tokens":3,"completion_tokens":4,"total_tokens":7}}"#
        )}),
    );
    let (got, body) = env.chat("routed-1").await;
    assert_eq!(got, 200, "{body}");
    assert_eq!(calls(&env.records, "rt", "executor.execute").len(), 1);
    no_more_records().await;
    env.finish().await;
}
