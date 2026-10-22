use std::collections::HashMap;

use sap_core::account::{Capacity, CapacitySource};
use sap_core::ids::AccountId;
use sap_core::quota::{QuotaWindow, WindowKind};
use sap_core::routing::RoutingConfig;

/// First ASCII case-insensitive substring match in configuration table order.
pub fn plan_capacity(plan: &str, cfg: &RoutingConfig) -> Option<f64> {
    let plan = plan.to_ascii_lowercase();
    cfg.plan_capacity
        .iter()
        .find(|(name, _)| plan.contains(&name.to_ascii_lowercase()))
        .map(|(_, capacity)| *capacity)
}

#[derive(Debug)]
struct Estimate {
    previous: QuotaWindow,
    tokens: u64,
    ewma: Option<f64>,
    samples: usize,
}

/// Per-account, per-window EWMA of tokens consumed per percentage point.
///
/// Supply cumulative tokens from the same accounting scope on each observation.
/// Duplicate or older observations are ignored. Reset changes, falling usage and
/// falling token counters establish a new baseline, preserving the learned EWMA.
/// Call `forget_account` when an account is removed to bound retained state.
#[derive(Debug)]
pub struct CapacityLearner {
    alpha: f64,
    min_samples: usize,
    baseline_tokens_per_percent: f64,
    estimates: HashMap<(AccountId, WindowKind), Estimate>,
}

impl CapacityLearner {
    /// `alpha` must be in (0, 1], `min_samples` positive, and the baseline finite
    /// and positive. Samples are positive usage/token deltas, not observations.
    pub fn new(alpha: f64, min_samples: usize, baseline_tokens_per_percent: f64) -> Result<Self, &'static str> {
        if !alpha.is_finite() || !(0.0 < alpha && alpha <= 1.0) {
            return Err("alpha must be in (0, 1]");
        }
        if min_samples == 0 || !baseline_tokens_per_percent.is_finite() || baseline_tokens_per_percent <= 0.0 {
            return Err("minimum samples and baseline must be positive and finite");
        }
        Ok(Self {
            alpha,
            min_samples,
            baseline_tokens_per_percent,
            estimates: HashMap::new(),
        })
    }

    /// Observe one window and return its learned multiplier once ready. Unknown
    /// reset times cannot establish that two observations belong to one window.
    pub fn observe(&mut self, account: &AccountId, window: &QuotaWindow, cumulative_tokens: u64) -> Option<Capacity> {
        if !window.used_percent.is_finite() || !(0.0..=100.0).contains(&window.used_percent) {
            return None;
        }
        let estimate = self
            .estimates
            .entry((account.clone(), window.kind.clone()))
            .or_insert_with(|| Estimate {
                previous: window.clone(),
                tokens: cumulative_tokens,
                ewma: None,
                samples: 0,
            });
        if window.observed_at <= estimate.previous.observed_at {
            return learned(estimate, self.min_samples, self.baseline_tokens_per_percent);
        }
        let delta_percent = window.used_percent - estimate.previous.used_percent;
        let delta_tokens = cumulative_tokens.checked_sub(estimate.tokens);
        if window.reset_at.is_some()
            && window.reset_at == estimate.previous.reset_at
            && delta_percent >= 0.0
            && delta_tokens.is_some()
        {
            if delta_percent == 0.0 {
                // Retain token/percentage baselines while tracking the latest timestamp.
                estimate.previous.observed_at = window.observed_at;
                return learned(estimate, self.min_samples, self.baseline_tokens_per_percent);
            }
            let sample = delta_tokens.unwrap_or(0) as f64 / delta_percent;
            if sample.is_finite() && sample > 0.0 {
                let next = estimate
                    .ewma
                    .map_or(sample, |old| self.alpha * sample + (1.0 - self.alpha) * old);
                if (next / self.baseline_tokens_per_percent).is_finite() {
                    estimate.ewma = Some(next);
                    estimate.samples = estimate.samples.saturating_add(1);
                }
            }
        }
        estimate.previous = window.clone();
        estimate.tokens = cumulative_tokens;
        learned(estimate, self.min_samples, self.baseline_tokens_per_percent)
    }

    pub fn forget_account(&mut self, account: &AccountId) {
        self.estimates.retain(|(id, _), _| id != account);
    }
}

fn learned(estimate: &Estimate, min_samples: usize, baseline: f64) -> Option<Capacity> {
    if estimate.samples < min_samples {
        return None;
    }
    estimate.ewma.map(|value| Capacity {
        multiplier: value / baseline,
        source: CapacitySource::Learned,
    })
}
