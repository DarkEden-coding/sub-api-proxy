//! Codex Sign in with ChatGPT (SIWC): the authorization URL, the code exchange and the
//! bound credential file (Go internal/auth/codex/siwc_auth.go, openai.go
//! `CodexSIWCTokenData` and sdk/cliproxy/auth/codex_siwc_bind.go). SIWC is a second
//! token partition (`siwc`) of an existing Codex OAuth credential. Refresh and request
//! use (`use_siwc`) belong to the Codex executor.

use chrono::{Local, TimeDelta};
use cpa_core::config::Config;
use cpa_core::exec::{ExecError, FailureScope};
use cpa_core::gostr::GoStr;
use serde::Deserialize;
use serde_json::{Map, Value};
use url::Url;

use crate::codex_oauth::CodexOAuth;

pub const DYNAMIC_CLIENT_ID: &str = "dynamic_agent_client";
pub const DEFAULT_IDENTIFIER: &str = "CLIProxyAPI";
pub const AUTHORIZE_URL: &str = "https://auth.openai.com/api/accounts/authorize";
pub const TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
pub const RESOURCE: &str = "https://api.openai.com/v1";
/// The fixed loopback redirect. Nothing listens there for a management binding: the
/// browser's final URL is pasted into the v8 callback.
pub const REDIRECT_URI: &str = "http://127.0.0.1:1455/auth/callback";
pub const DIRECT_TOKEN_SCOPE: &str = "chatgpt.tokens.use.direct";
pub const SCOPE: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const EXPIRY_MARGIN: TimeDelta = TimeDelta::minutes(3);

/// `oauth.providers.codex.signin-with-chatgpt-identifier`, trimmed, else `CLIProxyAPI`
/// (Go `EffectiveSignInWithChatGPTIdentifier`).
pub fn identifier(cfg: &Config) -> String {
    ["oauth", "providers", "codex", "signin-with-chatgpt-identifier"]
        .iter()
        .try_fold(&cfg.document, |node, key| node.get(*key))
        // A Go string field: yaml.v3 takes a number or bool as its text.
        .map(cpa_core::config::go_string)
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_IDENTIFIER.to_owned())
}

/// Go `codexUseSIWC`: the `use_siwc` attribute (`strconv.ParseBool`), else the metadata
/// bool or string.
pub fn enabled(credential: &cpa_core::credential::Credential) -> bool {
    let attribute = credential
        .attributes
        .get("use_siwc")
        .and_then(|raw| parse_bool(raw.trim()))
        .unwrap_or(false);
    attribute
        || match credential.metadata.get("use_siwc") {
            Some(Value::Bool(b)) => *b,
            Some(Value::String(s)) => parse_bool(s.trim()).unwrap_or(false),
            _ => false,
        }
}

/// Go `codexSIWCAccessToken`: `siwc.access_token`, else `siwc.access`, trimmed.
pub fn access_token(credential: &cpa_core::credential::Credential) -> &str {
    let Some(Value::Object(siwc)) = credential.metadata.get("siwc") else {
        return "";
    };
    ["access_token", "access"]
        .iter()
        .filter_map(|k| siwc.get(*k).and_then(Value::as_str).map(str::trim))
        .find(|t| !t.is_empty())
        .unwrap_or_default()
}

/// Go `ensureCodexSIWCCredentials`: an SIWC credential without its token fails before
/// any request is built.
pub fn require_token(credential: &cpa_core::credential::Credential) -> Result<(), ExecError> {
    if enabled(credential) && access_token(credential).is_empty() {
        return Err(siwc_error(
            401,
            "codex executor: use_siwc is enabled but siwc access_token is missing",
        ));
    }
    Ok(())
}

/// Go `CodexSIWCRefreshLead`.
const REFRESH_LEAD: TimeDelta = TimeDelta::minutes(5);

fn partition(credential: &cpa_core::credential::Credential) -> Option<&Map<String, Value>> {
    match credential.metadata.get("siwc") {
        Some(Value::Object(siwc)) if !siwc.is_empty() => Some(siwc),
        _ => None,
    }
}

fn text<'a>(siwc: &'a Map<String, Value>, key: &str) -> &'a str {
    siwc.get(key).and_then(Value::as_str).map(str::trim).unwrap_or_default()
}

/// Go `SIWCExpirationTime`: the partition's expiry keys, else its access token's JWT `exp`.
// ponytail: Go also reads `expires_in` plus a timestamp and a nested `token` map; `ToMap`
// writes only `expired`, so those shapes come only from hand-edited files.
fn expiry(siwc: &Map<String, Value>) -> Option<chrono::DateTime<chrono::Utc>> {
    ["expired", "expire", "expires_at", "expiresAt", "expiry", "expires"]
        .iter()
        .find_map(|k| siwc.get(*k).and_then(crate::codex_oauth::parse_time))
        .or_else(|| crate::codex_oauth::jwt_exp(text(siwc, "access_token")))
}

/// Go `CodexSIWCNeedsRefresh`: `None` when the partition cannot refresh (no refresh
/// token or client ID), else whether it is due at `now`: no access token, five minutes
/// before its expiry, five minutes after its last refresh, or now when neither is known.
pub fn refresh_due(credential: &cpa_core::credential::Credential, now: chrono::DateTime<chrono::Utc>) -> Option<bool> {
    let siwc = partition(credential)?;
    if text(siwc, "refresh_token").is_empty() || text(siwc, "client_id").is_empty() {
        return None;
    }
    if text(siwc, "access_token").is_empty() {
        return Some(true);
    }
    if let Some(expiry) = expiry(siwc) {
        return Some(expiry - REFRESH_LEAD <= now);
    }
    let last = ["last_refresh", "lastRefresh"]
        .iter()
        .find_map(|k| siwc.get(*k).and_then(crate::codex_oauth::parse_time));
    Some(last.is_none_or(|last| last + REFRESH_LEAD <= now))
}

/// The partition's request token is present and not expired at `now`.
pub(crate) fn access_usable(credential: &cpa_core::credential::Credential, now: chrono::DateTime<chrono::Utc>) -> bool {
    !access_token(credential).is_empty() && partition(credential).and_then(expiry).is_none_or(|exp| exp > now)
}

/// The `siwc` patch of a refresh: the stored partition with the refreshed fields over it.
pub(crate) fn refresh_patch(
    credential: &cpa_core::credential::Credential,
    tokens: &Tokens,
) -> cpa_core::credential::MetadataPatch {
    let mut merged = partition(credential).cloned().unwrap_or_default();
    merged.extend(tokens.to_map());
    let mut patch = cpa_core::credential::MetadataPatch::default();
    patch.set.insert("siwc".into(), Value::Object(merged));
    patch
}

/// The partition's refresh grant inputs: refresh token, client ID and device ID.
pub(crate) fn refresh_inputs(credential: &cpa_core::credential::Credential) -> (&str, &str, &str) {
    partition(credential).map_or(("", "", ""), |siwc| {
        (
            text(siwc, "refresh_token"),
            text(siwc, "client_id"),
            text(siwc, "device_id"),
        )
    })
}

/// Go `codexSIWCUserAgent`: `<identifier> (<GOOS>; <GOARCH>)`.
pub fn user_agent(cfg: &Config) -> String {
    format!(
        "{} ({}; {})",
        identifier(cfg),
        crate::kimi_http::go_os(),
        crate::kimi_http::go_arch()
    )
}

/// Go `strconv.ParseBool`.
fn parse_bool(s: &str) -> Option<bool> {
    match s {
        "1" | "t" | "T" | "true" | "TRUE" | "True" => Some(true),
        "0" | "f" | "F" | "false" | "FALSE" | "False" => Some(false),
        _ => None,
    }
}

/// Go `IsValidUUID`: a canonical 36-character UUID, any case, surrounding space ignored.
pub fn is_valid_uuid(raw: &str) -> bool {
    let raw = raw.trim().as_bytes();
    raw.len() == 36
        && raw.iter().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => *b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// Go `GenerateRandomBase64URL`: 32 random bytes, unpadded base64url.
pub fn random_base64url() -> Result<String, ExecError> {
    use base64::Engine;
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| siwc_error(500, "failed to generate random bytes"))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

/// Go `GenerateSIWCAuthURL`; `url.Values.Encode` sorts the parameters.
pub fn authorize_url(
    state: &str,
    nonce: &str,
    device_id: &str,
    redirect: &str,
    challenge: &str,
    identifier: &str,
) -> Result<String, ExecError> {
    let device_id = device_id.trim();
    if !is_valid_uuid(device_id) {
        return Err(siwc_error(400, "Sign in with ChatGPT requires a valid device UUID"));
    }
    if state.trim().is_empty() {
        return Err(siwc_error(400, "state is required"));
    }
    if nonce.trim().is_empty() {
        return Err(siwc_error(400, "nonce is required"));
    }
    let redirect = match redirect.trim() {
        "" => REDIRECT_URI,
        r => r,
    };
    let host = format!("urn:uuid:{}", device_id.go_lower());
    let mut url = Url::parse(AUTHORIZE_URL).expect("constant URL");
    url.query_pairs_mut().extend_pairs([
        ("agent_name_hint", identifier),
        ("client_id", DYNAMIC_CLIENT_ID),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("ext_agent_host_id", &host),
        ("nonce", nonce.trim()),
        ("redirect_uri", redirect),
        ("resource", RESOURCE),
        ("response_type", "code"),
        ("scope", SCOPE),
        ("state", state.trim()),
    ]);
    Ok(url.into())
}

/// Go `CodexSIWCTokenData`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: String,
    pub client_id: String,
    pub scope: String,
    pub scopes: Vec<String>,
    pub expired: String,
    pub last_refresh: String,
    pub device_id: String,
}

impl Tokens {
    /// Go `ToMap`: the `siwc` partition as persisted; optional fields only when set.
    pub fn to_map(&self) -> Map<String, Value> {
        let mut out = Map::new();
        let mut set = |k: &str, v: &str| {
            out.insert(k.into(), Value::String(v.to_owned()));
        };
        set("access_token", &self.access_token);
        set("refresh_token", &self.refresh_token);
        set("client_id", &self.client_id);
        set("expired", &self.expired);
        set("last_refresh", &self.last_refresh);
        if !self.id_token.is_empty() {
            set("id_token", &self.id_token);
        }
        if !self.scope.is_empty() {
            set("scope", &self.scope);
        }
        if !self.device_id.is_empty() {
            set("device_id", &self.device_id);
        }
        if !self.scopes.is_empty() {
            out.insert("scopes".into(), self.scopes.clone().into());
        }
        out
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    #[serde(default)]
    id_token: String,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    expires_in: i64,
}

fn siwc_error(status: u16, message: &str) -> ExecError {
    ExecError::local(status, FailureScope::Credential, message)
}

impl CodexOAuth {
    /// Go `requestSIWCToken`. Upstream bodies are never echoed: the management session
    /// reports a fixed message.
    pub(crate) async fn siwc_token_request(
        &self,
        form: &[(&str, &str)],
        client_id: &str,
        device_id: &str,
        require_id_token: bool,
    ) -> Result<Tokens, ExecError> {
        // url.Values.Encode sorts keys; callers pass them sorted.
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(form)
            .finish();
        let (status, body) = self
            .post(
                self.siwc_token_url(),
                "application/x-www-form-urlencoded",
                body.into_bytes(),
            )
            .await?;
        if status != 200 && !require_id_token {
            // A refresh keeps the OAuth error code (Go echoes the body), so
            // `refresh_token_reused` stops the retries; bodies may echo request material.
            return Err(crate::codex_oauth::refresh_error(
                status,
                &format!(
                    "OpenAI OAuth token request failed ({status}): {}",
                    crate::codex_oauth::oauth_code(&body)
                ),
            ));
        }
        if status != 200 {
            return Err(siwc_error(
                status,
                &format!("OpenAI OAuth token request failed ({status})"),
            ));
        }
        let parsed: TokenResponse =
            serde_json::from_slice(&body).map_err(|_| siwc_error(502, "failed to parse SIWC token response"))?;
        let invalid = |field: &str| siwc_error(502, &format!("OpenAI OAuth token response has invalid {field}"));
        if parsed.access_token.trim().is_empty() {
            return Err(invalid("access_token"));
        }
        if parsed.refresh_token.trim().is_empty() {
            return Err(invalid("refresh_token"));
        }
        if require_id_token && parsed.id_token.trim().is_empty() {
            return Err(siwc_error(
                502,
                "OpenAI OAuth token response did not contain an ID token",
            ));
        }
        if parsed.scope.trim().is_empty() {
            return Err(invalid("scope"));
        }
        if parsed.expires_in <= 0 {
            return Err(invalid("expires_in"));
        }
        let scopes: Vec<String> = parsed.scope.split_whitespace().map(str::to_owned).collect();
        if !scopes.iter().any(|s| s == DIRECT_TOKEN_SCOPE) {
            return Err(siwc_error(
                502,
                &format!("OpenAI OAuth grant did not include {DIRECT_TOKEN_SCOPE}"),
            ));
        }
        let now = Local::now();
        let lifetime = TimeDelta::try_seconds(parsed.expires_in).unwrap_or(TimeDelta::MAX);
        let ttl = match lifetime - EXPIRY_MARGIN {
            ttl if ttl > TimeDelta::zero() => ttl,
            _ => lifetime,
        };
        let rfc3339 = |t: chrono::DateTime<Local>| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        Ok(Tokens {
            access_token: parsed.access_token,
            refresh_token: parsed.refresh_token,
            id_token: parsed.id_token,
            client_id: client_id.trim().to_owned(),
            scope: parsed.scope.trim().to_owned(),
            scopes,
            expired: rfc3339(now.checked_add_signed(ttl).unwrap_or(now)),
            last_refresh: rfc3339(now),
            device_id: device_id.trim().to_owned(),
        })
    }

    /// Go `ExchangeSIWCCodeForTokens`: the issued client ID from the callback, never the
    /// dynamic one in the authorization URL.
    pub async fn exchange_siwc(
        &self,
        code: &str,
        client_id: &str,
        redirect: &str,
        device_id: &str,
        verifier: &str,
    ) -> Result<Tokens, ExecError> {
        let client_id = client_id.trim();
        if client_id.is_empty() {
            return Err(siwc_error(
                400,
                "OpenAI OAuth registration callback did not contain an issued client ID",
            ));
        }
        let code = code.trim();
        if code.is_empty() {
            return Err(siwc_error(400, "authorization code is required"));
        }
        let redirect = match redirect.trim() {
            "" => REDIRECT_URI,
            r => r,
        };
        self.siwc_token_request(
            &[
                ("client_id", client_id),
                ("code", code),
                ("code_verifier", verifier),
                ("grant_type", "authorization_code"),
                ("redirect_uri", redirect),
                ("resource", RESOURCE),
            ],
            client_id,
            device_id,
            true,
        )
        .await
    }
}

/// Go `BindCodexSIWC`'s file: every top-level member of `disk` kept as written (large
/// numbers included), `siwc` replaced by `tokens` and `use_siwc` set, encoded like Go's
/// `json.Marshal` of the merged map (sorted keys, compact, HTML-escaped, no newline).
/// `None` when `disk` is not a JSON object.
pub fn bind_file(disk: &[u8], tokens: &Tokens) -> Option<Vec<u8>> {
    let mut members: std::collections::BTreeMap<String, Box<serde_json::value::RawValue>> =
        serde_json::from_slice(disk).ok()?;
    let siwc = crate::codex_oauth::encode_go_json(&Value::Object(tokens.to_map()));
    members.insert(
        "siwc".into(),
        serde_json::value::RawValue::from_string(siwc.trim_end().to_owned()).ok()?,
    );
    members.insert(
        "use_siwc".into(),
        serde_json::value::RawValue::from_string("true".into()).ok()?,
    );
    let mut out = String::with_capacity(disk.len() + 512);
    out.push('{');
    for (i, (key, value)) in members.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&crate::codex_json::go_quote(key, true));
        out.push(':');
        compact_html(value.get(), &mut out);
    }
    out.push('}');
    Some(out.into_bytes())
}

/// Go `compact(dst, src, escapeHTML=true)` over valid JSON: whitespace outside strings
/// dropped, `<`, `>`, `&`, U+2028 and U+2029 escaped, everything else verbatim.
fn compact_html(raw: &str, out: &mut String) {
    let (mut in_string, mut escaped) = (false, false);
    for c in raw.chars() {
        if in_string {
            match c {
                '<' => out.push_str("\\u003c"),
                '>' => out.push_str("\\u003e"),
                '&' => out.push_str("\\u0026"),
                '\u{2028}' => out.push_str("\\u2028"),
                '\u{2029}' => out.push_str("\\u2029"),
                _ => out.push(c),
            }
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_string = false,
                _ => {}
            }
        } else if !matches!(c, ' ' | '\t' | '\n' | '\r') {
            in_string = c == '"';
            out.push(c);
        }
    }
}

#[cfg(test)]
#[path = "codex_siwc_tests.rs"]
mod tests;
