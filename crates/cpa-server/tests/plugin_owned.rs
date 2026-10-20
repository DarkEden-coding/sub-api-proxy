//! Go v8.0.35 plugin-owned providers without a model router: a catalog-only plugin's
//! models bind to the credentials of their provider (with the credential's excluded
//! models applied), and a plugin executor serves those credentials through the
//! ordinary selection loop. Private auth-dir, native test plugins, no upstream.
#![cfg(unix)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
            codex: Default::default(),
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
        let response = wreq::Client::new()
            .post(format!("http://{}/v1/chat/completions", self.proxy))
            .header("content-type", "application/json")
            .body(format!(
                r#"{{"model":"{model}","messages":[{{"role":"user","content":"hi"}}]}}"#
            ))
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
    let tmp = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let Some(built) = cpa_plugin::testing::built_plugins(tmp, "plugin_owned") else {
        return;
    };
    let dir = cpa_plugin::testing::scratch(tmp, "plugin-owned");
    let (auths, records, plugins) = (dir.join("auths"), dir.join("records"), dir.join("plugins"));
    for d in [&auths, &records, &plugins] {
        std::fs::create_dir_all(d).unwrap();
    }
    // `catalog` lists models for provider `acme`; `acme` executes them.
    let mut yaml = format!(
        "auth-dir: {}\nplugins:\n  enabled: true\n  dir: {}\n  configs:\n",
        auths.display(),
        plugins.display()
    );
    for (label, caps) in [("catalog", "model_provider"), ("acme", "executor")] {
        std::fs::copy(built.join("recorder.so"), plugins.join(format!("{label}.so"))).unwrap();
        yaml.push_str(&format!(
            "    {label}:\n      enabled: true\n      label: {label}\n      record: {}\n      caps: {caps}\n",
            records.display()
        ));
    }
    canned(
        &records,
        "catalog",
        "model.static",
        json!({"Provider":"ACME","Models":[{"ID":" acme-1 ","OwnedBy":"acme"},{"ID":"acme-hidden"}]}),
    );
    std::fs::write(
        auths.join("acme.json"),
        r#"{"type":"acme","token":"t-1","excluded_models":["acme-hidden"]}"#,
    )
    .unwrap();
    let cfg = Config::parse(&yaml).unwrap();
    let credentials = cpa_core::config::credentials::load(&cfg);
    let rt = Arc::new(cpa_server::testing::runtime(
        cfg,
        credentials,
        Executors {
            claude: cpa_exec::claude::ClaudeExecutor::new("http://127.0.0.1:1").unwrap(),
            codex: Default::default(),
            devices: Default::default(),
            openai: Default::default(),
            google: Default::default(),
        },
    ));
    cpa_server::plugins::start(&rt).await;
    let registry = rt.registry();
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
    let proxy = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = cpa_server::router(rt.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        addr
    };
    let client = wreq::Client::new();
    let models: Value = client
        .get(format!("http://{proxy}/v1/models"))
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
    let body = r#"{"model":"acme-1","messages":[{"role":"user","content":"hi"}]}"#;
    let response = client
        .post(format!("http://{proxy}/v1/chat/completions"))
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    // The recorder echoes the payload it was given.
    assert_eq!(response.text().await.unwrap(), body);
    let executed = calls(&records, "acme", "executor.execute");
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
    let response = client
        .post(format!("http://{proxy}/v1/chat/completions"))
        .header("content-type", "application/json")
        .body(r#"{"model":"acme-hidden","messages":[]}"#)
        .send()
        .await
        .unwrap();
    assert_ne!(response.status().as_u16(), 200);
    assert_eq!(calls(&records, "acme", "executor.execute").len(), 1);
    rt.plugins().shutdown_all(None).await;
    std::fs::remove_dir_all(dir).unwrap();
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

/// A failed credential-bound plugin execution is an attempt on that credential (Go's
/// `rpcError` status): the selection loop moves on to the next credential.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failing_plugin_credential_fails_over_to_the_next() {
    let Some(env) = env(
        "plugin-owned-failover",
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
    for status in [429, 500, 401] {
        let dir = env.records.join("respond").join("acme");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("executor.execute.once.json"),
            json!({"ok":false,"error":{"code":"x","message":"slow down","http_status":status}}).to_string(),
        )
        .unwrap();
        let _ = std::fs::remove_file(env.records.join("acme.jsonl"));
        let (got, body) = env.chat("acme-1").await;
        assert_eq!(got, 200, "after a {status}: {body}");
        let ids: Vec<Value> = calls(&env.records, "acme", "executor.execute")
            .into_iter()
            .map(|r| r["AuthID"].clone())
            .collect();
        assert_eq!(ids.len(), 2, "after a {status}: {ids:?}");
        assert_ne!(ids[0], ids[1], "the second attempt uses the other credential");
        // Cooling may now park the failed credential; let both serve again.
        for id in ["acme-a.json", "acme-b.json"] {
            env.rt.store().reset_cooldown(id);
        }
    }
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
