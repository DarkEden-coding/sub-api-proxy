use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use http::{HeaderMap, Request, Response, Uri, header};
use http_body_util::{BodyExt, StreamBody};
use hyper::{body::Incoming, client::conn::http1::SendRequest};
use hyper_util::rt::TokioIo;
use tokio::{net::TcpStream, task::JoinHandle};

use crate::{
    config::{Config, Server},
    failover,
    health::Health,
    websocket,
};

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub type Body = http_body_util::combinators::UnsyncBoxBody<Bytes, BoxError>;

pub fn full(bytes: impl Into<Bytes>) -> Body {
    http_body_util::Full::new(bytes.into())
        .map_err(|never| match never {})
        .boxed_unsync()
}

#[derive(Clone)]
pub struct Proxy {
    pub config: Arc<Config>,
    pub health: Arc<Health>,
}

pub struct Connection {
    pub sender: SendRequest<Body>,
    task: JoinHandle<()>,
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.task.abort();
    }
}

// No request is constructed or polled until TCP and HTTP setup have succeeded.
pub async fn connect(server: &Server, timeout_ms: u64) -> anyhow::Result<Connection> {
    tokio::time::timeout(Duration::from_millis(timeout_ms), async {
        let uri: Uri = server.base_url.parse()?;
        let host = uri.host().ok_or_else(|| anyhow::anyhow!("missing host"))?;
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let stream = TcpStream::connect((host, uri.port_u16().unwrap_or(80))).await?;
        stream.set_nodelay(true)?;
        let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
        let task = tokio::spawn(async move {
            let _ = connection.with_upgrades().await;
        });
        Ok(Connection { sender, task })
    })
    .await?
}

fn strip_hop_headers(headers: &mut HeaderMap) {
    let nominated: Vec<_> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|name| http::HeaderName::from_bytes(name.trim().as_bytes()).ok())
        .collect();
    for name in nominated {
        headers.remove(name);
    }
    for name in [
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ] {
        headers.remove(name);
    }
}

pub async fn forward(proxy: Proxy, mut request: Request<Body>) -> Response<Body> {
    if request.method() == http::Method::GET && request.uri().path() == "/sap/router/status" {
        return Response::builder()
            .header("content-type", "application/json")
            .body(full(serde_json::to_vec(&proxy.health.status()).unwrap()))
            .unwrap();
    }
    let upgrade = websocket::is_upgrade(&request);
    for index in proxy.health.candidates() {
        let Some((mut down, generation)) = proxy.health.subscribe(index) else {
            continue;
        };
        let mut connection = match tokio::select! {
            biased;
            _ = failover::down(&mut down, generation) => continue,
            result = connect(&proxy.config.servers[index], proxy.config.connect_timeout_ms) => result,
        } {
            Ok(connection) => connection,
            Err(_) => {
                proxy.health.observe(index, false);
                continue;
            }
        };
        strip_hop_headers(request.headers_mut());
        if upgrade {
            request
                .headers_mut()
                .insert(header::CONNECTION, "upgrade".parse().unwrap());
            request
                .headers_mut()
                .insert(header::UPGRADE, "websocket".parse().unwrap());
        }
        let uri: Uri = proxy.config.servers[index].base_url.parse().unwrap();
        request
            .headers_mut()
            .insert(header::HOST, uri.authority().unwrap().as_str().parse().unwrap());
        request.headers_mut().insert(
            sap_core::health::CLIENT_HOST_HEADER,
            proxy.config.client_host.parse().unwrap(),
        );
        // Preserve the raw path/query, including percent escapes and ordering.
        *request.uri_mut() = request
            .uri()
            .path_and_query()
            .map_or_else(|| "/".parse().unwrap(), |p| p.as_str().parse().unwrap());
        *request.version_mut() = http::Version::HTTP_11;
        let client_upgrade = upgrade.then(|| hyper::upgrade::on(&mut request));
        let response = tokio::select! {
            biased;
            _ = failover::down(&mut down, generation) => return failover::overloaded(),
            response = connection.sender.send_request(request) => response,
        };
        let mut response = match response {
            Ok(response) => response,
            // Transmission began: never replay an ambiguous request, including a GET.
            Err(_) => return failover::overloaded(),
        };
        if response.status() == http::StatusCode::SWITCHING_PROTOCOLS {
            let Some(client_upgrade) = client_upgrade else {
                return failover::overloaded();
            };
            let server_upgrade = hyper::upgrade::on(&mut response);
            strip_hop_headers(response.headers_mut());
            response
                .headers_mut()
                .insert(header::CONNECTION, "upgrade".parse().unwrap());
            response
                .headers_mut()
                .insert(header::UPGRADE, "websocket".parse().unwrap());
            tokio::spawn(websocket::relay(
                client_upgrade,
                server_upgrade,
                down,
                generation,
                connection,
            ));
            let (parts, _) = response.into_parts();
            return Response::from_parts(parts, full(""));
        }
        strip_hop_headers(response.headers_mut());
        let (parts, body) = response.into_parts();
        // The stream owns the upstream connection until EOF or a Down transition.
        return Response::from_parts(parts, streamed(body, down, generation, connection));
    }
    failover::overloaded()
}

fn streamed(body: Incoming, down: tokio::sync::watch::Receiver<u64>, generation: u64, connection: Connection) -> Body {
    let stream = futures_util::stream::unfold(Some((body, down, connection)), move |state| async move {
        let (mut body, mut down, connection) = state?;
        tokio::select! {
            biased;
            _ = failover::down(&mut down, generation) => Some((Err(std::io::Error::new(std::io::ErrorKind::ConnectionAborted, "server down")), None)),
            frame = body.frame() => match frame {
                Some(Ok(frame)) => Some((Ok(frame), Some((body, down, connection)))),
                Some(Err(_)) => Some((Err(std::io::Error::new(std::io::ErrorKind::ConnectionAborted, "upstream stream ended")), None)),
                None => None,
            }
        }
    });
    StreamBody::new(stream)
        .map_err(|error| -> BoxError { Box::new(error) })
        .boxed_unsync()
}
