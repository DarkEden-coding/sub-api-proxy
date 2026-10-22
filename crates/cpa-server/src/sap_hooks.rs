//! Optional Sub-Api-Proxy integration. Implementations must not log request payloads or headers.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use cpa_core::exec::{ExecError, FailureScope, UsageObserver, UsageSink};
use cpa_core::format::Format;
use sap_core::health::HealthReport;
use sap_core::thread::{ThreadIdentity, ThreadUsage};

use crate::Runtime;
use crate::runtime::{AcquireError, Outcome, Pick};

pub struct SapRequest {
    pub identity: ThreadIdentity,
    pub data: Box<dyn Any + Send + Sync>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SapCandidate {
    pub credential: String,
    pub provider: String,
    pub priority: i64,
}

pub enum SapPick {
    Configured,
    Credential(String),
    Reply {
        status: u16,
        content_type: &'static str,
        body: Bytes,
    },
    /// Serve the current WebSocket response, then close normally.
    CloseSocket,
}

/// Classified without exposing upstream error text, headers or credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptResult {
    pub status: Option<u16>,
    pub error: Option<AttemptError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptError {
    RateLimited,
    Unauthorized,
    Overloaded,
    Request,
    Upstream,
    Cancelled,
}

#[derive(Debug, Clone)]
pub enum Observation {
    ResponseId(String),
    /// Cumulative within this attempt, not an additive delta for each event.
    Usage(ThreadUsage),
    /// Only the executor's allowlisted passive quota signals, never response headers.
    /// The application maps these provider-specific signals into sap-core windows.
    Quota {
        observed_at: std::time::SystemTime,
        signals: BTreeMap<String, String>,
    },
}

pub trait SapHooks: Send + Sync {
    /// Called once per HTTP inference request or response.create frame, before rewrites.
    fn identify(&self, path: &str, headers: &HeaderMap, body: &[u8]) -> Option<Arc<SapRequest>>;
    fn select(&self, request: &SapRequest, candidates: &[SapCandidate]) -> SapPick;
    fn on_outcome(&self, request: &SapRequest, credential: &str, result: &AttemptResult);
    fn on_observation(&self, request: &SapRequest, credential: &str, observation: Observation);
    fn on_stream(&self, request: &SapRequest, open: bool);
    fn health(&self) -> HealthReport;
    // sub-api-proxy: mount node-owned sync and management routes.
    fn routes(&self) -> Option<axum::Router> {
        None
    }
}

pub(crate) struct Context {
    hooks: Arc<dyn SapHooks>,
    request: Arc<SapRequest>,
    reply: Mutex<Option<(u16, &'static str, Bytes)>>,
    close: std::sync::atomic::AtomicBool,
}

tokio::task_local! { static CURRENT: Option<Arc<Context>>; }

pub(crate) fn current() -> Option<Arc<Context>> {
    CURRENT.try_with(Clone::clone).ok().flatten()
}

pub(crate) fn dispatch_context(rt: &Runtime, call: &crate::dispatch::Call) -> Option<Arc<Context>> {
    // A successful scope lookup includes identify returning None; never identify twice.
    CURRENT
        .try_with(Clone::clone)
        .unwrap_or_else(|_| identify(rt, &call.request_path, &call.headers, &call.body))
}

pub(crate) async fn scope<T>(context: Option<Arc<Context>>, future: impl Future<Output = T>) -> T {
    CURRENT.scope(context, future).await
}

pub(crate) fn identify(rt: &Runtime, path: &str, headers: &HeaderMap, body: &[u8]) -> Option<Arc<Context>> {
    let hooks = rt.sap_hooks()?;
    let request = hooks.identify(path, headers, body)?;
    Some(Arc::new(Context {
        hooks,
        request,
        reply: Mutex::default(),
        close: Default::default(),
    }))
}

impl Context {
    pub(crate) fn has_reply(&self) -> bool {
        self.reply.lock().unwrap_or_else(PoisonError::into_inner).is_some()
    }
    pub(crate) fn close_requested(&self) -> bool {
        self.close.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub(crate) fn select(
        &self,
        candidates: Result<Vec<Arc<cpa_core::credential::Credential>>, AcquireError>,
    ) -> Result<Pick, AcquireError> {
        let visible = candidates
            .as_ref()
            .map(|v| {
                v.iter()
                    .map(|c| SapCandidate {
                        credential: c.id.clone(),
                        provider: crate::registry::provider_key(c),
                        priority: crate::scheduler::priority(c),
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        match self.hooks.select(&self.request, &visible) {
            SapPick::Configured => candidates.map(|_| Pick::Configured),
            SapPick::Credential(id) => {
                if !visible.iter().any(|c| c.credential == id) {
                    return Err(AcquireError::Scheduler(ExecError::local(
                        400,
                        FailureScope::Request,
                        "routing hook selected an unavailable credential",
                    )));
                }
                candidates.map(|_| Pick::Auth(id))
            }
            SapPick::Reply {
                status,
                content_type,
                body,
            } => {
                *self.reply.lock().unwrap_or_else(PoisonError::into_inner) = Some((status, content_type, body));
                Err(AcquireError::Scheduler(ExecError::local(
                    400,
                    FailureScope::Request,
                    "request rejected by routing hook",
                )))
            }
            SapPick::CloseSocket => {
                self.close.store(true, std::sync::atomic::Ordering::Relaxed);
                candidates.map(|_| Pick::Configured)
            }
        }
    }

    pub(crate) fn outcome(&self, credential: &str, outcome: &Outcome) {
        let result = match outcome {
            Outcome::Success => AttemptResult {
                status: Some(200),
                error: None,
            },
            Outcome::Cancelled => AttemptResult {
                status: None,
                error: Some(AttemptError::Cancelled),
            },
            Outcome::Failure(e) | Outcome::Neutral(e) => {
                let status = crate::classify::response_status(e);
                let error = match status {
                    401 | 403 => AttemptError::Unauthorized,
                    429 => AttemptError::RateLimited,
                    503 | 529 => AttemptError::Overloaded,
                    400..=499 => AttemptError::Request,
                    _ => AttemptError::Upstream,
                };
                AttemptResult {
                    status: Some(status),
                    error: Some(error),
                }
            }
        };
        self.hooks.on_outcome(&self.request, credential, &result);
    }

    fn observation(&self, credential: &str, value: Observation) {
        self.hooks.on_observation(&self.request, credential, value);
    }

    pub(crate) fn take_reply(&self) -> Option<Response> {
        self.reply
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .map(|(status, content_type, body)| {
                let mut response = Response::new(Body::from(body));
                *response.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                response
                    .headers_mut()
                    .insert(header::CONTENT_TYPE, axum::http::HeaderValue::from_static(content_type));
                response
            })
    }
}

pub(crate) async fn health(State(rt): State<Arc<Runtime>>) -> Response {
    match rt.sap_hooks() {
        Some(hooks) => axum::Json(hooks.health()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub(crate) async fn identify_http(State(rt): State<Arc<Runtime>>, request: Request, next: Next) -> Response {
    if rt.sap_hooks().is_none()
        || request.method() != axum::http::Method::POST
        || !matches!(
            request.uri().path(),
            "/v1/messages"
                | "/v1/responses"
                | "/backend-api/codex/responses"
                | "/v1/responses/compact"
                | "/backend-api/codex/responses/compact"
        )
    {
        return next.run(request).await;
    }
    let (parts, body) = request.into_parts();
    let body = match to_bytes(body, crate::MAX_REQUEST_BYTES).await {
        Ok(body) => body,
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    let context = identify(&rt, parts.uri.path(), &parts.headers, &body);
    let response = scope(context.clone(), next.run(Request::from_parts(parts, Body::from(body)))).await;
    context.and_then(|s| s.take_reply()).unwrap_or(response)
}

struct Observer {
    context: Arc<Context>,
    credential: String,
    rt: Arc<Runtime>,
    provider: String,
    inner: UsageSink,
    usage: Mutex<crate::usage_record::StreamUsage>,
}

impl Observer {
    fn payload(&self, format: Format, payload: &[u8], streaming: bool) {
        let detail = if streaming {
            let mut usage = self.usage.lock().unwrap_or_else(PoisonError::into_inner);
            usage.line(format, payload);
            usage.detail().cloned()
        } else {
            Some(crate::usage_record::parse_body(format, payload))
        };
        if let Some(d) = detail {
            self.context.observation(
                &self.credential,
                Observation::Usage(ThreadUsage {
                    requests: 1,
                    input_tokens: if format == Format::Claude {
                        d.input
                    } else {
                        d.input.saturating_sub(d.cache_read).saturating_sub(d.cache_creation)
                    }
                    .max(0) as u64,
                    output_tokens: d.output.max(0) as u64,
                    cache_read_tokens: d.cache_read.max(0) as u64,
                    cache_write_tokens: d.cache_creation.max(0) as u64,
                }),
            );
        }
        // Borrow only the ID fields. No transcript or response bytes are retained.
        #[derive(serde::Deserialize)]
        struct Head<'a> {
            #[serde(borrow)]
            id: Option<&'a str>,
            #[serde(borrow)]
            message: Option<Id<'a>>,
            #[serde(borrow)]
            response: Option<Id<'a>>,
        }
        #[derive(serde::Deserialize)]
        struct Id<'a> {
            #[serde(borrow)]
            id: Option<&'a str>,
        }
        let payload = payload.strip_prefix(b"data:").unwrap_or(payload).trim_ascii();
        if let Ok(head) = serde_json::from_slice::<Head<'_>>(payload)
            && let Some(id) = head
                .message
                .and_then(|v| v.id)
                .or_else(|| head.response.and_then(|v| v.id))
                .or(head.id)
        {
            self.context
                .observation(&self.credential, Observation::ResponseId(id.to_owned()));
        }
    }
}

impl Drop for Observer {
    fn drop(&mut self) {
        let snapshot = match self.provider.as_str() {
            "claude" => self.rt.executors.claude.quota().snapshot(&self.credential),
            "codex" => self.rt.executors.codex.quota().snapshot(&self.credential),
            _ => None,
        };
        if let Some(snapshot) = snapshot {
            self.context.observation(
                &self.credential,
                Observation::Quota {
                    observed_at: snapshot.observed_at,
                    signals: snapshot.signals,
                },
            );
        }
    }
}

pub(crate) fn observe(
    rt: &Arc<Runtime>,
    credential: &cpa_core::credential::Credential,
    request: &mut cpa_core::exec::ExecRequest,
) {
    let Some(context) = current() else { return };
    let capture = request.capture().clone();
    let inner = std::mem::take(&mut request.usage);

    let websocket = inner.websocket();
    request.usage = UsageSink::new(Arc::new(Observer {
        context,
        credential: credential.id.clone(),
        rt: rt.clone(),
        provider: credential.provider.clone(),
        inner,
        usage: Mutex::default(),
    }))
    .with_capture(capture)
    .with_websocket(websocket);
}

impl UsageObserver for Observer {
    fn response_body(&self, format: Format, body: &[u8]) {
        self.inner.response_body(format, body);
        self.payload(format, body, false);
    }
    fn response_line(&self, format: Format, line: &[u8]) {
        self.inner.response_line(format, line);
        self.payload(format, line, true);
    }
    fn wants_claude_cache(&self) -> bool {
        self.inner.wants_claude_cache()
    }
    fn claude_cache(&self, telemetry: cpa_core::exec::ClaudeCacheTelemetry) {
        self.inner.claude_cache(telemetry)
    }
    fn request(&self, format: Format, payload: &[u8]) {
        self.inner.request(format, payload)
    }
    fn plugin_executor(&self, provider: &str) {
        self.inner.plugin_executor(provider)
    }
    fn plugin_response_body(&self, protocol: &str, body: &[u8]) {
        self.inner.plugin_response_body(protocol, body)
    }
    fn plugin_response_line(&self, protocol: &str, line: &[u8]) {
        self.inner.plugin_response_line(protocol, line)
    }
    fn request_for(&self, identifier: &str, payload: &[u8]) {
        self.inner.request_for(identifier, payload)
    }
    fn upstream_model(&self, model: &str) {
        self.inner.upstream_model(model)
    }
    fn response_model(&self, model: &str) {
        self.inner.response_model(model)
    }
    fn response_model_line(&self, line: &[u8]) {
        self.inner.response_model_line(line)
    }
    fn round_trip_started(&self) {
        self.inner.round_trip_started()
    }
    fn first_byte(&self) {
        self.inner.first_byte()
    }
    fn token_event(&self, is_token: bool) {
        self.inner.token_event(is_token)
    }
    fn publish(&self) {
        self.inner.publish()
    }
    fn publish_failure(&self, status: u16, body: &str) {
        self.inner.publish_failure(status, body)
    }
    fn publish_failure_with_usage(&self, status: u16, body: &str) {
        self.inner.publish_failure_with_usage(status, body)
    }
    fn usage_required(&self) {
        self.inner.usage_required()
    }
    fn discard(&self) {
        self.inner.discard()
    }
    fn upstream_stage(&self, stage: cpa_core::exec::UpstreamStage) {
        self.inner.upstream_stage(stage)
    }
}

/// Own the stream lifetime independently of executor sink clones.
pub(crate) fn stream(inner: cpa_core::exec::ExecStream) -> cpa_core::exec::ExecStream {
    use futures_util::StreamExt;
    let Some(context) = current() else { return inner };
    struct Guard(Arc<Context>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.hooks.on_stream(&self.0.request, false);
        }
    }
    context.hooks.on_stream(&context.request, true);
    let guard = Guard(context);
    futures_util::stream::unfold((inner, guard), |(mut inner, guard)| async move {
        inner.next().await.map(|item| (item, (inner, guard)))
    })
    .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpa_core::config::Config;
    use cpa_exec::Executors;
    use futures_util::{SinkExt, StreamExt};
    use sap_core::health::{NodeRole, NodeState, UpstreamHealth};
    use sap_core::ids::{NodeId, Provider, ThreadKey};
    use sap_core::thread::{ClientKind, Continuation, LineageKind, RequestClass};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tower::ServiceExt;

    #[derive(Default)]
    struct Fake {
        identified: Mutex<Vec<(String, HeaderMap, Vec<u8>)>>,
        candidates: Mutex<Vec<SapCandidate>>,
        pick: Mutex<Option<SapPick>>,
        outcomes: Mutex<Vec<(String, AttemptResult)>>,
        observations: Mutex<Vec<Observation>>,
        streams: Mutex<Vec<bool>>,
    }

    impl SapHooks for Fake {
        fn identify(&self, path: &str, headers: &HeaderMap, body: &[u8]) -> Option<Arc<SapRequest>> {
            self.identified
                .lock()
                .unwrap()
                .push((path.into(), headers.clone(), body.to_vec()));
            Some(Arc::new(SapRequest {
                identity: ThreadIdentity {
                    key: ThreadKey::new(Provider::Claude, "test"),
                    client: ClientKind::Other,
                    class: RequestClass::Turn,
                    lineage: LineageKind::Root,
                    root_session: "test".into(),
                    parent: None,
                    forked_from: None,
                    previous_message_id: None,
                    continuation: Continuation::FullHistory,
                    compaction: None,
                    window_number: None,
                    agent_label: None,
                    turn_id: None,
                    model: None,
                    client_host: None,
                },
                data: Box::new(()),
            }))
        }
        fn select(&self, _: &SapRequest, candidates: &[SapCandidate]) -> SapPick {
            *self.candidates.lock().unwrap() = candidates.to_vec();
            self.pick.lock().unwrap().take().unwrap_or(SapPick::Configured)
        }
        fn on_outcome(&self, _: &SapRequest, credential: &str, result: &AttemptResult) {
            self.outcomes.lock().unwrap().push((credential.into(), result.clone()));
        }
        fn on_observation(&self, _: &SapRequest, _: &str, observation: Observation) {
            self.observations.lock().unwrap().push(observation);
        }
        fn on_stream(&self, _: &SapRequest, open: bool) {
            self.streams.lock().unwrap().push(open);
        }
        // sub-api-proxy: exercise merging a node-owned, already state-bound router.
        fn routes(&self) -> Option<axum::Router> {
            Some(axum::Router::new().route("/sap/test-node-route", axum::routing::get(|| async { "node-owned" })))
        }
        fn health(&self) -> HealthReport {
            HealthReport {
                node: NodeId("test".into()),
                role: NodeRole::Primary,
                state: NodeState::Serving,
                version: "test".into(),
                now: sap_core::Utc::now(),
                started_at: sap_core::Utc::now(),
                upstream: UpstreamHealth {
                    claude: true,
                    codex: true,
                },
                open_streams: 0,
                sync: Default::default(),
            }
        }
    }

    struct Server {
        url: String,
        task: tokio::task::JoinHandle<()>,
    }
    impl Drop for Server {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    async fn serve(app: axum::Router) -> Server {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        Server {
            url,
            task: tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            }),
        }
    }

    struct Fixture {
        rt: Arc<Runtime>,
        dir: std::path::PathBuf,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
    fn runtime(upstream: &str, codex: bool) -> Fixture {
        let dir = std::env::temp_dir().join(format!("sap-hooks-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let provider = if codex { "codex" } else { "claude" };
        let model = if codex { "gpt-5.5" } else { "claude-sonnet-4-5" };
        let mut config = Config::parse(&format!(
            "{provider}-api-key:\n  - api-key: fake-a\n    base-url: {upstream}\n    models:\n      - name: {model}\n  - api-key: fake-b\n    base-url: {upstream}\n    models:\n      - name: {model}\n"
        )).unwrap();
        config.auth_dir = dir.clone();
        let credentials = cpa_core::config::credentials::load(&config);
        Fixture {
            rt: Arc::new(crate::testing::runtime(
                config,
                credentials,
                Executors {
                    claude: cpa_exec::claude::ClaudeExecutor::new(upstream).unwrap(),
                    codex: Default::default(),
                    devices: Default::default(),
                    openai: Default::default(),
                    google: Default::default(),
                },
            )),
            dir,
        }
    }

    async fn request(rt: &Arc<Runtime>, body: &'static [u8]) -> Response {
        tokio::time::timeout(
            Duration::from_secs(10),
            crate::router(rt.clone()).oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .header(sap_core::health::CLIENT_HOST_HEADER, "workstation")
                    .body(Body::from(body))
                    .unwrap(),
            ),
        )
        .await
        .unwrap()
        .unwrap()
    }

    const BODY: &[u8] = b"{ \"model\":\"claude-sonnet-4-5\", \"max_tokens\":10, \"messages\":[] }\n";

    #[tokio::test]
    async fn sap_hooks_exact_reply_and_original_bytes_without_upstream() {
        let fixture = runtime("http://127.0.0.1:9", false);
        let hooks = Arc::new(Fake::default());
        let reply = Bytes::from_static(b"{ \"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\"} }\n\xff");
        *hooks.pick.lock().unwrap() = Some(SapPick::Reply {
            status: 400,
            content_type: "application/json",
            body: reply.clone(),
        });
        fixture.rt.set_sap_hooks(hooks.clone());
        let response = request(&fixture.rt, BODY).await;
        assert_eq!(response.status(), 400);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        assert_eq!(to_bytes(response.into_body(), 4096).await.unwrap(), reply);
        let seen = hooks.identified.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].2, BODY);
        assert_eq!(seen[0].1[sap_core::health::CLIENT_HOST_HEADER], "workstation");
        assert!(hooks.outcomes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn sap_hooks_pin_observe_strip_and_no_hooks_passthrough() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mock_seen = seen.clone();
        let upstream = serve(axum::Router::new().fallback(move |request: Request| {
            let seen = mock_seen.clone();
            async move {
                let (parts, body) = request.into_parts();
                let body = to_bytes(body, 4096).await.unwrap();
                seen.lock().unwrap().push((parts.headers, body));
                ([("content-type", "application/json"), ("anthropic-ratelimit-unified-5h-utilization", "0.25")],
                    r#"{"id":"msg_test","type":"message","role":"assistant","content":[],"usage":{"input_tokens":9,"output_tokens":3,"cache_read_input_tokens":4,"cache_creation_input_tokens":2}}"#)
            }
        })).await;
        let fixture = runtime(&upstream.url, false);
        let baseline = request(&fixture.rt, BODY).await;
        assert_eq!(baseline.status(), 200);
        let baseline = to_bytes(baseline.into_body(), 4096).await.unwrap();
        let id = fixture
            .rt
            .store()
            .snapshot()
            .iter()
            .find(|c| c.attributes.get("api_key").is_some_and(|v| v == "fake-b"))
            .unwrap()
            .id
            .clone();
        let hooks = Arc::new(Fake::default());
        *hooks.pick.lock().unwrap() = Some(SapPick::Credential(id.clone()));
        fixture.rt.set_sap_hooks(hooks.clone());
        let response = request(&fixture.rt, BODY).await;
        assert_eq!(response.status(), 200);
        assert_eq!(to_bytes(response.into_body(), 4096).await.unwrap(), baseline);
        assert_eq!(hooks.identified.lock().unwrap().len(), 1);
        assert!(hooks.candidates.lock().unwrap().iter().any(|c| c.credential == id));
        assert_eq!(
            *hooks.outcomes.lock().unwrap(),
            [(
                id,
                AttemptResult {
                    status: Some(200),
                    error: None
                }
            )]
        );
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(!seen[1].0.contains_key(sap_core::health::CLIENT_HOST_HEADER));
        assert_eq!(seen[1].0["authorization"], "Bearer fake-b");
        // Existing executor rules add stream:false; hooks must add no rewrite.
        assert_eq!(seen[1].1, seen[0].1);
        assert_eq!(hooks.identified.lock().unwrap()[0].2, BODY);
        let observed = hooks.observations.lock().unwrap();
        assert!(
            observed
                .iter()
                .any(|o| matches!(o, Observation::ResponseId(id) if id == "msg_test"))
        );
        assert!(observed.iter().any(|o| matches!(o, Observation::Quota { signals, .. }
            if signals.get("Anthropic-Ratelimit-Unified-5h-Utilization").is_some_and(|v| v == "0.25"))));
        assert!(observed.iter().any(|o| matches!(o, Observation::Usage(u) if u.input_tokens == 9 && u.cache_read_tokens == 4 && u.cache_write_tokens == 2 && u.output_tokens == 3)));
    }

    #[tokio::test]
    async fn sap_hooks_health_is_opt_in() {
        let fixture = runtime("http://127.0.0.1:9", false);
        let health = || {
            Request::builder()
                .uri(sap_core::health::HEALTH_PATH)
                .body(Body::empty())
                .unwrap()
        };
        let response = crate::router(fixture.rt.clone()).oneshot(health()).await.unwrap();
        assert_eq!(response.status(), 404);
        fixture.rt.set_sap_hooks(Arc::new(Fake::default()));
        let response = crate::router(fixture.rt.clone()).oneshot(health()).await.unwrap();
        assert_eq!(response.status(), 200);
        let report: HealthReport =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(report.node.0, "test");
        // sub-api-proxy: health and node routes are mounted together at startup.
        let response = crate::router(fixture.rt.clone())
            .oneshot(
                Request::builder()
                    .uri("/sap/test-node-route")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(to_bytes(response.into_body(), 4096).await.unwrap(), "node-owned");
    }

    #[tokio::test]
    async fn sap_hooks_codex_frames_observations_and_normal_close() {
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let upstream = serve(axum::Router::new().fallback(move |request: Request| {
            let count = count.clone();
            async move {
                assert!(!request.headers().contains_key(sap_core::health::CLIENT_HOST_HEADER));
                count.fetch_add(1, Ordering::SeqCst);
                ([("content-type", "text/event-stream")], concat!(
                    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_test\",\"output\":[]}}\n\n",
                    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_test\",\"output\":[],\"usage\":{\"input_tokens\":10,\"output_tokens\":2,\"input_tokens_details\":{\"cached_tokens\":4}}}}\n\n"
                ))
            }
        })).await;
        let fixture = runtime(&upstream.url, true);
        let hooks = Arc::new(Fake::default());
        fixture.rt.set_sap_hooks(hooks.clone());
        let proxy = serve(crate::router(fixture.rt.clone())).await;
        let url = proxy.url.replacen("http:", "ws:", 1) + "/backend-api/codex/responses";
        let (mut socket, _) = tokio::time::timeout(Duration::from_secs(5), tokio_tungstenite::connect_async(url))
            .await
            .unwrap()
            .unwrap();
        let frame = r#"{ "type":"response.create", "model":"gpt-5.5", "input":[] }"#;
        for close in [false, true] {
            if close {
                *hooks.pick.lock().unwrap() = Some(SapPick::CloseSocket);
            }
            socket
                .send(tokio_tungstenite::tungstenite::Message::Text(frame.into()))
                .await
                .unwrap();
            loop {
                let message = tokio::time::timeout(Duration::from_secs(10), socket.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                if let tokio_tungstenite::tungstenite::Message::Text(text) = message {
                    if text.contains("response.completed") {
                        break;
                    }
                    assert!(!text.contains("\"type\":\"error\""), "{text}");
                }
            }
        }
        let closed = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            matches!(closed, tokio_tungstenite::tungstenite::Message::Close(Some(frame)) if u16::from(frame.code) == 1000)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let identified = hooks.identified.lock().unwrap();
        assert_eq!(identified.len(), 2);
        assert!(identified.iter().all(|(_, _, body)| body == frame.as_bytes()));
        assert_eq!(*hooks.streams.lock().unwrap(), [true, false, true, false]);
        assert_eq!(hooks.outcomes.lock().unwrap().len(), 2);
        let observed = hooks.observations.lock().unwrap();
        assert!(
            observed
                .iter()
                .any(|o| matches!(o, Observation::ResponseId(id) if id == "resp_test"))
        );
        assert!(observed.iter().any(|o| matches!(o, Observation::Usage(u) if u.input_tokens == 6 && u.cache_read_tokens == 4 && u.output_tokens == 2)));
    }
}
