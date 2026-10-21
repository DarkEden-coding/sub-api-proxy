//! Plugin translator hooks around the registry (Go sdk/translator registry_test.go's
//! plugin hook cases). Separate test binary: the hooks are process-global.
use std::sync::{Arc, Mutex};

use cpa_core::format::Format;
use cpa_translate::hooks::{self, PluginHooks, Response};
use cpa_translate::{RequestCtx, ResponseCtx, translate_non_stream, translate_request};

#[derive(Default)]
struct Log(Mutex<Vec<String>>);

impl Log {
    fn push(&self, call: String) {
        self.0.lock().unwrap().push(call);
    }
}

impl PluginHooks for Log {
    fn normalize_request(&self, from: &str, to: &str, _: &str, body: Vec<u8>, _: bool) -> Vec<u8> {
        self.push(format!(
            "normalize_request {from}->{to} {}",
            String::from_utf8_lossy(&body)
        ));
        [&body[..body.len() - 1], br#","normalized":true}"#].concat()
    }
    fn translate_request(&self, from: &str, to: &str, _: &str, body: &[u8], _: bool) -> Option<Vec<u8>> {
        self.push(format!(
            "translate_request {from}->{to} {}",
            String::from_utf8_lossy(body)
        ));
        Some(br#"{"translated":true}"#.to_vec())
    }
    fn normalize_response_before(&self, r: &Response<'_>, body: Vec<u8>) -> Vec<u8> {
        self.push(format!("before {}->{} stream={}", r.from, r.to, r.stream));
        body
    }
    fn translate_response(&self, r: &Response<'_>, _: &[u8]) -> Option<Vec<u8>> {
        self.push(format!("translate_response {}->{}", r.from, r.to));
        Some(b"plugin".to_vec())
    }
    fn normalize_response_after(&self, _: &Response<'_>, body: Vec<u8>) -> Vec<u8> {
        self.push("after".into());
        [&body[..], b"+after"].concat()
    }
}

#[test]
fn hooks_follow_the_registry_order() {
    let log = Arc::new(Log::default());
    hooks::install(Some(log.clone()));
    let ctx = RequestCtx {
        model: "m",
        stream: false,
    };
    let calls = || std::mem::take(&mut *log.0.lock().unwrap());

    // Installed but inactive: nothing runs.
    let same = translate_request(Format::Claude, Format::Claude, &ctx, br#"{"model":"m"}"#).unwrap();
    assert_eq!(same, br#"{"model":"m"}"#);
    assert!(calls().is_empty());

    hooks::set_active(true);
    // A registered pair: the native transform, then the normalizer on its output.
    let out = translate_request(Format::OpenAI, Format::OpenAI, &ctx, br#"{"model":"m"}"#).unwrap();
    assert!(
        out.ends_with(br#","normalized":true}"#),
        "{}",
        String::from_utf8_lossy(&out)
    );
    assert_eq!(calls().len(), 1);

    // An unregistered pair: the model rewrite, the normalizer, then the translator.
    let out = translate_request(Format::Claude, Format::Claude, &ctx, br#"{"model":"x"}"#).unwrap();
    assert_eq!(out, br#"{"translated":true}"#);
    assert_eq!(
        calls(),
        [
            r#"normalize_request claude->claude {"model":"m"}"#,
            r#"translate_request claude->claude {"model":"m","normalized":true}"#,
        ]
    );

    // Responses: `from` is the upstream format. An unregistered pair (a plugin
    // executor's own format) is translated by the plugin.
    let rctx = ResponseCtx {
        model: "m",
        original_request: b"{}",
        translated_request: b"{}",
    };
    let out = translate_non_stream("openai", "acme", &rctx, b"raw").unwrap();
    assert_eq!(out, b"plugin+after");
    assert_eq!(
        calls(),
        [
            "before acme->openai stream=false",
            "translate_response acme->openai",
            "after"
        ]
    );
    let mut stream = cpa_translate::go_stream_between("openai", "acme", &rctx).unwrap();
    assert_eq!(stream.line(b"line").unwrap(), [b"plugin+after".to_vec()]);
    assert_eq!(calls()[0], "before acme->openai stream=true");

    hooks::set_active(false);
    assert!(cpa_translate::go_stream_between("openai", "acme", &rctx).is_none());
    assert_eq!(translate_non_stream("openai", "acme", &rctx, b"raw").unwrap(), b"raw");
    assert!(calls().is_empty());
}
