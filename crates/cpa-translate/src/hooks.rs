//! Plugin translator hooks (Go `sdktranslator.PluginHooks`). Go's server always installs
//! its plugin host with `SetPluginHooks`, and every registry call walks the active
//! plugins that have the capability. Here the installed hooks run only while
//! [`set_active`] says a plugin offers one of them, so without such a plugin a
//! translation costs one atomic load.
//!
//! Formats are Go's format strings (a plugin executor may name formats this crate does
//! not know). On the response side `from` is the upstream format and `to` the client's,
//! as in Go's `TranslateStream(ctx, from, to, ...)`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

use crate::stream::GoStream;
use crate::{Error, ResponseCtx};

/// One response-side hook call's context.
#[derive(Debug, Clone, Copy, Default)]
pub struct Response<'a> {
    pub from: &'a str,
    pub to: &'a str,
    pub model: &'a str,
    pub original_request: &'a [u8],
    pub translated_request: &'a [u8],
    pub stream: bool,
}

/// Go `translator.PluginHooks`. Implementations block for the plugin calls; the
/// normalizers return their input when no plugin answers, the translators `None`.
pub trait PluginHooks: Send + Sync {
    fn normalize_request(&self, from: &str, to: &str, model: &str, body: Vec<u8>, stream: bool) -> Vec<u8>;
    fn translate_request(&self, from: &str, to: &str, model: &str, body: &[u8], stream: bool) -> Option<Vec<u8>>;
    fn normalize_response_before(&self, response: &Response<'_>, body: Vec<u8>) -> Vec<u8>;
    fn translate_response(&self, response: &Response<'_>, body: &[u8]) -> Option<Vec<u8>>;
    fn normalize_response_after(&self, response: &Response<'_>, body: Vec<u8>) -> Vec<u8>;
}

static ACTIVE: AtomicBool = AtomicBool::new(false);
static HOOKS: RwLock<Option<Arc<dyn PluginHooks>>> = RwLock::new(None);

/// Installs (or removes) the hooks. They run once [`set_active`] turns them on.
pub fn install(hooks: Option<Arc<dyn PluginHooks>>) {
    *HOOKS.write().unwrap_or_else(PoisonError::into_inner) = hooks;
}

/// Whether an active plugin offers a translator or normalizer.
pub fn set_active(active: bool) {
    ACTIVE.store(active, Ordering::Release);
}

/// The hooks to run now, if any.
pub fn current() -> Option<Arc<dyn PluginHooks>> {
    if !ACTIVE.load(Ordering::Acquire) {
        return None;
    }
    HOOKS.read().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Go `sdktranslator.NormalizeRequest`, for callers that translate without the
/// registry (the API-key model compatibility translators).
pub fn normalize_request(from: &str, to: &str, model: &str, body: Vec<u8>, stream: bool) -> Vec<u8> {
    match current() {
        Some(hooks) => hooks.normalize_request(from, to, model, body, stream),
        None => body,
    }
}

fn response<'a>(from: &'a str, to: &'a str, ctx: &'a ResponseCtx<'a>, stream: bool) -> Response<'a> {
    Response {
        from,
        to,
        model: ctx.model,
        original_request: ctx.original_request,
        translated_request: ctx.translated_request,
        stream,
    }
}

/// Go `TranslateNonStream` around a registered transform: the before-normalizers, the
/// transform, then the after-normalizers unless the transform failed (Go's nil body).
pub(crate) fn non_stream(
    from: &str,
    to: &str,
    ctx: &ResponseCtx<'_>,
    body: &[u8],
    native: impl FnOnce(&[u8]) -> Result<Vec<u8>, Error>,
) -> Result<Vec<u8>, Error> {
    let Some(hooks) = current() else {
        return native(body);
    };
    let r = response(from, to, ctx, false);
    let body = hooks.normalize_response_before(&r, body.to_vec());
    let out = native(&body)?;
    Ok(hooks.normalize_response_after(&r, out))
}

/// Go `TranslateNonStream` for a pair without a registered transform: the plugin
/// translator's body, or the normalized body when none answers.
pub fn unregistered_non_stream(from: &str, to: &str, ctx: &ResponseCtx<'_>, body: &[u8]) -> Vec<u8> {
    let Some(hooks) = current() else {
        return body.to_vec();
    };
    let r = response(from, to, ctx, false);
    let body = hooks.normalize_response_before(&r, body.to_vec());
    let out = hooks.translate_response(&r, &body).unwrap_or(body);
    hooks.normalize_response_after(&r, out)
}

/// One stream's hook state: Go calls the hooks around each `TranslateStream` call,
/// which is one [`GoStream::line`] here.
struct Hooked {
    hooks: Arc<dyn PluginHooks>,
    /// `None` for an unregistered pair: the plugin translator stands in.
    inner: Option<Box<dyn GoStream>>,
    from: String,
    to: String,
    model: String,
    original_request: Vec<u8>,
    translated_request: Vec<u8>,
}

impl GoStream for Hooked {
    fn line(&mut self, line: &[u8]) -> Result<Vec<Vec<u8>>, Error> {
        let r = Response {
            from: &self.from,
            to: &self.to,
            model: &self.model,
            original_request: &self.original_request,
            translated_request: &self.translated_request,
            stream: true,
        };
        let body = self.hooks.normalize_response_before(&r, line.to_vec());
        let outputs = match &mut self.inner {
            Some(inner) => {
                let outputs = inner.line(&body)?;
                // Go: a retained tool-input failure skips the after-normalizers.
                if inner.tool_input_failed() {
                    return Ok(outputs);
                }
                outputs
            }
            None => vec![self.hooks.translate_response(&r, &body).unwrap_or(body)],
        };
        Ok(outputs
            .into_iter()
            .map(|out| self.hooks.normalize_response_after(&r, out))
            .collect())
    }

    fn tool_input_failed(&self) -> bool {
        self.inner.as_ref().is_some_and(|inner| inner.tool_input_failed())
    }

    fn finalize_tool_input(&mut self) -> Vec<Vec<u8>> {
        self.inner
            .as_mut()
            .map(|inner| inner.finalize_tool_input())
            .unwrap_or_default()
    }
}

fn hooked(
    hooks: Arc<dyn PluginHooks>,
    from: &str,
    to: &str,
    ctx: &ResponseCtx<'_>,
    inner: Option<Box<dyn GoStream>>,
) -> Box<dyn GoStream> {
    Box::new(Hooked {
        hooks,
        inner,
        from: from.to_owned(),
        to: to.to_owned(),
        model: ctx.model.to_owned(),
        original_request: ctx.original_request.to_vec(),
        translated_request: ctx.translated_request.to_vec(),
    })
}

/// A registered stream transform with the hooks active when the stream opens.
pub(crate) fn wrap_stream(from: &str, to: &str, ctx: &ResponseCtx<'_>, inner: Box<dyn GoStream>) -> Box<dyn GoStream> {
    match current() {
        Some(hooks) => hooked(hooks, from, to, ctx, Some(inner)),
        None => inner,
    }
}

/// Go `TranslateStream` for a pair without a registered transform, while hooks are
/// active: each line through the normalizers and the plugin translator. `None` passes
/// lines through unchanged.
pub fn unregistered_stream(from: &str, to: &str, ctx: &ResponseCtx<'_>) -> Option<Box<dyn GoStream>> {
    current().map(|hooks| hooked(hooks, from, to, ctx, None))
}
