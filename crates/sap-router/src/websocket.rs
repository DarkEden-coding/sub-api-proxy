use crate::proxy::Body;
use http::{Request, header};
use hyper::upgrade::OnUpgrade;
use hyper_util::rt::TokioIo;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

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
    let activity = Activity::default();
    tokio::select! {
        biased;
        _ = health.lost_and_stalled(&mut lease, || activity.idle()) => {
            tracing::warn!(server = %health.name(lease.index), "websocket closed: server down and socket stalled, another server available");
        },
        _ = async {
            let (Ok(client), Ok(server)) = tokio::join!(client, server) else { return; };
            let mut client = Tracked { io: TokioIo::new(client), activity: &activity };
            let mut server = Tracked { io: TokioIo::new(server), activity: &activity };
            // Tunnel wire frames unchanged, with bounded copy buffers and backpressure.
            let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
        } => {},
    }
}

/// When bytes last moved in either direction across a relayed socket.
struct Activity {
    start: Instant,
    last_ms: AtomicU64,
}
impl Default for Activity {
    fn default() -> Self {
        Self {
            start: Instant::now(),
            last_ms: AtomicU64::new(0),
        }
    }
}
impl Activity {
    fn touch(&self) {
        let now = u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.last_ms.store(now, Ordering::Relaxed);
    }
    fn idle(&self) -> Duration {
        self.start
            .elapsed()
            .saturating_sub(Duration::from_millis(self.last_ms.load(Ordering::Relaxed)))
    }
}

/// An upgraded connection that records each read on its socket's [`Activity`].
struct Tracked<'a, T> {
    io: T,
    activity: &'a Activity,
}
impl<T: AsyncRead + Unpin> AsyncRead for Tracked<'_, T> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let poll = Pin::new(&mut self.io).poll_read(cx, buf);
        if buf.filled().len() > before {
            self.activity.touch();
        }
        poll
    }
}
impl<T: AsyncWrite + Unpin> AsyncWrite for Tracked<'_, T> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.io).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}
