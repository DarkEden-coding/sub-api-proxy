//! `server.max-request-body-bytes`, Go v8.0.35 `maxRequestBodyBytesMiddleware`
//! (dd7b4334). Off by default. When set, a declared `Content-Length` over the limit is
//! refused before anything is read, and every other body ends in [`TooLarge`] as soon
//! as it passes the limit, so whoever reads it (request capture, client auth plugins,
//! handlers) answers Go's 413 for its route.
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, ready};

use axum::body::{Body, Bytes, HttpBody};
use axum::extract::rejection::BytesRejection;
use axum::extract::{FromRequest, FromRequestParts, Request, State};
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

/// A body read the way Go handlers read it under the limit: the bytes, or the read error
/// together with the bytes that arrived before it, which Go's JSON and multipart decoders
/// scan before they report the error.
pub(crate) struct Read {
    pub(crate) body: Result<Bytes, BytesRejection>,
    /// The permitted prefix when the limit stopped the read, else empty.
    pub(crate) prefix: Bytes,
}

impl<S: Send + Sync> FromRequest<S> for Read {
    type Rejection = std::convert::Infallible;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        // Without a limit there is no prefix to keep.
        if request.extensions().get::<Limit>().is_none_or(|l| l.0.is_none()) {
            let body = Bytes::from_request(request, state).await;
            return Ok(Read {
                body,
                prefix: Bytes::new(),
            });
        }
        let (parts, body) = request.into_parts();
        let (body, seen) = Tee::wrap(body);
        let body = Bytes::from_request(Request::from_parts(parts, body), state).await;
        let prefix = match &body {
            Err(rejection) if is_too_large(rejection) => seen.prefix(),
            _ => Bytes::new(),
        };
        Ok(Read { body, prefix })
    }
}

/// [`Read::first_json`] over `axum::body::to_bytes(body, usize::MAX)`.
pub(crate) async fn first_json_body(body: Body) -> Result<Bytes, axum::Error> {
    let (body, seen) = Tee::wrap(body);
    match axum::body::to_bytes(body, usize::MAX).await {
        Err(error) if is_too_large(&error) => {
            let prefix = seen.prefix();
            if first_value_complete(&prefix) {
                Ok(prefix)
            } else {
                Err(error)
            }
        }
        body => body,
    }
}

impl Read {
    /// gin `ShouldBindJSON` and `json.Decoder.Decode`: a first JSON value complete
    /// before the limit binds as if the body ended there.
    pub(crate) fn first_json(self) -> Result<Bytes, BytesRejection> {
        match self.body {
            Err(_) if first_value_complete(&self.prefix) => Ok(self.prefix),
            body => body,
        }
    }
}

#[cfg(test)]
impl From<Bytes> for Read {
    fn from(body: Bytes) -> Self {
        Read {
            body: Ok(body),
            prefix: Bytes::new(),
        }
    }
}

/// [`Read::first_json`] as an extractor.
pub(crate) struct FirstJson(pub(crate) Bytes);

impl<S: Send + Sync> FromRequest<S> for FirstJson {
    type Rejection = BytesRejection;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let Ok(read) = Read::from_request(request, state).await;
        read.first_json().map(FirstJson)
    }
}

/// Whether Go's `json.Decoder` finishes its first value within `prefix` when the read
/// error follows it: a syntax error inside the prefix also ends the scan, a value cut
/// off does not. A top-level object or array ends at its closing bracket; a scalar needs
/// one more byte to end.
fn first_value_complete(prefix: &[u8]) -> bool {
    let mut values = serde_json::Deserializer::from_slice(prefix).into_iter::<serde::de::IgnoredAny>();
    match values.next() {
        Some(Ok(_)) => {
            let end = values.byte_offset();
            end < prefix.len() || matches!(prefix[end - 1], b'}' | b']')
        }
        Some(Err(e)) => !e.is_eof(),
        None => false,
    }
}

/// Keeps a handle on each data frame that passes (no copy).
struct Tee {
    body: Body,
    seen: Seen,
}

#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Vec<Bytes>>>);

impl Seen {
    fn prefix(&self) -> Bytes {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner))
            .concat()
            .into()
    }
}

impl Tee {
    /// `body`, wrapped, and the handle on what passes.
    fn wrap(body: Body) -> (Body, Seen) {
        let seen = Seen::default();
        (
            Body::new(Tee {
                body,
                seen: seen.clone(),
            }),
            seen,
        )
    }
}

impl HttpBody for Tee {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        let frame = ready!(Pin::new(&mut self.body).poll_frame(cx));
        if let Some(Ok(frame)) = &frame
            && let Some(data) = frame.data_ref()
        {
            self.seen
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(data.clone());
        }
        Poll::Ready(frame)
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
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
