use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use bytes::Bytes;
use http::{Method, Request, Uri};
use http_body_util::{BodyExt, Full, Limited};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::TokioExecutor,
};
use sap_core::{
    ids::NodeId,
    sync::{ChangeBatch, ChangeLog, SyncCursor},
};
use serde::Serialize;
use tokio::{sync::Notify, task::JoinHandle};

/// A configured remote node. No Debug implementation, to avoid exposing secrets.
#[derive(Clone)]
pub struct Peer {
    pub node: NodeId,
    base_url: String,
    secret: Vec<u8>,
}

impl Peer {
    /// HTTP runs inside Tailscale's encrypted network. The base URL must be an
    /// origin with a literal Tailscale or loopback IP, without credentials, path,
    /// query, or fragment. DNS names are rejected to avoid address rebinding.
    pub fn new(node: NodeId, base_url: &str, secret: Vec<u8>) -> Result<Self, String> {
        let base_url = base_url.trim_end_matches('/');
        let uri = base_url.parse::<Uri>().map_err(|_| "invalid peer URL")?;
        if uri.scheme_str() != Some("http")
            || uri.authority().is_none()
            || uri.authority().is_some_and(|a| a.as_str().contains('@'))
            || uri.path_and_query().is_some_and(|p| p.as_str() != "/")
            || base_url.contains('#')
            || secret.is_empty()
        {
            return Err("peer requires an HTTP origin and a nonempty secret".into());
        }
        let host = uri
            .host()
            .ok_or("missing peer host")?
            .trim_start_matches('[')
            .trim_end_matches(']');
        let ip = host.parse().map_err(|_| "peer must use a literal IP address")?;
        if !crate::server::permitted(ip, true) {
            return Err("peer must use a Tailscale or loopback address".into());
        }
        crate::auth::header(&node.0)?;
        Ok(Self {
            node,
            base_url: base_url.into(),
            secret,
        })
    }
}

/// Times are Unix seconds. Lag is the age of the newest applied remote HLC,
/// reset to zero when the peer has no more changes. This is sampled at last_success,
/// not a live backlog measurement. None means not yet observed.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PeerStatus {
    pub last_success: Option<u64>,
    pub last_error: Option<String>,
    pub lag_secs: Option<u64>,
    pub cursor: SyncCursor,
}

/// One pull task per peer. Dropping the agent aborts its tasks.
pub struct SyncAgent {
    tasks: Vec<JoinHandle<()>>,
    statuses: Arc<Mutex<BTreeMap<NodeId, PeerStatus>>>,
}

impl SyncAgent {
    pub fn spawn<C: ChangeLog + 'static>(log: Arc<C>, peers: Vec<Peer>, notify: Arc<Notify>) -> Self {
        let statuses = Arc::new(Mutex::new(BTreeMap::new()));
        let mut tasks = Vec::new();
        for peer in peers {
            // One task per identity even if configuration accidentally repeats it.
            let mut entries = statuses.lock().expect("status lock poisoned");
            if entries.contains_key(&peer.node) {
                continue;
            }
            entries.insert(peer.node.clone(), PeerStatus::default());
            drop(entries);
            tasks.push(tokio::spawn(pull_loop(
                log.clone(),
                peer,
                notify.clone(),
                statuses.clone(),
            )));
        }
        Self { tasks, statuses }
    }

    pub fn statuses(&self) -> BTreeMap<NodeId, PeerStatus> {
        self.statuses.lock().expect("status lock poisoned").clone()
    }

    pub async fn shutdown(mut self) {
        for task in &self.tasks {
            task.abort();
        }
        for task in self.tasks.drain(..) {
            let _ = task.await;
        }
    }
}

impl Drop for SyncAgent {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

type HttpClient = Client<HttpConnector, Full<Bytes>>;

async fn request(
    client: &HttpClient,
    local: &NodeId,
    peer: &Peer,
    method: Method,
    path: &str,
    body: Vec<u8>,
) -> Result<Vec<u8>, String> {
    let uri: Uri = format!("{}{path}", peer.base_url)
        .parse()
        .map_err(|_| "invalid request URL")?;
    let timestamp = crate::now().to_string();
    let signature = crate::auth::sign(&peer.secret, &method, &uri, &timestamp, &body);
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-sap-peer", crate::auth::header(&local.0)?)
        .header("x-sap-timestamp", timestamp)
        .header("x-sap-signature", signature)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body)))
        .map_err(|_| "invalid request")?;
    tokio::time::timeout(Duration::from_secs(30), async {
        let response = client
            .request(request)
            .await
            .map_err(|_| "peer connection failed".to_string())?;
        if !response.status().is_success() {
            return Err(format!("peer returned HTTP {}", response.status().as_u16()));
        }
        Limited::new(response.into_body(), crate::MAX_BODY)
            .collect()
            .await
            .map(|body| body.to_bytes().to_vec())
            .map_err(|_| "invalid or oversized response".into())
    })
    .await
    .map_err(|_| "peer request timed out".to_string())?
}

fn encode_query(value: &str) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(char::from(b));
        } else {
            write!(out, "%{b:02X}").expect("writing to String");
        }
    }
    out
}

async fn exchange<C: ChangeLog + 'static>(
    client: &HttpClient,
    log: Arc<C>,
    peer: &Peer,
    notify: &Notify,
) -> Result<(SyncCursor, Option<u64>, bool), String> {
    let copy = log.clone();
    let cursor = tokio::task::spawn_blocking(move || copy.cursor())
        .await
        .map_err(|_| "cursor task failed")?
        .map_err(|_| "cursor read failed")?;
    let since = encode_query(&serde_json::to_string(&cursor).map_err(|_| "cursor encoding failed")?);
    let path = format!(
        "/sap/sync/changes?since={since}&limit={}&wait_ms={}",
        crate::MAX_BATCH,
        crate::MAX_WAIT_MS
    );
    let body = request(client, log.node(), peer, Method::GET, &path, Vec::new()).await?;
    let batch: ChangeBatch = serde_json::from_slice(&body).map_err(|_| "invalid change batch")?;
    if batch.from != peer.node || batch.changes.len() > crate::MAX_BATCH || (batch.more && batch.changes.is_empty()) {
        return Err("invalid batch identity or size".into());
    }
    let newest = batch.changes.iter().map(|c| c.hlc.wall_ms).max();
    let copy = log.clone();
    let changes = batch.changes;
    let applied = tokio::task::spawn_blocking(move || copy.apply_remote(&changes))
        .await
        .map_err(|_| "apply task failed")?
        .map_err(|_| "batch apply failed")?;
    if applied.applied > 0 {
        notify.notify_waiters();
    }
    let copy = log.clone();
    let updated_cursor = tokio::task::spawn_blocking(move || copy.cursor())
        .await
        .map_err(|_| "cursor task failed")?
        .map_err(|_| "cursor read failed")?;
    if batch.more && updated_cursor == cursor {
        return Err("peer batch made no cursor progress".into());
    }
    let cursor = updated_cursor;
    let body = serde_json::to_vec(&cursor).map_err(|_| "cursor encoding failed")?;
    request(client, log.node(), peer, Method::POST, "/sap/sync/ack", body).await?;
    let lag = if batch.more {
        newest.map(|ms| crate::now().saturating_sub(ms.max(0) as u64 / 1000))
    } else {
        Some(0)
    };
    Ok((cursor, lag, batch.more))
}

async fn pull_loop<C: ChangeLog + 'static>(
    log: Arc<C>,
    peer: Peer,
    notify: Arc<Notify>,
    statuses: Arc<Mutex<BTreeMap<NodeId, PeerStatus>>>,
) {
    let mut connector = HttpConnector::new();
    connector.set_connect_timeout(Some(Duration::from_secs(3)));
    let client = Client::builder(TokioExecutor::new()).build(connector);
    let mut backoff = Duration::from_millis(100);
    loop {
        match exchange(&client, log.clone(), &peer, &notify).await {
            Ok((cursor, lag_secs, more)) => {
                *statuses
                    .lock()
                    .expect("status lock poisoned")
                    .get_mut(&peer.node)
                    .expect("configured peer") = PeerStatus {
                    last_success: Some(crate::now()),
                    last_error: None,
                    lag_secs,
                    cursor,
                };
                backoff = Duration::from_millis(100);
                // Bound retries even when a peer incorrectly returns immediate empty polls.
                if !more {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
            Err(error) => {
                statuses
                    .lock()
                    .expect("status lock poisoned")
                    .get_mut(&peer.node)
                    .expect("configured peer")
                    .last_error = Some(error);
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(5));
            }
        }
    }
}
