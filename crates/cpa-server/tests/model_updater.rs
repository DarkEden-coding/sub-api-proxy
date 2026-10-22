//! Remote model catalog refresh against local mock sources. Its own test binary: the
//! refresh swaps the process-wide static catalog.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use cpa_core::registry::{catalog_generation, pinned};

const EMBEDDED: &str = include_str!("../../cpa-core/src/registry/models.json");

async fn serve(app: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

/// A Gemini model only Go's typed decode reads as intended: a field name in another
/// case, nulls in a string list and a header map, and HTML characters.
const TYPED_GEMINI: &str = r#"{"id":"gemini-typed","InputTokenLimit":123,"supportedInputModalities":[null,"text"],"config":{"override_header":{"X-Test":null,"b":"<v>"}},"THINKING":{"Min":1,"levels":[null],"DISABLE_TYPE":"x"},"Display_Name":"D","created":-0,"native_capabilities":{"web_search":true},"unknown":{"a":[1.5]}}"#;

/// Go 1.26.4 `json.Marshal` of [`TYPED_GEMINI`] decoded into `registry.ModelInfo`.
const TYPED_GEMINI_GO: &str = r#"{"id":"gemini-typed","object":"","created":0,"owned_by":"","type":"","display_name":"D","inputTokenLimit":123,"supportedInputModalities":["","text"],"thinking":{"min":1,"disable_type":"x","levels":[""]},"config":{"override_header":{"X-Test":"","b":"\u003cv\u003e"}}}"#;

/// The embedded catalog with one extra Claude model and [`TYPED_GEMINI`].
fn with_extra_claude_model() -> String {
    let mut root: serde_json::Map<String, serde_json::Value> = serde_json::from_str(EMBEDDED).unwrap();
    let mut extra = root["claude"][0].as_object().unwrap().clone();
    extra.insert("id".into(), "claude-refreshed-1".into());
    root["claude"].as_array_mut().unwrap().push(extra.into());
    root["gemini"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::from_str(TYPED_GEMINI).unwrap());
    serde_json::to_string(&root).unwrap()
}

#[tokio::test]
async fn refresh_skips_failing_sources_and_swaps_a_changed_catalog() {
    let hits = Arc::new(AtomicUsize::new(0));
    let modified = with_extra_claude_model();
    let invalid = r#"{"claude":[{"id":"a"},{"id":"a"}]}"#.to_owned();
    // Go's typed decode refuses a fractional token limit.
    let mistyped = {
        let mut root: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&modified).unwrap();
        root["gemini"][0]["inputTokenLimit"] = serde_json::json!(1.5);
        serde_json::to_string(&root).unwrap()
    };
    let without_meta = {
        let mut root: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&modified).unwrap();
        root.insert("meta".into(), serde_json::Value::Array(Vec::new()));
        serde_json::to_string(&root).unwrap()
    };
    let app = axum::Router::new()
        .route(
            "/{name}",
            axum::routing::get(move |State(hits): State<Arc<AtomicUsize>>, Path(name): Path<String>| {
                let (modified, invalid, mistyped, without_meta) = (
                    modified.clone(),
                    invalid.clone(),
                    mistyped.clone(),
                    without_meta.clone(),
                );
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    match name.as_str() {
                        "down" => (StatusCode::INTERNAL_SERVER_ERROR, String::new()).into_response(),
                        "invalid" => invalid.into_response(),
                        "mistyped" => mistyped.into_response(),
                        "modified" => modified.into_response(),
                        _ => without_meta.into_response(),
                    }
                }
            }),
        )
        .with_state(hits.clone());
    let base = serve(app).await;
    let urls = |names: &[&str]| names.iter().map(|n| format!("{base}/{n}")).collect::<Vec<_>>();

    assert!(pinned().lookup("claude-refreshed-1").is_none());
    let mut metadata = serde_json::Map::new();
    metadata.insert("type".into(), "claude".into());
    metadata.insert("access_token".into(), "fake-token".into());
    let credential = cpa_core::credential::Credential::from_file(
        std::path::Path::new("/auth"),
        std::path::Path::new("/auth/c.json"),
        metadata,
    )
    .unwrap();
    let executors = cpa_exec::Executors {
        claude: cpa_exec::claude::ClaudeExecutor::new("http://127.0.0.1:9").unwrap(),
        codex: Default::default(),
        devices: Default::default(),
        openai: Default::default(),
        google: Default::default(),
    };
    let auth = std::env::temp_dir().join(format!("model-updater-{}", std::process::id()));
    std::fs::create_dir_all(&auth).unwrap();
    let config = auth.join("config.yaml");
    let hash = bcrypt::hash("fake-management-only", 4).unwrap();
    let yaml = format!(
        "management:\n  secret-key: '{hash}'\noauth:\n  auth-dir: {}\n",
        auth.display()
    );
    std::fs::write(&config, yaml).unwrap();
    let rt = Arc::new(cpa_server::testing::runtime(
        cpa_core::config::Config::load(&config).unwrap(),
        vec![credential],
        executors,
    ));
    let registered = |rt: &cpa_server::Runtime| rt.registry().ids().any(|id| id == "claude-refreshed-1");
    assert!(!registered(&rt));
    let meta_before = pinned().channel("meta").len();
    let generation = catalog_generation();

    // Every source fails: the current catalog stays.
    cpa_server::model_updater::refresh(&urls(&["down", "invalid", "mistyped"]), "test").await;
    assert_eq!(catalog_generation(), generation);

    // A 500 and an invalid catalog are skipped; the third source is applied.
    cpa_server::model_updater::refresh(&urls(&["down", "invalid", "modified"]), "test").await;
    assert_eq!(hits.load(Ordering::SeqCst), 6);
    assert_eq!(catalog_generation(), generation + 1);
    assert!(pinned().lookup("claude-refreshed-1").is_some());
    // Management's model definitions read the refreshed catalog.
    let definitions = cpa_core::registry::dynamic::static_definitions("claude").unwrap();
    assert_eq!(definitions.last().unwrap().id, "claude-refreshed-1");
    // ... and render a refreshed model as Go's typed decode and encode do.
    let management = cpa_server::management::Management::new(rt.clone(), config);
    let app = cpa_server::management::router(management);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap()
    });
    let body = wreq::Client::new()
        .get(format!("http://{addr}/v8/management/routing/model-definitions/gemini"))
        .bearer_auth("fake-management-only")
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(body.ends_with(&format!(",{TYPED_GEMINI_GO}]}}")), "{body}");
    std::fs::remove_dir_all(&auth).unwrap();
    // Go's refresh callback re-registers the credentials of changed providers.
    assert!(registered(&rt));

    // The same content again changes nothing.
    cpa_server::model_updater::refresh(&urls(&["modified"]), "test").await;
    assert_eq!(catalog_generation(), generation + 1);

    // An empty remote `meta` section keeps the current one (Go tryRefreshModels).
    cpa_server::model_updater::refresh(&urls(&["no-meta"]), "test").await;
    assert_eq!(catalog_generation(), generation + 1, "identical once meta is kept");
    assert_eq!(pinned().channel("meta").len(), meta_before);
}
