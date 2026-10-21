//! `prepare_for_upload` against Go v8.0.35's `PrepareConfigFileForUpload` (f9437700),
//! recorded by `tests/reference/prepare_upload/main.go`: the outcome, the error, whether
//! the file was left alone, the prepared file's content (bcrypt hashes masked) and
//! whether its secret is the hash of the expected plaintext.
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
        let go_error = case["error"].as_str().unwrap();
        std::fs::write(&path, yaml).unwrap();
        let result = cpa_core::config::prepare_for_upload(&path);
        let saved = std::fs::read_to_string(&path).unwrap();
        let mut fail = |what: String| failures.push(format!("{name}: {what}"));
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
        if !case["result"].is_null() {
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
            let verifies = !plain.is_empty() && bcrypt::verify(plain, secret).unwrap_or(false);
            if verifies != case["verifies"].as_bool().unwrap() {
                fail(format!(
                    "secret {secret:?} verifies {verifies}, Go {}",
                    case["verifies"]
                ));
            }
        }
        assert!(
            case["effective_same"].as_bool().unwrap(),
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
