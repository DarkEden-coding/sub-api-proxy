//! Plugin translator hooks (Go `sdktranslator.PluginHooks`). Go's server always installs
//! its plugin host with `SetPluginHooks`, and every registry call walks the active
//! plugins that have the capability. Here the installed hooks run only while
//! [`publish`] says a plugin offers one of them, so without such a plugin a translation
//! costs one atomic load. The flag is read on every call and every stream line, so open
//! streams follow plugins as they load, unload or fuse.
//!
//! Formats are Go's format strings (a plugin executor may name formats this crate does
//! not know). On the response side `from` is the upstream format and `to` the client's,
//! as in Go's `TranslateStream(ctx, from, to, ...)`.

use std::cell::Cell;
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

/// How a thread may wait for a plugin call. A plugin can call back into the host
/// (`host.http.do`, `host.model.execute`), and those callbacks run on the async runtime,
/// so the thread that waits must not be the one that runtime needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blocking {
    /// Call on this thread; no runtime worker waits on it.
    Direct,
    /// A multi-thread runtime worker: hand its other tasks off while waiting.
    Yield,
    /// The runtime cannot run while this thread waits (a current-thread runtime or a
    /// `LocalSet`): plugin calls are refused.
    Refuse,
}

/// Go `translator.PluginHooks`. Implementations block for the plugin calls; the
/// normalizers return their input when no plugin answers, the translators `None`.
pub trait PluginHooks: Send + Sync {
    fn normalize_request(&self, from: &str, to: &str, model: &str, body: Vec<u8>, stream: bool) -> Vec<u8>;
    fn translate_request(&self, from: &str, to: &str, model: &str, body: &[u8], stream: bool) -> Option<Vec<u8>>;
    fn normalize_response_before(&self, response: &Response<'_>, body: Vec<u8>) -> Vec<u8>;
    fn translate_response(&self, response: &Response<'_>, body: &[u8]) -> Option<Vec<u8>>;
    fn normalize_response_after(&self, response: &Response<'_>, body: Vec<u8>) -> Vec<u8>;
    /// How this thread may wait, judged from its async runtime.
    fn blocking(&self) -> Blocking;
    /// Runs `f` with this worker's other tasks handed off. False, without running `f`,
    /// when the runtime does not allow it.
    fn yield_while(&self, f: &mut dyn FnMut()) -> bool;
}

/// The hooks are installed and the plugin host is enabled: streams keep the context a
/// hook call needs, and translation on a deep stack keeps its worker free.
static ENABLED: AtomicBool = AtomicBool::new(false);
/// An active, unfused plugin offers a translator or normalizer.
static ACTIVE: AtomicBool = AtomicBool::new(false);
static HOOKS: RwLock<Option<Arc<dyn PluginHooks>>> = RwLock::new(None);

thread_local! {
    /// [`Blocking`] passed from the thread that started a deep-stack thread.
    static INHERITED: Cell<Option<Blocking>> = const { Cell::new(None) };
}

/// Installs (or removes) the hooks. They run once [`publish`] turns them on.
pub fn install(hooks: Option<Arc<dyn PluginHooks>>) {
    *HOOKS.write().unwrap_or_else(PoisonError::into_inner) = hooks;
}

/// The plugin host's state: whether it is enabled, and whether an active plugin offers a
/// translator or normalizer.
pub fn publish(enabled: bool, active: bool) {
    ENABLED.store(enabled, Ordering::Release);
    ACTIVE.store(active, Ordering::Release);
}

fn installed() -> Option<Arc<dyn PluginHooks>> {
    HOOKS.read().unwrap_or_else(PoisonError::into_inner).clone()
}

/// The hooks to run now, if any.
pub fn current() -> Option<Arc<dyn PluginHooks>> {
    if !ACTIVE.load(Ordering::Acquire) {
        return None;
    }
    installed()
}

/// The [`Blocking`] a deep-stack thread inherited from the worker that started it.
pub fn inherited() -> Option<Blocking> {
    INHERITED.with(Cell::get)
}

/// Runs `run` (which starts a deep-stack thread and joins it) so a plugin call on that
/// thread cannot deadlock the runtime: a worker yields while it joins, and the thread
/// learns how it may block. `run` gets the [`Blocking`] for the new thread.
pub(crate) fn deep_stack<T>(mut run: impl FnMut(Option<Blocking>) -> T) -> T {
    let hooks = if ENABLED.load(Ordering::Acquire) {
        installed()
    } else {
        None
    };
    let Some(hooks) = hooks else {
        return run(None);
    };
    match inherited().unwrap_or_else(|| hooks.blocking()) {
        Blocking::Yield => {
            let mut out = None;
            if hooks.yield_while(&mut || out = Some(run(Some(Blocking::Direct)))) {
                out.expect("yield_while ran the join")
            } else {
                run(Some(Blocking::Refuse))
            }
        }
        mode => run(Some(mode)),
    }
}

/// Sets the [`Blocking`] a deep-stack thread inherited.
pub(crate) fn inherit(mode: Option<Blocking>) {
    INHERITED.with(|cell| cell.set(mode));
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
/// transform, then the after-normalizers unless the transform failed. Go skips them only
/// for a nil body, which its translators return on a tool-input failure (an `Err` here);
/// an empty body (Go `[]byte{}`) still goes through them.
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

/// One stream's hook context: Go calls the hooks around each `TranslateStream` call,
/// which is one [`GoStream::line`] here, and finds the hook plugins at each call.
struct Hooked {
    /// `None` for an unregistered pair: the plugin translator stands in.
    inner: Option<Box<dyn GoStream>>,
    from: String,
    to: String,
    model: String,
    original_request: Vec<u8>,
    translated_request: Vec<u8>,
}

impl Hooked {
    fn unhooked(&mut self, line: &[u8]) -> Result<Vec<Vec<u8>>, Error> {
        match &mut self.inner {
            Some(inner) => inner.line(line),
            None => Ok(vec![line.to_vec()]),
        }
    }
}

impl GoStream for Hooked {
    fn line(&mut self, line: &[u8]) -> Result<Vec<Vec<u8>>, Error> {
        let Some(hooks) = current() else {
            return self.unhooked(line);
        };
        let r = Response {
            from: &self.from,
            to: &self.to,
            model: &self.model,
            original_request: &self.original_request,
            translated_request: &self.translated_request,
            stream: true,
        };
        let body = hooks.normalize_response_before(&r, line.to_vec());
        let outputs = match &mut self.inner {
            Some(inner) => {
                let outputs = inner.line(&body)?;
                // Go: a retained tool-input failure skips the after-normalizers.
                if inner.tool_input_failed() {
                    return Ok(outputs);
                }
                outputs
            }
            None => vec![hooks.translate_response(&r, &body).unwrap_or(body)],
        };
        Ok(outputs
            .into_iter()
            .map(|out| hooks.normalize_response_after(&r, out))
            .collect())
    }

    fn framing_line(&mut self, line: &[u8]) -> Result<Vec<Vec<u8>>, Error> {
        self.unhooked(line)
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

fn hooked(from: &str, to: &str, ctx: &ResponseCtx<'_>, inner: Option<Box<dyn GoStream>>) -> Box<dyn GoStream> {
    Box::new(Hooked {
        inner,
        from: from.to_owned(),
        to: to.to_owned(),
        model: ctx.model.to_owned(),
        original_request: ctx.original_request.to_vec(),
        translated_request: ctx.translated_request.to_vec(),
    })
}

/// A registered stream transform, with the context for hook calls while the plugin host
/// is enabled (without plugins the stream is returned as is).
pub(crate) fn wrap_stream(from: &str, to: &str, ctx: &ResponseCtx<'_>, inner: Box<dyn GoStream>) -> Box<dyn GoStream> {
    if ENABLED.load(Ordering::Acquire) {
        hooked(from, to, ctx, Some(inner))
    } else {
        inner
    }
}

/// Go `TranslateStream` for a pair without a registered transform, while the plugin host
/// is enabled: each line through the normalizers and the plugin translator when they are
/// active, unchanged otherwise. `None` passes lines through unchanged.
pub fn unregistered_stream(from: &str, to: &str, ctx: &ResponseCtx<'_>) -> Option<Box<dyn GoStream>> {
    ENABLED.load(Ordering::Acquire).then(|| hooked(from, to, ctx, None))
}
