pub mod config;
mod failover;
mod health;
mod proxy;
mod websocket;

use std::{convert::Infallible, sync::Arc};

use config::Config;
use health::Health;
use http_body_util::BodyExt;
use hyper_util::rt::TokioIo;
use tokio::{net::TcpListener, task::JoinSet};

/// Serve until shutdown, then cancel probes and downstream connections.
pub async fn serve(
    config: Config,
    listener: TcpListener,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    config.validate()?;
    let config = Arc::new(config);
    let health = Health::new(config.clone());
    struct StopHealth(Arc<Health>);
    impl Drop for StopHealth {
        fn drop(&mut self) {
            self.0.shutdown();
        }
    }
    let _stop_health = StopHealth(health.clone());
    let mut probes = JoinSet::new();
    probes.spawn(health.clone().run());
    let mut connections = JoinSet::new();
    tokio::pin!(shutdown);
    let result = loop {
        tokio::select! {
            _ = &mut shutdown => break Ok(()),
            accepted = listener.accept() => {
                let (stream, _) = match accepted { Ok(pair) => pair, Err(error) => break Err(error) };
                let proxy = proxy::Proxy { config: config.clone(), health: health.clone() };
                connections.spawn(async move {
                    let service = hyper::service::service_fn(move |request: http::Request<hyper::body::Incoming>| {
                        let proxy = proxy.clone();
                        async move {
                            let request = request.map(|body| body.map_err(|error| -> proxy::BoxError { Box::new(error) }).boxed_unsync());
                            Ok::<_, Infallible>(proxy::forward(proxy, request).await)
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service).with_upgrades().await;
                });
            }
            _ = connections.join_next(), if !connections.is_empty() => {},
        }
    };
    probes.abort_all();
    result?;
    Ok(())
}
