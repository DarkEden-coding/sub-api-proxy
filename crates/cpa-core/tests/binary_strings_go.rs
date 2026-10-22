//! `Config::parse` against Go v8.0.35's config decoder for `!!binary` and locally
//! tagged scalars,
//! recorded by `tests/reference/binary_strings/main.go`: the error, the management key
//! as decoded, `proxy-url`, `port` and `api-keys`. A value a Rust string cannot hold
//! (bytes that are not UTF-8) is refused where Go decodes it
//! (docs/DIFFERENCES-FROM-GO.md).
use cpa_core::config::Config;
use serde_json::Value as Json;

const NOT_UTF8: &[&str] = &["not UTF-8"];

#[test]
fn binary_scalars_decode_like_go() {
    let cases: Vec<Json> = serde_json::from_str(include_str!("fixtures/binary_strings_go.json")).unwrap();
    let mut failures = Vec::new();
    for case in &cases {
        let name = case["name"].as_str().unwrap();
        let go_error = case["error"].as_str().unwrap();
        let mut fail = |what: String| failures.push(format!("{name}: {what}"));
        let config = match (Config::parse(case["yaml"].as_str().unwrap()), go_error.is_empty()) {
            (Err(e), _) if NOT_UTF8.contains(&name) => {
                if e.to_string() != "a !!binary value in the config file is not UTF-8 text" {
                    fail(format!("error {e:#}, expected the UTF-8 refusal"));
                }
                continue;
            }
            (Ok(config), true) => config,
            (Err(e), false) => {
                // The base64 error is Go's text; other errors only fail in both.
                if go_error.contains("!!binary value") && e.to_string() != go_error {
                    fail(format!("error {e:#}, Go {go_error:?}"));
                }
                continue;
            }
            (Ok(_), false) => {
                fail(format!("accepted; Go: {go_error}"));
                continue;
            }
            (Err(e), true) => {
                fail(format!("refused: {e:#}"));
                continue;
            }
        };
        let doc = &config.document;
        let text = |v: Option<&serde_yaml_ng::Value>| v.and_then(|v| v.as_str()).unwrap_or_default().to_owned();
        let got = serde_json::json!({
            "secret_key": config.management.secret_key,
            "proxy_url": text(doc.get("requests").and_then(|r| r.get("proxy-url"))),
            "port": config.port,
            "api_keys": (!config.api_keys.is_empty()).then_some(&config.api_keys),
        });
        for field in ["secret_key", "proxy_url", "port", "api_keys"] {
            if got[field] != case[field] {
                fail(format!("{field} {}, Go {}", got[field], case[field]));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} cases differ:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}
