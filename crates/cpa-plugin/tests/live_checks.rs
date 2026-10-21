//! Go rechecks each plugin when it is about to be called, so a plugin fused or replaced
//! while an earlier plugin's call was running is skipped. Scripted in-process plugins
//! make that interleaving deterministic: the first plugin's call fuses the second.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use bytes::Bytes;
use cpa_core::config::Config;
use cpa_plugin::Host;
use cpa_plugin::client::{CallbackHandler, CallbackInstance, PluginClient};
use cpa_plugin::host::Loader;
use cpa_plugin::platform::{self, PluginFile};

type Log = Arc<Mutex<Vec<String>>>;

/// One scripted plugin: its capabilities, canned results per method, and an optional
/// plugin it fuses when a given method is called.
#[derive(Clone, Default)]
struct Script {
    caps: &'static str,
    results: BTreeMap<&'static str, &'static str>,
    fuse_on: Option<(&'static str, &'static str)>,
}

struct ScriptClient {
    id: String,
    script: Script,
    host: Arc<OnceLock<Host>>,
    log: Log,
}

impl PluginClient for ScriptClient {
    fn call(&self, method: &str, _: &[u8]) -> Result<Bytes, String> {
        if method == "plugin.register" || method == "plugin.reconfigure" {
            return Ok(Bytes::from(format!(
                r#"{{"ok":true,"result":{{"schema_version":6,"metadata":{{"Name":"{}","Version":"1","Author":"t","GitHubRepository":"r"}},"capabilities":{{{}}}}}}}"#,
                self.id, self.script.caps
            )));
        }
        self.log.lock().unwrap().push(format!("{} {method}", self.id));
        if let Some((on, victim)) = self.script.fuse_on
            && on == method
        {
            self.host.get().unwrap().fuse(victim, method, "test");
        }
        let result = self.script.results.get(method).copied().unwrap_or("{}");
        Ok(Bytes::from(format!(r#"{{"ok":true,"result":{result}}}"#)))
    }
    fn shutdown(&self) {}
}

struct ScriptLoader {
    scripts: BTreeMap<&'static str, Script>,
    host: Arc<OnceLock<Host>>,
    log: Log,
}

impl Loader for ScriptLoader {
    fn open(
        &self,
        file: &PluginFile,
        _: Arc<dyn CallbackHandler>,
        _: Arc<CallbackInstance>,
    ) -> Result<Arc<dyn PluginClient>, String> {
        Ok(Arc::new(ScriptClient {
            id: file.id.clone(),
            script: self.scripts[file.id.as_str()].clone(),
            host: self.host.clone(),
            log: self.log.clone(),
        }))
    }
}

/// A host with the scripted plugins loaded; `a` has priority 5, `b` priority 1.
async fn host_with(name: &str, scripts: BTreeMap<&'static str, Script>) -> (Host, Log) {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("live-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut yaml = format!("plugins:\n  enabled: true\n  dir: {}\n  configs:\n", dir.display());
    for (i, id) in scripts.keys().enumerate() {
        std::fs::write(dir.join(format!("{id}{}", platform::extension(platform::goos()))), b"").unwrap();
        yaml.push_str(&format!(
            "    {id}:\n      enabled: true\n      priority: {}\n",
            5 - 4 * i
        ));
    }
    let slot = Arc::new(OnceLock::new());
    let log: Log = Arc::default();
    let host = Host::with_loader(Arc::new(ScriptLoader {
        scripts,
        host: slot.clone(),
        log: log.clone(),
    }));
    let _ = slot.set(host.clone());
    host.apply_config(Arc::new(Config::parse(&yaml).unwrap())).await;
    log.lock().unwrap().clear();
    (host, log)
}

fn calls(log: &Log) -> Vec<String> {
    std::mem::take(&mut *log.lock().unwrap())
}

#[tokio::test]
async fn chained_hooks_skip_a_plugin_fused_by_an_earlier_call() {
    let a = Script {
        caps: r#""request_normalizer":true,"request_interceptor":true,"response_stream_interceptor":true"#,
        results: [("request.normalize", r#"{"Body":"YQ=="}"#)].into(),
        fuse_on: Some(("request.normalize", "b")),
    };
    let b = Script {
        caps: a.caps,
        results: [("request.normalize", r#"{"Body":"Yg=="}"#)].into(),
        fuse_on: None,
    };
    let (host, log) = host_with("chain", [("a", a), ("b", b)].into()).await;
    let out = host.normalize_request("openai", "claude", "m", Bytes::from_static(b"{}"), false);
    assert_eq!(&out[..], b"a", "b's body would win if it were still called");
    assert_eq!(calls(&log), ["a request.normalize"]);
    let scope = Default::default();
    host.intercept_request_before_auth(Default::default(), "", &scope).await;
    host.intercept_stream_chunk(Default::default(), "", &scope).await;
    assert_eq!(
        calls(&log),
        ["a request.intercept_before", "a response.intercept_stream_chunk"]
    );
}

#[tokio::test]
async fn quota_lookup_skips_a_provider_fused_while_describing() {
    // a describes first (second pass) and fuses b; Go never asks b, so "x" is unknown.
    let a = Script {
        caps: r#""quota_provider":true"#,
        results: [("quota.describe", r#"{"supported_providers":["y"]}"#)].into(),
        fuse_on: Some(("quota.describe", "b")),
    };
    let b = Script {
        caps: r#""quota_provider":true"#,
        results: [("quota.describe", r#"{"supported_providers":["x"]}"#)].into(),
        fuse_on: None,
    };
    let (host, log) = host_with("quota", [("a", a), ("b", b)].into()).await;
    assert!(host.quota_provider_record("x").await.is_none());
    assert_eq!(calls(&log), ["a quota.describe"]);
}

#[tokio::test]
async fn fusing_a_plugin_drops_its_thinking_provider() {
    let a = Script {
        caps: r#""thinking_applier":true"#,
        results: [("thinking.identifier", r#"{"identifier":"T"}"#)].into(),
        fuse_on: None,
    };
    let (host, _) = host_with("thinking", [("a", a)].into()).await;
    assert_eq!(host.thinking_provider("t").as_deref(), Some("a"));
    host.fuse("a", "ThinkingApplier.ApplyThinking", "test");
    assert_eq!(host.thinking_provider("t"), None);
}

/// Fusing the last translator plugin restores the registry's no-hook fast path.
#[tokio::test]
async fn fusing_the_last_translator_plugin_turns_the_hooks_off() {
    let a = Script {
        caps: r#""request_normalizer":true"#,
        results: BTreeMap::new(),
        fuse_on: None,
    };
    let (host, _) = host_with("hooks", [("a", a)].into()).await;
    host.install_translator_hooks();
    assert!(cpa_translate::hooks::current().is_some());
    host.fuse("a", "RequestNormalizer.NormalizeRequest", "test");
    assert!(cpa_translate::hooks::current().is_none());
}

#[tokio::test]
async fn a_stale_executor_provider_replaced_by_a_builtin_one_is_kept() {
    for builtin_took_over in [false, true] {
        let a = Script {
            caps: r#""executor":true,"executor_model_scope":"both","executor_input_formats":["openai"],"executor_output_formats":["openai"]"#,
            results: [("executor.identifier", r#"{"identifier":"p"}"#)].into(),
            fuse_on: None,
        };
        let (host, _) = host_with("executor", [("a", a)].into()).await;
        let changes = host.register_executors(&|_| false, &|_| Vec::new()).await.unwrap();
        let registered: Vec<_> = changes
            .register
            .iter()
            .map(|a| (a.provider.as_str(), a.record.id.as_str()))
            .collect();
        assert_eq!(registered, [("p", "a")]);
        host.fuse("a", "Executor.Execute", "test");
        let native = move |p: &str| builtin_took_over && p == "p";
        let changes = host.register_executors(&native, &|_| Vec::new()).await.unwrap();
        let want: &[&str] = if builtin_took_over { &[] } else { &["p"] };
        assert_eq!(changes.unregister, want, "builtin_took_over={builtin_took_over}");
    }
}

#[tokio::test]
async fn a_builtin_provider_catalog_reserves_its_models_from_plugin_executors() {
    // Go TestRegisterExecutorsPreservesNativeCatalogPriority: first load, reloads
    // before and after the native credential publishes the model, a reload after it
    // withdraws it, then removal of the catalog.
    for (native_executor, extra_model) in [(true, false), (true, true), (false, false)] {
        let catalog = Script {
            caps: r#""model_registrar":true"#,
            results: [(
                "model.register",
                r#"{"Provider":"native-provider","Models":[{"ID":"shared-model"}]}"#,
            )]
            .into(),
            fuse_on: None,
        };
        let models = if extra_model {
            r#"{"Provider":"plugin-provider","Models":[{"ID":"shared-model"},{"ID":"plugin-only"}]}"#
        } else {
            r#"{"Provider":"plugin-provider","Models":[{"ID":"shared-model"}]}"#
        };
        let competitor = Script {
            caps: r#""model_registrar":true,"executor":true,"executor_model_scope":"both","executor_input_formats":["openai"],"executor_output_formats":["openai"]"#,
            results: [
                ("model.register", models),
                ("executor.identifier", r#"{"identifier":"plugin-provider"}"#),
            ]
            .into(),
            fuse_on: None,
        };
        let (host, _) = host_with("catalog-priority", [("catalog", catalog), ("comp", competitor)].into()).await;
        let native = move |p: &str| native_executor && p == "native-provider";
        let ids = |provider: &str| {
            host.models_for_provider(provider)
                .into_iter()
                .map(|m| m.id)
                .collect::<Vec<_>>()
        };
        let want: &[&str] = match (native_executor, extra_model) {
            (true, false) => &[],
            (true, true) => &["plugin-only"],
            (false, _) => &["shared-model"],
        };
        for (stage, published) in [
            ("first load", false),
            ("reload", false),
            ("published", true),
            ("withdrawn", false),
        ] {
            let case = format!("native_executor={native_executor} extra_model={extra_model} stage={stage}");
            host.register_models().await;
            // The native credential lists shared-model once it has published it.
            let providers = move |m: &str| {
                if published && native_executor && m == "shared-model" {
                    vec!["native-provider".to_owned()]
                } else {
                    Vec::new()
                }
            };
            let changes = host.register_executors(&native, &providers).await.unwrap();
            assert_eq!(ids("plugin-provider"), want, "{case}");
            assert_eq!(
                changes.register.is_empty(),
                want.is_empty(),
                "competing executor registered: {case}"
            );
            assert_eq!(ids("native-provider"), ["shared-model"], "{case}");
        }
        // Removing the catalog releases the reservation on the next pass.
        assert!(host.unload_plugin("catalog", None).await);
        host.register_models().await;
        let changes = host.register_executors(&native, &|_| Vec::new()).await.unwrap();
        let all: &[&str] = if extra_model {
            &["shared-model", "plugin-only"]
        } else {
            &["shared-model"]
        };
        assert_eq!(
            ids("plugin-provider"),
            all,
            "after removal: native_executor={native_executor}"
        );
        assert_eq!(changes.register.len(), 1);
    }
}
