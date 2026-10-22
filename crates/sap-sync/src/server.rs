use std::{
    collections::BTreeMap,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{ConnectInfo, DefaultBodyLimit, Query, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use sap_core::{
    ids::NodeId,
    sync::{ChangeBatch, ChangeLog, SyncCursor},
};
use serde::Deserialize;
use tokio::sync::Notify;

/// Server configuration shared through `Arc`. Secrets are deliberately excluded from Debug.
pub struct ServerState<C> {
    pub(crate) log: Arc<C>,
    pub(crate) notify: Arc<Notify>,
    secrets: BTreeMap<NodeId, Vec<u8>>,
    acknowledgements: Mutex<BTreeMap<NodeId, SyncCursor>>,
    allow_loopback: bool,
    telemetry: Option<Arc<dyn Fn() -> Option<sap_core::health::PeerTelemetry> + Send + Sync>>,
}

impl<C: ChangeLog + 'static> ServerState<C> {
    pub fn new(log: Arc<C>, secrets: BTreeMap<NodeId, Vec<u8>>, notify: Arc<Notify>) -> Self {
        Self {
            log,
            secrets,
            notify,
            acknowledgements: Mutex::new(BTreeMap::new()),
            allow_loopback: false,
            telemetry: None,
        }
    }

    /// Supply a bounded, in-memory snapshot. The callback must not block on I/O.
    pub fn with_telemetry(
        mut self,
        source: impl Fn() -> Option<sap_core::health::PeerTelemetry> + Send + Sync + 'static,
    ) -> Self {
        self.telemetry = Some(Arc::new(source));
        self
    }

    /// Explicit opt-in for local deployments and localhost tests.
    pub fn allow_loopback(mut self) -> Self {
        self.allow_loopback = true;
        self
    }

    /// In-memory view of acknowledgements accepted during this process.
    pub fn acknowledgements(&self) -> BTreeMap<NodeId, SyncCursor> {
        self.acknowledgements.lock().expect("ACK lock poisoned").clone()
    }
}

/// Serve with `into_make_service_with_connect_info::<SocketAddr>()` so address
/// checks use the actual connection, never a forwarded header.
pub fn router<C: ChangeLog + 'static>(state: Arc<ServerState<C>>) -> Router {
    Router::new()
        .route("/sap/sync/changes", get(changes::<C>))
        .route("/sap/sync/ack", post(ack::<C>))
        .route("/sap/sync/telemetry", get(telemetry::<C>))
        .route_layer(middleware::from_fn_with_state(state.clone(), authenticate::<C>))
        .layer(DefaultBodyLimit::max(crate::MAX_BODY))
        .with_state(state)
}

async fn telemetry<C: ChangeLog + 'static>(State(state): State<Arc<ServerState<C>>>) -> Response {
    let Some(sample) = state.telemetry.as_ref().and_then(|source| source()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match serde_json::to_vec(&sample) {
        Ok(body) if body.len() <= crate::MAX_TELEMETRY_BODY => {
            ([("content-type", "application/json")], body).into_response()
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub(crate) fn permitted(ip: IpAddr, loopback: bool) -> bool {
    if loopback && ip.is_loopback() {
        return true;
    }
    match ip {
        IpAddr::V4(ip) => (u32::from(ip) & 0xffc0_0000) == 0x6440_0000,
        IpAddr::V6(ip) => {
            if let Some(ip) = ip.to_ipv4_mapped() {
                return permitted(IpAddr::V4(ip), loopback);
            }
            ip.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0]
        }
    }
}

async fn authenticate<C: ChangeLog + 'static>(
    State(state): State<Arc<ServerState<C>>>,
    request: Request,
    next: Next,
) -> Response {
    let Some(ConnectInfo(address)) = request.extensions().get::<ConnectInfo<SocketAddr>>() else {
        return StatusCode::FORBIDDEN.into_response();
    };
    if !permitted(address.ip(), state.allow_loopback) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let (mut parts, body) = request.into_parts();
    let Some(peer) = parts
        .headers
        .get("x-sap-peer")
        .and_then(|v| v.to_str().ok())
        .map(|v| NodeId(v.into()))
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(secret) = state.secrets.get(&peer) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if secret.is_empty() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let cap = if parts.uri.path() == "/sap/sync/telemetry" {
        crate::MAX_TELEMETRY_BODY
    } else {
        crate::MAX_BODY
    };
    let body = match tokio::time::timeout(Duration::from_secs(10), to_bytes(body, cap)).await {
        Ok(Ok(body)) => body,
        Ok(Err(_)) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
        Err(_) => return StatusCode::REQUEST_TIMEOUT.into_response(),
    };
    if !crate::auth::verify(secret, &parts.method, &parts.uri, &parts.headers, &body) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    parts.extensions.insert(peer);
    next.run(Request::from_parts(parts, Body::from(body))).await
}

#[derive(Deserialize)]
struct ChangesQuery {
    since: String,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    wait_ms: u64,
}
fn default_limit() -> usize {
    crate::MAX_BATCH
}

async fn changes<C: ChangeLog + 'static>(
    State(state): State<Arc<ServerState<C>>>,
    Query(query): Query<ChangesQuery>,
) -> Response {
    let Ok(since) = serde_json::from_str::<SyncCursor>(&query.since) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if query.limit == 0 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let limit = query.limit.min(crate::MAX_BATCH);
    let deadline = tokio::time::Instant::now() + Duration::from_millis(query.wait_ms.min(crate::MAX_WAIT_MS));
    loop {
        // Register before reading the log so notify_waiters cannot race the read.
        let notified = state.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let log = state.log.clone();
        let read_cursor = since.clone();
        let batch = tokio::task::spawn_blocking(move || log.changes_since(&read_cursor, limit)).await;
        let Ok(Ok(batch)) = batch else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };
        if !batch.changes.is_empty() || tokio::time::Instant::now() >= deadline {
            return batch_response(batch);
        }
        if tokio::time::timeout_at(deadline, notified).await.is_err() {
            // Read once more at the deadline to include writes racing the timer.
            let log = state.log.clone();
            return match tokio::task::spawn_blocking(move || log.changes_since(&since, limit)).await {
                Ok(Ok(batch)) => batch_response(batch),
                _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            };
        }
    }
}

/// Size changes in one pass and stop before the wire-size ceiling. Oversized
/// individual changes cannot be transported and return 413 rather than looping.
fn batch_response(mut batch: ChangeBatch) -> Response {
    let mut size = serde_json::to_vec(&ChangeBatch {
        from: batch.from.clone(),
        changes: Vec::new(),
        more: false,
    })
    .map_or(crate::MAX_BODY, |bytes| bytes.len());
    let mut count = 0;
    for change in &batch.changes {
        let Ok(bytes) = serde_json::to_vec(change) else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };
        let needed = bytes.len() + usize::from(count > 0);
        if size.saturating_add(needed) > crate::MAX_BODY {
            // ponytail: a single change must fit in 8 MiB; a future chunked
            // protocol is needed if replicated records exceed this ceiling.
            if count == 0 {
                return StatusCode::PAYLOAD_TOO_LARGE.into_response();
            }
            batch.more = true;
            break;
        }
        size += needed;
        count += 1;
    }
    batch.changes.truncate(count);
    // A second serialization is still linear and avoids a hand-built envelope.
    match serde_json::to_vec(&batch) {
        Ok(body) => ([(axum::http::header::CONTENT_TYPE, "application/json")], body).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn ack<C: ChangeLog + 'static>(
    State(state): State<Arc<ServerState<C>>>,
    axum::Extension(peer): axum::Extension<NodeId>,
    Json(cursor): Json<SyncCursor>,
) -> StatusCode {
    let log = state.log.clone();
    let durable_peer = peer.clone();
    let durable_cursor = cursor.clone();
    if !matches!(
        tokio::task::spawn_blocking(move || { log.record_peer_ack(&durable_peer, &durable_cursor) }).await,
        Ok(Ok(()))
    ) {
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    let mut acknowledgements = state.acknowledgements.lock().expect("ACK lock poisoned");
    let stored = acknowledgements.entry(peer).or_default();
    for (origin, seq) in cursor {
        stored.entry(origin).and_modify(|v| *v = (*v).max(seq)).or_insert(seq);
    }
    StatusCode::NO_CONTENT
}
