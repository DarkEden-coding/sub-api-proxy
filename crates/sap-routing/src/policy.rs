use sap_core::account::AccountRuntime;
use sap_core::ids::AccountId;
use sap_core::quota::WindowKind;
use sap_core::routing::{
    AccountScore, Decision, Outcome, Reason, RoutingConfig, RoutingEvent, RoutingInput, RoutingPolicy,
};
use sap_core::{DateTime, Utc};

use crate::scoring::{self, Scored};

/// Stateless policy. All live state and time are supplied by the caller.
#[derive(Debug, Clone)]
pub struct ThreadAffinityPolicy {
    config: RoutingConfig,
}

impl ThreadAffinityPolicy {
    pub fn new(config: RoutingConfig) -> Self {
        Self { config }
    }

    /// Pick an account by weight. An excluded account is rescored as ineligible, so
    /// its share is redistributed instead of leaving the others at zero.
    fn select(
        &self,
        input: &RoutingInput,
        scores: &[Scored],
        exclude: Option<&AccountId>,
        reason: impl FnOnce(&Scored, &[Scored]) -> Reason,
    ) -> Decision {
        let rescored;
        let scores = match exclude {
            Some(id) => {
                rescored = scoring::score(input, &self.config, None, Some(id));
                &rescored[..]
            }
            None => scores,
        };
        match scoring::pick(scores, input) {
            Some(chosen) => Decision::Use {
                account: chosen.public.account.clone(),
                reason: reason(chosen, scores),
            },
            None => reject(input, None),
        }
    }

    fn continue_on(&self, input: &RoutingInput, current: &AccountId, scores: &[Scored], compacted: bool) -> Decision {
        let runtime = input.accounts.iter().find(|a| &a.account.id == current);
        if let Some(account) = runtime.filter(|a| scoring::ineligible(a, input, false).is_none()) {
            if compacted {
                // Include the current account even when cooling, so it has a comparable score.
                let comparison = scoring::score(input, &self.config, Some(current), None);
                let current_score = comparison
                    .iter()
                    .find(|s| &s.public.account == current)
                    .expect("current account is present")
                    .public
                    .score;
                if let Some(other) = scoring::best(&comparison, input, Some(current)) {
                    let margin = if self.config.compaction_move_margin.is_finite() {
                        self.config.compaction_move_margin.max(0.0)
                    } else {
                        f64::INFINITY
                    };
                    if other.public.score > current_score + current_score.abs() * margin {
                        return Decision::Use {
                            account: other.public.account.clone(),
                            reason: Reason::MovedAfterCompaction { from: current.clone() },
                        };
                    }
                }
            }
            return Decision::Use {
                account: account.account.id.clone(),
                reason: if compacted {
                    Reason::KeptAfterCompaction
                } else {
                    Reason::Kept
                },
            };
        }
        let exhausted = runtime.is_some_and(|a| {
            a.exhausted_until
                .as_ref()
                .is_some_and(|until| until.is_none_or(|at| at > input.now))
        });
        self.select(input, scores, Some(current), |_, _| {
            if exhausted {
                Reason::MovedExhausted {
                    from: current.clone(),
                    window: exhaustion_window(runtime.expect("exhausted runtime exists")),
                }
            } else {
                Reason::MovedIneligible { from: current.clone() }
            }
        })
    }
}

impl RoutingPolicy for ThreadAffinityPolicy {
    fn decide(&self, input: &RoutingInput) -> Decision {
        let scores = scoring::score(input, &self.config, None, None);
        match &input.event {
            RoutingEvent::NewThread => self.select(input, &scores, None, |chosen, scores| Reason::BestScore {
                detail: scoring::detail(chosen, scores, &self.config, input),
            }),
            RoutingEvent::ChildStart { parent_account } => {
                if let Some(parent) = parent_account
                    .as_ref()
                    .and_then(|id| scores.iter().find(|s| &s.public.account == id && s.public.eligible))
                {
                    return Decision::Use {
                        account: parent.public.account.clone(),
                        reason: Reason::ParentAffinity,
                    };
                }
                self.select(input, &scores, None, |chosen, scores| Reason::ParentUnavailable {
                    detail: scoring::detail(chosen, scores, &self.config, input),
                })
            }
            RoutingEvent::Continue { current } => self.continue_on(input, &current.account, &scores, false),
            RoutingEvent::CompactionCompleted { current } => self.continue_on(input, &current.account, &scores, true),
            RoutingEvent::AfterOutcome {
                current,
                outcome,
                first_failure_at,
            } => match outcome {
                Outcome::Success => self.continue_on(input, &current.account, &scores, false),
                Outcome::RateLimited { retry_after } | Outcome::Overloaded { retry_after } => {
                    let deadline = add_seconds(*first_failure_at, self.config.retry_deadline_secs);
                    let cooling = input
                        .accounts
                        .iter()
                        .find(|a| a.account.id == current.account)
                        .and_then(|a| a.cooling_until);
                    let known_at = retry_after
                        .iter()
                        .chain(cooling.iter())
                        .copied()
                        .filter(|at| *at > input.now)
                        .max();
                    let at = known_at.unwrap_or(input.now);
                    if input.now < deadline {
                        Decision::RetrySame {
                            account: current.account.clone(),
                            at,
                        }
                    } else {
                        Decision::Reject {
                            retryable: true,
                            earliest_usable_at: known_at,
                            message: "Temporary limit exceeded the retry deadline; keep the thread assignment".into(),
                        }
                    }
                }
                Outcome::Exhausted { window, reset_at } => {
                    let decision = self.select(input, &scores, Some(&current.account), |_, _| Reason::MovedExhausted {
                        from: current.account.clone(),
                        window: window.clone(),
                    });
                    if matches!(decision, Decision::Reject { .. }) {
                        reject(input, *reset_at)
                    } else {
                        decision
                    }
                }
                Outcome::Revoked | Outcome::ModelIneligible => {
                    self.select(input, &scores, Some(&current.account), |_, _| Reason::MovedIneligible {
                        from: current.account.clone(),
                    })
                }
            },
        }
    }

    fn scores(&self, input: &RoutingInput) -> Vec<AccountScore> {
        scoring::score(input, &self.config, None, None)
            .into_iter()
            .map(|s| s.public)
            .collect()
    }
}

fn add_seconds(now: DateTime<Utc>, seconds: u64) -> DateTime<Utc> {
    DateTime::from_timestamp(
        now.timestamp().saturating_add(seconds.min(i64::MAX as u64) as i64),
        now.timestamp_subsec_nanos(),
    )
    .unwrap_or(DateTime::<Utc>::MAX_UTC)
}

fn exhaustion_window(runtime: &AccountRuntime) -> WindowKind {
    // ponytail: runtime exhaustion has no window kind. Use the tightest observed
    // window for the label until sap-core carries the confirmed kind explicitly.
    runtime
        .windows
        .iter()
        .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent))
        .map(|w| w.kind.clone())
        .unwrap_or_else(|| WindowKind::Other("unknown".into()))
}

fn reject(input: &RoutingInput, extra_reset: Option<DateTime<Utc>>) -> Decision {
    let earliest = input
        .accounts
        .iter()
        .filter(|a| {
            a.account.provider == input.provider && input.model.as_ref().is_none_or(|m| a.account.serves_model(m))
        })
        .flat_map(|a| {
            a.windows
                .iter()
                .filter_map(|w| w.reset_at)
                .chain(a.exhausted_until.flatten())
                .chain(a.cooling_until)
        })
        .chain(extra_reset)
        .filter(|at| *at > input.now)
        .min();
    Decision::Reject {
        retryable: true,
        earliest_usable_at: earliest,
        message: "No eligible account can serve this request".into(),
    }
}
