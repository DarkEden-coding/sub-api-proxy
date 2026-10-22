use super::*;
use crate::control::*;
use axum::{
    body::{Body, to_bytes},
    extract::Request,
    handler::Handler,
    http::{StatusCode, header},
    response::Response,
    routing::any_service,
};
use futures_util::StreamExt;
use sap_core::{api::*, ids::*, reset_aware::*, sync::Conflict};
use serde_json::{Value, json};
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::broadcast;

struct Fake {
    view: FlowSnapshot,
    changes: broadcast::Sender<ControlEvent>,
    calls: Mutex<Vec<String>>,
}
impl Fake {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            view: serde_json::from_str(include_str!("../../../sap-core/tests/fixtures/flow.json")).unwrap(),
            changes: broadcast::channel(128).0,
            calls: Mutex::default(),
        })
    }
    fn called(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }
    fn conflict(&self) -> Conflict {
        serde_json::from_value(json!({"id":"c/1","table":"accounts","key":"a","field":"label","applied":{"node":"n","hlc":{"wall_ms":0,"counter":0,"node":"n"},"value":"x"},"other":{"node":"m","hlc":{"wall_ms":0,"counter":0,"node":"m"},"value":"y"},"detected_at":self.view.now,"resolved":false})).unwrap()
    }
}
impl Control for Fake {
    fn flow(&self) -> FlowSnapshot {
        self.view.clone()
    }
    fn subscribe(&self) -> broadcast::Receiver<ControlEvent> {
        self.changes.subscribe()
    }
    fn accounts(&self) -> Vec<AccountView> {
        self.view.accounts.clone()
    }
    fn patch_account(&self, id: &AccountId, patch: AccountPatch) -> ControlResult<AccountView> {
        self.called(format!("patch:{id}:{}", serde_json::to_string(&patch).unwrap()));
        self.view
            .accounts
            .iter()
            .find(|a| &a.id == id)
            .cloned()
            .ok_or_else(|| ControlError::not_found("account missing"))
    }
    fn start_login(&self, request: LoginRequest) -> BoxFuture<'_, ControlResult<LoginStart>> {
        self.called(format!("login:{}", request.provider));
        Box::pin(async {
            Ok(LoginStart {
                url: "http://localhost/login".into(),
                login_id: "login-1".into(),
            })
        })
    }
    fn account_history(&self, id: &AccountId, hours: u32) -> ControlResult<AccountHistory> {
        self.called(format!("account-history:{id}:{hours}"));
        Ok(AccountHistory {
            account: id.clone(),
            samples: vec![],
        })
    }
    fn threads(&self, active_only: bool) -> Vec<ThreadView> {
        self.called(format!("threads:{active_only}"));
        self.view.threads.clone()
    }
    fn assign_thread(&self, key: &ThreadKey, account: &AccountId) -> ControlResult<ThreadView> {
        self.called(format!("assign:{key}:{account}"));
        let mut t = self.view.threads[0].clone();
        t.key = key.clone();
        t.account = Some(account.clone());
        Ok(t)
    }
    fn nodes(&self) -> Vec<NodeView> {
        self.view.nodes.clone()
    }
    fn conflicts(&self) -> Vec<Conflict> {
        vec![self.conflict()]
    }
    fn resolve_conflict(&self, id: &str, keep: &NodeId) -> ControlResult<Conflict> {
        self.called(format!("resolve:{id}:{keep}"));
        let mut c = self.conflict();
        c.resolved = true;
        Ok(c)
    }
    fn events(&self, limit: usize) -> Vec<EventView> {
        self.called(format!("events:{limit}"));
        self.view.events.iter().take(limit).cloned().collect()
    }
    fn reset_aware_status(&self) -> StatusResponse {
        serde_json::from_value(json!({"now":self.view.now,"polling":false,"settings":{"strategy":"thread-affinity","active":true,"drain_window_seconds":0,"drain_floor_percent":0,"protect_after_seconds":0,"protect_used_percent":0,"five_hour_guard_percent":0,"reset_weight_exponent":0},"accounts":[]})).unwrap()
    }
    fn reset_aware_history(&self, hours: u32) -> HistoryResponse {
        HistoryResponse {
            now: self.view.now,
            hours,
            accounts: vec![],
        }
    }
    fn reset_aware_requests(&self) -> RequestsResponse {
        RequestsResponse {
            now: self.view.now,
            bucket_seconds: 60,
            groups: vec![],
        }
    }
    fn reset_aware_refresh(&self) -> BoxFuture<'_, StatusResponse> {
        self.called("refresh".into());
        Box::pin(async { self.reset_aware_status() })
    }
}
fn app(fake: Arc<Fake>) -> Router {
    router(fake, ManagementAuth::test_only())
}
async fn request(app: Router, method: &str, path: &str, body: Value, auth: bool) -> Response {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    if auth {
        req = req.header(header::AUTHORIZATION, "Bearer fake-management");
    }
    any_service(app)
        .call(req.body(Body::from(body.to_string())).unwrap(), ())
        .await
}
async fn value(response: Response) -> Value {
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap()).unwrap()
}

#[tokio::test]
async fn api_routes_follow_control_shapes_and_arguments() {
    let fake = Fake::new();
    let app = app(fake.clone());
    let account = &fake.view.accounts[0].id.0;
    let cases = vec![
        ("GET", "/sap/api/flow".into(), json!(null), json!(fake.flow())),
        ("GET", "/sap/api/accounts".into(), json!(null), json!(fake.accounts())),
        (
            "PATCH",
            format!("/sap/api/accounts/{account}"),
            json!({"capacity_override":null}),
            json!(fake.accounts()[0]),
        ),
        (
            "POST",
            "/sap/api/accounts/login".into(),
            json!({"provider":"claude"}),
            json!({"url":"http://localhost/login","login_id":"login-1"}),
        ),
        (
            "GET",
            format!("/sap/api/accounts/{account}/history?hours=7"),
            json!(null),
            json!({"account":account,"samples":[]}),
        ),
        (
            "GET",
            "/sap/api/threads?active=true".into(),
            json!(null),
            json!(fake.view.threads),
        ),
        ("GET", "/sap/api/nodes".into(), json!(null), json!(fake.nodes())),
        ("GET", "/sap/api/conflicts".into(), json!(null), json!(fake.conflicts())),
        (
            "POST",
            "/sap/api/conflicts/c%2F1/resolve".into(),
            json!({"keep":"n"}),
            {
                let mut c = fake.conflict();
                c.resolved = true;
                json!(c)
            },
        ),
        (
            "GET",
            "/sap/api/events?limit=1".into(),
            json!(null),
            json!(fake.events(1)),
        ),
        (
            "GET",
            format!("{}/status", BASE_PATH),
            json!(null),
            json!(fake.reset_aware_status()),
        ),
        (
            "GET",
            format!("{}/history?hours=3", BASE_PATH),
            json!(null),
            json!(fake.reset_aware_history(3)),
        ),
        (
            "GET",
            format!("{}/requests", BASE_PATH),
            json!(null),
            json!(fake.reset_aware_requests()),
        ),
        (
            "POST",
            format!("{}/refresh", BASE_PATH),
            json!(null),
            json!(fake.reset_aware_status()),
        ),
    ];
    for (method, path, body, expected) in cases {
        let denied = request(app.clone(), method, &path, body.clone(), false).await;
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(value(denied).await["error"], "unauthorized");
        let response = request(app.clone(), method, &path, body, true).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(value(response).await, expected, "{path}");
    }
    for (encoded, decoded) in [
        ("claude%3Asession%2Fchild", "claude:session/child"),
        ("claude%3Asession%252Fchild", "claude:session%2Fchild"),
    ] {
        let path = format!("/sap/api/threads/{encoded}/assign");
        let res = request(app.clone(), "POST", &path, json!({"account":account}), true).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(value(res).await["key"], decoded);
        assert_eq!(
            request(app.clone(), "POST", &path, json!({"account":account}), false)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let calls = fake.calls.lock().unwrap();
    assert!(calls.contains(&format!("account-history:{account}:7")));
    assert!(calls.contains(&"threads:true".into()));
    assert!(calls.contains(&"resolve:c/1:n".into()));
    assert!(calls.iter().any(|s| s.contains("\"capacity_override\":null")));
}

#[tokio::test]
async fn api_errors_and_dashboard_assets() {
    let app = app(Fake::new());
    for (method, path, body, status) in [
        ("PATCH", "/sap/api/accounts/missing", json!({}), 404),
        ("GET", "/sap/api/missing", json!(null), 404),
        ("PUT", "/sap/api/flow", json!(null), 405),
        ("GET", "/v8/management/routing/reset-aware/refresh", json!(null), 405),
        ("GET", "/sap/api/events?limit=bad", json!(null), 400),
        ("POST", "/sap/api/threads/bad/assign", json!({"account":"a"}), 400),
        ("POST", "/sap/api/accounts/login", json!({"provider":"invalid"}), 400),
    ] {
        let res = request(app.clone(), method, path, body, true).await;
        assert_eq!(res.status().as_u16(), status);
        let body = value(res).await;
        assert!(body["message"].is_string());
    }
    let index = request(app.clone(), "GET", "/sap/", json!(null), false).await;
    assert_eq!(index.status(), StatusCode::OK);
    assert_eq!(index.headers()[header::CACHE_CONTROL], "no-cache");
    assert_eq!(index.headers()[header::CONTENT_TYPE], "text/html; charset=utf-8");
    let index = to_bytes(index.into_body(), 1024 * 1024).await.unwrap();
    let html = std::str::from_utf8(&index).unwrap();
    let asset = html.split("src=\"").nth(1).unwrap().split('"').next().unwrap();
    let response = request(app.clone(), "GET", asset, json!(null), false).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "public, max-age=31536000, immutable"
    );
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/javascript; charset=utf-8"
    );
    assert!(!to_bytes(response.into_body(), 1024 * 1024).await.unwrap().is_empty());
    let fallback = request(app.clone(), "GET", "/sap/accounts", json!(null), false).await;
    assert_eq!(to_bytes(fallback.into_body(), 1024 * 1024).await.unwrap(), index);
    assert_eq!(
        request(app.clone(), "GET", "/sap/assets/missing.js", json!(null), false)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(app.clone(), "GET", "/sap/api/missing", json!(null), true)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(app, "GET", "/sap", json!(null), false).await.headers()[header::LOCATION],
        "/sap/"
    );
}

#[tokio::test]
async fn api_sse_snapshots_coalesce_and_events_are_delivered() {
    let fake = Fake::new();
    let app = app(fake.clone());
    assert_eq!(
        request(app.clone(), "GET", "/sap/api/stream", json!(null), false)
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let response = request(app, "GET", "/sap/api/stream", json!(null), true).await;
    assert_eq!(response.headers()[header::CONTENT_TYPE], "text/event-stream");
    let mut stream = response.into_body().into_data_stream();
    let first = stream.next().await.unwrap().unwrap();
    assert!(std::str::from_utf8(&first).unwrap().starts_with("event: snapshot\n"));
    for _ in 0..10 {
        fake.changes.send(ControlEvent::Changed).unwrap();
    }
    let event = fake.view.events[0].clone();
    fake.changes.send(ControlEvent::Event(event.clone())).unwrap();
    let frame = stream.next().await.unwrap().unwrap();
    let text = std::str::from_utf8(&frame).unwrap();
    assert!(text.starts_with("event: event\n"));
    assert!(text.contains(&serde_json::to_string(&event).unwrap()));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), stream.next())
            .await
            .is_err()
    );
    let next = tokio::time::timeout(Duration::from_secs(1), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(std::str::from_utf8(&next).unwrap().starts_with("event: snapshot\n"));
    assert!(
        tokio::time::timeout(Duration::from_millis(300), stream.next())
            .await
            .is_err()
    );
    drop(stream);
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(fake.changes.receiver_count(), 0);
}

#[tokio::test]
async fn api_sse_drops_slow_subscribers_independently() {
    let fake = Fake::new();
    let app = app(fake.clone());
    let slow = request(app.clone(), "GET", "/sap/api/stream", json!(null), true).await;
    let mut fast = request(app, "GET", "/sap/api/stream", json!(null), true)
        .await
        .into_body()
        .into_data_stream();
    fast.next().await.unwrap().unwrap();
    for _ in 0..40 {
        fake.changes
            .send(ControlEvent::Event(fake.view.events[0].clone()))
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), fast.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    assert_eq!(fake.changes.receiver_count(), 1);
    let bytes = tokio::time::timeout(Duration::from_secs(1), to_bytes(slow.into_body(), 1024 * 1024))
        .await
        .unwrap()
        .unwrap();
    assert!(std::str::from_utf8(&bytes).unwrap().contains("event: snapshot"));
}

// Paused time skips the real 15-second interval.
#[tokio::test(start_paused = true)]
async fn api_sse_keeps_idle_connections_alive() {
    let response = request(app(Fake::new()), "GET", "/sap/api/stream", json!(null), true).await;
    let mut stream = response.into_body().into_data_stream();
    stream.next().await.unwrap().unwrap();
    let keepalive = tokio::time::timeout(Duration::from_secs(16), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(std::str::from_utf8(&keepalive).unwrap().starts_with(':'));
}
