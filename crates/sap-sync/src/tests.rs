use super::*;
use bytes::Bytes;
use http::{Method, Request, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::TokioExecutor,
};
use sap_core::{
    hlc::Hlc,
    ids::NodeId,
    sync::{ApplyReport, Change, ChangeBatch, ChangeLog, ChangeOp, SyncCursor, Table},
};
use std::{
    collections::BTreeMap,
    convert::Infallible,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpListener, sync::Notify, task::JoinHandle};

#[derive(Default)]
struct MemoryLog {
    node: String,
    id: Option<NodeId>,
    changes: Mutex<BTreeMap<(NodeId, u64), Change>>,
}
impl MemoryLog {
    fn new(node: &str) -> Arc<Self> {
        Arc::new(Self {
            node: node.into(),
            id: Some(NodeId(node.into())),
            ..Self::default()
        })
    }
    fn write(&self, notify: &Notify) {
        let mut changes = self.changes.lock().unwrap();
        let seq = changes.keys().filter(|(id, _)| id == self.node()).count() as u64 + 1;
        let change = Change {
            origin: self.node().clone(),
            seq,
            hlc: Hlc {
                wall_ms: crate::now() as i64 * 1000,
                counter: seq as u32,
                node: self.node().clone(),
            },
            table: Table::Settings,
            key: format!("{}-{seq}", self.node),
            op: ChangeOp::Set {
                field: None,
                value: serde_json::json!(seq),
            },
        };
        changes.insert((self.node().clone(), seq), change);
        drop(changes);
        notify.notify_waiters();
    }
}
impl ChangeLog for MemoryLog {
    type Error = Infallible;
    fn node(&self) -> &NodeId {
        self.id.as_ref().unwrap()
    }
    fn cursor(&self) -> Result<SyncCursor, Self::Error> {
        let mut cursor = SyncCursor::new();
        for (origin, seq) in self.changes.lock().unwrap().keys() {
            cursor
                .entry(origin.clone())
                .and_modify(|v| *v = (*v).max(*seq))
                .or_insert(*seq);
        }
        Ok(cursor)
    }
    fn changes_since(&self, since: &SyncCursor, limit: usize) -> Result<ChangeBatch, Self::Error> {
        let mut changes: Vec<_> = self
            .changes
            .lock()
            .unwrap()
            .values()
            .filter(|c| c.seq > since.get(&c.origin).copied().unwrap_or(0))
            .cloned()
            .collect();
        changes.sort_by(|a, b| a.hlc.cmp(&b.hlc));
        let more = changes.len() > limit;
        changes.truncate(limit);
        Ok(ChangeBatch {
            from: self.node().clone(),
            changes,
            more,
        })
    }
    fn apply_remote(&self, incoming: &[Change]) -> Result<ApplyReport, Self::Error> {
        let mut report = ApplyReport::default();
        let mut changes = self.changes.lock().unwrap();
        for change in incoming {
            if let std::collections::btree_map::Entry::Vacant(entry) =
                changes.entry((change.origin.clone(), change.seq))
            {
                entry.insert(change.clone());
                report.applied += 1;
            } else {
                report.ignored += 1;
            }
        }
        Ok(report)
    }
}

struct Server {
    address: SocketAddr,
    task: JoinHandle<()>,
    state: Arc<ServerState<MemoryLog>>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn serve(log: Arc<MemoryLog>, remote: &str, notify: Arc<Notify>, address: Option<SocketAddr>) -> Server {
    let listener = TcpListener::bind(address.unwrap_or_else(|| "127.0.0.1:0".parse().unwrap()))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let state = Arc::new(
        ServerState::new(
            log,
            BTreeMap::from([(NodeId(remote.into()), b"shared-test-secret".to_vec())]),
            notify,
        )
        .allow_loopback(),
    );
    let app = router(state.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
            .await
            .unwrap();
    });
    Server { address, task, state }
}
fn peer(node: &str, address: SocketAddr) -> Peer {
    Peer::new(
        NodeId(node.into()),
        &format!("http://{address}"),
        b"shared-test-secret".to_vec(),
    )
    .unwrap()
}
async fn until(mut predicate: impl FnMut() -> bool) {
    while !predicate() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
fn client() -> Client<HttpConnector, Full<Bytes>> {
    Client::builder(TokioExecutor::new()).build_http()
}
async fn get(
    server: SocketAddr,
    path: &str,
    secret: &[u8],
    timestamp: u64,
    body: &'static str,
    signed_path: Option<&str>,
) -> (StatusCode, Vec<u8>) {
    let uri: http::Uri = format!("http://{server}{path}").parse().unwrap();
    let signed_uri = signed_path
        .map(|p| format!("http://{server}{p}").parse().unwrap())
        .unwrap_or_else(|| uri.clone());
    let timestamp = timestamp.to_string();
    let signature = crate::auth::sign(secret, &Method::GET, &signed_uri, &timestamp, body.as_bytes());
    let request = Request::builder()
        .uri(uri)
        .header("x-sap-peer", "a")
        .header("x-sap-timestamp", timestamp)
        .header("x-sap-signature", signature)
        .body(Full::new(Bytes::from_static(body.as_bytes())))
        .unwrap();
    let response = client().request(request).await.unwrap();
    let status = response.status();
    (
        status,
        response.into_body().collect().await.unwrap().to_bytes().to_vec(),
    )
}

#[tokio::test]
async fn two_nodes_converge_within_one_second_and_ack() {
    tokio::time::timeout(Duration::from_secs(4), async {
        let a = MemoryLog::new("a");
        let b = MemoryLog::new("b");
        let an = Arc::new(Notify::new());
        let bn = Arc::new(Notify::new());
        let sa = serve(a.clone(), "b", an.clone(), None).await;
        let sb = serve(b.clone(), "a", bn.clone(), None).await;
        let aa = SyncAgent::spawn(a.clone(), vec![peer("b", sb.address)], an.clone());
        let ba = SyncAgent::spawn(b.clone(), vec![peer("a", sa.address)], bn.clone());
        a.write(&an);
        b.write(&bn);
        tokio::time::timeout(
            Duration::from_secs(1),
            until(|| a.cursor().unwrap().len() == 2 && a.cursor().unwrap() == b.cursor().unwrap()),
        )
        .await
        .unwrap();
        until(|| {
            sa.state
                .acknowledgements()
                .get(&NodeId("b".into()))
                .is_some_and(|c| c.len() == 2)
                && aa.statuses()[&NodeId("b".into())].last_success.is_some()
        })
        .await;
        assert_eq!(aa.statuses()[&NodeId("b".into())].last_error, None);
        aa.shutdown().await;
        ba.shutdown().await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn partition_and_reconnect_preserve_changes() {
    tokio::time::timeout(Duration::from_secs(8), async {
        let a = MemoryLog::new("a");
        let b = MemoryLog::new("b");
        let an = Arc::new(Notify::new());
        let bn = Arc::new(Notify::new());
        let mut sb = serve(b.clone(), "a", bn.clone(), None).await;
        let address = sb.address;
        sb.task.abort();
        let _ = (&mut sb.task).await;
        let agent = SyncAgent::spawn(a.clone(), vec![peer("b", address)], an.clone());
        until(|| agent.statuses()[&NodeId("b".into())].last_error.is_some()).await;
        a.write(&an);
        b.write(&bn);
        b.write(&bn);
        let sa = serve(a.clone(), "b", an.clone(), None).await;
        let _sb = serve(b.clone(), "a", bn.clone(), Some(address)).await;
        let other = SyncAgent::spawn(b.clone(), vec![peer("a", sa.address)], bn);
        until(|| {
            a.cursor().unwrap().len() == 2
                && a.cursor().unwrap() == b.cursor().unwrap()
                && agent.statuses()[&NodeId("b".into())].last_error.is_none()
        })
        .await;
        assert_eq!(a.changes.lock().unwrap().len(), 3);
        agent.shutdown().await;
        other.shutdown().await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn authentication_binds_query_body_and_timestamp() {
    tokio::time::timeout(Duration::from_secs(4), async {
        let log = MemoryLog::new("b");
        let notify = Arc::new(Notify::new());
        let server = serve(log, "a", notify, None).await;
        let path = "/sap/sync/changes?since=%7B%7D&limit=1";
        for (secret, timestamp, signed_path) in [
            (b"wrong".as_slice(), crate::now(), None),
            (b"shared-test-secret".as_slice(), crate::now() - 61, None),
            (b"shared-test-secret".as_slice(), crate::now() + 120, None),
            (
                b"shared-test-secret".as_slice(),
                crate::now(),
                Some("/sap/sync/changes?since=%7B%7D&limit=2"),
            ),
        ] {
            assert_eq!(
                get(server.address, path, secret, timestamp, "", signed_path).await.0,
                StatusCode::UNAUTHORIZED
            );
        }
        assert_eq!(
            get(server.address, path, b"shared-test-secret", crate::now(), "", None)
                .await
                .0,
            StatusCode::OK
        );
        let uri = path.parse().unwrap();
        let mut headers = http::HeaderMap::new();
        let timestamp = crate::now().to_string();
        headers.insert("x-sap-timestamp", timestamp.parse().unwrap());
        headers.insert(
            "x-sap-signature",
            crate::auth::sign(b"secret", &Method::POST, &uri, &timestamp, b"{}")
                .parse()
                .unwrap(),
        );
        assert!(!crate::auth::verify(b"secret", &Method::POST, &uri, &headers, b"{ }"));
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn long_poll_wakes_all_waiters_and_expires() {
    tokio::time::timeout(Duration::from_secs(4), async {
        let log = MemoryLog::new("b");
        let notify = Arc::new(Notify::new());
        let server = serve(log.clone(), "a", notify.clone(), None).await;
        let address = server.address;
        let path = "/sap/sync/changes?since=%7B%7D&limit=1&wait_ms=2000";
        let first = tokio::spawn(get(address, path, b"shared-test-secret", crate::now(), "", None));
        let second = tokio::spawn(get(address, path, b"shared-test-secret", crate::now(), "", None));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!first.is_finished());
        assert!(!second.is_finished());
        log.write(&notify);
        for task in [first, second] {
            let (status, body) = tokio::time::timeout(Duration::from_millis(500), task)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(status, StatusCode::OK);
            assert_eq!(serde_json::from_slice::<ChangeBatch>(&body).unwrap().changes.len(), 1);
        }
        let start = tokio::time::Instant::now();
        let (_, body) = get(
            address,
            "/sap/sync/changes?since=%7B%22b%22%3A1%7D&wait_ms=30",
            b"shared-test-secret",
            crate::now(),
            "",
            None,
        )
        .await;
        assert!(start.elapsed() >= Duration::from_millis(30));
        assert!(serde_json::from_slice::<ChangeBatch>(&body).unwrap().changes.is_empty());
        log.write(&notify);
        let (_, body) = get(
            address,
            "/sap/sync/changes?since=%7B%7D&limit=1",
            b"shared-test-secret",
            crate::now(),
            "",
            None,
        )
        .await;
        assert!(serde_json::from_slice::<ChangeBatch>(&body).unwrap().more);
    })
    .await
    .unwrap();
}

#[test]
fn peer_configuration_restricts_destinations() {
    for url in [
        "https://100.64.0.1",
        "http://example.com",
        "http://192.168.1.2",
        "http://user@100.64.0.1",
        "http://100.64.0.1/path",
        "http://100.64.0.1?query=1",
        "http://100.64.0.1/#fragment",
    ] {
        assert!(Peer::new(NodeId("b".into()), url, b"secret".to_vec()).is_err(), "{url}");
    }
    for url in [
        "http://100.64.0.1:8317",
        "http://[fd7a:115c:a1e0::1]:8317",
        "http://127.0.0.1:1234",
    ] {
        assert!(Peer::new(NodeId("b".into()), url, b"secret".to_vec()).is_ok(), "{url}");
    }
    for address in ["100.63.255.255", "100.128.0.0", "::1", "127.0.0.1", "fd7a:115c:a1e1::1"] {
        assert!(!crate::server::permitted(address.parse().unwrap(), false));
    }
    for address in [
        "100.64.0.0",
        "100.127.255.255",
        "fd7a:115c:a1e0::1",
        "::ffff:100.64.0.1",
    ] {
        assert!(crate::server::permitted(address.parse().unwrap(), false));
    }
}

#[tokio::test]
async fn response_size_cap_paginates_and_rejects_oversized_records() {
    tokio::time::timeout(Duration::from_secs(8), async {
        let log = MemoryLog::new("b");
        let notify = Arc::new(Notify::new());
        for _ in 0..2 {
            log.write(&notify);
        }
        for change in log.changes.lock().unwrap().values_mut() {
            change.op = ChangeOp::Set {
                field: None,
                value: serde_json::Value::String("x".repeat(5 * 1024 * 1024)),
            };
        }
        let server = serve(log.clone(), "a", notify, None).await;
        let (status, body) = get(
            server.address,
            "/sap/sync/changes?since=%7B%7D",
            b"shared-test-secret",
            crate::now(),
            "",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.len() <= crate::MAX_BODY);
        let batch: ChangeBatch = serde_json::from_slice(&body).unwrap();
        assert!(batch.more);
        assert_eq!(batch.changes.len(), 1);
        let (status, body) = get(
            server.address,
            "/sap/sync/changes?since=%7B%22b%22%3A1%7D",
            b"shared-test-secret",
            crate::now(),
            "",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let batch: ChangeBatch = serde_json::from_slice(&body).unwrap();
        assert!(!batch.more);
        assert_eq!(batch.changes[0].seq, 2);
        log.changes.lock().unwrap().values_mut().next().unwrap().op = ChangeOp::Set {
            field: None,
            value: serde_json::Value::String("x".repeat(9 * 1024 * 1024)),
        };
        assert_eq!(
            get(
                server.address,
                "/sap/sync/changes?since=%7B%7D",
                b"shared-test-secret",
                crate::now(),
                "",
                None
            )
            .await
            .0,
            StatusCode::PAYLOAD_TOO_LARGE
        );
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn default_server_rejects_loopback_even_with_valid_signature() {
    tokio::time::timeout(Duration::from_secs(4), async {
        let log = MemoryLog::new("b");
        let state = Arc::new(ServerState::new(
            log,
            BTreeMap::from([(NodeId("a".into()), b"shared-test-secret".to_vec())]),
            Arc::new(Notify::new()),
        ));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = router(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
                .await
                .unwrap();
        });
        let server = Server { address, task, state };
        assert_eq!(
            get(
                server.address,
                "/sap/sync/changes?since=%7B%7D",
                b"shared-test-secret",
                crate::now(),
                "",
                None
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    })
    .await
    .unwrap();
}
