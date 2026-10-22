use http::HeaderMap;
use sap_core::ids::{AccountId, NodeId, Provider, ThreadKey};
use sap_core::routing::Reason;
use sap_core::thread::{Assignment, LineageKind, RequestClass, ThreadIdentity, ThreadUsage};
use sap_core::{DateTime, Utc};
use sap_threads::{Registry, identify_claude};
use std::time::Duration;

fn identity(id: &str) -> ThreadIdentity {
    let mut headers = HeaderMap::new();
    headers.insert("x-claude-code-session-id", id.parse().unwrap());
    identify_claude(&headers, b"{}").unwrap()
}
fn time(seconds: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(seconds, 0).unwrap()
}
fn node() -> NodeId {
    NodeId("local".into())
}

#[test]
fn lifecycle_forks_usage_and_active_window() {
    let mut registry = Registry::new(4);
    let root = identity("root");
    let key = &root.key;
    assert!(registry.observe(&root, time(10), node()).is_new);
    assert!(!registry.observe(&root, time(11), node()).is_new);
    registry.record_response(key, "message");
    let mut fork = identity("fork");
    fork.previous_message_id = Some("message".into());
    let observed = registry.observe(&fork, time(12), node());
    assert_eq!(observed.lineage, LineageKind::Fork);
    assert_eq!(observed.parent.as_ref(), Some(key));
    assert_eq!(registry.children(key).len(), 1);
    assert_eq!(registry.observe(&fork, time(13), node()).lineage, LineageKind::Fork);
    registry.record_usage(
        key,
        ThreadUsage {
            requests: 1,
            input_tokens: 20,
            cache_read_tokens: 30,
            cache_write_tokens: 40,
            output_tokens: 10,
        },
    );
    registry.record_usage(
        key,
        ThreadUsage {
            requests: 2,
            input_tokens: 5,
            ..ThreadUsage::default()
        },
    );
    assert_eq!(registry.get(key).unwrap().usage.requests, 3);
    assert_eq!(registry.get(key).unwrap().usage.input_tokens, 25);
    assert_eq!(registry.get(key).unwrap().usage.cache_read_tokens, 30);
    let assignment = Assignment {
        account: AccountId("account".into()),
        reason: Reason::ParentAffinity,
        at: time(14),
    };
    assert!(registry.assign(key, assignment.clone()));
    assert_eq!(registry.get(key).unwrap().assignment, Some(assignment));
    assert_eq!(registry.compaction_completed(key), Some(1));
    let mut codex = identity("codex");
    codex.key = ThreadKey::new(Provider::Codex, "codex");
    codex.window_number = Some(3);
    registry.observe(&codex, time(15), node());
    assert_eq!(registry.compaction_completed(&codex.key), Some(4));
    codex.window_number = Some(4);
    assert_eq!(
        registry
            .observe(&codex, time(16), node())
            .record
            .unwrap()
            .compaction_generation,
        4
    );
    codex.window_number = Some(6);
    assert_eq!(
        registry
            .observe(&codex, time(17), node())
            .record
            .unwrap()
            .compaction_generation,
        6
    );
    assert_eq!(registry.active(time(17), Duration::from_secs(4)).len(), 2);
    registry.stream_opened(key);
    assert_eq!(registry.active(time(100), Duration::from_secs(1)).len(), 1);
    registry.stream_closed(key);
    registry.stream_closed(key);
    assert!(registry.active(time(100), Duration::from_secs(1)).is_empty());
    assert!(!registry.record_response(&identity("missing").key, "ignored"));
}

#[test]
fn capacity_protects_streams_and_auxiliary_does_not_create() {
    let mut registry = Registry::new(2);
    let a = identity("a");
    let b = identity("b");
    let c = identity("c");
    let mut auxiliary = identity("aux");
    auxiliary.class = RequestClass::Auxiliary;
    assert!(registry.observe(&auxiliary, time(0), node()).record.is_none());
    registry.observe(&a, time(1), node());
    registry.observe(&b, time(2), node());
    registry.stream_opened(&a.key);
    registry.observe(&c, time(3), node());
    assert!(registry.get(&b.key).is_none());
    registry.stream_opened(&c.key);
    assert!(registry.observe(&b, time(4), node()).record.is_none());
    assert_eq!(registry.export().len(), 2);
    registry.stream_closed(&a.key);
    assert!(registry.observe(&b, time(4), node()).is_new);
    assert!(registry.get(&a.key).is_none());
    assert!(Registry::new(0).observe(&a, time(1), node()).record.is_none());
}

#[test]
fn import_preserves_bound_newest_records_and_local_streams() {
    let mut source = Registry::new(4);
    for i in 1..=4 {
        source.observe(&identity(&i.to_string()), time(i), node());
    }
    source.stream_opened(&identity("4").key);
    let mut registry = Registry::new(2);
    registry.import(source.export());
    let exported = registry.export();
    assert_eq!(
        exported.iter().map(|r| r.key.id.as_str()).collect::<Vec<_>>(),
        ["3", "4"]
    );
    assert!(exported.iter().all(|r| r.open_streams == 0));
    registry.stream_opened(&identity("4").key);
    source.observe(&identity("4"), time(5), node());
    registry.import(source.export());
    assert_eq!(registry.get(&identity("4").key).unwrap().open_streams, 1);
    assert_eq!(registry.export().len(), 2);
    let mut roundtrip = Registry::new(2);
    roundtrip.import(exported.clone());
    assert_eq!(roundtrip.export(), exported);
}
