use crate::node::{Node, State};
use cpa_server::sap_hooks::SapRequest;
use sap_core::{
    DateTime, Utc,
    account::{Capacity, CapacitySource},
    ids::AccountId,
    quota::{ObservationSource, QuotaObservation, QuotaWindow, WindowKind},
    routing::Outcome,
};
use std::collections::BTreeMap;
fn timestamp(s: &str) -> Option<DateTime<Utc>> {
    s.parse::<i64>()
        .ok()
        .and_then(|t| DateTime::from_timestamp(t, 0))
        .or_else(|| DateTime::parse_from_rfc3339(s).ok().map(|t| t.with_timezone(&Utc)))
}
impl Node {
    pub(crate) fn quota(
        &self,
        state: &mut State,
        request: &SapRequest,
        account: &AccountId,
        at: DateTime<Utc>,
        signals: BTreeMap<String, String>,
    ) -> anyhow::Result<()> {
        let local = state.accounts.entry(account.clone()).or_default();
        if local.quota_at.is_some_and(|previous| at < previous) {
            return Ok(());
        }
        local.quota_at = Some(at);
        // Signal names are HTTP header names and therefore ASCII.
        let s: BTreeMap<_, _> = signals.into_iter().map(|(k, v)| (k.to_ascii_lowercase(), v)).collect();
        let get = |key: &str| s.get(key).map(String::as_str);
        let mut windows = Vec::new();
        for (name, kind, minutes) in [("5h", WindowKind::FiveHour, 300), ("7d", WindowKind::Weekly, 10080)] {
            let prefix = format!("anthropic-ratelimit-unified-{name}");
            if let Some(used) = get(&format!("{prefix}-utilization"))
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
            {
                windows.push(QuotaWindow {
                    kind,
                    used_percent: used * 100.0,
                    reset_at: get(&format!("{prefix}-reset")).and_then(timestamp),
                    window_minutes: Some(minutes),
                    observed_at: at,
                    source: ObservationSource::Passive,
                });
            }
        }
        // ponytail: only primary/secondary Codex windows are mapped; add additional allowlisted families when their naming contract is shared.
        for name in ["primary", "secondary"] {
            let prefix = format!("x-codex-{name}");
            let minutes = get(&format!("{prefix}-window-minutes")).and_then(|s| s.parse::<u32>().ok());
            if let Some(used) = get(&format!("{prefix}-used-percent"))
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|v| v.is_finite() && (0.0..=100.0).contains(v))
            {
                let reset = get(&format!("{prefix}-reset-at")).and_then(timestamp).or_else(|| {
                    get(&format!("{prefix}-reset-after-seconds"))
                        .and_then(|s| s.parse::<i64>().ok())
                        .filter(|n| *n >= 0)
                        .and_then(|n| at.checked_add_signed(chrono::Duration::seconds(n)))
                });
                windows.push(QuotaWindow {
                    kind: match minutes {
                        Some(300) => WindowKind::FiveHour,
                        Some(10080) => WindowKind::Weekly,
                        _ => WindowKind::Other(name.into()),
                    },
                    used_percent: used,
                    reset_at: reset,
                    window_minutes: minutes,
                    observed_at: at,
                    source: ObservationSource::Passive,
                });
            }
        }
        let claim = get("anthropic-ratelimit-unified-representative-claim").unwrap_or_default();
        let rejected = get("anthropic-ratelimit-unified-status") == Some("rejected");
        let exhausted_kind = if get("anthropic-ratelimit-unified-5h-status") == Some("rejected")
            || rejected && claim == "five_hour"
        {
            Some(WindowKind::FiveHour)
        } else if get("anthropic-ratelimit-unified-7d-status") == Some("rejected") || rejected && claim == "seven_day" {
            Some(WindowKind::Weekly)
        } else if get("x-codex-limit-reached") == Some("true") {
            Some(
                windows
                    .iter()
                    .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent))
                    .map(|w| w.kind.clone())
                    .unwrap_or(WindowKind::Other("unknown".into())),
            )
        } else {
            None
        };
        if let Some(window) = exhausted_kind {
            let reset_at = windows.iter().find(|w| w.kind == window).and_then(|w| w.reset_at);
            let local = state.accounts.entry(account.clone()).or_default();
            let state_changed = local.exhausted != Some(reset_at) || local.exhausted_window.as_ref() != Some(&window);
            local.exhausted = Some(reset_at);
            local.exhausted_window = Some(window.clone());
            let outcome = Outcome::Exhausted { window, reset_at };
            state
                .failures
                .entry(request.identity.key.clone())
                .and_modify(|f| f.0 = outcome.clone())
                .or_insert((outcome, at));
            if state_changed {
                self.event(
                    sap_core::api::EventKind::AccountState,
                    Some(request.identity.key.clone()),
                    Some(account.clone()),
                    "Account quota exhausted",
                );
            }
        }
        if get("x-codex-allowed") == Some("true") && get("x-codex-limit-reached") != Some("true")
            || get("anthropic-ratelimit-unified-status")
                .is_some_and(|status| matches!(status, "allowed" | "allowed_warning"))
        {
            let local = state.accounts.entry(account.clone()).or_default();
            local.exhausted = None;
            local.exhausted_window = None;
        }
        if let Some(seconds) = get("retry-after")
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|v| *v >= 0)
        {
            state.accounts.entry(account.clone()).or_default().cooling =
                at.checked_add_signed(chrono::Duration::seconds(seconds));
        }
        let plan = get("x-codex-plan-type")
            .or_else(|| get("anthropic-ratelimit-unified-rate-limit-tier"))
            .map(str::to_owned);
        let tokens = state.accounts.get(account).map_or(0, |a| a.tokens);
        if let Some(mut a) = self.store.accounts()?.into_iter().find(|a| &a.id == account) {
            if plan.is_some() {
                a.plan = plan.clone();
            }
            if a.capacity.source != CapacitySource::Override {
                if matches!(a.capacity.source, CapacitySource::Default | CapacitySource::Plan)
                    && let Some(multiplier) = a
                        .plan
                        .as_deref()
                        .and_then(|p| sap_routing::plan_capacity(p, &self.config.routing))
                {
                    a.capacity = Capacity {
                        multiplier,
                        source: CapacitySource::Plan,
                    };
                }
                for w in &windows {
                    if let Some(capacity) = state.learner.observe(account, w, tokens) {
                        a.capacity = capacity;
                    }
                }
            }
            self.store.upsert_account(a)?;
        }
        let old = self.store.latest_windows(account)?;
        let changed = windows.iter().any(|w| {
            old.iter().find(|o| o.kind == w.kind).is_none_or(|o| {
                o.used_percent != w.used_percent
                    || o.reset_at != w.reset_at
                    || at.signed_duration_since(o.observed_at).num_seconds() >= 300
            })
        });
        if changed {
            self.store.append_quota(QuotaObservation {
                account: account.clone(),
                node: self.config.node.clone(),
                seq: 0,
                windows,
                plan,
                allowed: get("x-codex-allowed").and_then(|s| s.parse().ok()),
            })?;
        }
        self.persist_account(state, account)?;
        Ok(())
    }
}
