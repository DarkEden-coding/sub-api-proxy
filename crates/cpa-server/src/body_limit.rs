//! `server.max-request-body-bytes`, Go v8.0.35 `maxRequestBodyBytesMiddleware`
//! (dd7b4334). Off by default. When set, a declared `Content-Length` over the limit is
//! refused before anything is read, and every other body ends in [`TooLarge`] as soon
//! as it passes the limit, so whoever reads it (request capture, client auth plugins,
//! handlers) answers Go's 413 for its route.
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};

use axum::body::{Body, Bytes, HttpBody};
use axum::extract::{FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::Response;
use http_body::{Frame, SizeHint};

use crate::Runtime;

/// Go `http.MaxBytesError.Error()`.
pub(crate) const MESSAGE: &str = "http: request body too large";

/// A body read stopped at the configured limit.
#[derive(Debug)]
pub(crate) struct TooLarge;

impl std::fmt::Display for TooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(MESSAGE)
    }
}

impl std::error::Error for TooLarge {}

/// The limit the middleware applied to this request, `None` when off (Go stores it on
/// the request as `MaxRequestBodyBytesContextKey`). Readers use this value, never the
/// live config, so a reload during the request does not change it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Limit(pub Option<u64>);

impl<S: Send + Sync> FromRequestParts<S> for Limit {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(parts.extensions.get::<Limit>().copied().unwrap_or_default())
    }
}

/// Whether `error`, or anything it wraps, is [`TooLarge`] (Go `errors.As(err, &maxBytesErr)`).
pub(crate) fn is_too_large(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut next = Some(error);
    while let Some(error) = next {
        if error.is::<TooLarge>() {
            return true;
        }
        next = error.source();
    }
    false
}

/// The middleware's own answer, and request capture's.
pub(crate) fn too_large() -> Response {
    crate::respond::gin_json(413, format!(r#"{{"error":"{MESSAGE}"}}"#))
}

/// Applies the limit to `app`, outside request capture (Go registers it before the
/// request-logging middleware). The limit is read from the current config per request.
pub fn layer(rt: &Arc<Runtime>, app: axum::Router) -> axum::Router {
    app.layer(axum::middleware::from_fn_with_state(rt.clone(), limit))
}

async fn limit(State(rt): State<Arc<Runtime>>, mut request: Request, next: Next) -> Response {
    let Some(max) = rt.config().max_request_body_bytes() else {
        return next.run(request).await;
    };
    // hyper reports a declared Content-Length as the body's exact size.
    if request.body().size_hint().lower() > max {
        return too_large();
    }
    request.extensions_mut().insert(Limit(Some(max)));
    next.run(request.map(|body| {
        Body::new(Capped {
            body,
            left: max,
            over: false,
        })
    }))
    .await
}

/// Go `http.MaxBytesReader`: passes frames through until more than `left` bytes have
/// arrived. The frame that crosses the limit yields its permitted prefix (a slice, not a
/// copy), and every later poll fails. Nothing is buffered.
struct Capped {
    body: Body,
    left: u64,
    over: bool,
}

impl HttpBody for Capped {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        if self.over {
            return Poll::Ready(Some(Err(axum::Error::new(TooLarge))));
        }
        let frame = ready!(Pin::new(&mut self.body).poll_frame(cx));
        let Some(Ok(frame)) = frame else {
            return Poll::Ready(frame);
        };
        let Some(len) = frame.data_ref().map(|data| data.len() as u64) else {
            return Poll::Ready(Some(Ok(frame)));
        };
        if len <= self.left {
            self.left -= len;
            return Poll::Ready(Some(Ok(frame)));
        }
        self.over = true;
        let left = std::mem::take(&mut self.left) as usize;
        match frame.into_data() {
            Ok(data) if left > 0 => Poll::Ready(Some(Ok(Frame::data(data.slice(..left))))),
            _ => Poll::Ready(Some(Err(axum::Error::new(TooLarge)))),
        }
    }

    fn is_end_stream(&self) -> bool {
        !self.over && self.body.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn frame(body: &mut Capped) -> Option<Result<Frame<Bytes>, axum::Error>> {
        std::future::poll_fn(|cx| Pin::new(&mut *body).poll_frame(cx)).await
    }

    /// The frame that crosses the limit yields its permitted prefix, then the error.
    #[tokio::test]
    async fn the_crossing_frame_keeps_its_permitted_prefix() {
        let frames =
            futures_util::stream::iter(["abc", "defgh", "ij"].map(|s| Ok::<_, std::io::Error>(Bytes::from(s))));
        let mut body = Capped {
            body: Body::from_stream(frames),
            left: 5,
            over: false,
        };
        let mut seen = Vec::new();
        let error = loop {
            match frame(&mut body).await {
                Some(Ok(frame)) => seen.push(frame.into_data().unwrap()),
                Some(Err(error)) => break error,
                None => panic!("the body ended without the limit error"),
            }
        };
        assert_eq!(seen, ["abc", "de"]);
        assert!(is_too_large(&error));
        assert!(matches!(frame(&mut body).await, Some(Err(_))), "the error is sticky");
    }
}
