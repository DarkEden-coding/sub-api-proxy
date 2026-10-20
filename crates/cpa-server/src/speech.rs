//! `POST /v1/audio/speech` and `POST /v1/tts`: OpenAI text-to-speech requests served by
//! xAI's unary `/tts` (sdk/api/handlers/openai/openai_speech_handlers.go). The handler
//! rewrites the request into xAI's shape; the raw audio comes back as is.

use std::sync::Arc;

use axum::Extension;
use axum::body::{Body, Bytes};
use axum::extract::rejection::BytesRejection;
use axum::extract::{MatchedPath, OriginalUri, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use cpa_common::gostr::GoStr;
use cpa_common::json::{self as gj, Kind};
use cpa_core::exec::{Caller, Operation};
use cpa_core::format::Format;

use crate::claude::read_failed;
use crate::dispatch::{self, Call, Done, Media, MediaKind};
use crate::gojson::{Obj, trim};
use crate::images::{bad_request, text};
use crate::{Runtime, errors, respond};

const DEFAULT_MODEL: &str = "grok-tts";
const VOICE_MODEL: &str = "grok-voice-tts-1.0";
const DEFAULT_VOICE: &str = "eve";
const MAX_RUNES: usize = 60_000;
const MAX_BODY: usize = 1 << 20;
const DEFAULT_SAMPLE_RATE: i64 = 24_000;

/// `openAISpeechVoices`: OpenAI voice names on Grok voices. Other names pass through, so
/// Grok voice IDs keep working.
const VOICES: [(&str, &str); 11] = [
    ("alloy", "ara"),
    ("ash", "orion"),
    ("ballad", "luna"),
    ("coral", "celeste"),
    ("echo", "rex"),
    ("fable", "sal"),
    ("onyx", "leo"),
    ("nova", "eve"),
    ("sage", "iris"),
    ("shimmer", "aurora"),
    ("verse", "lumen"),
];

/// `handleXAISpeech`, for both routes.
#[allow(clippy::too_many_arguments)]
pub async fn speech(
    State(rt): State<Arc<Runtime>>,
    Extension(caller): Extension<Caller>,
    peer: dispatch::Peer,
    matched: Option<MatchedPath>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let body = match body {
        Ok(body) => body,
        Err(rejection) => return read_failed(&rejection),
    };
    if body.len() > MAX_BODY {
        return bad_request("request body is larger than 1MB");
    }
    let requested = trim(&text(&body, "model")).to_owned();
    let Some(model) = routing_model(&requested) else {
        return bad_request(&format!(
            "Model {requested} is not supported on /v1/audio/speech. Use {DEFAULT_MODEL}."
        ));
    };
    let (payload, format) = match build_payload(&body) {
        Ok(built) => built,
        Err(message) => return bad_request(&message),
    };
    let call = Call {
        entry: Format::OpenAI,
        response: Format::OpenAI,
        operation: Operation::Generate,
        model: model.to_owned(),
        body: Bytes::from(payload),
        stream: false,
        alt: None,
        headers,
        caller,
        forced_provider: None,
        selection_model: None,
        execution_session: None,
        request_path: dispatch::route_path(matched.as_ref(), &uri),
        peer: dispatch::peer(peer),
        turn: None,
        media: Some(Arc::new(Media {
            kind: MediaKind::Speech,
            pinned: None,
            on_selected: None,
            sse: false,
            disallow_free: false,
        })),
    };
    // Go reads the upstream Content-Type from the headers the executor call hands back,
    // which exist only under `requests.passthrough-headers`.
    // ponytail: a response interceptor's header change outside passthrough is not
    // consulted here; serve still writes it to the response.
    let passthrough = respond::passthrough_headers(&rt.config());
    dispatch::serve(&rt, call, move |result| async move {
        match result {
            Ok(Done::Buffered { headers, body }) => {
                let upstream = passthrough
                    .then(|| {
                        respond::filter_upstream_headers(&headers)
                            .get(header::CONTENT_TYPE)
                            .cloned()
                    })
                    .flatten();
                audio(format, upstream, body)
            }
            Ok(Done::Stream { .. }) => unreachable!("a non-stream call is buffered"),
            Err(failure) => errors::openai(&failure),
        }
    })
    .await
}

/// The audio response: the upstream Content-Type unless it names no audio type
/// (`speechResponseContentType`), then the format's own.
fn audio(format: &str, upstream: Option<HeaderValue>, body: Bytes) -> Response {
    let content_type = match upstream {
        Some(value) if !content_type_needs_default(value.to_str().unwrap_or_default()) => value,
        _ => HeaderValue::from_static(match format {
            "wav" => "audio/wav",
            "pcm" => "audio/pcm",
            _ => "audio/mpeg",
        }),
    };
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(header::CONTENT_TYPE, content_type);
    response
}

/// `speechContentTypeNeedsDefault`.
fn content_type_needs_default(content_type: &str) -> bool {
    let media = content_type.split(';').next().unwrap_or_default();
    matches!(
        trim(media).go_lower().as_str(),
        "" | "application/json" | "application/octet-stream" | "text/plain"
    )
}

/// `speechRoutingModel`: the OpenAI TTS models and Grok TTS route to `grok-tts`; the
/// voice model routes to itself; anything else is refused.
fn routing_model(model: &str) -> Option<&'static str> {
    match model_base(model).as_str() {
        "" | "tts-1" | "tts-1-hd" | "gpt-4o-mini-tts" | DEFAULT_MODEL => Some(DEFAULT_MODEL),
        VOICE_MODEL => Some(VOICE_MODEL),
        _ => None,
    }
}

/// `speechModelBase`: no provider prefix, no `(...)` suffix, lower case.
fn model_base(model: &str) -> String {
    let mut model = trim(model);
    let lower = model.go_lower();
    if let Some(prefix) = ["xai/", "x-ai/", "grok/"].into_iter().find(|p| lower.starts_with(p)) {
        model = trim(model.get(prefix.len()..).unwrap_or(model));
    }
    if let Some(open) = model.rfind('(')
        && open > 0
        && model.ends_with(')')
    {
        model = trim(&model[..open]);
    }
    trim(model).go_lower()
}

/// `buildXAISpeechPayload`: the xAI `/tts` body (`json.Marshal` of a map, so keys sorted)
/// and the audio format the client asked for.
fn build_payload(raw: &[u8]) -> Result<(Vec<u8>, &'static str), String> {
    if !gj::valid(raw) {
        return Err("body must be valid JSON".into());
    }
    let mut input = trim(&text(raw, "input")).to_owned();
    if input.is_empty() {
        input = trim(&text(raw, "text")).to_owned();
    }
    if input.is_empty() {
        return Err("input is required".into());
    }
    if input.chars().count() > MAX_RUNES {
        return Err("input is longer than 60000 characters".into());
    }
    let mut voice = trim(&text(raw, "voice")).to_owned();
    if voice.is_empty() {
        voice = text(raw, "voice_id");
    }
    let mut language = trim(&text(raw, "language")).to_owned();
    if language.is_empty() {
        language = "auto".into();
    }
    let speed = gj::get(raw, "speed");
    let speed = if speed.kind == Kind::Number && speed.float() > 0.0 {
        let value = speed.float();
        // json.Marshal refuses an infinite float64 (a number too large to parse).
        Some(gj::json_float(value).ok_or_else(|| format!("json: unsupported value: {}", gj::fmt_float(value)))?)
    } else {
        None
    };
    let (format, output) = output_format(raw)?;
    let mut body = Obj::new().str("language", &language);
    if let Some((codec, rate)) = output {
        let output = Obj::new()
            .str("codec", codec)
            .raw("sample_rate", &rate.to_string())
            .finish();
        body = body.raw("output_format", &output);
    }
    if let Some(speed) = &speed {
        body = body.raw("speed", speed);
    }
    let body = body.str("text", &input).str("voice_id", &map_voice(&voice)).finish();
    Ok((body.into_bytes(), format))
}

/// `mapXAISpeechVoice`: lower case, OpenAI names mapped.
fn map_voice(voice: &str) -> String {
    let voice = trim(voice).go_lower();
    if voice.is_empty() {
        return DEFAULT_VOICE.into();
    }
    VOICES
        .iter()
        .find(|(openai, _)| *openai == voice)
        .map_or(voice.clone(), |(_, grok)| (*grok).to_owned())
}

/// `speechOutputFormat`: `response_format` (at 24 kHz), else a native `output_format`
/// object, else MP3. MP3 sends no `output_format`.
type Output = Option<(&'static str, i64)>;

fn output_format(raw: &[u8]) -> Result<(&'static str, Output), String> {
    let response_format = trim(&text(raw, "response_format")).go_lower();
    if !response_format.is_empty() {
        return codec_format(&response_format, DEFAULT_SAMPLE_RATE);
    }
    let native = gj::get(raw, "output_format");
    if !native.exists() {
        return Ok(("mp3", None));
    }
    if !native.is_object() {
        return Err("output_format must be an object".into());
    }
    let codec = trim(&native.get("codec").str()).go_lower();
    let mut rate = DEFAULT_SAMPLE_RATE;
    let given = native.get("sample_rate");
    if given.exists() && given.kind == Kind::Number && given.float() > 0.0 {
        // Go's `int(float64)`: truncation, and MinInt64 past the range on amd64.
        let value = given.float();
        rate = if value < i64::MAX as f64 {
            value as i64
        } else {
            i64::MIN
        };
    }
    codec_format(&codec, rate)
}

/// `speechCodecFormat`.
fn codec_format(codec: &str, rate: i64) -> Result<(&'static str, Output), String> {
    let rate = if rate <= 0 { DEFAULT_SAMPLE_RATE } else { rate };
    match codec {
        "" | "mp3" => Ok(("mp3", None)),
        "wav" => Ok(("wav", Some(("wav", rate)))),
        "pcm" => Ok(("pcm", Some(("pcm", rate)))),
        _ => Err("response_format must be mp3, wav, or pcm".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Under passthrough the upstream type wins unless it names no audio; the speech
    /// goldens run without passthrough, so only the format default reaches them.
    #[test]
    fn upstream_content_type_kept_only_when_it_names_audio() {
        let content_type = |format, upstream: Option<&'static str>| {
            let response = audio(format, upstream.map(HeaderValue::from_static), Bytes::new());
            response.headers()[header::CONTENT_TYPE].to_str().unwrap().to_owned()
        };
        assert_eq!(
            content_type("wav", Some("audio/ogg; codecs=opus")),
            "audio/ogg; codecs=opus"
        );
        assert_eq!(
            content_type("wav", Some(" Application/JSON ; charset=utf-8")),
            "audio/wav"
        );
        assert_eq!(content_type("pcm", Some("text/plain")), "audio/pcm");
        assert_eq!(content_type("mp3", Some("application/octet-stream")), "audio/mpeg");
        assert_eq!(content_type("pcm", None), "audio/pcm");
    }
}
