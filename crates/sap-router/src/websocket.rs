use crate::proxy::Body;
use http::{Request, header};
use hyper::upgrade::OnUpgrade;
use hyper_util::rt::TokioIo;
use std::sync::Arc;

use crate::{
    health::{Health, Lease},
    proxy::Connection,
};

pub fn is_upgrade(request: &Request<Body>) -> bool {
    request
        .headers()
        .get(header::UPGRADE)
        .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"websocket"))
        && request.headers().get_all(header::CONNECTION).iter().any(|value| {
            value.to_str().is_ok_and(|value| {
                value
                    .split(',')
                    .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
            })
        })
}

pub async fn relay(
    client: OnUpgrade,
    server: OnUpgrade,
    health: Arc<Health>,
    mut lease: Lease,
    _connection: Connection,
) {
    tokio::select! {
        biased;
        _ = health.lost(&mut lease) => {
            tracing::warn!(server = %health.name(lease.index), "websocket closed: server down, another server available");
        },
        _ = async {
            let (Ok(client), Ok(server)) = tokio::join!(client, server) else { return; };
            let mut client = TokioIo::new(client);
            let mut server = TokioIo::new(server);
            // Tunnel wire frames unchanged, with bounded copy buffers and backpressure.
            let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
        } => {},
    }
}
