use std::{
    convert::Infallible,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, Full, StreamBody, combinators::UnsyncBoxBody};
use hyper::{
    body::{Frame, Incoming},
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use sap_router::config::{Config, Server};
use tokio::{
    net::{TcpListener, TcpStream},
    task::{JoinHandle, JoinSet},
    time::{sleep, timeout},
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, protocol::Role},
};

type Error = Box<dyn std::error::Error + Send + Sync>;
type Body = UnsyncBoxBody<Bytes, Error>;
fn full(bytes: impl Into<Bytes>) -> Body {
    Full::new(bytes.into()).map_err(|never| match never {}).boxed_unsync()
}
fn pending_body() -> Body {
    let stream = futures_util::stream::once(async { Ok::<_, Error>(Frame::data(Bytes::from_static(b"first\0\xff"))) })
        .chain(futures_util::stream::pending());
    StreamBody::new(stream).boxed_unsync()
}

#[derive(Default)]
struct Behavior {
    blackhole: AtomicBool,
    not_serving: AtomicBool,
    requests: AtomicUsize,
    captured: Mutex<Vec<(http::HeaderMap, String, Bytes)>>,
}
struct Fake {
    url: String,
    behavior: Arc<Behavior>,
    task: JoinHandle<()>,
}
impl Drop for Fake {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fake {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let behavior = Arc::new(Behavior::default());
        let state = behavior.clone();
        let task = tokio::spawn(async move {
            let mut children = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (stream, _) = accepted.unwrap();
                        let state = state.clone();
                        children.spawn(async move {
                            let service = service_fn(move |request| respond(state.clone(), request));
                            let _ = hyper::server::conn::http1::Builder::new()
                                .serve_connection(TokioIo::new(stream), service).with_upgrades().await;
                        });
                    }
                    _ = children.join_next(), if !children.is_empty() => {},
                }
            }
        });
        Self { url, behavior, task }
    }
}

async fn respond(state: Arc<Behavior>, mut request: Request<Incoming>) -> Result<Response<Body>, Infallible> {
    if state.blackhole.load(Ordering::SeqCst) {
        std::future::pending::<()>().await;
    }
    let path = request.uri().path().to_owned();
    if path == "/sap/health" {
        let state = if state.not_serving.load(Ordering::SeqCst) {
            "degraded"
        } else {
            "serving"
        };
        return Ok(Response::new(full(
            serde_json::to_vec(&serde_json::json!({
                "node":"fake", "role":"primary", "state":state,"version":"test",
                "now":"2026-10-22T00:00:00Z","started_at":"2026-10-22T00:00:00Z",
                "upstream":{"claude":true,"codex":true},"open_streams":0,"sync":{}
            }))
            .unwrap(),
        )));
    }
    state.requests.fetch_add(1, Ordering::SeqCst);
    let headers = request.headers().clone();
    let uri = request.uri().to_string();
    if path == "/ws" {
        state.captured.lock().unwrap().push((headers, uri, Bytes::new()));
        let response =
            tokio_tungstenite::tungstenite::handshake::server::create_response(&request.map_ref_unit()).unwrap();
        let upgraded = hyper::upgrade::on(&mut request);
        tokio::spawn(async move {
            let Ok(upgraded) = upgraded.await else {
                return;
            };
            let mut websocket = WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Server, None).await;
            while let Some(Ok(message)) = websocket.next().await {
                if message.is_close() {
                    let _ = websocket.close(None).await;
                    break;
                }
                if websocket.send(message).await.is_err() {
                    break;
                }
            }
        });
        return Ok(response.map(|_| full("")));
    }
    if path == "/duplex" {
        return Ok(Response::new(
            request
                .into_body()
                .map_err(|error| -> Error { Box::new(error) })
                .boxed_unsync(),
        ));
    }
    let body = request.into_body().collect().await.unwrap().to_bytes();
    state.captured.lock().unwrap().push((headers, uri, body.clone()));
    if path == "/pending" {
        std::future::pending::<()>().await;
    }
    if path == "/stream" {
        return Ok(Response::new(pending_body()));
    }
    Ok(Response::builder()
        .header("x-test", "retained")
        .body(full(body))
        .unwrap())
}

// Build just the handshake head; the request's streaming body remains unconsumed.
trait RequestHead {
    fn map_ref_unit(&self) -> Request<()>;
}
impl<B> RequestHead for Request<B> {
    fn map_ref_unit(&self) -> Request<()> {
        let mut head = Request::builder()
            .method(self.method())
            .uri(self.uri())
            .version(self.version())
            .body(())
            .unwrap();
        *head.headers_mut() = self.headers().clone();
        head
    }
}

struct Local {
    address: String,
    task: JoinHandle<()>,
}
impl Drop for Local {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Local {
    async fn start(servers: &[&Fake], defaults: bool) -> Self {
        Self::start_with(servers, defaults, None).await
    }
    async fn start_with(servers: &[&Fake], defaults: bool, client_key: Option<&str>) -> Self {
        let config = Config {
            client_auth: client_key.map(|key| sap_router::config::bearer(key).unwrap()),
            servers: servers
                .iter()
                .enumerate()
                .map(|(i, s)| Server {
                    name: format!("server{i}"),
                    base_url: s.url.clone(),
                })
                .collect(),
            client_host: "test-machine".into(),
            probe_interval_ms: if defaults { 1000 } else { 50 },
            probe_timeout_ms: if defaults { 1500 } else { 80 },
            stable_secs: 1,
            ..Config::default()
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let task = tokio::spawn(async move {
            sap_router::serve(config, listener, std::future::pending())
                .await
                .unwrap();
        });
        let local = Self { address, task };
        local.wait_preferred("server0").await;
        local
    }
    async fn request(&self, path: &str, body: Body) -> Response<Incoming> {
        let stream = TcpStream::connect(&self.address).await.unwrap();
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .unwrap();
        tokio::spawn(async move {
            let _ = connection.with_upgrades().await;
        });
        sender
            .send_request(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("host", &self.address)
                    .header("authorization", "Bearer test-key")
                    .header("x-sap-client-host", "spoofed")
                    .body(body)
                    .unwrap(),
            )
            .await
            .unwrap()
    }
    async fn status(&self) -> serde_json::Value {
        let stream = TcpStream::connect(&self.address).await.unwrap();
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .unwrap();
        tokio::spawn(async move {
            let _ = connection.await;
        });
        let response = sender
            .send_request(
                Request::builder()
                    .uri("/sap/router/status")
                    .header("host", &self.address)
                    .body(full(""))
                    .unwrap(),
            )
            .await
            .unwrap();
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
    }
    async fn wait_preferred(&self, name: &str) {
        timeout(Duration::from_secs(5), async {
            loop {
                if self.status().await["preferred"] == name {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn byte_exact_streaming_and_healthy_silence() {
    timeout(Duration::from_secs(5), async {
        let primary = Fake::start().await;
        let local = Local::start(&[&primary], false).await;
        // Keep request EOF pending while checking ordinary body preservation.
        let first = Bytes::from_static(b" { \"text\": \"unchanged\" }\n\0\xff");
        let (send, receive) = tokio::sync::mpsc::channel::<Result<Frame<Bytes>, Error>>(1);
        send.send(Ok(Frame::data(first.clone()))).await.unwrap();
        let stream = futures_util::stream::unfold(receive, |mut receiver| async {
            receiver.recv().await.map(|v| (v, receiver))
        });
        let body = StreamBody::new(stream).boxed_unsync();
        let request = local.request("/echo?raw=%2f&x=1&x=2", body);
        tokio::pin!(request);
        assert!(timeout(Duration::from_millis(100), &mut request).await.is_err());
        drop(send);
        let response = request.await;
        assert_eq!(response.headers()["x-test"], "retained");
        assert_eq!(response.into_body().collect().await.unwrap().to_bytes(), first);
        {
            let captured = primary.behavior.captured.lock().unwrap();
            assert_eq!(captured[0].0["authorization"], "Bearer test-key");
            assert_eq!(captured[0].0["x-sap-client-host"], "test-machine");
            assert_eq!(captured[0].1, "/echo?raw=%2f&x=1&x=2");
        }
        // The duplex endpoint echoes each chunk while the request remains open.
        let (send, receive) = tokio::sync::mpsc::channel::<Result<Frame<Bytes>, Error>>(1);
        let stream = futures_util::stream::unfold(receive, |mut receiver| async {
            receiver.recv().await.map(|frame| (frame, receiver))
        });
        send.send(Ok(Frame::data(Bytes::from_static(b"before EOF"))))
            .await
            .unwrap();
        let mut duplex = local
            .request("/duplex", StreamBody::new(stream).boxed_unsync())
            .await
            .into_body();
        assert_eq!(
            duplex.frame().await.unwrap().unwrap().into_data().unwrap(),
            "before EOF"
        );
        send.send(Ok(Frame::data(Bytes::from_static(b"after first response chunk"))))
            .await
            .unwrap();
        assert_eq!(
            duplex.frame().await.unwrap().unwrap().into_data().unwrap(),
            "after first response chunk"
        );
        drop(send);
        assert!(duplex.frame().await.is_none());
        let mut response = local.request("/stream", full("")).await;
        assert_eq!(
            response.body_mut().frame().await.unwrap().unwrap().into_data().unwrap(),
            b"first\0\xff"[..]
        );
        assert!(
            timeout(Duration::from_millis(250), response.body_mut().frame())
                .await
                .is_err()
        );
        assert_eq!(local.status().await["servers"][0]["state"], "healthy");
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn killed_server_and_refused_port_fail_over() {
    timeout(Duration::from_secs(6), async {
        let primary = Fake::start().await;
        let backup = Fake::start().await;
        let local = Local::start(&[&primary, &backup], false).await;
        sleep(Duration::from_millis(60)).await;
        let start = Instant::now();
        primary.task.abort();
        // A healthy snapshot still contains primary: connect refusal must try backup.
        let response = local.request("/echo", full("once")).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.into_body().collect().await.unwrap().to_bytes(), "once");
        assert_eq!(backup.behavior.requests.load(Ordering::SeqCst), 1);
        println!("refused connection to backup: {:?}", start.elapsed());
        local.wait_preferred("server1").await;
        println!("kill to preferred backup: {:?}", start.elapsed());
        assert!(start.elapsed() < Duration::from_secs(5));
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn blackhole_cancels_pending_response_and_open_stream() {
    timeout(Duration::from_secs(9), async {
        let primary = Fake::start().await;
        let backup = Fake::start().await;
        let local = Local::start(&[&primary, &backup], true).await;
        let mut stream = local.request("/stream", full("")).await.into_body();
        stream.frame().await.unwrap().unwrap();
        primary.behavior.blackhole.store(true, Ordering::SeqCst);
        let start = Instant::now();
        let (pending, ended) = tokio::join!(local.request("/pending", full("never replay")), stream.frame());
        assert_eq!(pending.status(), StatusCode::SERVICE_UNAVAILABLE);
        let error: serde_json::Value =
            serde_json::from_slice(&pending.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(error["error"]["type"], "overloaded_error");
        assert!(ended.is_none() || ended.unwrap().is_err());
        local.wait_preferred("server1").await;
        let response = local.request("/echo", full("backup")).await;
        assert_eq!(response.into_body().collect().await.unwrap().to_bytes(), "backup");
        println!(
            "default probes blackhole to backup and stream end: {:?}",
            start.elapsed()
        );
        assert!(start.elapsed() < Duration::from_secs(5));
        assert_eq!(backup.behavior.requests.load(Ordering::SeqCst), 1);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn recovery_requires_continuous_stability() {
    timeout(Duration::from_secs(6), async {
        let primary = Fake::start().await;
        let backup = Fake::start().await;
        let local = Local::start(&[&primary, &backup], false).await;
        primary.behavior.not_serving.store(true, Ordering::SeqCst);
        local.wait_preferred("server1").await;
        let mut backup_stream = local.request("/stream", full("")).await.into_body();
        backup_stream.frame().await.unwrap().unwrap();
        primary.behavior.not_serving.store(false, Ordering::SeqCst);
        sleep(Duration::from_millis(600)).await;
        assert_eq!(local.status().await["preferred"], "server1");
        primary.behavior.not_serving.store(true, Ordering::SeqCst);
        sleep(Duration::from_millis(120)).await;
        primary.behavior.not_serving.store(false, Ordering::SeqCst);
        sleep(Duration::from_millis(600)).await;
        assert_eq!(local.status().await["preferred"], "server1");
        local.wait_preferred("server0").await;
        assert!(
            timeout(Duration::from_millis(150), backup_stream.frame())
                .await
                .is_err()
        );
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn single_server_keeps_serving_through_probe_failures() {
    timeout(Duration::from_secs(6), async {
        let only = Fake::start().await;
        let local = Local::start(&[&only], false).await;
        let mut stream = local.request("/stream", full("")).await.into_body();
        stream.frame().await.unwrap().unwrap();
        // Probes fail long enough to mark the server down, then recover.
        only.behavior.not_serving.store(true, Ordering::SeqCst);
        timeout(Duration::from_secs(2), async {
            while local.status().await["servers"][0]["state"] != "down" {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        // With no server to fail over to, a down server is still tried, not refused.
        let response = local.request("/echo", full("while down")).await;
        assert_eq!(response.status(), StatusCode::OK);
        only.behavior.not_serving.store(false, Ordering::SeqCst);
        timeout(Duration::from_secs(2), async {
            while local.status().await["servers"][0]["state"] != "healthy" {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        // Healthy again but inside the stability period: no preference, still served.
        let status = local.status().await;
        assert_eq!(status["preferred"], serde_json::Value::Null);
        assert_eq!(status["servers"][0]["stable"], false);
        let response = local.request("/echo", full("recovering")).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.into_body().collect().await.unwrap().to_bytes(), "recovering");
        // The stream opened before the outage was never cut off.
        assert!(timeout(Duration::from_millis(100), stream.frame()).await.is_err());
        local.wait_preferred("server0").await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn websocket_frames_and_down_close() {
    timeout(Duration::from_secs(6), async {
        let primary = Fake::start().await;
        let backup = Fake::start().await;
        let local = Local::start(&[&primary, &backup], false).await;
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let mut request = format!("ws://{}/ws", local.address).into_client_request().unwrap();
        request
            .headers_mut()
            .insert("authorization", "Bearer test-key".parse().unwrap());
        request.headers_mut().insert("thread-id", "thread-1".parse().unwrap());
        let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        for message in [
            Message::Text("{ \"type\": \"response.create\" }".into()),
            Message::Binary(vec![0, 255, 1, 2].into()),
            Message::Ping(vec![7].into()),
        ] {
            socket.send(message.clone()).await.unwrap();
            let received = socket.next().await.unwrap().unwrap();
            if message.is_ping() {
                assert!(received.is_pong() || received.is_ping());
            } else {
                assert_eq!(received, message);
            }
        }
        let head = primary.behavior.captured.lock().unwrap()[0].0.clone();
        assert_eq!(head["authorization"], "Bearer test-key");
        assert_eq!(head["x-sap-client-host"], "test-machine");
        assert_eq!(head["thread-id"], "thread-1");
        let start = Instant::now();
        primary.behavior.blackhole.store(true, Ordering::SeqCst);
        loop {
            match socket.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                _ => {}
            }
        }
        println!("websocket close on down: {:?}", start.elapsed());
        local.wait_preferred("server1").await;
        let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws", local.address))
            .await
            .unwrap();
        socket.send(Message::Text("backup".into())).await.unwrap();
        assert_eq!(socket.next().await.unwrap().unwrap(), Message::Text("backup".into()));
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn client_key_replaces_credentials_on_inference_paths_only() {
    timeout(Duration::from_secs(5), async {
        let primary = Fake::start().await;
        let local = Local::start_with(&[&primary], false, Some("client-key")).await;
        local.request("/v1/messages?beta=true", full("{}")).await;
        local.request("/sap/api/flow", full("{}")).await;
        let captured = primary.behavior.captured.lock().unwrap();
        assert_eq!(
            captured[0].0["authorization"], "Bearer client-key",
            "inference uses the client key"
        );
        assert_eq!(
            captured[1].0["authorization"], "Bearer test-key",
            "management keeps its own key"
        );
    })
    .await
    .unwrap();
}
