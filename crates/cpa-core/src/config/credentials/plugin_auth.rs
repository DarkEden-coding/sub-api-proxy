//! Auth files a plugin parses: the `PluginAuthParser` branch of Go's file synthesizer
//! (internal/watcher/synthesizer/file.go) and `FileTokenStore.readAuthFiles`
//! (sdk/auth/filestore.go). The plugin host installs a parser; while no loaded plugin
//! offers an auth provider, loading reads one atomic flag and takes the built-in path.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use serde_json::{Map, Value as Json};

use super::{excluded_models_from_metadata, excluded_union, oauth_aliases, parse_weight};
use crate::credential::{Credential, Source};
use crate::gostr::GoStr;

/// Go `pluginapi.AuthParseRequest` for one auth file.
pub struct ParseRequest<'a> {
    /// The file's lowercased `type` (`gemini` asks as `gemini-cli`); empty asks every
    /// auth provider in order.
    pub provider: &'a str,
    pub path: &'a Path,
    pub file_name: &'a str,
    pub raw_json: &'a [u8],
}

/// One auth a plugin parsed (Go `pluginAuthDataToCoreAuth`). `metadata` is the JSON
/// document the credential keeps and persists.
#[derive(Clone, Default)]
pub struct ParsedAuth {
    pub provider: String,
    pub id: String,
    pub file_name: String,
    pub label: String,
    pub prefix: String,
    pub proxy_url: String,
    pub disabled: bool,
    pub metadata: Map<String, Json>,
    pub attributes: BTreeMap<String, String>,
}

/// Go `ParseAuths`' outcome.
pub enum Parsed {
    /// No plugin took the file, or the call failed: the built-in synthesizer runs.
    NotHandled,
    /// A plugin took the file. No auths suppresses the built-in synthesizer.
    Handled(Vec<ParsedAuth>),
}

/// Go `synthesizer.PluginAuthParser` (`ParseAuths`).
pub trait PluginAuthParser: Send + Sync {
    fn parse_auths(&self, req: &ParseRequest<'_>) -> Parsed;
}

/// Whether a loaded plugin offers an auth provider; the owner of the installed parser
/// keeps it current. One plugin host owns it.
static ACTIVE: AtomicBool = AtomicBool::new(false);
static PARSER: RwLock<Option<Arc<dyn PluginAuthParser>>> = RwLock::new(None);

/// Go `RegisterPluginAuthParser` / `Watcher.SetPluginAuthParser`. Clearing it also
/// clears the activity flag.
pub fn set_plugin_auth_parser(parser: Option<Arc<dyn PluginAuthParser>>) {
    let mut slot = PARSER.write().unwrap_or_else(std::sync::PoisonError::into_inner);
    if parser.is_none() {
        ACTIVE.store(false, Ordering::Release);
    }
    *slot = parser;
}

/// Set by the parser's owner whenever the set of plugins offering an auth provider
/// changes.
pub fn set_plugin_auth_parser_active(active: bool) {
    ACTIVE.store(active, Ordering::Release);
}

/// Whether auth files go to the installed parser first.
pub fn plugin_auth_parser_active() -> bool {
    ACTIVE.load(Ordering::Acquire)
        && PARSER
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
}

/// The installed parser while a plugin offers an auth provider: one atomic load
/// otherwise.
pub(super) fn current() -> Option<Arc<dyn PluginAuthParser>> {
    if !ACTIVE.load(Ordering::Acquire) {
        return None;
    }
    PARSER.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
}

/// Go `ValidateAuthWeight` on a parsed auth.
fn weight_valid(auth: &ParsedAuth) -> bool {
    auth.attributes
        .get("weight")
        .is_none_or(|w| parse_weight(&Json::String(w.clone())).is_ok())
        && auth.metadata.get("weight").is_none_or(|w| parse_weight(w).is_ok())
}

/// The plugin branch of Go `synthesizeFileAuths` for a file whose JSON is `meta`
/// (normalized) and lowercased type `provider`. `None`: no plugin took it.
pub(super) fn credentials(
    parser: &dyn PluginAuthParser,
    meta: &Map<String, Json>,
    provider: &str,
    path: &Path,
    data: &[u8],
) -> Option<Result<Vec<Credential>, String>> {
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    let provider = if provider == "gemini" { "gemini-cli" } else { provider };
    let Parsed::Handled(auths) = parser.parse_auths(&ParseRequest {
        provider,
        path,
        file_name: &file_name,
        raw_json: data,
    }) else {
        return None;
    };
    // Go `compactPluginAuths`: an auth with an invalid weight is dropped.
    let auths: Vec<ParsedAuth> = auths.into_iter().filter(weight_valid).collect();
    Some(build(auths, meta, path, &file_name))
}

fn build(
    auths: Vec<ParsedAuth>,
    meta: &Map<String, Json>,
    path: &Path,
    file_name: &str,
) -> Result<Vec<Credential>, String> {
    let (per_account, invalid) = excluded_models_from_metadata(meta);
    if let Some(error) = invalid {
        tracing::warn!(%error, "ignoring invalid excluded models in auth file {file_name}");
    }
    if auths.is_empty() {
        return Ok(Vec::new());
    }
    let full = path.display().to_string();
    let excluded = excluded_union(&[&per_account]);
    let aliases = oauth_aliases(meta.get("model_aliases"));
    let file_disabled = meta.get("disabled").and_then(Json::as_bool) == Some(true);
    let file_str = |key: &str| meta.get(key).and_then(Json::as_str);
    let fingerprint = file_str("fingerprint_profile")
        .map(|s| s.trim().go_lower())
        .filter(|p| !p.is_empty());
    let count = auths.len();
    let mut out = Vec::with_capacity(count);
    for (index, mut auth) in auths.into_iter().enumerate() {
        super::normalize_metadata(&mut auth.metadata);
        let attrs = &mut auth.attributes;
        if count > 1 {
            // Go `MarkPluginVirtualAuth`.
            attrs.insert("plugin_virtual".into(), "true".into());
            attrs.insert("virtual_source".into(), full.clone());
            let seed = [auth.id.trim(), auth.file_name.trim()]
                .into_iter()
                .find(|s| !s.is_empty())
                .map_or_else(|| index.to_string(), str::to_owned);
            let seed = format!("{}|{full}|{seed}|{index}", auth.provider.trim().go_lower());
            attrs.insert("auth_index_seed".into(), seed);
        }
        for key in ["path", "source"] {
            attrs.insert(key.into(), full.clone());
        }
        attrs.insert("source_backend".into(), "file".into());
        if file_disabled {
            auth.disabled = true;
            auth.metadata.insert("disabled".into(), true.into());
        }
        if auth.proxy_url.is_empty()
            && let Some(proxy) = file_str("proxy_url")
        {
            auth.proxy_url = proxy.trim().to_owned();
        }
        if auth.prefix.is_empty()
            && let Some(prefix) = file_str("prefix")
        {
            auth.prefix = prefix.trim().trim_matches('/').to_owned();
        }
        // Go `ApplyAuthWeightMetadata`: the auth was validated above.
        if let Some(weight) = meta.get("weight") {
            let weight = parse_weight(weight)
                .map_err(|e| format!("invalid plugin auth weight in {file_name}: invalid metadata weight: {e}"))?;
            attrs.insert("weight".into(), weight.to_string());
        }
        // Go `ApplyAuthPriorityMetadata` from the file.
        attrs.remove("file_priority");
        let priority = match meta.get("priority") {
            Some(Json::Number(n)) => Some(n.as_f64().map(|f| f.trunc() as i64).unwrap_or(0).to_string()),
            Some(Json::String(s)) if s.trim().parse::<i64>().is_ok() => Some(s.trim().to_owned()),
            _ => None,
        };
        if let Some(priority) = priority {
            auth.metadata.insert("priority".into(), meta["priority"].clone());
            attrs.insert("priority".into(), priority);
            attrs.insert("file_priority".into(), "true".into());
        }
        if let Some(aliases) = &aliases {
            attrs.insert("model_aliases".into(), aliases.clone());
        }
        // Go `ApplyAuthExcludedModelsMeta`: the file's own rules replace the plugin's.
        if !excluded.is_empty() {
            attrs.insert("excluded_models".into(), excluded.join(","));
        }
        attrs.insert("auth_kind".into(), "oauth".into());
        // Go `ApplyCustomHeadersFromMetadata` reads the parsed auth's metadata.
        if let Some(Json::Object(headers)) = auth.metadata.get("headers") {
            for (k, v) in headers {
                if let (k, Some(v)) = (k.trim(), v.as_str().map(str::trim))
                    && !k.is_empty()
                    && !v.is_empty()
                {
                    attrs.insert(format!("header:{k}"), v.into());
                }
            }
        }
        if let Some(profile) = &fingerprint {
            attrs.insert("fingerprint_profile".into(), profile.clone());
        }
        // Go keeps `Prefix` and `ProxyURL` as fields; here they are attributes, as for
        // every file credential.
        attrs.insert("prefix".into(), auth.prefix);
        if !auth.proxy_url.is_empty() {
            attrs.insert("proxy_url".into(), auth.proxy_url);
        }
        // ponytail: the label and disabled state come from the plugin; a later metadata
        // patch recomputes them from the metadata as for every credential.
        out.push(Credential {
            id: auth.id,
            provider: auth.provider,
            source: Source::File(path.to_owned()),
            disabled: auth.disabled,
            label: auth.label,
            attributes: auth.attributes,
            metadata: auth.metadata,
            revision: 0,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use super::super::{FileCredentials, auth_index, file_credentials};
    use super::*;

    /// A parser answering from a closure and recording what it was asked.
    struct Fake<F> {
        answer: F,
        asked: Mutex<Vec<(String, String, String)>>,
    }

    impl<F: Fn() -> Parsed + Send + Sync> PluginAuthParser for Fake<F> {
        fn parse_auths(&self, req: &ParseRequest<'_>) -> Parsed {
            self.asked.lock().unwrap().push((
                req.provider.to_owned(),
                req.path.display().to_string(),
                req.file_name.to_owned(),
            ));
            (self.answer)()
        }
    }

    fn fake<F: Fn() -> Parsed + Send + Sync>(answer: F) -> Fake<F> {
        Fake {
            answer,
            asked: Mutex::default(),
        }
    }

    fn auth(id: &str, provider: &str, metadata: Json, attributes: &[(&str, &str)]) -> ParsedAuth {
        ParsedAuth {
            provider: provider.into(),
            id: id.into(),
            metadata: metadata.as_object().cloned().unwrap_or_default(),
            attributes: attributes.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            ..Default::default()
        }
    }

    const DIR: &str = "/auth";

    fn synth(parser: &dyn PluginAuthParser, name: &str, raw: &str) -> Result<Option<FileCredentials>, String> {
        file_credentials(Some(parser), Path::new(DIR), &Path::new(DIR).join(name), raw.as_bytes())
    }

    fn plugin_creds(parser: &dyn PluginAuthParser, name: &str, raw: &str) -> Vec<Credential> {
        match synth(parser, name, raw) {
            Ok(Some(FileCredentials::Plugin(all))) => all,
            other => panic!("{raw}: want a plugin result, got {:?}", other.map(|o| o.is_some())),
        }
    }

    /// Go `TestSynthesizeAuthFileExpandsPluginMultiAuths`.
    #[test]
    fn expands_plugin_multi_auths() {
        let parser = fake(|| {
            let headers = json!({"X-Test": "value"});
            Parsed::Handled(vec![
                auth(
                    "geminicli.json",
                    "gemini-cli",
                    json!({"type": "gemini-cli", "headers": headers}),
                    &[],
                ),
                auth(
                    "geminicli-project-a.json",
                    "gemini-cli",
                    json!({"type": "gemini-cli", "project_id": "project-a", "headers": headers}),
                    &[],
                ),
            ])
        });
        let raw = r#"{"type":"gemini-cli","excluded_models":["model-a"],"headers":{"X-Test":"value"}}"#;
        let auths = plugin_creds(&parser, "geminicli.json", raw);
        let full = "/auth/geminicli.json";
        assert_eq!(
            *parser.asked.lock().unwrap(),
            [("gemini-cli".to_owned(), full.to_owned(), "geminicli.json".to_owned())]
        );
        assert_eq!(auths.len(), 2);
        let (first, second) = (auth_index(&auths[0]), auth_index(&auths[1]));
        assert!(!first.is_empty() && first != second, "auth indexes {first}/{second}");
        for a in &auths {
            let attr = |k: &str| a.attributes.get(k).map(String::as_str);
            assert_eq!(attr("plugin_virtual"), Some("true"));
            assert_eq!(attr("virtual_source"), Some(full));
            assert_eq!((attr("path"), attr("source")), (Some(full), Some(full)));
            assert_eq!(attr("header:X-Test"), Some("value"));
            assert_eq!(attr("auth_kind"), Some("oauth"));
            assert_eq!(attr("excluded_models"), Some("model-a"));
            assert_eq!(a.source, Source::File(full.into()));
        }
        assert_eq!(auths[1].metadata["project_id"], "project-a");
        assert_eq!(
            auths[1].attributes["auth_index_seed"],
            "gemini-cli|/auth/geminicli.json|geminicli-project-a.json|1"
        );
    }

    /// A single parsed auth is not virtual: it owns its file.
    #[test]
    fn a_single_plugin_auth_is_not_virtual() {
        let parser = fake(|| Parsed::Handled(vec![auth("plugin.json", "acme", json!({}), &[])]));
        let auths = plugin_creds(
            &parser,
            "plugin.json",
            r#"{"type":"acme","prefix":"/team/","proxy_url":" socks5://p "}"#,
        );
        assert_eq!(auths.len(), 1);
        let attrs = &auths[0].attributes;
        assert!(!attrs.contains_key("plugin_virtual") && !attrs.contains_key("auth_index_seed"));
        assert_eq!(attrs["prefix"], "team");
        assert_eq!(attrs["proxy_url"], "socks5://p");
    }

    /// Go `TestSynthesizeAuthFileAppliesSourcePriorityToPluginAuths`.
    #[test]
    fn applies_source_priority_to_plugin_auths() {
        for (raw, want, want_meta) in [
            (r#"{"type":"plugin","priority":1}"#, "1", json!(1)),
            (r#"{"type":"plugin","priority":" 2 "}"#, "2", json!(" 2 ")),
        ] {
            let parser = fake(|| {
                Parsed::Handled(vec![
                    auth("first", "plugin", json!({"project_id": "first"}), &[]),
                    auth("second", "plugin", json!({"project_id": "second"}), &[]),
                ])
            });
            let auths = plugin_creds(&parser, "plugin.json", raw);
            assert_eq!(auths.len(), 2);
            for a in &auths {
                assert_eq!(a.attributes["priority"], want, "{raw}");
                assert_eq!(a.attributes["file_priority"], "true", "{raw}");
                assert_eq!(a.metadata["priority"], want_meta, "{raw}");
            }
        }
    }

    /// Go `TestSynthesizeAuthFileSkipsInvalidPluginAuthWeight`.
    #[test]
    fn skips_invalid_plugin_auth_weight() {
        let parser = fake(|| {
            Parsed::Handled(vec![
                auth("invalid", "plugin", json!({}), &[("weight", "1.5")]),
                auth("valid", "plugin", json!({}), &[("weight", "0")]),
            ])
        });
        let auths = plugin_creds(&parser, "plugin.json", r#"{"type":"plugin"}"#);
        assert_eq!(auths.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["valid"]);
    }

    /// Go `TestSynthesizeAuthFileAppliesSourceDisabledToPluginMultiAuths`.
    #[test]
    fn applies_source_disabled_to_plugin_multi_auths() {
        let parser = fake(|| {
            Parsed::Handled(vec![
                auth("geminicli.json", "gemini-cli", json!({"type": "gemini-cli"}), &[]),
                auth(
                    "geminicli-project-a.json",
                    "gemini-cli",
                    json!({"project_id": "project-a"}),
                    &[],
                ),
            ])
        });
        let auths = plugin_creds(&parser, "geminicli.json", r#"{"type":"gemini-cli","disabled":true}"#);
        assert_eq!(auths.len(), 2);
        for a in &auths {
            assert!(a.disabled, "{}", a.id);
            assert_eq!(a.metadata["disabled"], true, "{}", a.id);
        }
    }

    /// Go `TestSynthesizeAuthFilePluginHandledEmptySuppressesBuiltin`.
    #[test]
    fn plugin_handled_empty_suppresses_builtin() {
        let parser = fake(|| Parsed::Handled(Vec::new()));
        let raw = r#"{"type":"codex","access_token":"token"}"#;
        assert!(plugin_creds(&parser, "codex.json", raw).is_empty());
        let none = fake(|| Parsed::NotHandled);
        assert!(matches!(
            synth(&none, "codex.json", raw),
            Ok(Some(FileCredentials::One(_)))
        ));
    }

    /// Go `TestSynthesizeAuthFileKeepsPluginExcludedModelsWithoutFileRules`.
    #[test]
    fn keeps_plugin_excluded_models_without_file_rules() {
        let parser = fake(|| {
            Parsed::Handled(vec![auth(
                "plugin.json",
                "acme",
                json!({}),
                &[
                    ("excluded_models", "plugin-model"),
                    ("excluded_models_hash", "plugin-hash"),
                ],
            )])
        });
        for (raw, want) in [
            (r#"{"type":"acme"}"#, "plugin-model"),
            (r#"{"type":"acme","excluded_models":[]}"#, "plugin-model"),
            (r#"{"type":"acme","excluded_models":["File-Model"]}"#, "file-model"),
        ] {
            let auths = plugin_creds(&parser, "plugin.json", raw);
            assert_eq!(auths.len(), 1, "{raw}");
            assert_eq!(auths[0].attributes["excluded_models"], want, "{raw}");
        }
    }

    /// A file no plugin takes builds exactly what the built-in synthesizer builds; Go
    /// asks with the lowercased type, `gemini` as `gemini-cli`, and an empty type asks
    /// every auth provider. The file's own weight is still checked first.
    #[test]
    fn unhandled_files_take_the_builtin_path() {
        let parser = fake(|| Parsed::NotHandled);
        for raw in [
            r#"{"type":" Claude ","email":"a@example.test","priority":3,"prefix":"p"}"#,
            r#"{"type":"gemini"}"#,
            r#"{"email":"x@example.test"}"#,
            "not json",
        ] {
            let path = Path::new(DIR).join("a.json");
            let plain = file_credentials(None, Path::new(DIR), &path, raw.as_bytes());
            let asked = synth(&parser, "a.json", raw);
            let show = |r: Result<Option<FileCredentials>, String>| match r {
                Ok(Some(FileCredentials::One(c))) => format!("{c:?}"),
                Ok(Some(FileCredentials::Plugin(all))) => format!("plugin {all:?}"),
                Ok(None) => "none".into(),
                Err(e) => e,
            };
            assert_eq!(show(plain), show(asked), "{raw}");
        }
        let providers: Vec<String> = parser.asked.lock().unwrap().iter().map(|a| a.0.clone()).collect();
        assert_eq!(providers, ["claude", "gemini-cli", ""]);
        let err = synth(&parser, "a.json", r#"{"type":"acme","weight":1.5}"#);
        assert!(err.is_err());
        assert_eq!(
            parser.asked.lock().unwrap().len(),
            3,
            "an invalid file weight is never parsed"
        );
    }
}
