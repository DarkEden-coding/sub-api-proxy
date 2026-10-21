//! `prepare_for_upload` against Go v8.0.35's `PrepareConfigFileForUpload` (f9437700),
//! recorded by `tests/reference/prepare_upload/main.go`: the outcome, the error, whether
//! the file was left alone, the prepared file's content (bcrypt hashes masked) and
//! whether its secret is the hash of the expected plaintext.
//!
//! Where re-emitting the alias-expanded tree would change another scalar's text, Rust
//! refuses and Go re-encodes (docs/DIFFERENCES-FROM-GO.md); those cases expect Rust's
//! refusal.
use serde_json::Value as Json;
use serde_yaml_ng::Value;

fn masked(v: Json) -> Json {
    match v {
        Json::Object(map) => Json::Object(map.into_iter().map(|(k, v)| (k, masked(v))).collect()),
        Json::Array(items) => Json::Array(items.into_iter().map(masked).collect()),
        Json::String(s) if s.len() == 60 && s.starts_with("$2") => Json::String("<bcrypt>".into()),
        v => v,
    }
}

const REWRITE_REFUSED: &[&str] = &[
    "aliased hex secret",
    "merge key beside a hex value",
    "binary host beside a merged secret",
    "hex key beside a merged secret",
];
const REWRITE_REFUSAL: &str =
    "failed to update remote management key: rewriting the config file would change the text of another setting";

#[test]
fn prepare_for_upload_matches_go() {
    let cases: Vec<Json> = serde_json::from_str(include_str!("fixtures/prepare_upload_go.json")).unwrap();
    let dir = std::env::temp_dir().join(format!("cpa-prepare-go-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.yaml");
    let mut failures = Vec::new();
    for case in &cases {
        let name = case["name"].as_str().unwrap();
        let yaml = case["yaml"].as_str().unwrap();
        let plain = case["plain"].as_str().unwrap_or_default();
        // A plaintext that is not UTF-8 is recorded as base64.
        let plain_bytes = match case["plain_bytes"].as_str() {
            Some(b) => {
                use base64::Engine as _;
                base64::engine::general_purpose::STANDARD.decode(b).unwrap()
            }
            None => plain.as_bytes().to_vec(),
        };
        let go_error = case["error"].as_str().unwrap();
        std::fs::write(&path, yaml).unwrap();
        let result = cpa_core::config::prepare_for_upload(&path);
        let saved = std::fs::read_to_string(&path).unwrap();
        let mut fail = |what: String| failures.push(format!("{name}: {what}"));
        if REWRITE_REFUSED.contains(&name) {
            match &result {
                Err(e) if format!("{e:#}") == REWRITE_REFUSAL => {}
                other => fail(format!("{other:?}, expected the rewrite refusal")),
            }
            if saved != yaml {
                fail("changed".into());
            }
            continue;
        }
        match (&result, go_error.is_empty()) {
            (Ok(()), true) => {}
            (Err(e), false) => {
                let text = format!("{e:#}");
                let kind = go_error.split(':').next().unwrap();
                // The refusal is Go's text; other errors share Go's leading context.
                let same = if go_error.starts_with("plaintext") {
                    e.to_string() == go_error
                } else {
                    text.starts_with(kind)
                };
                if !same {
                    fail(format!("error {text:?}, Go {go_error:?}"));
                }
                if !plain.is_empty() && text.contains(plain) {
                    fail(format!("the error quotes the secret: {text:?}"));
                }
            }
            (Ok(()), false) => fail(format!("accepted; Go: {go_error}")),
            (Err(e), true) => fail(format!("refused: {e:#}")),
        }
        if (saved == yaml) != case["unchanged"].as_bool().unwrap() {
            fail(format!(
                "unchanged {}, Go {}:\n{saved}",
                saved == yaml,
                case["unchanged"]
            ));
        }
        // An unchanged file is the input, which Go's generic decode reads.
        if !case["result"].is_null() && saved != yaml {
            let mut doc: Value = serde_yaml_ng::from_str(&saved).unwrap();
            cpa_core::config::expand_merges(&mut doc).unwrap();
            let got = masked(serde_json::to_value(&doc).unwrap());
            if got != case["result"] {
                fail(format!("prepared {got}, Go {}", case["result"]));
            }
            let secret = ["management", "remote-management"]
                .iter()
                .find_map(|s| doc.get(*s)?.get("secret-key"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let verifies = !plain_bytes.is_empty() && bcrypt::verify(&plain_bytes, secret).unwrap_or(false);
            if verifies != case["verifies"].as_bool().unwrap() {
                fail(format!(
                    "secret {secret:?} verifies {verifies}, Go {}",
                    case["verifies"]
                ));
            }
        }
        assert!(
            case["effective_same"].as_bool() != Some(false),
            "{name}: the recording changed Go's config"
        );
    }
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(
        failures.is_empty(),
        "{} of {} cases differ:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}
