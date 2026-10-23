use std::cmp::Ordering;

use sap_core::account::{AccountRuntime, CapacitySource};
use sap_core::ids::AccountId;
use sap_core::quota::{QuotaWindow, WindowKind};
use sap_core::routing::{AccountScore, RoutingConfig, RoutingInput, ScoreDetail};
use sap_core::{DateTime, Utc};

const WEEK_MINUTES: u32 = 7 * 24 * 60;

pub(crate) struct Scored {
    pub runtime_index: usize,
    /// `score` and `weight_percent` hold the account's share of new threads.
    pub public: AccountScore,
    pub urgency: f64,
    pub headroom: f64,
    pub load: f64,
    /// The weekly reset, when it falls inside the drain window with allowance left.
    pub drain_reset: Option<DateTime<Utc>>,
    /// Five-hour taper factor, 1 below the taper start and 0 at a full window.
    pub taper: f64,
}

pub(crate) fn ineligible(account: &AccountRuntime, input: &RoutingInput, cooling: bool) -> Option<&'static str> {
    if account.account.provider != input.provider {
        Some("provider mismatch")
    } else if account.account.disabled {
        Some("disabled")
    } else if account.revoked {
        Some("revoked")
    } else if !account.credential_usable {
        Some("credential unavailable")
    } else if input
        .model
        .as_ref()
        .is_some_and(|model| !account.account.serves_model(model))
    {
        Some("model unavailable")
    } else if account
        .exhausted_until
        .as_ref()
        .is_some_and(|until| until.is_none_or(|at| at > input.now))
    {
        Some("exhausted")
    } else if cooling && account.cooling_until.is_some_and(|at| at > input.now) {
        Some("cooling")
    } else {
        None
    }
}

fn nonnegative(value: f64) -> f64 {
    if value.is_finite() { value.max(0.0) } else { 0.0 }
}

fn valid(window: &QuotaWindow) -> bool {
    window.used_percent.is_finite() && (0.0..=100.0).contains(&window.used_percent)
}

/// A window whose reset has passed has fresh allowance, but its observation no
/// longer describes it.
fn elapsed(window: &QuotaWindow, now: DateTime<Utc>) -> bool {
    window.reset_at.is_some_and(|reset| reset <= now)
}

/// Percentages compare directly between accounts of one plan. A manual override
/// scales an account, otherwise its plan size does. A stored value from the retired
/// capacity learner is ignored.
fn capacity(runtime: &AccountRuntime, cfg: &RoutingConfig) -> f64 {
    let account = &runtime.account;
    if account.capacity.source == CapacitySource::Override {
        return nonnegative(account.capacity.multiplier);
    }
    account
        .plan
        .as_deref()
        .and_then(|plan| crate::plan_capacity(plan, cfg))
        .map_or(1.0, nonnegative)
}

/// Share multiplier from the five-hour window: new threads shift away as the window
/// fills past the taper start, without a hard cutoff before it is exhausted.
fn five_hour_taper(runtime: &AccountRuntime, now: DateTime<Utc>, cfg: &RoutingConfig) -> f64 {
    let start = if cfg.five_hour_taper_start_percent.is_finite() {
        cfg.five_hour_taper_start_percent.clamp(0.0, 100.0)
    } else {
        100.0
    };
    runtime
        .windows
        .iter()
        .filter(|w| w.kind == WindowKind::FiveHour && valid(w) && !elapsed(w, now))
        .map(|w| {
            if w.used_percent <= start || start >= 100.0 {
                1.0
            } else {
                ((100.0 - w.used_percent) / (100.0 - start)).clamp(0.0, 1.0)
            }
        })
        .fold(1.0, f64::min)
}

/// Score every account. `exclude` marks an account ineligible for an outcome-driven
/// move before the caller has updated its runtime state.
pub(crate) fn score(
    input: &RoutingInput,
    cfg: &RoutingConfig,
    keep_cooling: Option<&AccountId>,
    exclude: Option<&AccountId>,
) -> Vec<Scored> {
    let mut weekly_stale = Vec::with_capacity(input.accounts.len());
    let mut scores: Vec<_> = input
        .accounts
        .iter()
        .enumerate()
        .map(|(index, runtime)| {
            let id = &runtime.account.id;
            let reason = if exclude == Some(id) {
                Some("excluded")
            } else {
                ineligible(runtime, input, keep_cooling != Some(id))
            };
            let age_stale = |w: &QuotaWindow| {
                input.now.signed_duration_since(w.observed_at).num_seconds()
                    > cfg.stale_after_secs.min(i64::MAX as u64) as i64
            };
            // ponytail: sap-core has no required-window list. Expect the two
            // shared provider windows until the contract makes this configurable.
            let stale = ![WindowKind::FiveHour, WindowKind::Weekly]
                .iter()
                .all(|kind| runtime.windows.iter().any(|w| &w.kind == kind))
                || runtime
                    .windows
                    .iter()
                    .any(|w| !valid(w) || age_stale(w) || elapsed(w, input.now));
            let mut urgency = 0.0;
            let mut headroom = 0.0;
            let mut drain_reset = None;
            let weekly = runtime
                .windows
                .iter()
                .find(|w| w.kind == WindowKind::Weekly && valid(w));
            weekly_stale.push(weekly.is_none_or(|w| age_stale(w) || elapsed(w, input.now)));
            if let Some(window) = weekly {
                let elapsed = elapsed(window, input.now);
                let used = if elapsed { 0.0 } else { window.used_percent };
                let remaining = capacity(runtime, cfg) * (1.0 - used / 100.0);
                headroom = remaining;
                // A window past its reset starts over with a full window to go. An
                // unknown reset contributes no urgency rather than fabricating one.
                let seconds = if elapsed {
                    Some(window.window_minutes.unwrap_or(WEEK_MINUTES) as f64 * 60.0)
                } else {
                    window
                        .reset_at
                        .map(|reset| reset.signed_duration_since(input.now).num_milliseconds() as f64 / 1000.0)
                };
                if let Some(seconds) = seconds {
                    urgency = remaining / seconds.max(cfg.reset_floor_secs.max(1) as f64);
                    if !elapsed && remaining > 0.0 && seconds <= cfg.drain_window_secs as f64 {
                        drain_reset = window.reset_at;
                    }
                }
            }
            Scored {
                runtime_index: index,
                public: AccountScore {
                    account: id.clone(),
                    eligible: reason.is_none(),
                    ineligible_reason: reason.map(str::to_owned),
                    score: 0.0,
                    weight_percent: 0.0,
                    stale,
                },
                urgency,
                headroom,
                load: f64::from(runtime.active_threads),
                drain_reset,
                taper: five_hour_taper(runtime, input.now, cfg),
            }
        })
        .collect();
    // Normalize each quota term by its maximum. Load is its share of total active threads.
    let eligible =
        |scores: &[Scored]| -> Vec<usize> { (0..scores.len()).filter(|&i| scores[i].public.eligible).collect() };
    let candidates = eligible(&scores);
    let max_urgency = candidates.iter().map(|&i| scores[i].urgency).fold(0.0, f64::max);
    let max_headroom = candidates.iter().map(|&i| scores[i].headroom).fold(0.0, f64::max);
    let total_load: f64 = candidates.iter().map(|&i| scores[i].load).sum();
    let penalty = if cfg.stale_penalty.is_finite() {
        cfg.stale_penalty.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let mut shares = vec![0.0; scores.len()];
    for &i in &candidates {
        let s = &mut scores[i];
        let ratio = |value: f64, max: f64| if max > 0.0 { value / max } else { 0.0 };
        s.urgency = ratio(s.urgency, max_urgency);
        s.headroom = ratio(s.headroom, max_headroom);
        s.load = ratio(s.load, total_load);
        if weekly_stale[i] {
            s.urgency *= penalty;
            s.headroom *= penalty;
            s.load *= penalty;
        }
        let raw = nonnegative(cfg.urgency_weight) * s.urgency + nonnegative(cfg.headroom_weight) * s.headroom
            - nonnegative(cfg.load_weight) * s.load;
        shares[i] = raw.max(0.0) * s.taper;
    }
    let weights = weights(&scores, &candidates, &shares, input);
    for &i in &candidates {
        scores[i].public.score = weights[i];
        scores[i].public.weight_percent = 100.0 * weights[i];
    }
    scores
}

/// Split new threads, summing to 1 over eligible accounts. The draining account with
/// the earliest weekly reset takes everything its five-hour taper allows; the rest
/// is shared in proportion to the remaining accounts' scores.
fn weights(scores: &[Scored], candidates: &[usize], shares: &[f64], input: &RoutingInput) -> Vec<f64> {
    let drainer = candidates
        .iter()
        .copied()
        .filter(|&i| scores[i].drain_reset.is_some() && scores[i].taper > 0.0)
        .min_by(|&a, &b| {
            scores[a]
                .drain_reset
                .cmp(&scores[b].drain_reset)
                .then_with(|| rank(&scores[b], &scores[a], input))
        });
    let others: Vec<usize> = candidates.iter().copied().filter(|&i| Some(i) != drainer).collect();
    let kept = match drainer {
        Some(_) if others.is_empty() => 1.0,
        Some(d) => scores[d].taper,
        None => 0.0,
    };
    let mut weights = vec![0.0; scores.len()];
    if let Some(d) = drainer {
        weights[d] = kept;
    }
    let total: f64 = others.iter().map(|&i| shares[i]).sum();
    for &i in &others {
        // ponytail: with no positive shares there is no proportional split. Use
        // equal weights until the contract can represent unknown weights explicitly.
        let share = if total > 0.0 {
            shares[i] / total
        } else {
            1.0 / others.len() as f64
        };
        weights[i] = (1.0 - kept) * share;
    }
    weights
}

/// Higher score first, then higher priority, then the lexicographically smaller ID.
fn rank(a: &Scored, b: &Scored, input: &RoutingInput) -> Ordering {
    a.public
        .score
        .total_cmp(&b.public.score)
        .then_with(|| {
            input.accounts[a.runtime_index]
                .account
                .priority
                .cmp(&input.accounts[b.runtime_index].account.priority)
        })
        .then_with(|| b.public.account.cmp(&a.public.account))
}

pub(crate) fn best<'a>(scores: &'a [Scored], input: &RoutingInput, exclude: Option<&AccountId>) -> Option<&'a Scored> {
    scores
        .iter()
        .filter(|s| s.public.eligible && Some(&s.public.account) != exclude)
        .max_by(|a, b| rank(a, b, input))
}

/// Choose an eligible account in proportion to its weight using `input.pick`. The
/// heaviest account comes first, so a sample near zero takes the best account.
pub(crate) fn pick<'a>(scores: &'a [Scored], input: &RoutingInput) -> Option<&'a Scored> {
    let mut candidates: Vec<&Scored> = scores.iter().filter(|s| s.public.eligible).collect();
    candidates.sort_by(|a, b| rank(b, a, input));
    let sample = if input.pick.is_finite() {
        input.pick.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let mut cumulative = 0.0;
    for s in &candidates {
        cumulative += s.public.score;
        if sample < cumulative {
            return Some(s);
        }
    }
    // Rounding can leave the sum just below 1.
    candidates.into_iter().rfind(|s| s.public.score > 0.0)
}

pub(crate) fn detail(winner: &Scored, scores: &[Scored], cfg: &RoutingConfig, input: &RoutingInput) -> ScoreDetail {
    let Some(runner_up) = best(scores, input, Some(&winner.public.account)) else {
        return ScoreDetail::OnlyChoice;
    };
    if winner.drain_reset.is_some() {
        return ScoreDetail::ResetSoon;
    }
    let urgency = nonnegative(cfg.urgency_weight) * (winner.urgency - runner_up.urgency);
    let headroom = nonnegative(cfg.headroom_weight) * (winner.headroom - runner_up.headroom);
    let load = nonnegative(cfg.load_weight) * (runner_up.load - winner.load);
    // ponytail: ScoreDetail has no tie, freshness or five-hour variant. Those
    // choices use ResetSoon until the shared explanation contract is extended.
    if load > 0.0 && load > urgency && load > headroom {
        ScoreDetail::LeastLoaded
    } else if headroom > 0.0 && headroom > urgency {
        ScoreDetail::MostHeadroom
    } else {
        ScoreDetail::ResetSoon
    }
}
