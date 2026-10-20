//! Go v8.0.35 plugin-owned providers without a model router: a catalog-only plugin's
//! models bind to the credentials of their provider (with the credential's excluded
//! models applied), and a plugin executor serves those credentials through the
//! ordinary selection loop. Private auth-dir, native test plugins, no upstream.
#![cfg(unix)]

use std::path::Path;
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
