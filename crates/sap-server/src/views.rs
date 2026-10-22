use crate::{control::*, node::Node};
use sap_core::{
    Utc,
    account::{Capacity, CapacitySource},
    api::*,
    ids::*,
    reset_aware as ra,
    routing::{RoutingEvent, RoutingPolicy},
    thread::ThreadRecord,
};
use std::{collections::BTreeSet, time::Duration};
fn thread(r: ThreadRecord) -> ThreadView {
    ThreadView {
        key: r.key,
        client: r.client,
        machine: r.client_host,
        title: r.title,
        lineage: r.lineage,
        parent: r.parent,
        agent_label: r.agent_label,
        account: r.assignment.as_ref().map(|a| a.account.clone()),
        reason: r.assignment.as_ref().map(|a| a.reason.label()),
        serving_node: r.serving_node,
        streaming: r.open_streams > 0,
        cache_hit_percent: r.usage.cache_hit_percent(),
        compactions: r.compaction_generation,
        requests: r.usage.requests,
        first_seen: r.first_seen,
        last_seen: r.last_seen,
    }
}
pub(crate) fn internal(_: impl std::fmt::Display) -> ControlError {
    ControlError::internal("State operation failed")
}
impl Control for Node {
    fn flow(&self) -> FlowSnapshot {
        FlowSnapshot {
            now: Utc::now(),
            node: self.config.node.clone(),
            nodes: self.nodes(),
            accounts: self.accounts(),
            threads: self.threads(true),
            events: self.events(100),
            stats: self.stats(),
            open_conflicts: self.conflicts().iter().filter(|c| !c.resolved).count() as u32,
        }
    }
    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<ControlEvent> {
        self.changes.subscribe()
    }
    fn accounts(&self) -> Vec<AccountView> {
        let mapping = self.discover().unwrap_or_default();
        let available: BTreeSet<_> = mapping.into_values().collect();
        let state = self.lock();
        let mut views = Vec::new();
        for provider in [Provider::Claude, Provider::Codex] {
            let Ok(input) = self.input(&state, provider, None, RoutingEvent::NewThread, &available) else {
                continue;
            };
            let scores = sap_routing::ThreadAffinityPolicy::new(self.config.routing.clone()).scores(&input);
            for runtime in input.accounts {
                let score = scores.iter().find(|s| s.account == runtime.account.id);
                let status = if runtime.account.disabled {
                    AccountState::Disabled
                } else if !runtime.credential_usable || runtime.revoked {
                    AccountState::LoginNeeded
                } else if runtime.exhausted_until.is_some() {
                    AccountState::Exhausted
                } else if runtime.cooling_until.is_some() {
                    AccountState::Cooling
                } else if runtime.active_threads > 0 {
                    AccountState::Active
                } else {
                    AccountState::Idle
                };
                views.push(AccountView {
                    id: runtime.account.id,
                    provider,
                    label: runtime.account.label,
                    email: runtime.account.email,
                    plan: runtime.account.plan,
                    capacity_multiplier: runtime.account.capacity.multiplier,
                    capacity_source: runtime.account.capacity.source,
                    priority: runtime.account.priority,
                    state: status,
                    weight_percent: score.map_or(0.0, |s| s.weight_percent),
                    windows: runtime
                        .windows
                        .into_iter()
                        .map(|w| WindowView {
                            kind: w.kind,
                            used_percent: w.used_percent,
                            reset_at: w.reset_at,
                            observed_at: w.observed_at,
                            source: w.source,
                        })
                        .collect(),
                    active_threads: runtime.active_threads,
                    available_at: runtime.exhausted_until.flatten().or(runtime.cooling_until),
                    login_needed_on: if runtime.credential_usable && !runtime.revoked {
                        vec![]
                    } else {
                        vec![self.config.node.clone()]
                    },
                    stale: score.is_none_or(|s| s.stale),
                });
            }
        }
        views
    }
    fn patch_account(&self, id: &AccountId, patch: AccountPatch) -> ControlResult<AccountView> {
        let mut a = self
            .store
            .accounts()
            .map_err(internal)?
            .into_iter()
            .find(|a| &a.id == id)
            .ok_or_else(|| ControlError::not_found("Account not found"))?;
        if let Some(label) = patch.label {
            a.label = label;
        }
        if let Some(disabled) = patch.disabled {
            a.disabled = disabled;
        }
        if let Some(priority) = patch.priority {
            a.priority = priority;
        }
        if let Some(capacity) = patch.capacity_override {
            a.capacity = match capacity {
                Some(multiplier) if multiplier.is_finite() && multiplier > 0.0 => Capacity {
                    multiplier,
                    source: CapacitySource::Override,
                },
                Some(_) => return Err(ControlError::invalid("Capacity must be positive and finite")),
                None => a
                    .plan
                    .as_deref()
                    .and_then(|p| sap_routing::plan_capacity(p, &self.config.routing))
                    .map(|multiplier| Capacity {
                        multiplier,
                        source: CapacitySource::Plan,
                    })
                    .unwrap_or_default(),
            };
        }
        self.store.upsert_account(a).map_err(internal)?;
        self.changed();
        self.accounts()
            .into_iter()
            .find(|a| &a.id == id)
            .ok_or_else(|| ControlError::not_found("Account not found"))
    }
    fn start_login(&self, request: LoginRequest) -> BoxFuture<'_, ControlResult<LoginStart>> {
        Box::pin(self.login(request))
    }

    fn account_history(&self, id: &AccountId, hours: u32) -> ControlResult<AccountHistory> {
        if !self.store.accounts().map_err(internal)?.iter().any(|a| &a.id == id) {
            return Err(ControlError::not_found("Account not found"));
        }
        let observations = self
            .store
            .quota_history(id, Utc::now() - chrono::Duration::hours(hours.min(24 * 365) as i64))
            .map_err(internal)?;
        Ok(AccountHistory {
            account: id.clone(),
            samples: observations
                .into_iter()
                .filter_map(|o| {
                    Some(HistoryPoint {
                        t: o.windows.first()?.observed_at,
                        five_hour_used_percent: o
                            .windows
                            .iter()
                            .find(|w| w.kind == sap_core::quota::WindowKind::FiveHour)
                            .map(|w| w.used_percent),
                        weekly_used_percent: o
                            .windows
                            .iter()
                            .find(|w| w.kind == sap_core::quota::WindowKind::Weekly)
                            .map(|w| w.used_percent),
                    })
                })
                .collect(),
        })
    }
    fn threads(&self, active_only: bool) -> Vec<ThreadView> {
        let state = self.lock();
        let records = if active_only {
            state
                .registry
                .active(Utc::now(), Duration::from_secs(self.config.idle_window_secs))
        } else {
            state.registry.export()
        };
        records
            .into_iter()
            .map(|record| {
                let streaming = state.streams.get(&record.key).is_some_and(|n| *n > 0);
                let mut view = thread(record);
                view.streaming = streaming;
                view
            })
            .collect()
    }
    fn assign_thread(&self, key: &ThreadKey, account: &AccountId) -> ControlResult<ThreadView> {
        if !self
            .store
            .accounts()
            .map_err(internal)?
            .iter()
            .any(|a| &a.id == account && a.provider == key.provider && !a.disabled)
        {
            return Err(ControlError::invalid(
                "Account is unavailable or has the wrong provider",
            ));
        }
        let mut state = self.lock();
        let old = state
            .registry
            .get(key)
            .ok_or_else(|| ControlError::not_found("Thread not found"))?
            .assignment
            .clone();
        if old.as_ref().is_none_or(|a| &a.account != account) {
            state.resend.insert(key.clone());
            state.registry.record_response(key, "");
        }
        state.registry.assign(
            key,
            sap_core::thread::Assignment {
                account: account.clone(),
                reason: sap_core::routing::Reason::Manual,
                at: Utc::now(),
            },
        );
        state.failures.remove(key);
        self.persist(&mut state, key, true).map_err(internal)?;
        self.event(
            EventKind::ThreadMoved,
            Some(key.clone()),
            Some(account.clone()),
            "Thread reassigned manually",
        );
        Ok(thread(state.registry.get(key).unwrap().clone()))
    }
    fn nodes(&self) -> Vec<NodeView> {
        let mut nodes = vec![NodeView {
            id: self.config.node.clone(),
            role: self.config.role,
            state: sap_core::health::NodeState::Serving,
            reachable: true,
            version: Some(env!("CARGO_PKG_VERSION").into()),
            requests_per_minute: 0.0,
            open_streams: self.lock().streams.values().copied().sum(),
            added_latency_p50_ms: None,
            rss_bytes: None,
            sync_lag_secs: Some(0.0),
            last_failover_at: None,
            last_seen: Some(Utc::now()),
        }];
        // ponytail: sync supplies cursors only; extend peer health for real roles, stream counts and rates.
        for (id, status) in self.agent.statuses() {
            nodes.push(NodeView {
                id,
                role: sap_core::health::NodeRole::Fallback,
                state: sap_core::health::NodeState::Starting,
                reachable: status.last_error.is_none() && status.last_success.is_some(),
                version: None,
                requests_per_minute: 0.0,
                open_streams: 0,
                added_latency_p50_ms: None,
                rss_bytes: None,
                sync_lag_secs: status.lag_secs.map(|s| s as f64),
                last_failover_at: None,
                last_seen: status
                    .last_success
                    .and_then(|t| sap_core::DateTime::from_timestamp(t as i64, 0)),
            });
        }
        nodes
    }
    fn conflicts(&self) -> Vec<sap_core::sync::Conflict> {
        self.store.conflicts().unwrap_or_default()
    }
    fn resolve_conflict(&self, id: &str, keep: &NodeId) -> ControlResult<sap_core::sync::Conflict> {
        let c = self.store.resolve_conflict(id, keep.clone()).map_err(internal)?;
        self.changed();
        Ok(c)
    }
    fn events(&self, limit: usize) -> Vec<EventView> {
        self.store.events(limit.min(1000)).unwrap_or_default()
    }
    fn reset_aware_status(&self) -> ra::StatusResponse {
        self.visualizer_status()
    }
    fn reset_aware_history(&self, hours: u32) -> ra::HistoryResponse {
        self.visualizer_history(hours)
    }
    fn reset_aware_requests(&self) -> ra::RequestsResponse {
        self.visualizer_requests()
    }
    fn reset_aware_refresh(&self) -> BoxFuture<'_, ra::StatusResponse> {
        self.visualizer_refresh()
    }
}
impl Node {
    fn stats(&self) -> StatsView {
        let mut series = std::collections::BTreeMap::<_, SeriesPoint>::new();
        for account in self.accounts() {
            for (start, failed) in self.failure_buckets(&account.id) {
                let point = series.entry(start).or_insert(SeriesPoint {
                    start,
                    requests: 0,
                    cache_read_tokens: 0,
                    input_tokens: 0,
                });
                point.requests = point.requests.saturating_add(failed);
            }
            for (start, u) in self
                .store
                .usage_buckets(&account.id, Utc::now() - chrono::Duration::hours(3))
                .unwrap_or_default()
            {
                let s = series.entry(start).or_insert(SeriesPoint {
                    start,
                    requests: 0,
                    cache_read_tokens: 0,
                    input_tokens: 0,
                });
                s.requests += u.requests;
                s.cache_read_tokens += u.cache_read_tokens;
                s.input_tokens += u.input_tokens + u.cache_write_tokens;
            }
        }
        let series: Vec<_> = series.into_values().collect();
        let reads: u64 = series.iter().map(|s| s.cache_read_tokens).sum();
        let total = reads + series.iter().map(|s| s.input_tokens).sum::<u64>();
        StatsView {
            requests_3h: series.iter().map(|s| s.requests).sum(),
            cache_hit_percent_3h: (total > 0).then(|| reads as f64 * 100.0 / total as f64),
            added_latency_p50_ms: None,
            rss_bytes: None,
            series,
        }
    }
}
