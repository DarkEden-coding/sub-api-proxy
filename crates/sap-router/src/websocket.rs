use crate::proxy::Body;
use http::{Request, header};
use hyper::upgrade::OnUpgrade;
use hyper_util::rt::TokioIo;
use tokio::sync::watch;

use crate::{failover, proxy::Connection};

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
    mut down: watch::Receiver<u64>,
    generation: u64,
    _connection: Connection,
) {
    tokio::select! {
        biased;
        _ = failover::down(&mut down, generation) => {},
        _ = async {
            let (Ok(client), Ok(server)) = tokio::join!(client, server) else { return; };
            let mut client = TokioIo::new(client);
            let mut server = TokioIo::new(server);
            // Tunnel wire frames unchanged, with bounded copy buffers and backpressure.
            let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
        } => {},
    }
}
