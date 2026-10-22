use crate::{
    control::{BoxFuture, Control},
    node::Node,
};
use futures_util::{StreamExt, stream};
use sap_core::{Utc, api::AccountState, ids::Provider, reset_aware as ra};
use std::collections::BTreeMap;

#[derive(Default)]
pub(crate) struct Refresh {
    error: Option<&'static str>,
    running: Option<tokio::sync::watch::Receiver<bool>>,
    // ponytail: attempt metadata lasts for this process; persist it separately if
    // poll diagnostics must survive restart. Quota observations already persist.
    polls: BTreeMap<sap_core::ids::AccountId, (sap_core::DateTime<Utc>, Option<String>)>,
}

impl Node {
    pub(crate) fn visualizer_status(&self) -> ra::StatusResponse {
        let refresh = self.refresh.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        ra::StatusResponse {
            now: Utc::now(),
            polling: refresh
                .running
                .as_ref()
                .is_some_and(|r| r.has_changed().is_ok() && !*r.borrow()),
            settings: ra::RoutingSettings {
                strategy: "thread-affinity".into(),
                active: true,
                drain_window_seconds: 0.0,
                drain_floor_percent: 0.0,
                protect_after_seconds: 0.0,
                protect_used_percent: 0.0,
                five_hour_guard_percent: 0.0,
                reset_weight_exponent: 0.0,
            },
            accounts: self
                .accounts()
                .into_iter()
                .map(|a| {
                    let window = |kind| {
                        a.windows.iter().find(|w| w.kind == kind).map(|w| ra::WindowStatus {
                            used_percent: w.used_percent,
                            reset_at: w.reset_at,
                            source: Some(
                                match w.source {
                                    sap_core::quota::ObservationSource::Passive => "passive",
                                    sap_core::quota::ObservationSource::Probe => "probe",
                                }
                                .into(),
                            ),
                            observed_at: Some(w.observed_at),
                        })
                    };
                    ra::AccountStatus {
                        auth_index: a.id.0.clone(),
                        auth_id: a.id.0.clone(),
                        provider: a.provider.to_string(),
                        label: a.label.clone(),
                        email: a.email.clone(),
                        priority: a.priority,
                        disabled: a.state == AccountState::Disabled,
                        unavailable: !matches!(a.state, AccountState::Active | AccountState::Idle),
                        auth_kind: "oauth".into(),
                        state: if a.windows.is_empty() {
                            "unknown"
                        } else if matches!(a.state, AccountState::Cooling | AccountState::Exhausted) {
                            "guarded"
                        } else {
                            "normal"
                        }
                        .into(),
                        weight: a.weight_percent,
                        share_percent: a.weight_percent,
                        weekly: window(sap_core::quota::WindowKind::Weekly),
                        five_hour: window(sap_core::quota::WindowKind::FiveHour),
                        last_poll_at: refresh.polls.get(&a.id).map(|p| p.0),
                        last_poll_error: refresh
                            .error
                            .map(str::to_owned)
                            .or_else(|| refresh.polls.get(&a.id).and_then(|p| p.1.clone())),
                        reason: None,
                    }
                })
                .collect(),
        }
    }
    pub(crate) fn visualizer_history(&self, hours: u32) -> ra::HistoryResponse {
        ra::HistoryResponse {
            now: Utc::now(),
            hours,
            accounts: self
                .accounts()
                .into_iter()
                .map(|a| ra::HistoryAccount {
                    auth_index: a.id.0.clone(),
                    provider: a.provider.to_string(),
                    samples: self
                        .store
                        .quota_history(&a.id, Utc::now() - chrono::Duration::hours(hours.min(24 * 365) as i64))
                        .unwrap_or_default()
                        .into_iter()
                        .filter_map(|observation| {
                            let five = observation
                                .windows
                                .iter()
                                .find(|w| w.kind == sap_core::quota::WindowKind::FiveHour);
                            let weekly = observation
                                .windows
                                .iter()
                                .find(|w| w.kind == sap_core::quota::WindowKind::Weekly);
                            Some(ra::HistorySample {
                                t: observation.windows.first()?.observed_at,
                                weekly_used_percent: weekly.map(|w| w.used_percent),
                                five_hour_used_percent: five.map(|w| w.used_percent),
                                weekly_reset_at: weekly.and_then(|w| w.reset_at),
                                five_hour_reset_at: five.and_then(|w| w.reset_at),
                            })
                        })
                        .collect(),
                })
                .collect(),
        }
    }
    pub(crate) fn visualizer_requests(&self) -> ra::RequestsResponse {
        ra::RequestsResponse {
            now: Utc::now(),
            bucket_seconds: 900,
            groups: [Provider::Claude, Provider::Codex]
                .into_iter()
                .map(|provider| ra::RequestGroup {
                    provider: provider.to_string(),
                    accounts: self
                        .accounts()
                        .into_iter()
                        .filter(|a| a.provider == provider)
                        .map(|a| ra::RequestAccount {
                            auth_index: a.id.0.clone(),
                            buckets: {
                                let mut buckets: std::collections::BTreeMap<_, _> = self
                                    .store
                                    .usage_buckets(&a.id, Utc::now() - chrono::Duration::hours(3))
                                    .unwrap_or_default()
                                    .into_iter()
                                    .map(|(start, u)| {
                                        (
                                            start,
                                            ra::RequestBucket {
                                                start,
                                                success: u.requests,
                                                failed: 0,
                                            },
                                        )
                                    })
                                    .collect();
                                for (start, failed) in self.failure_buckets(&a.id) {
                                    buckets
                                        .entry(start)
                                        .or_insert(ra::RequestBucket {
                                            start,
                                            success: 0,
                                            failed: 0,
                                        })
                                        .failed = failed;
                                }
                                buckets.into_values().collect()
                            },
                        })
                        .collect(),
                })
                .collect(),
        }
    }
    pub(crate) fn visualizer_refresh(&self) -> BoxFuture<'_, ra::StatusResponse> {
        Box::pin(async move {
            let mut receiver = {
                let mut refresh = self.refresh.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(receiver) = refresh
                    .running
                    .as_ref()
                    .filter(|r| r.has_changed().is_ok() && !*r.borrow())
                {
                    receiver.clone()
                } else {
                    let (done, receiver) = tokio::sync::watch::channel(false);
                    refresh.running = Some(receiver.clone());
                    refresh.error = None;
                    self.changed();
                    let weak = self.self_weak.clone();
                    tokio::spawn(async move {
                        if let Some(node) = weak.upgrade() {
                            node.refresh_usage().await;
                            let _ = done.send(true);
                            node.changed();
                        }
                    });
                    receiver
                }
            };
            let _ = receiver.wait_for(|done| *done).await;
            self.reset_aware_status()
        })
    }

    async fn refresh_usage(&self) {
        let Some(rt) = self.runtime.upgrade() else {
            self.refresh
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .error = Some("Proxy runtime unavailable");
            return;
        };
        let Ok(mapping) = self.discover() else {
            self.refresh
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .error = Some("Credential discovery failed");
            return;
        };
        let enabled: std::collections::BTreeSet<_> = match self.store.accounts() {
            Ok(accounts) => accounts.into_iter().filter(|a| !a.disabled).map(|a| a.id).collect(),
            Err(_) => {
                self.refresh
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .error = Some("Quota state unavailable");
                return;
            }
        };
        // One local credential per enabled logical account; never probe peer-only accounts.
        let mut credentials = BTreeMap::new();
        for credential in rt.store().snapshot() {
            if let Some(account) = mapping.get(&credential.id).filter(|id| enabled.contains(*id)) {
                credentials.entry(account.clone()).or_insert(credential);
            }
        }
        let config = rt.config();
        let mut pending = stream::iter(credentials.into_iter().map(|(account, credential)| {
            let rt = &rt;
            let config = &config;
            async move {
                let result = match credential.provider.as_str() {
                    "claude" => rt.executors.claude.probe_usage(&credential, config).await,
                    "codex" => rt.executors.codex.probe_usage(&credential, config).await,
                    _ => return (account, Err("Usage probe is unsupported".to_owned())),
                };
                (account, result.map_err(|e| e.to_string()))
            }
        }))
        .buffer_unordered(4);
        while let Some((account, result)) = pending.next().await {
            let at = Utc::now();
            let error = match result {
                Ok(signals) => self
                    .quota_observation(
                        &mut self.lock(),
                        None,
                        &account,
                        crate::quota::Signals {
                            at,
                            values: signals,
                            source: sap_core::quota::ObservationSource::Probe,
                        },
                    )
                    .err()
                    .map(|_| "Quota persistence failed".to_owned()),
                Err(error) => Some(error),
            };
            self.refresh
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .polls
                .insert(account, (at, error));
            self.changed();
        }
    }
}
