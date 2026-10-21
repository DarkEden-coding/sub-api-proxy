//! `server.max-request-body-bytes`, Go v8.0.35 `maxRequestBodyBytesMiddleware`
//! (dd7b4334). Off by default. When set, a declared `Content-Length` over the limit is
//! refused before anything is read, and every other body ends in [`TooLarge`] as soon
//! as it passes the limit, so whoever reads it (request capture, client auth plugins,
//! handlers) answers Go's 413 for its route.
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};

use axum::body::{Body, Bytes, HttpBody};
use axum::extract::{Request, State};
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

async fn limit(State(rt): State<Arc<Runtime>>, request: Request, next: Next) -> Response {
    let Some(max) = rt.config().max_request_body_bytes() else {
        return next.run(request).await;
    };
    // hyper reports a declared Content-Length as the body's exact size.
    if request.body().size_hint().lower() > max {
        return too_large();
    }
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
/// arrived, then fails every later poll. Nothing is buffered.
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
        if let Some(Ok(frame)) = &frame
            && let Some(data) = frame.data_ref()
        {
            let len = data.len() as u64;
            if len > self.left {
                self.over = true;
                return Poll::Ready(Some(Err(axum::Error::new(TooLarge))));
            }
            self.left -= len;
        }
        Poll::Ready(frame)
    }

    fn is_end_stream(&self) -> bool {
        !self.over && self.body.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}
