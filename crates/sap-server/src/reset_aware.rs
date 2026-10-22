use crate::{
    control::{BoxFuture, Control},
    node::Node,
};
use sap_core::{Utc, api::AccountState, ids::Provider, reset_aware as ra};

impl Node {
    pub(crate) fn visualizer_status(&self) -> ra::StatusResponse {
        ra::StatusResponse {
            now: Utc::now(),
            polling: false,
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
                            source: Some("passive".into()),
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
                        last_poll_at: a.windows.iter().map(|w| w.observed_at).max(),
                        last_poll_error: None,
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
        // ponytail: refresh currently returns passive observations only. Add an executor usage-probe API before enabling active account polling.
        Box::pin(async { self.reset_aware_status() })
    }
}
