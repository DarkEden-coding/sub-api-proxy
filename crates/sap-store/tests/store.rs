use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use sap_core::account::{Account, Capacity, NodeCredential};
use sap_core::api::{EventKind, EventView};
use sap_core::hlc::Hlc;
use sap_core::ids::{AccountId, NodeId, Provider, ThreadKey};
use sap_core::quota::{ObservationSource, QuotaObservation, QuotaWindow, WindowKind};
use sap_core::sync::{Change, ChangeLog, ChangeOp, SyncCursor, Table};
use sap_core::thread::{Assignment, ClientKind, LineageKind, ThreadRecord, ThreadUsage};
use sap_core::{DateTime, Utc};
use sap_store::{Limits, Store};
use serde_json::json;

fn node(s: &str) -> NodeId {
    NodeId(s.into())
}
fn account() -> Account {
    Account {
        id: AccountId("account".into()),
        provider: Provider::Claude,
        label: "Original".into(),
        email: None,
        plan: None,
        capacity: Capacity::default(),
        priority: 0,
        disabled: false,
        models: None,
        created_at: DateTime::from_timestamp_millis(1_000).unwrap(),
    }
}
fn store(s: &str) -> Store {
    Store::open_in_memory(node(s)).unwrap()
}
fn patch(store: &Store, field: &str, value: serde_json::Value) {
    store
        .patch_account(&account().id, &BTreeMap::from([(field.into(), value)]))
        .unwrap();
}
fn pull(from: &Store, to: &Store) -> usize {
    let batch = from.changes_since(&to.cursor().unwrap(), 3).unwrap();
    to.apply_remote(&batch.changes).unwrap();
    batch.changes.len()
}
fn exchange(a: &Store, b: &Store) {
    for _ in 0..50 {
        if pull(a, b) + pull(b, a) == 0 {
            return;
        }
    }
    panic!("replication failed to settle");
}
fn event(id: &str, at: DateTime<Utc>) -> EventView {
    EventView {
        id: id.into(),
        at,
        kind: EventKind::AccountState,
        message: "State changed".into(),
        node: node("a"),
        thread: None,
        account: Some(account().id),
    }
}
fn thread() -> ThreadRecord {
    ThreadRecord {
        key: ThreadKey::new(Provider::Claude, "thread"),
        client: ClientKind::ClaudeCode,
        lineage: LineageKind::Root,
        root_session: "thread".into(),
        parent: None,
        client_host: None,
        agent_label: None,
        title: None,
        assignment: None,
        compaction_generation: 0,
        last_response_id: None,
        first_seen: Utc::now(),
        last_seen: Utc::now(),
        serving_node: Some(node("a")),
        open_streams: 0,
        usage: ThreadUsage::default(),
    }
}

#[test]
fn concurrent_fields_converge_and_resolution_replicates() {
    let a = store("a");
    let b = store("b");
    a.upsert_account(account()).unwrap();
    exchange(&a, &b);
    patch(&a, "label", json!("A"));
    patch(&a, "priority", json!(7));
    patch(&b, "label", json!("B"));
    patch(&b, "disabled", json!(true));
    exchange(&a, &b);
    assert_eq!(a.accounts().unwrap(), b.accounts().unwrap());
    assert_eq!(a.conflicts().unwrap(), b.conflicts().unwrap());
    assert_eq!(a.conflicts().unwrap().len(), 1);
    let record = &a.accounts().unwrap()[0];
    assert_eq!(record.priority, 7);
    assert!(record.disabled);
    let conflict = a.conflicts().unwrap().remove(0);
    a.resolve_conflict(&conflict.id, node("a")).unwrap();
    exchange(&a, &b);
    assert_eq!(a.accounts().unwrap()[0].label, "A");
    assert_eq!(a.accounts().unwrap(), b.accounts().unwrap());
    assert!(b.conflicts().unwrap()[0].resolved);
    assert_eq!(a.conflicts().unwrap(), b.conflicts().unwrap());
}

#[test]
fn causally_ordered_edits_and_late_ancestors_do_not_conflict() {
    let a = store("a");
    let b = store("b");
    let c = store("c");
    a.upsert_account(account()).unwrap();
    exchange(&a, &b);
    exchange(&a, &c);
    patch(&a, "label", json!("A"));
    exchange(&a, &b);
    patch(&b, "label", json!("B"));
    // C receives B's descendant before A's edit. B's context proves causality.
    let descendant = b.changes_since(&BTreeMap::from([(node("a"), 2)]), 100).unwrap();
    c.apply_remote(&descendant.changes).unwrap();
    exchange(&a, &b);
    exchange(&b, &c);
    assert!(a.conflicts().unwrap().is_empty());
    assert!(b.conflicts().unwrap().is_empty());
    assert!(c.conflicts().unwrap().is_empty());
    assert_eq!(a.accounts().unwrap(), c.accounts().unwrap());
    assert_eq!(a.accounts().unwrap()[0].label, "B");
}

#[test]
fn append_deduplication_and_usage_sum_across_nodes() {
    let a = store("a");
    let b = store("b");
    let now = Utc::now();
    let observation = a
        .append_quota(QuotaObservation {
            account: account().id,
            node: node("ignored"),
            seq: 999,
            windows: vec![QuotaWindow {
                kind: WindowKind::FiveHour,
                used_percent: 12.0,
                reset_at: None,
                window_minutes: Some(300),
                observed_at: now,
                source: ObservationSource::Passive,
            }],
            plan: None,
            allowed: Some(true),
        })
        .unwrap();
    assert_eq!(observation.node, node("a"));
    assert_eq!(observation.seq, 1);
    a.add_usage(
        &account().id,
        now,
        ThreadUsage {
            requests: 2,
            input_tokens: 10,
            ..Default::default()
        },
    )
    .unwrap();
    b.add_usage(
        &account().id,
        now,
        ThreadUsage {
            requests: 3,
            cache_read_tokens: 20,
            ..Default::default()
        },
    )
    .unwrap();
    a.append_event(event("event", now)).unwrap();
    let batch = a.changes_since(&SyncCursor::new(), 100).unwrap();
    b.apply_remote(&batch.changes).unwrap();
    let report = b.apply_remote(&batch.changes).unwrap();
    assert_eq!(report.applied, 0);
    assert_eq!(report.ignored, batch.changes.len());
    exchange(&a, &b);
    assert_eq!(
        b.quota_history(&account().id, DateTime::<Utc>::MIN_UTC).unwrap(),
        vec![observation]
    );
    assert_eq!(
        a.latest_windows(&account().id).unwrap(),
        b.latest_windows(&account().id).unwrap()
    );
    assert_eq!(a.events(100).unwrap().len(), 1);
    let buckets = b.usage_buckets(&account().id, DateTime::<Utc>::MIN_UTC).unwrap();
    assert_eq!(buckets[&now].requests, 5);
    assert_eq!(buckets[&now].input_tokens, 10);
    assert_eq!(buckets[&now].cache_read_tokens, 20);
    assert_eq!(
        a.usage_buckets(&account().id, DateTime::<Utc>::MIN_UTC).unwrap(),
        buckets
    );
}

#[test]
fn thread_tombstones_survive_replay_and_newer_records_can_return() {
    let a = store("a");
    let b = store("b");
    let record = thread();
    a.put_thread(record.clone()).unwrap();
    exchange(&a, &b);
    let old = a.changes_since(&SyncCursor::new(), 100).unwrap();
    b.delete_thread(&record.key).unwrap();
    exchange(&a, &b);
    b.apply_remote(&old.changes).unwrap();
    assert!(b.threads().unwrap().is_empty());
    b.put_thread(record.clone()).unwrap();
    exchange(&a, &b);
    assert_eq!(a.threads().unwrap(), vec![record]);
    assert!(a.conflicts().unwrap().is_empty());
}

#[test]
fn divergent_assignments_emit_an_event_without_a_conflict() {
    let a = store("a");
    let b = store("b");
    let mut record = thread();
    a.put_thread(record.clone()).unwrap();
    exchange(&a, &b);
    record.assignment = Some(Assignment {
        account: AccountId("A".into()),
        reason: sap_core::routing::Reason::ParentAffinity,
        at: Utc::now(),
    });
    a.put_thread(record.clone()).unwrap();
    record.assignment.as_mut().unwrap().account = AccountId("B".into());
    b.put_thread(record).unwrap();
    exchange(&a, &b);
    assert_eq!(a.threads().unwrap(), b.threads().unwrap());
    assert!(a.events(100).unwrap().iter().any(|e| e.kind == EventKind::SyncConflict));
    assert!(a.conflicts().unwrap().is_empty());
    let count = a.events(100).unwrap().len();
    let mut record = a.threads().unwrap().remove(0);
    record.assignment.as_mut().unwrap().account = AccountId("C".into());
    a.put_thread(record).unwrap();
    exchange(&a, &b);
    assert_eq!(a.events(100).unwrap().len(), count);
}

#[test]
fn settings_client_access_and_credentials_are_isolated() {
    let a = store("a");
    let b = store("b");
    a.set_setting("routing", json!({"retry_secs": 10, "enabled": true}))
        .unwrap();
    a.set_client_access("client", json!({"enabled": true})).unwrap();
    let before = a.cursor().unwrap();
    let credential = NodeCredential {
        account: account().id,
        node: node("a"),
        credential_file: "/private/mock.json".into(),
        last_refresh: None,
        last_error: None,
    };
    a.put_credential(credential.clone()).unwrap();
    assert_eq!(a.cursor().unwrap(), before);
    assert!(b.put_credential(credential.clone()).is_err());
    exchange(&a, &b);
    assert_eq!(a.credentials().unwrap(), vec![credential]);
    assert!(b.credentials().unwrap().is_empty());
    assert_eq!(a.get_setting("routing").unwrap(), b.get_setting("routing").unwrap());
    assert_eq!(b.client_access("client").unwrap(), Some(json!({"enabled": true})));
    let raw = serde_json::to_string(&a.changes_since(&SyncCursor::new(), 100).unwrap()).unwrap();
    assert!(!raw.contains("/private/mock.json"));
}

#[test]
fn invalid_batches_roll_back_and_sequence_gaps_are_rejected() {
    let a = store("a");
    let b = store("b");
    a.upsert_account(account()).unwrap();
    patch(&a, "label", json!("new"));
    let batch = a.changes_since(&SyncCursor::new(), 100).unwrap();
    assert!(b.apply_remote(&batch.changes[1..]).is_err());
    assert!(b.cursor().unwrap().is_empty());
    let mut invalid = batch.changes.clone();
    invalid[1].op = ChangeOp::Append { value: json!(null) };
    assert!(b.apply_remote(&invalid).is_err());
    assert!(b.accounts().unwrap().is_empty());
    assert!(b.cursor().unwrap().is_empty());
    b.apply_remote(&batch.changes).unwrap();
    let before = b.cursor().unwrap();
    assert!(
        b.patch_account(&account().id, &BTreeMap::from([("priority".into(), json!("bad"))]))
            .is_err()
    );
    assert_eq!(b.cursor().unwrap(), before);
    let page = a.changes_since(&SyncCursor::new(), 1).unwrap();
    assert!(page.more);
    assert_eq!(page.changes.len(), 1);
    assert!(a.changes_since(&SyncCursor::new(), 0).unwrap().more);
}

#[test]
fn receiving_future_hlc_advances_local_writes() {
    let a = store("a");
    let remote = Change {
        context: SyncCursor::new(),
        origin: node("b"),
        seq: 1,
        hlc: Hlc {
            wall_ms: Utc::now().timestamp_millis() + 1_000_000,
            counter: 4,
            node: node("b"),
        },
        table: Table::Settings,
        key: "remote".into(),
        op: ChangeOp::Set {
            field: None,
            value: json!({"enabled": true}),
        },
    };
    a.apply_remote(std::slice::from_ref(&remote)).unwrap();
    a.set_setting("local", json!({"enabled": true})).unwrap();
    let batch = a.changes_since(&SyncCursor::new(), 100).unwrap();
    assert!(batch.changes.iter().find(|c| c.origin == node("a")).unwrap().hlc > remote.hlc);
}

static NEXT_DATABASE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
struct Database(std::path::PathBuf);
impl Database {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "sap-store-{}-{}",
            std::process::id(),
            NEXT_DATABASE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> std::path::PathBuf {
        self.0.join("state.db")
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn durable_state_permissions_and_simultaneous_writers() {
    let db = Database::new();
    let a = Arc::new(Store::open(db.path(), node("a"), Limits::default()).unwrap());
    let second = Arc::new(Store::open(db.path(), node("a"), Limits::default()).unwrap());
    let mut handles = Vec::new();
    for i in 0..4 {
        let a = if i % 2 == 0 { a.clone() } else { second.clone() };
        handles.push(std::thread::spawn(move || {
            for j in 0..10 {
                a.append_event(event(&format!("{i}-{j}"), Utc::now())).unwrap();
            }
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
    assert_eq!(a.cursor().unwrap()[&node("a")], 40);
    assert_eq!(a.events(100).unwrap().len(), 40);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(db.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
        for suffix in ["-wal", "-shm"] {
            let path = std::path::PathBuf::from(format!("{}{suffix}", db.path().display()));
            assert_eq!(std::fs::metadata(path).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }
    a.record_peer_ack(&node("b"), &a.cursor().unwrap()).unwrap();
    a.record_peer_ack(&node("b"), &SyncCursor::new()).unwrap();
    drop(a);
    drop(second);
    let reopened = Store::open(db.path(), node("a"), Limits::default()).unwrap();
    assert_eq!(reopened.events(100).unwrap().len(), 40);
    assert_eq!(reopened.peer_cursors().unwrap()[&node("b")][&node("a")], 40);
    reopened.append_event(event("after-reopen", Utc::now())).unwrap();
    assert_eq!(reopened.cursor().unwrap()[&node("a")], 41);
    assert!(Store::open(db.path(), node("different"), Limits::default()).is_err());
}

#[test]
fn retention_requires_all_peer_acks_and_replay_stays_deduplicated() {
    let a = store("a");
    let b = store("b");
    let now = Utc::now();
    a.append_event(event("old", now - Duration::from_secs(8 * 86_400)))
        .unwrap();
    exchange(&a, &b);
    let original = a.changes_since(&SyncCursor::new(), 100).unwrap();
    let future = now + Duration::from_secs(20 * 86_400);
    a.record_peer_ack(&node("b"), &a.cursor().unwrap()).unwrap();
    a.set_peer_cursor(node("c"), &SyncCursor::new()).unwrap();
    let report = a.compact_at(future).unwrap();
    assert_eq!(report.append_rows_deleted, 1);
    assert_eq!(report.op_log_rows_deleted, 0);
    a.set_peer_cursor(node("c"), &a.cursor().unwrap()).unwrap();
    let report = a.compact_at(future).unwrap();
    assert_eq!(report.op_log_rows_deleted, 1);
    assert!(a.changes_since(&SyncCursor::new(), 100).is_err());
    assert!(a.changes_since(&a.cursor().unwrap(), 100).unwrap().changes.is_empty());
    assert_eq!(a.cursor().unwrap()[&node("a")], 1);
    assert_eq!(a.apply_remote(&original.changes).unwrap().ignored, 1);
    assert!(a.events(100).unwrap().is_empty());
    // A regressing ACK cannot undo prior acknowledgement.
    a.set_peer_cursor(node("c"), &SyncCursor::new()).unwrap();
    assert_eq!(a.peer_cursors().unwrap()[&node("c")][&node("a")], 1);
    a.set_peer_cursor(node("b"), &BTreeMap::from([(node("b"), 10)]))
        .unwrap();
    assert_eq!(a.peer_cursors().unwrap()[&node("b")][&node("b")], 10);
    assert!(
        a.set_peer_cursor(node("b"), &BTreeMap::from([(node("a"), 10)]))
            .is_err()
    );
}

#[test]
fn byte_budget_is_enforced_after_ack_and_reports_pinned_data() {
    let db = Database::new();
    let a = Store::open(
        db.path(),
        node("a"),
        Limits {
            max_bytes: 256 * 1024,
            ..Default::default()
        },
    )
    .unwrap();
    for i in 0..5 {
        let mut e = event(&i.to_string(), Utc::now());
        e.message = "x".repeat(200_000);
        a.append_event(e).unwrap();
    }
    let first = a.compact().unwrap();
    assert!(!first.budget_met);
    assert_eq!(first.op_log_rows_deleted, 0);
    a.record_peer_ack(&node("b"), &a.cursor().unwrap()).unwrap();
    let second = a.compact().unwrap();
    assert!(second.budget_met);
    assert!(second.retained_bytes <= 256 * 1024);
    assert!(second.op_log_rows_deleted > 0);
}

#[test]
fn inactive_threads_are_evicted_while_open_streams_survive() {
    let a = store("a");
    let mut inactive = thread();
    inactive.last_seen = Utc::now() - Duration::from_secs(31 * 86_400);
    let mut active = inactive.clone();
    active.key = ThreadKey::new(Provider::Claude, "active");
    active.open_streams = 1;
    a.put_thread(inactive).unwrap();
    a.put_thread(active.clone()).unwrap();
    let report = a.compact().unwrap();
    assert_eq!(report.threads_deleted, 1);
    assert_eq!(a.threads().unwrap(), vec![active]);
}

#[test]
fn client_access_merges_changed_fields_and_settings_accept_scalars() {
    let a = store("a");
    let b = store("b");
    a.set_client_access("client", json!({"enabled": true, "label": "base"}))
        .unwrap();
    a.set_client_access("empty", json!({})).unwrap();
    a.set_setting("retry_secs", json!(30)).unwrap();
    exchange(&a, &b);
    assert_eq!(b.get_setting("retry_secs").unwrap(), Some(json!(30)));
    assert_eq!(b.client_access("empty").unwrap(), Some(json!({})));
    a.set_client_access("client", json!({"enabled": false, "label": "base"}))
        .unwrap();
    b.set_client_access("client", json!({"enabled": true, "label": "changed"}))
        .unwrap();
    exchange(&a, &b);
    assert_eq!(
        a.client_access("client").unwrap(),
        Some(json!({"enabled": false, "label": "changed"}))
    );
    assert_eq!(a.client_access("client").unwrap(), b.client_access("client").unwrap());
    assert!(a.conflicts().unwrap().is_empty());
}

#[test]
fn conflicts_relay_to_a_third_node_and_stale_creation_cannot_reopen_resolution() {
    let a = store("a");
    let b = store("b");
    let c = store("c");
    a.upsert_account(account()).unwrap();
    exchange(&a, &b);
    exchange(&a, &c);
    patch(&a, "label", json!("A"));
    patch(&b, "label", json!("B"));
    exchange(&a, &b);
    exchange(&b, &c);
    assert_eq!(a.conflicts().unwrap(), c.conflicts().unwrap());
    let conflict = a.conflicts().unwrap().remove(0);
    a.resolve_conflict(&conflict.id, node("a")).unwrap();
    exchange(&a, &c);
    exchange(&b, &c);
    exchange(&a, &b);
    assert_eq!(a.conflicts().unwrap(), b.conflicts().unwrap());
    assert_eq!(a.conflicts().unwrap(), c.conflicts().unwrap());
    assert!(c.conflicts().unwrap()[0].resolved);
    let late_creation = Change {
        context: SyncCursor::new(),
        origin: node("late"),
        seq: 1,
        hlc: Hlc {
            wall_ms: Utc::now().timestamp_millis() + 1_000_000,
            counter: 0,
            node: node("late"),
        },
        table: Table::Conflicts,
        key: conflict.id.clone(),
        op: ChangeOp::Set {
            field: None,
            value: serde_json::to_value(conflict).unwrap(),
        },
    };
    c.apply_remote(&[late_creation]).unwrap();
    exchange(&a, &c);
    exchange(&b, &c);
    assert!(a.conflicts().unwrap()[0].resolved);
    assert!(b.conflicts().unwrap()[0].resolved);
    assert!(c.conflicts().unwrap()[0].resolved);
}

#[test]
fn configuration_tombstone_and_newer_field_converge_in_either_order() {
    check_delete_edit(1, false);
    check_delete_edit(-1, false);
    check_delete_edit(1, true);
}

fn check_delete_edit(edit_offset: i64, causal: bool) {
    let seed = store("seed");
    let a = store("a");
    let b = store("b");
    seed.upsert_account(account()).unwrap();
    exchange(&seed, &a);
    exchange(&seed, &b);
    let wall = Utc::now().timestamp_millis() + 1_000_000;
    let delete = Change {
        context: BTreeMap::from([(node("seed"), 1)]),
        origin: node("delete"),
        seq: 1,
        hlc: Hlc {
            wall_ms: wall,
            counter: 0,
            node: node("delete"),
        },
        table: Table::Accounts,
        key: account().id.0,
        op: ChangeOp::Delete,
    };
    let mut edit_context = BTreeMap::from([(node("seed"), 1)]);
    if causal {
        edit_context.insert(node("delete"), 1);
    }
    let edit = Change {
        context: edit_context,
        origin: node("edit"),
        seq: 1,
        hlc: Hlc {
            wall_ms: wall + edit_offset,
            counter: 0,
            node: node("edit"),
        },
        table: Table::Accounts,
        key: account().id.0,
        op: ChangeOp::Set {
            field: Some("label".into()),
            value: json!("revived"),
        },
    };
    a.apply_remote(&[delete.clone(), edit.clone()]).unwrap();
    b.apply_remote(&[edit, delete]).unwrap();
    assert_eq!(a.accounts().unwrap(), b.accounts().unwrap());
    if edit_offset > 0 {
        assert_eq!(a.accounts().unwrap()[0].label, "revived");
        assert_eq!(a.accounts().unwrap()[0].provider, Provider::Claude);
    } else {
        assert!(a.accounts().unwrap().is_empty());
    }
    assert_eq!(a.conflicts().unwrap(), b.conflicts().unwrap());
    assert_eq!(a.conflicts().unwrap().len(), usize::from(!causal));
}

#[test]
fn every_operation_rejects_context_observing_its_own_future() {
    for op in [
        ChangeOp::Delete,
        ChangeOp::Set {
            field: Some("value".into()),
            value: json!(1),
        },
        ChangeOp::Append { value: json!({}) },
    ] {
        let a = store("a");
        let change = Change {
            origin: node("b"),
            seq: 1,
            context: BTreeMap::from([(node("b"), 1)]),
            hlc: Hlc::zero(node("b")),
            table: Table::Settings,
            key: "key".into(),
            op,
        };
        assert!(
            a.apply_remote(&[change])
                .unwrap_err()
                .to_string()
                .contains("own future")
        );
        assert!(a.cursor().unwrap().is_empty());
    }
}

#[test]
fn pages_merge_origins_in_insertion_order_without_skips() {
    let a = store("a");
    let b = store("b");
    let mut expected = Vec::new();
    for i in 0..150 {
        a.append_event(event(&format!("a-{i}"), Utc::now())).unwrap();
        expected.push((node("a"), i + 1));
        b.append_event(event(&format!("b-{i}"), Utc::now())).unwrap();
        a.apply_remote(&b.changes_since(&a.cursor().unwrap(), 1).unwrap().changes)
            .unwrap();
        expected.push((node("b"), i + 1));
    }
    let mut since = SyncCursor::new();
    let mut actual = Vec::new();
    loop {
        let batch = a.changes_since(&since, 7).unwrap();
        for change in batch.changes {
            since.insert(change.origin.clone(), change.seq);
            actual.push((change.origin, change.seq));
        }
        assert_eq!(batch.more, actual.len() < expected.len());
        if !batch.more {
            break;
        }
    }
    assert_eq!(actual, expected);
    assert!(a.changes_since(&since, 7).unwrap().changes.is_empty());
    // An origin absent from a nearly current cursor must still be sent.
    since.remove(&node("b"));
    let batch = a.changes_since(&since, 1).unwrap();
    assert_eq!((batch.changes[0].origin.clone(), batch.changes[0].seq), (node("b"), 1));
    assert!(batch.more);
}
