use crate::node::Node;
use bytes::Bytes;
use cpa_server::sap_hooks::{AttemptResult, Observation, SapCandidate, SapHooks, SapPick, SapRequest};
use sap_core::{
    Utc,
    health::{HealthReport, NodeState, UpstreamHealth},
    ids::Provider,
    routing::Outcome,
    sync::ChangeLog,
    thread::{RequestClass, ThreadUsage},
};
use std::sync::{Arc, Mutex};

struct RequestGuard {
    state: std::sync::Weak<Mutex<crate::node::State>>,
    key: sap_core::ids::ThreadKey,
}
impl Drop for RequestGuard {
    fn drop(&mut self) {
        if let Some(state) = self.state.upgrade() {
            state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .registry
                .stream_closed(&self.key);
        }
    }
}
pub(crate) struct RequestState {
    _guard: RequestGuard,
    pub(crate) websocket: bool,
    pub(crate) persistence_failed: bool,
    compacted: std::sync::atomic::AtomicBool,
    successes: Mutex<std::collections::BTreeSet<String>>,
    // ponytail: counters are per request/credential until the hook exposes a distinct attempt ID.
    usage: Mutex<std::collections::BTreeMap<String, ThreadUsage>>,
}
pub(crate) fn reply(provider: Provider, status: u16, message: &str, retry: Option<sap_core::DateTime<Utc>>) -> SapPick {
    let body = match provider {
        Provider::Claude => {
            serde_json::json!({"type":"error", "error":{"type":if status == 400 {"invalid_request_error"} else {"rate_limit_error"}, "message":message}, "retry_at":retry})
        }
        Provider::Codex => {
            serde_json::json!({"error":{"type":if status == 400 {"invalid_request_error"} else {"rate_limit_error"}, "code":if status == 400 {"previous_response_not_found"} else {"rate_limit_exceeded"}, "param":if status == 400 {Some("previous_response_id")} else {None}, "message":message}, "retry_at":retry})
        }
    };
    SapPick::Reply {
        status,
        content_type: "application/json",
        body: Bytes::from(body.to_string()),
    }
}
impl SapHooks for Node {
    fn identify(&self, path: &str, headers: &http::HeaderMap, body: &[u8]) -> Option<Arc<SapRequest>> {
        let compact = path.ends_with("/compact");
        let path = path.strip_suffix("/compact").unwrap_or(path);
        let mut identity = sap_threads::identify(path, headers, body)?;
        if compact {
            identity.class = RequestClass::Compaction;
        }
        let mut state = self.lock();
        let before = state.registry.get(&identity.key).cloned();
        let observed = state.registry.observe(&identity, Utc::now(), self.config.node.clone());
        if observed.record.is_none() && identity.class == RequestClass::Auxiliary {
            return None;
        }
        if before
            .as_ref()
            .is_some_and(|r| r.serving_node.as_ref().is_some_and(|n| n != &self.config.node))
        {
            state.resend.insert(identity.key.clone());
            state.registry.record_response(&identity.key, "");
        }
        let inferred_compaction = before
            .as_ref()
            .zip(observed.record.as_ref())
            .is_some_and(|(old, new)| new.compaction_generation > old.compaction_generation);
        if inferred_compaction {
            state.compacted.insert(identity.key.clone());
            self.event(
                sap_core::api::EventKind::Compaction,
                Some(identity.key.clone()),
                before
                    .as_ref()
                    .and_then(|r| r.assignment.as_ref())
                    .map(|a| a.account.clone()),
                "Compaction window advanced",
            );
        }
        let force = inferred_compaction
            || state.resend.contains(&identity.key)
            || observed.is_new
            || before.as_ref().is_some_and(|r| r.parent != observed.parent);
        let persistence_failed = self.persist(&mut state, &identity.key, force).is_err();
        state.registry.stream_opened(&identity.key);
        let guard = RequestGuard {
            state: Arc::downgrade(&self.state),
            key: identity.key.clone(),
        };
        Some(Arc::new(SapRequest {
            identity,
            data: Box::new(RequestState {
                _guard: guard,
                persistence_failed,
                websocket: headers
                    .get(http::header::UPGRADE)
                    .is_some_and(|h| h.as_bytes().eq_ignore_ascii_case(b"websocket")),
                usage: Mutex::default(),
                compacted: std::sync::atomic::AtomicBool::new(false),
                successes: Mutex::default(),
            }),
        }))
    }
    fn select(&self, request: &SapRequest, candidates: &[SapCandidate]) -> SapPick {
        self.choose(request, candidates)
            .unwrap_or_else(|_| reply(request.identity.provider(), 429, "Routing state unavailable", None))
    }
    fn on_outcome(&self, request: &SapRequest, credential: &str, result: &AttemptResult) {
        let Ok(mapping) = self.discover() else { return };
        let Some(account) = mapping.get(credential) else { return };
        let mut state = self.lock();
        let key = &request.identity.key;
        // Executor snapshots can be available before the sink's final quota callback.
        if let Some(rt) = self.runtime.upgrade() {
            let snapshot = match request.identity.provider() {
                Provider::Claude => rt.executors.claude.quota().snapshot(credential),
                Provider::Codex => rt.executors.codex.quota().snapshot(credential),
            };
            if let Some(snapshot) = snapshot
                && self
                    .quota(
                        &mut state,
                        request,
                        account,
                        snapshot.observed_at.into(),
                        snapshot.signals,
                    )
                    .is_err()
            {
                tracing::warn!("SAP quota persistence failed");
            }
        }
        let mut outcome = if result.error == Some(cpa_server::sap_hooks::AttemptError::ModelIneligible) {
            Outcome::ModelIneligible
        } else {
            match result.status {
                Some(401 | 403) => Outcome::Revoked,
                Some(429) => Outcome::RateLimited { retry_after: None },
                Some(503 | 529) => Outcome::Overloaded { retry_after: None },
                Some(200..=299) if result.error.is_none() => Outcome::Success,
                _ => return,
            }
        };
        if result.status == Some(429)
            && let Some(local) = state.accounts.get(account)
            && let Some(reset_at) = local.exhausted.filter(|reset| reset.is_none_or(|t| t > Utc::now()))
        {
            outcome = Outcome::Exhausted {
                window: local
                    .exhausted_window
                    .clone()
                    .unwrap_or(sap_core::quota::WindowKind::Other("unknown".into())),
                reset_at,
            };
        }
        if let Some(data) = request.data.downcast_ref::<RequestState>() {
            let mut usage = data.usage.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let attempt = usage.entry(credential.to_owned()).or_default();
            if attempt.requests == 0 {
                state.registry.record_usage(
                    key,
                    ThreadUsage {
                        requests: 1,
                        ..Default::default()
                    },
                );
                attempt.requests = 1;
            }
        }
        if outcome == Outcome::Success {
            if request.data.downcast_ref::<RequestState>().is_some_and(|r| {
                r.successes
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(credential.to_owned())
            }) && self.record_success(account).is_err()
            {
                tracing::warn!("SAP success count persistence failed");
            }
            state.failures.remove(key);
            state.upstream_at.insert(request.identity.provider(), Utc::now());
            if request.identity.class == RequestClass::Compaction
                && request
                    .data
                    .downcast_ref::<RequestState>()
                    .is_some_and(|r| !r.compacted.swap(true, std::sync::atomic::Ordering::Relaxed))
            {
                state.registry.compaction_completed(key);
                state.compacted.insert(key.clone());
                let _ = self.persist(&mut state, key, true);
                self.event(
                    sap_core::api::EventKind::Compaction,
                    Some(key.clone()),
                    Some(account.clone()),
                    "Compaction completed",
                );
            }
        } else {
            if self.record_failure(account).is_err() {
                tracing::warn!("SAP failure count persistence failed");
            }
            let local = state.accounts.entry(account.clone()).or_default();
            if outcome == Outcome::Revoked {
                local.revoked = true;
            }
            // Do not invent a provider cooldown when no retry time was observed.
            let now = Utc::now();
            state
                .failures
                .entry(key.clone())
                .and_modify(|f| f.0 = outcome.clone())
                .or_insert((outcome, now));
            self.event(
                sap_core::api::EventKind::AccountState,
                Some(key.clone()),
                Some(account.clone()),
                "Account attempt failed",
            );
        }
        if self.persist_account(&state, account).is_err() {
            tracing::warn!("SAP account state persistence failed");
        }
        self.changed();
    }
    fn on_observation(&self, request: &SapRequest, credential: &str, observation: Observation) {
        let Ok(mapping) = self.discover() else { return };
        let Some(account) = mapping.get(credential) else { return };
        let mut state = self.lock();
        let key = &request.identity.key;
        match observation {
            Observation::ResponseId(id) => {
                if state
                    .registry
                    .get(key)
                    .is_none_or(|r| r.last_response_id.as_ref() != Some(&id))
                {
                    state.registry.record_response(key, id);
                    let _ = self.persist(&mut state, key, true);
                }
            }
            Observation::Usage(usage) => {
                let Some(data) = request.data.downcast_ref::<RequestState>() else {
                    return;
                };
                let mut attempts = data.usage.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                let old = attempts.entry(credential.to_owned()).or_default();
                let delta = ThreadUsage {
                    requests: usage.requests.saturating_sub(old.requests),
                    input_tokens: usage.input_tokens.saturating_sub(old.input_tokens),
                    output_tokens: usage.output_tokens.saturating_sub(old.output_tokens),
                    cache_read_tokens: usage.cache_read_tokens.saturating_sub(old.cache_read_tokens),
                    cache_write_tokens: usage.cache_write_tokens.saturating_sub(old.cache_write_tokens),
                };
                old.requests = old.requests.max(usage.requests);
                old.input_tokens = old.input_tokens.max(usage.input_tokens);
                old.output_tokens = old.output_tokens.max(usage.output_tokens);
                old.cache_read_tokens = old.cache_read_tokens.max(usage.cache_read_tokens);
                old.cache_write_tokens = old.cache_write_tokens.max(usage.cache_write_tokens);
                // Streams repeat the cumulative usage on every event; an unchanged report
                // must not append (and fsync) another op-log row.
                if delta == ThreadUsage::default() {
                    return;
                }
                let local = state.accounts.entry(account.clone()).or_default();
                local.tokens = local
                    .tokens
                    .saturating_add(delta.input_tokens)
                    .saturating_add(delta.output_tokens)
                    .saturating_add(delta.cache_read_tokens)
                    .saturating_add(delta.cache_write_tokens);
                state.registry.record_usage(key, delta.clone());
                let now = Utc::now();
                let bucket =
                    sap_core::DateTime::from_timestamp(now.timestamp().div_euclid(900) * 900, 0).unwrap_or(now);
                let mut tokens = delta;
                tokens.requests = 0;
                if tokens != ThreadUsage::default() {
                    let _ = self.store.add_usage(account, bucket, tokens);
                }
            }
            Observation::Quota { observed_at, signals } => {
                if self
                    .quota(&mut state, request, account, observed_at.into(), signals)
                    .is_err()
                {
                    tracing::warn!("SAP quota persistence failed");
                }
            }
        }
        self.changed();
    }
    fn on_stream(&self, request: &SapRequest, open: bool) {
        let mut state = self.lock();
        let count = state.streams.entry(request.identity.key.clone()).or_default();
        if open {
            *count = count.saturating_add(1);
        } else {
            *count = count.saturating_sub(1);
        }
        if *count == 0 {
            state.streams.remove(&request.identity.key);
        }
        self.changed();
    }
    fn health(&self) -> HealthReport {
        let state = self.lock();
        HealthReport {
            node: self.config.node.clone(),
            role: self.config.role,
            state: NodeState::Serving,
            version: env!("CARGO_PKG_VERSION").into(),
            now: Utc::now(),
            started_at: self.started,
            upstream: UpstreamHealth {
                claude: state
                    .upstream_at
                    .get(&Provider::Claude)
                    .is_some_and(|t| *t > Utc::now() - chrono::Duration::minutes(5)),
                codex: state
                    .upstream_at
                    .get(&Provider::Codex)
                    .is_some_and(|t| *t > Utc::now() - chrono::Duration::minutes(5)),
            },
            open_streams: state.streams.values().copied().sum(),
            sync: self.store.cursor().unwrap_or_default(),
        }
    }
    fn routes(&self) -> Option<axum::Router> {
        let routes = self.sync_routes.clone();
        Some(match self.api_routes.get() {
            Some(api) => routes.merge(api.clone()),
            None => routes,
        })
    }
}
