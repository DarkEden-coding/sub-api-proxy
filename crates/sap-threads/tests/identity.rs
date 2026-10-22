use http::HeaderMap;
use sap_core::Utc;
use sap_core::ids::{NodeId, Provider, ThreadKey};
use sap_core::thread::{ClientKind, CompactionTrigger, Continuation, LineageKind, RequestClass};
use sap_threads::{Registry, identify, identify_claude, identify_codex};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
struct Fixture {
    scenario: String,
    provider: String,
    transport: String,
    headers: std::collections::BTreeMap<String, String>,
    body: Value,
}

#[test]
fn captured_requests_cover_identity_and_lineage() {
    let mut registry = Registry::new(100);
    let mut roots = std::collections::BTreeMap::new();
    let mut counts = std::collections::BTreeMap::new();
    for line in include_str!("../../sap-core/tests/fixtures/identity.jsonl").lines() {
        let fixture: Fixture = serde_json::from_str(line).unwrap();
        let mut headers = HeaderMap::new();
        for (name, value) in &fixture.headers {
            headers.insert(name.parse::<http::HeaderName>().unwrap(), value.parse().unwrap());
        }
        headers.insert("x-sap-client-host", "fixture-machine".parse().unwrap());
        let bytes = serde_json::to_vec(&fixture.body).unwrap();
        let path = if fixture.provider == "claude" {
            "/v1/messages"
        } else {
            "/backend-api/codex/responses"
        };
        assert!(matches!(fixture.transport.as_str(), "http" | "ws"));
        let identity = identify(path, &headers, &bytes).unwrap();
        assert_eq!(identity.client_host.as_deref(), Some("fixture-machine"));
        assert_eq!(identity.model.as_deref(), fixture.body["model"].as_str());
        let is_claude = fixture.provider == "claude";
        assert_eq!(
            identity.client,
            if is_claude {
                ClientKind::ClaudeCode
            } else {
                ClientKind::CodexCli
            }
        );
        let metadata: Value = fixture.body["client_metadata"]["x-codex-turn-metadata"]
            .as_str()
            .map(|s| serde_json::from_str(s).unwrap())
            .unwrap_or(Value::Null);
        let subagent = if is_claude {
            headers.contains_key("x-claude-code-agent-id")
        } else {
            metadata["thread_source"] == "subagent"
        };
        let kind = if is_claude {
            fixture.headers["x-claude-code-request-class"].as_str()
        } else {
            metadata["request_kind"].as_str().unwrap()
        };
        let expected_class = match kind {
            "auxiliary" => RequestClass::Auxiliary,
            "compaction" => RequestClass::Compaction,
            "prewarm" => RequestClass::Prewarm,
            _ if subagent => RequestClass::SubagentTurn,
            _ => RequestClass::Turn,
        };
        assert_eq!(identity.class, expected_class, "{}", fixture.scenario);
        if subagent {
            assert_eq!(identity.lineage, LineageKind::Subagent);
            let parent = identity.parent.as_ref().unwrap();
            if is_claude {
                assert_eq!(parent.id, identity.root_session);
                assert!(
                    identity
                        .key
                        .id
                        .ends_with(&format!("/{}", fixture.headers["x-claude-code-agent-id"]))
                );
                assert_eq!(
                    identity.agent_label.as_deref(),
                    Some(fixture.headers["x-claude-code-agent-type"].as_str())
                );
            } else {
                assert_eq!(parent.id, metadata["parent_thread_id"].as_str().unwrap());
            }
        }
        if !is_claude {
            assert_eq!(identity.key.id, fixture.headers["thread-id"]);
            assert_eq!(identity.root_session, fixture.headers["session-id"]);
            assert_eq!(identity.window_number, Some(0));
            assert_eq!(identity.agent_label.as_deref(), metadata["agent_name"].as_str());
            if let Some(source) = metadata["forked_from_thread_id"].as_str() {
                assert_eq!(identity.lineage, LineageKind::Fork);
                assert_eq!(identity.forked_from.as_ref().unwrap().id, source);
            }
        }
        let previous = if is_claude && fixture.body["thread"]["type"] == "continue" {
            fixture.body["thread"]["previous_message_id"].as_str()
        } else if !is_claude {
            fixture.body["previous_response_id"].as_str()
        } else {
            None
        };
        assert_eq!(
            identity.continuation,
            previous.map_or(Continuation::FullHistory, |p| Continuation::Continue {
                previous_id: p.to_owned()
            })
        );
        if expected_class == RequestClass::Compaction {
            assert_eq!(
                identity.compaction,
                Some(if is_claude {
                    CompactionTrigger::Manual
                } else {
                    CompactionTrigger::Auto
                })
            );
        }
        if fixture.scenario.ends_with(":new") && !subagent && expected_class != RequestClass::Auxiliary {
            roots
                .entry(fixture.provider.clone())
                .or_insert_with(|| identity.key.clone());
        }
        if fixture.scenario.ends_with(":resume") {
            assert_eq!(&identity.key, &roots[&fixture.provider]);
        }
        if fixture.scenario == "claude:fork" {
            assert!(registry.record_response(&roots["claude"], identity.previous_message_id.clone().unwrap()));
        }
        let observed = registry.observe(&identity, Utc::now(), NodeId("test".into()));
        if expected_class == RequestClass::Auxiliary {
            assert!(observed.record.is_none());
        }
        if fixture.scenario == "claude:fork" {
            assert_eq!(observed.lineage, LineageKind::Fork);
            assert_eq!(observed.parent.as_ref(), Some(&roots["claude"]));
        }
        *counts.entry(fixture.scenario).or_insert(0) += 1;
    }
    assert_eq!(counts.values().sum::<usize>(), 36);
    assert_eq!(counts.len(), 10);
    assert_eq!(registry.children(&roots["claude"]).len(), 2);
}

#[test]
fn fallback_precedence_and_invalid_inputs() {
    let mut headers = HeaderMap::new();
    let body = br#"{"metadata":{"user_id":"{\"session_id\":\"fallback\"}"},"messages":[{"content":"ignored"}]}"#;
    assert_eq!(identify_claude(&headers, body).unwrap().key.id, "fallback");
    let fallback = identify_claude(&headers, br#"{"metadata":{"user_id":{"session_id":"object"}}}"#).unwrap();
    assert_eq!(fallback.key.id, "object");
    assert_eq!(fallback.client, ClientKind::Other);
    headers.insert("x-claude-code-session-id", "header".parse().unwrap());
    assert_eq!(identify_claude(&headers, body).unwrap().key.id, "header");
    assert!(identify_claude(&headers, b"{").is_none());
    assert!(identify_claude(&headers, br#"{"thread":{"type":"continue"}}"#).is_none());
    assert!(identify("/unknown", &headers, b"{}").is_none());
    assert!(identify_codex(&headers, b"{}").is_none());
    headers.insert("thread-id", "thread".parse().unwrap());
    headers.insert(
        "x-codex-turn-metadata",
        r#"{"request_kind":"prewarm"}"#.parse().unwrap(),
    );
    headers.insert("x-codex-window-id", "thread:0".parse().unwrap());
    let frame = json!({"client_metadata": {
        "x-codex-turn-metadata": "{\"request_kind\":\"turn\",\"window_id\":\"thread:2\"}"
    }});
    let identity = identify_codex(&headers, &serde_json::to_vec(&frame).unwrap()).unwrap();
    assert_eq!(identity.class, RequestClass::Turn);
    assert_eq!(identity.window_number, Some(2));
    let frame = json!({"client_metadata": {
        "x-codex-turn-metadata": "{\"request_kind\":\"turn\",\"window_number\":3}"
    }});
    assert_eq!(
        identify_codex(&headers, &serde_json::to_vec(&frame).unwrap())
            .unwrap()
            .window_number,
        Some(3)
    );
    assert_eq!(identity.key, ThreadKey::new(Provider::Codex, "thread"));
    assert_eq!(
        identify_codex(&headers, br#"{"generate":false}"#).unwrap().class,
        RequestClass::Prewarm
    );
}
