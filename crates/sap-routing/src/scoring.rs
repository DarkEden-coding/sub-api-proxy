use sap_core::account::AccountRuntime;
use sap_core::quota::WindowKind;
use sap_core::routing::{AccountScore, RoutingConfig, RoutingInput, ScoreDetail};

pub(crate) struct Scored {
    pub runtime_index: usize,
    pub public: AccountScore,
    pub urgency: f64,
    pub headroom: f64,
    pub load: f64,
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

pub(crate) fn score(
    input: &RoutingInput,
    cfg: &RoutingConfig,
    keep_cooling: Option<&sap_core::ids::AccountId>,
) -> Vec<Scored> {
    let mut scores: Vec<_> = input
        .accounts
        .iter()
        .enumerate()
        .map(|(index, runtime)| {
            let reason = ineligible(runtime, input, keep_cooling != Some(&runtime.account.id));
            let capacity = nonnegative(runtime.account.capacity.multiplier);
            let mut urgency = 0.0;
            let mut headroom = None::<f64>;
            // ponytail: sap-core has no required-window list. Expect the two
            // shared provider windows until the contract makes this configurable.
            let mut stale = ![WindowKind::FiveHour, WindowKind::Weekly]
                .iter()
                .all(|kind| runtime.windows.iter().any(|w| &w.kind == kind));
            for window in &runtime.windows {
                let valid = window.used_percent.is_finite() && (0.0..=100.0).contains(&window.used_percent);
                stale |= !valid
                    || input.now.signed_duration_since(window.observed_at).num_seconds()
                        > cfg.stale_after_secs.min(i64::MAX as u64) as i64;
                if !valid {
                    continue;
                }
                // A window whose reset has passed has fresh allowance, but the
                // observation no longer describes it, so it is also stale.
                let elapsed = window.reset_at.is_some_and(|reset| reset <= input.now);
                stale |= elapsed;
                let used = if elapsed { 0.0 } else { window.used_percent };
                let remaining = capacity * (1.0 - used / 100.0);
                headroom = Some(headroom.map_or(remaining, |h| h.min(remaining)));
                // Unknown or elapsed reset times contribute no urgency, rather than
                // fabricating a reset.
                if let Some(reset) = window.reset_at.filter(|_| !elapsed) {
                    let seconds = reset.signed_duration_since(input.now).num_milliseconds() as f64 / 1000.0;
                    urgency += remaining / seconds.max(cfg.reset_floor_secs.max(1) as f64);
                }
            }
            Scored {
                runtime_index: index,
                public: AccountScore {
                    account: runtime.account.id.clone(),
                    eligible: reason.is_none(),
                    ineligible_reason: reason.map(str::to_owned),
                    score: 0.0,
                    weight_percent: 0.0,
                    stale,
                },
                urgency,
                headroom: headroom.unwrap_or(0.0),
                load: f64::from(runtime.active_threads),
            }
        })
        .collect();
    let eligible = || scores.iter().filter(|s| s.public.eligible);
    // Normalize each quota term by its maximum. Load is its share of total active threads.
    let max_urgency = eligible().map(|s| s.urgency).fold(0.0, f64::max);
    let max_headroom = eligible().map(|s| s.headroom).fold(0.0, f64::max);
    let total_load: f64 = eligible().map(|s| s.load).sum();
    for s in scores.iter_mut().filter(|s| s.public.eligible) {
        s.urgency = if max_urgency > 0.0 {
            s.urgency / max_urgency
        } else {
            0.0
        };
        s.headroom = if max_headroom > 0.0 {
            s.headroom / max_headroom
        } else {
            0.0
        };
        s.load = if total_load > 0.0 { s.load / total_load } else { 0.0 };
        s.public.score = nonnegative(cfg.urgency_weight) * s.urgency + nonnegative(cfg.headroom_weight) * s.headroom
            - nonnegative(cfg.load_weight) * s.load;
        if s.public.stale {
            let penalty = if cfg.stale_penalty.is_finite() {
                cfg.stale_penalty.clamp(0.0, 1.0)
            } else {
                0.0
            };
            s.urgency *= penalty;
            s.headroom *= penalty;
            s.load *= penalty;
            s.public.score *= penalty;
        }
    }
    let positive_sum: f64 = scores.iter().map(|s| s.public.score.max(0.0)).sum();
    let count = scores.iter().filter(|s| s.public.eligible).count();
    for s in scores.iter_mut().filter(|s| s.public.eligible) {
        // ponytail: no positive scores have no proportional share. Display equal
        // weights until the contract can represent unknown weights explicitly.
        s.public.weight_percent = if positive_sum > 0.0 {
            100.0 * s.public.score.max(0.0) / positive_sum
        } else {
            100.0 / count as f64
        };
    }
    scores
}

pub(crate) fn best<'a>(
    scores: &'a [Scored],
    input: &RoutingInput,
    exclude: Option<&sap_core::ids::AccountId>,
) -> Option<&'a Scored> {
    scores
        .iter()
        .filter(|s| s.public.eligible && Some(&s.public.account) != exclude)
        .max_by(|a, b| {
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
        })
}

pub(crate) fn detail(winner: &Scored, scores: &[Scored], cfg: &RoutingConfig, input: &RoutingInput) -> ScoreDetail {
    let Some(runner_up) = best(scores, input, Some(&winner.public.account)) else {
        return ScoreDetail::OnlyChoice;
    };
    let urgency = nonnegative(cfg.urgency_weight) * (winner.urgency - runner_up.urgency);
    let headroom = nonnegative(cfg.headroom_weight) * (winner.headroom - runner_up.headroom);
    let load = nonnegative(cfg.load_weight) * (runner_up.load - winner.load);
    // ponytail: ScoreDetail has no tie/freshness variant. Exact contribution
    // ties use ResetSoon until the shared explanation contract is extended.
    if load > urgency && load > headroom {
        ScoreDetail::LeastLoaded
    } else if headroom > urgency {
        ScoreDetail::MostHeadroom
    } else {
        ScoreDetail::ResetSoon
    }
}
