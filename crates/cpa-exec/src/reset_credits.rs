//! Manual usage resets: the resets a subscription account can spend, and spending one.
//!
//! Go's server has no such route. The upstream contract follows CLIProxyAPI's management
//! center (`src/services/api/claudeResetGrants.ts`, `src/utils/quota/resetCredits.ts`,
//! `src/features/quota/providers/codex/data.ts`):
//!
//! - Claude lists grants in the `cedar_ember` block of
//!   `GET /api/oauth/usage?cedar_ember=1&skip_spend=1` and spends one with
//!   `POST /api/organizations/{org}/reset_rate_limits`; the organization comes from
//!   `GET /api/oauth/profile`.
//! - Codex lists credits at `GET /backend-api/wham/rate-limit-reset-credits` and spends one
//!   with `POST /backend-api/wham/rate-limit-reset-credits/consume`.
//!
//! Both spend calls carry a caller-chosen request ID, so retrying an unanswered spend with
//! the same ID cannot spend twice. Provider strings never reach errors; grant labels are
//! stripped of control characters and bounded.

use std::time::Duration;

use chrono::{DateTime, Utc};
use cpa_core::credential::Credential;
use serde_json::Value;

use crate::usage_probe::{ProbeError, send, target};

const CLAUDE_PROGRAM: &str = "cedar_ember";
const CLAUDE_USER_AGENT: &str = crate::claude::profile::BASELINE.user_agent;
const READ_TIMEOUT: Duration = Duration::from_secs(12);
/// The bound Claude Code uses for the claim.
const SPEND_TIMEOUT: Duration = Duration::from_secs(25);

#[derive(Debug, Clone, PartialEq)]
pub struct ResetCredit {
    /// Provider display text; empty for Codex credits.
    pub label: String,
    pub left: u32,
    /// `None` for Codex, whose credits are one reset each.
    pub total: Option<u32>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// Why no reset can be spent right now.
#[derive(Debug, Clone, PartialEq)]
pub enum Blocker {
    /// The provider says the account cannot use resets.
    Ineligible,
    /// Every reset is spent or expired.
    NoneLeft,
    /// The resets left only apply once a usage limit is reached.
    NotLimited,
    /// A reset was spent recently; the next is allowed from this time.
    Cooldown(Option<DateTime<Utc>>),
    /// Resets are left but the provider marks none usable now (paused or not started).
    Unusable,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResetCredits {
    pub credits: Vec<ResetCredit>,
    /// Resets left across every credit or unexpired grant.
    pub available: u32,
    /// `None` when one can be spent now.
    pub blocker: Option<Blocker>,
}

/// A terminal answer to a spend request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Redeemed {
    Reset,
    /// The same request ID was already spent.
    AlreadyUsed,
    NotLimited,
    Cooldown,
    Ineligible,
    Unavailable,
    /// Refused with HTTP 429 before anything was spent.
    RateLimited,
    /// Refused with HTTP 401 or 403 before anything was spent.
    Unauthorized,
    /// Codex answered 2xx without confirming a reset.
    NotConfirmed,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RedeemError {
    /// A read before the spend failed; nothing was spent.
    Probe(ProbeError),
    /// Nothing was sent.
    Blocked(Blocker),
    /// The spend may have been sent and no terminal answer came back. Retry with the same
    /// request ID and, for Claude, this grant.
    Unknown { grant: Option<String> },
}

/// A new spend request ID.
pub fn new_request_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub(crate) async fn read(
    client: wreq::Client,
    base: &str,
    credential: &Credential,
    claude: bool,
) -> Result<ResetCredits, ProbeError> {
    let now = Utc::now();
    if claude {
        let status = claude_status(&client, base, credential).await?;
        Ok(status.summary(now))
    } else {
        let (base, token) = target(base, credential, false)?;
        let url = join(&base, "/backend-api/wham/rate-limit-reset-credits")?;
        let request = codex_headers(client.get(url.as_str()).bearer_auth(token), credential)
            .header("openai-beta", "codex-1")
            .header("originator", "Codex Desktop");
        match send(request, READ_TIMEOUT).await? {
            (200..300, raw) => codex_credits(&raw),
            (status, _) => Err(ProbeError::Status(status)),
        }
    }
}

/// Spends one reset. `grant` is the Claude grant of an earlier unanswered attempt with the
/// same `request_id`; it skips the eligibility read, as a retry must reach the same grant.
/// Returns the Claude grant spent (`None` for Codex) and the answer.
pub(crate) async fn redeem(
    client: wreq::Client,
    base: &str,
    credential: &Credential,
    claude: bool,
    request_id: &str,
    grant: Option<&str>,
) -> Result<(Option<String>, Redeemed), RedeemError> {
    let probe = RedeemError::Probe;
    if claude {
        if !valid_request_id(request_id) || grant.is_some_and(|g| !valid_grant_id(g)) {
            return Err(probe(ProbeError::Unsupported));
        }
        let organization = claude_organization(&client, base, credential).await.map_err(probe)?;
        let grant = match grant {
            Some(grant) => grant.to_owned(),
            None => claude_status(&client, base, credential)
                .await
                .map_err(probe)?
                .select(Utc::now())
                .map_err(RedeemError::Blocked)?,
        };
        let (base, token) = target(base, credential, true).map_err(probe)?;
        let url = join(&base, &format!("/api/organizations/{organization}/reset_rate_limits")).map_err(probe)?;
        let body = serde_json::json!({"program": CLAUDE_PROGRAM, "grant_id": grant, "request_id": request_id});
        let request = claude_headers(client.post(url.as_str()).bearer_auth(token)).body(body.to_string());
        let answer = match send(request, SPEND_TIMEOUT).await {
            Ok((429, _)) => Redeemed::RateLimited,
            Ok((401 | 403, _)) => Redeemed::Unauthorized,
            Ok((200..300, raw)) => match serde_json::from_slice::<Value>(&raw)
                .ok()
                .as_ref()
                .and_then(|v| v["result"].as_str())
            {
                Some("reset") => Redeemed::Reset,
                Some("already_used") => Redeemed::AlreadyUsed,
                Some("not_limited") => Redeemed::NotLimited,
                Some("cooldown") => Redeemed::Cooldown,
                Some("ineligible") => Redeemed::Ineligible,
                Some("unavailable") => Redeemed::Unavailable,
                _ => return Err(RedeemError::Unknown { grant: Some(grant) }),
            },
            _ => return Err(RedeemError::Unknown { grant: Some(grant) }),
        };
        Ok((Some(grant), answer))
    } else {
        let (base, token) = target(base, credential, false).map_err(probe)?;
        let url = join(&base, "/backend-api/wham/rate-limit-reset-credits/consume").map_err(probe)?;
        let body = serde_json::json!({"redeem_request_id": request_id});
        let request = codex_headers(client.post(url.as_str()).bearer_auth(token), credential).body(body.to_string());
        let answer = match send(request, SPEND_TIMEOUT).await {
            Ok((429, _)) => Redeemed::RateLimited,
            Ok((401 | 403, _)) => Redeemed::Unauthorized,
            Ok((200..300, raw)) => match serde_json::from_slice::<Value>(&raw)
                .ok()
                .as_ref()
                .and_then(|v| v["code"].as_str())
            {
                Some("reset") => Redeemed::Reset,
                Some("already_redeemed") => Redeemed::AlreadyUsed,
                _ => Redeemed::NotConfirmed,
            },
            _ => return Err(RedeemError::Unknown { grant: None }),
        };
        Ok((None, answer))
    }
}

fn join(base: &url::Url, path: &str) -> Result<url::Url, ProbeError> {
    base.join(path).map_err(|_| ProbeError::Unsupported)
}

fn claude_headers(request: wreq::RequestBuilder) -> wreq::RequestBuilder {
    request
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("user-agent", CLAUDE_USER_AGENT)
        .header("content-type", "application/json")
}

fn codex_headers(request: wreq::RequestBuilder, credential: &Credential) -> wreq::RequestBuilder {
    let request = request
        .header("user-agent", crate::codex_request::USER_AGENT)
        .header("accept", "application/json")
        .header("content-type", "application/json");
    match credential.str("account_id").filter(|s| !s.is_empty()) {
        Some(account) => request.header("chatgpt-account-id", account),
        None => request,
    }
}

async fn claude_json(
    client: &wreq::Client,
    base: &str,
    credential: &Credential,
    path: &str,
) -> Result<Value, ProbeError> {
    let (base, token) = target(base, credential, true)?;
    let url = join(&base, path)?;
    match send(
        claude_headers(client.get(url.as_str()).bearer_auth(token)),
        READ_TIMEOUT,
    )
    .await?
    {
        (200..300, raw) => serde_json::from_slice(&raw).map_err(|_| ProbeError::InvalidResponse),
        (status, _) => Err(ProbeError::Status(status)),
    }
}

async fn claude_status(client: &wreq::Client, base: &str, credential: &Credential) -> Result<ClaudeStatus, ProbeError> {
    let usage = claude_json(client, base, credential, "/api/oauth/usage?cedar_ember=1&skip_spend=1").await?;
    ClaudeStatus::parse(&usage[CLAUDE_PROGRAM]).ok_or(ProbeError::InvalidResponse)
}

async fn claude_organization(client: &wreq::Client, base: &str, credential: &Credential) -> Result<String, ProbeError> {
    let profile = claude_json(client, base, credential, "/api/oauth/profile").await?;
    profile["organization"]["uuid"]
        .as_str()
        .and_then(|s| uuid::Uuid::try_parse(s).ok().filter(|_| s.len() == 36))
        .map(|u| u.hyphenated().to_string())
        .ok_or(ProbeError::InvalidResponse)
}

fn valid_grant_id(id: &str) -> bool {
    (1..=40).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

fn valid_request_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Absent or null is `Some(None)`; anything else unparsable is `None`.
fn timestamp(value: &Value) -> Option<Option<DateTime<Utc>>> {
    match value {
        Value::Null => Some(None),
        Value::String(s) => DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|t| Some(t.with_timezone(&Utc))),
        _ => None,
    }
}

/// Absent or null is `default`; a non-boolean is `None`.
fn flag(value: &Value, default: bool) -> Option<bool> {
    match value {
        Value::Null => Some(default),
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

fn label(value: &Value) -> String {
    let text: String = value
        .as_str()
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(120)
        .collect()
}

#[derive(Debug)]
struct Grant {
    id: String,
    label: String,
    total: u32,
    left: u32,
    starts_at: Option<DateTime<Utc>>,
    ends_at: Option<DateTime<Utc>>,
    paused: bool,
    usable_now: bool,
    requires_limit: bool,
}

impl Grant {
    fn parse(value: &Value) -> Option<Self> {
        let id = value["id"].as_str().filter(|id| valid_grant_id(id))?.to_owned();
        let count = |v: &Value| v.as_u64().and_then(|n| u32::try_from(n).ok());
        let (total, left) = (count(&value["resets_total"])?, count(&value["resets_left"])?);
        if left > total {
            return None;
        }
        Some(Self {
            id,
            label: label(&value["label"]),
            total,
            left,
            starts_at: timestamp(&value["starts_at"])?,
            ends_at: timestamp(&value["ends_at"])?,
            paused: flag(&value["paused"], false)?,
            // A grant that does not say it is usable is not offered for spending.
            usable_now: flag(&value["usable_now"], false)?,
            requires_limit: flag(&value["use_requires_limit"], true)?,
        })
    }

    fn live(&self, now: DateTime<Utc>) -> bool {
        self.starts_at.is_none_or(|t| t <= now) && self.ends_at.is_none_or(|t| t > now)
    }
}

#[derive(Debug)]
struct ClaudeStatus {
    eligible: bool,
    at_limit: bool,
    grants: Vec<Grant>,
    next: Option<String>,
    cooldown_until: Option<DateTime<Utc>>,
}

impl ClaudeStatus {
    /// One malformed grant, a duplicate ID or a bad timestamp rejects the whole block.
    fn parse(block: &Value) -> Option<Self> {
        let eligible = block["eligible"].as_bool()?;
        let grants = match &block["grants"] {
            Value::Null => Vec::new(),
            Value::Array(items) => items.iter().map(Grant::parse).collect::<Option<Vec<_>>>()?,
            _ => return None,
        };
        let mut ids: Vec<_> = grants.iter().map(|g| g.id.as_str()).collect();
        ids.sort_unstable();
        if ids.windows(2).any(|w| w[0] == w[1]) {
            return None;
        }
        Some(Self {
            eligible,
            at_limit: flag(&block["at_limit"], false)?,
            next: block["next_grant_id"]
                .as_str()
                .filter(|id| grants.iter().any(|g| g.id == *id))
                .map(str::to_owned),
            grants,
            cooldown_until: timestamp(&block["cooldown_until"])?,
        })
    }

    /// The grant to spend now: the provider's recommendation, else the lowest ID.
    fn select(&self, now: DateTime<Utc>) -> Result<String, Blocker> {
        if !self.eligible {
            return Err(Blocker::Ineligible);
        }
        if self.cooldown_until.is_some_and(|t| t > now) {
            return Err(Blocker::Cooldown(self.cooldown_until));
        }
        let spendable = |g: &&Grant| !g.paused && g.usable_now && g.left > 0 && g.live(now);
        let mut usable: Vec<_> = self
            .grants
            .iter()
            .filter(spendable)
            .filter(|g| !g.requires_limit || self.at_limit)
            .collect();
        if usable.is_empty() {
            return Err(if self.grants.iter().filter(|g| g.live(now)).all(|g| g.left == 0) {
                Blocker::NoneLeft
            } else if self.grants.iter().filter(spendable).any(|g| g.requires_limit) {
                Blocker::NotLimited
            } else {
                Blocker::Unusable
            });
        }
        usable.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(usable
            .iter()
            .find(|g| Some(&g.id) == self.next.as_ref())
            .unwrap_or(&usable[0])
            .id
            .clone())
    }

    fn summary(&self, now: DateTime<Utc>) -> ResetCredits {
        let live: Vec<_> = self
            .grants
            .iter()
            .filter(|g| g.ends_at.is_none_or(|t| t > now))
            .collect();
        ResetCredits {
            credits: live
                .iter()
                .map(|g| ResetCredit {
                    label: g.label.clone(),
                    left: g.left,
                    total: Some(g.total),
                    expires_at: g.ends_at,
                })
                .collect(),
            available: live.iter().map(|g| g.left).sum(),
            blocker: self.select(now).err(),
        }
    }
}

fn codex_credits(raw: &[u8]) -> Result<ResetCredits, ProbeError> {
    let value: Value = serde_json::from_slice(raw).map_err(|_| ProbeError::InvalidResponse)?;
    let record = value.as_object().ok_or(ProbeError::InvalidResponse)?;
    if !["credits", "available_count"].iter().any(|k| record.contains_key(*k)) {
        return Err(ProbeError::InvalidResponse);
    }
    let expires = |v: &Value| match v {
        Value::String(s) => DateTime::parse_from_rfc3339(s.trim())
            .ok()
            .map(|t| t.with_timezone(&Utc)),
        Value::Number(n) => n.as_i64().and_then(|t| DateTime::from_timestamp(t, 0)),
        _ => None,
    };
    let credits: Vec<_> = record
        .get("credits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|c| c["reset_type"] == "codex_rate_limits" && c["status"] == "available")
        .filter_map(|c| {
            Some(ResetCredit {
                label: String::new(),
                left: 1,
                total: None,
                expires_at: Some(expires(&c["expires_at"])?),
            })
        })
        .collect();
    let available = record
        .get("available_count")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(credits.len() as u32);
    Ok(ResetCredits {
        credits,
        available,
        blocker: (available == 0).then_some(Blocker::NoneLeft),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        routing::{get, post},
    };
    use serde_json::json;
    use std::{
        future::IntoFuture,
        sync::{Arc, Mutex},
    };

    const ORG: &str = "0f6c1d2e-3a4b-4c5d-8e9f-a0b1c2d3e4f5";

    fn credential(provider: &str) -> Credential {
        let mut credential = Credential::relay_session("test");
        credential.provider = provider.into();
        credential
            .metadata
            .insert("access_token".into(), "private-test-token".into());
        credential
            .metadata
            .insert("account_id".into(), "private-test-account".into());
        credential
    }

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn status(block: Value) -> ClaudeStatus {
        ClaudeStatus::parse(&block).unwrap()
    }

    fn grant(id: &str, left: u32, extra: Value) -> Value {
        let mut g = json!({"id": id, "label": "Weekly\u{7}  reset", "resets_total": 2, "resets_left": left, "usable_now": true, "use_requires_limit": true});
        g.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        g
    }

    #[test]
    fn claude_selection_follows_the_management_center() {
        let now = at("2026-11-09T12:00:00Z");
        let limited = status(json!({"eligible": true, "at_limit": true, "next_grant_id": "b",
            "grants": [grant("a", 1, json!({})), grant("b", 2, json!({}))]}));
        assert_eq!(limited.select(now), Ok("b".into()));
        let summary = limited.summary(now);
        assert_eq!((summary.available, summary.blocker), (3, None));
        assert_eq!(summary.credits[0].label, "Weekly reset");

        let not_limited = status(json!({"eligible": true, "grants": [grant("a", 1, json!({}))]}));
        assert_eq!(not_limited.select(now), Err(Blocker::NotLimited));
        let anytime =
            status(json!({"eligible": true, "grants": [grant("a", 1, json!({"use_requires_limit": false}))]}));
        assert_eq!(anytime.select(now), Ok("a".into()));

        let spent = status(json!({"eligible": true, "at_limit": true,
            "grants": [grant("a", 0, json!({})), grant("old", 2, json!({"ends_at": "2026-11-01T00:00:00Z"}))]}));
        assert_eq!(spent.select(now), Err(Blocker::NoneLeft));
        assert_eq!(spent.summary(now).available, 0, "expired grants are not counted");

        let cooling = status(
            json!({"eligible": true, "at_limit": true, "cooldown_until": "2026-11-09T13:00:00Z",
            "grants": [grant("a", 1, json!({}))]}),
        );
        assert_eq!(
            cooling.select(now),
            Err(Blocker::Cooldown(Some(at("2026-11-09T13:00:00Z"))))
        );
        let paused =
            status(json!({"eligible": true, "at_limit": true, "grants": [grant("a", 1, json!({"paused": true}))]}));
        assert_eq!(paused.select(now), Err(Blocker::Unusable));
        let ineligible = status(json!({"eligible": false, "ineligible_reason": "tier"}));
        assert_eq!(ineligible.select(now), Err(Blocker::Ineligible));
    }

    #[test]
    fn claude_rejects_malformed_blocks_whole() {
        for block in [
            json!({"grants": []}),
            json!({"eligible": true, "grants": [grant("A", 1, json!({}))]}),
            json!({"eligible": true, "grants": [grant("a", 3, json!({}))]}),
            json!({"eligible": true, "grants": [grant("a", 1, json!({})), grant("a", 1, json!({}))]}),
            json!({"eligible": true, "grants": [grant("a", 1, json!({"ends_at": "soon"}))]}),
            json!({"eligible": true, "grants": [grant("a", 1, json!({"usable_now": "yes"}))]}),
        ] {
            assert!(ClaudeStatus::parse(&block).is_none(), "{block}");
        }
    }

    #[test]
    fn codex_counts_only_available_rate_limit_credits() {
        let credits = codex_credits(
            br#"{"available_count":2,"credits":[
                {"reset_type":"codex_rate_limits","status":"available","expires_at":"2026-11-20T00:00:00Z"},
                {"reset_type":"codex_rate_limits","status":"redeemed","expires_at":"2026-11-20T00:00:00Z"},
                {"reset_type":"other","status":"available","expires_at":"2026-11-20T00:00:00Z"},
                {"reset_type":"codex_rate_limits","status":"available","expires_at":1795000000}]}"#,
        )
        .unwrap();
        assert_eq!(
            (credits.available, credits.credits.len(), credits.blocker),
            (2, 2, None)
        );
        assert_eq!(
            codex_credits(br#"{"credits":[]}"#).unwrap().blocker,
            Some(Blocker::NoneLeft)
        );
        assert_eq!(codex_credits(br#"{"plan":"plus"}"#), Err(ProbeError::InvalidResponse));
    }

    async fn serve(app: Router) -> (String, tokio::task::JoinHandle<Result<(), std::io::Error>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        (base, tokio::spawn(axum::serve(listener, app).into_future()))
    }

    #[tokio::test]
    async fn claude_redeem_reads_profile_and_status_then_claims() {
        let claims = Arc::new(Mutex::new(Vec::<(http::HeaderMap, Value)>::new()));
        let app = Router::new()
            .route("/api/oauth/profile", get(|| async { Json(json!({"organization": {"uuid": ORG.to_ascii_uppercase()}})) }))
            .route("/api/oauth/usage", get(|q: axum::extract::RawQuery| async move {
                assert_eq!(q.0.as_deref(), Some("cedar_ember=1&skip_spend=1"));
                Json(json!({"cedar_ember": {"eligible": true, "at_limit": true, "grants": [grant("g1", 1, json!({}))]}}))
            }))
            .route(&format!("/api/organizations/{ORG}/reset_rate_limits"), post({
                let claims = claims.clone();
                move |headers: http::HeaderMap, Json(body): Json<Value>| async move {
                    claims.lock().unwrap().push((headers, body));
                    Json(json!({"result": "reset"}))
                }
            }));
        let (base, server) = serve(app).await;
        let client = crate::proxy::default_client();
        let credits = read(client.clone(), &base, &credential("claude"), true).await.unwrap();
        assert_eq!((credits.available, credits.blocker), (1, None));
        let result = redeem(client.clone(), &base, &credential("claude"), true, "req-1", None).await;
        assert_eq!(result, Ok((Some("g1".into()), Redeemed::Reset)));
        let (headers, body) = claims.lock().unwrap().pop().unwrap();
        assert_eq!(headers["authorization"], "Bearer private-test-token");
        assert_eq!(headers["anthropic-beta"], "oauth-2025-04-20");
        assert_eq!(headers["user-agent"], CLAUDE_USER_AGENT);
        assert_eq!(
            body,
            json!({"program": "cedar_ember", "grant_id": "g1", "request_id": "req-1"})
        );
        assert_eq!(
            redeem(
                client,
                "https://untrusted.invalid",
                &credential("claude"),
                true,
                "req-1",
                None
            )
            .await,
            Err(RedeemError::Probe(ProbeError::Unsupported))
        );
        server.abort();
    }

    #[tokio::test]
    async fn claude_redeem_blocked_sends_nothing_and_unanswered_is_unknown() {
        let claimed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let app = Router::new()
            .route("/api/oauth/profile", get(|| async { Json(json!({"organization": {"uuid": ORG}})) }))
            .route("/api/oauth/usage", get(|| async {
                Json(json!({"cedar_ember": {"eligible": true, "at_limit": false, "grants": [grant("g1", 1, json!({}))]}}))
            }))
            .route(&format!("/api/organizations/{ORG}/reset_rate_limits"), post({
                let claimed = claimed.clone();
                move || async move {
                    claimed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    (http::StatusCode::BAD_GATEWAY, "upstream detail")
                }
            }));
        let (base, server) = serve(app).await;
        let client = crate::proxy::default_client();
        assert_eq!(
            redeem(client.clone(), &base, &credential("claude"), true, "req-1", None).await,
            Err(RedeemError::Blocked(Blocker::NotLimited))
        );
        assert_eq!(claimed.load(std::sync::atomic::Ordering::SeqCst), 0);
        // A retry names its grant, skips the eligibility read and sends the claim.
        assert_eq!(
            redeem(client, &base, &credential("claude"), true, "req-1", Some("g1")).await,
            Err(RedeemError::Unknown {
                grant: Some("g1".into())
            })
        );
        assert_eq!(claimed.load(std::sync::atomic::Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn codex_read_and_consume_wire() {
        let consumed = Arc::new(Mutex::new(Vec::<Value>::new()));
        let app = Router::new()
            .route(
                "/backend-api/wham/rate-limit-reset-credits",
                get(|headers: http::HeaderMap| async move {
                    assert_eq!(headers["authorization"], "Bearer private-test-token");
                    assert_eq!(headers["chatgpt-account-id"], "private-test-account");
                    assert_eq!(headers["openai-beta"], "codex-1");
                    Json(json!({"available_count": 1, "credits": [
                    {"reset_type": "codex_rate_limits", "status": "available", "expires_at": "2026-11-20T00:00:00Z"}]}))
                }),
            )
            .route(
                "/backend-api/wham/rate-limit-reset-credits/consume",
                post({
                    let consumed = consumed.clone();
                    move |Json(body): Json<Value>| async move {
                        let mut seen = consumed.lock().unwrap();
                        seen.push(body);
                        Json(json!({"code": if seen.len() == 1 { "reset" } else { "already_redeemed" }}))
                    }
                }),
            );
        let (base, server) = serve(app).await;
        let client = crate::proxy::default_client();
        let credits = read(client.clone(), &base, &credential("codex"), false).await.unwrap();
        assert_eq!(credits.credits[0].expires_at, Some(at("2026-11-20T00:00:00Z")));
        for want in [Redeemed::Reset, Redeemed::AlreadyUsed] {
            assert_eq!(
                redeem(client.clone(), &base, &credential("codex"), false, "req-1", None).await,
                Ok((None, want))
            );
        }
        assert_eq!(consumed.lock().unwrap()[0], json!({"redeem_request_id": "req-1"}));
        server.abort();
    }
}
