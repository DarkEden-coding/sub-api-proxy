use sap_core::account::{Account, AccountRuntime, Capacity, CapacitySource};
use sap_core::ids::{AccountId, Provider};
use sap_core::quota::{ObservationSource, QuotaWindow, WindowKind};
use sap_core::routing::{
    Decision, Outcome, Reason, RoutingConfig, RoutingEvent, RoutingInput, RoutingPolicy, ScoreDetail,
};
use sap_core::thread::Assignment;
use sap_core::{DateTime, Utc};
use sap_routing::ThreadAffinityPolicy;

const HOUR: i64 = 3600;
const DAY: i64 = 24 * HOUR;

fn at(seconds: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000 + seconds, 0).unwrap()
}
fn id(name: &str) -> AccountId {
    AccountId(name.into())
}
fn account(name: &str) -> AccountRuntime {
    AccountRuntime {
        account: Account {
            id: id(name),
            provider: Provider::Claude,
            label: name.into(),
            email: None,
            plan: None,
            capacity: Capacity::default(),
            priority: 0,
            disabled: false,
            models: None,
            created_at: at(0),
        },
        // Weekly resets in five days, outside the 36-hour drain window.
        windows: [(WindowKind::FiveHour, 3600), (WindowKind::Weekly, 5 * DAY)]
            .into_iter()
            .map(|(kind, reset)| QuotaWindow {
                kind,
                used_percent: 50.0,
                reset_at: Some(at(reset)),
                window_minutes: None,
                observed_at: at(0),
                source: ObservationSource::Passive,
            })
            .collect(),
        active_streams: 0,
        active_threads: 0,
        exhausted_until: None,
        cooling_until: None,
        credential_usable: true,
        revoked: false,
    }
}
fn assignment(name: &str) -> Assignment {
    Assignment {
        account: id(name),
        reason: Reason::Kept,
        at: at(0),
    }
}
fn input(event: RoutingEvent) -> RoutingInput {
    RoutingInput {
        now: at(0),
        provider: Provider::Claude,
        model: Some("model".into()),
        event,
        accounts: vec![account("a"), account("b")],
        pick: 0.0,
    }
}
fn overridden(multiplier: f64) -> Capacity {
    Capacity {
        multiplier,
        source: CapacitySource::Override,
    }
}
fn weekly(runtime: &mut AccountRuntime, used_percent: f64, reset: i64) {
    let window = &mut runtime.windows[1];
    window.used_percent = used_percent;
    window.reset_at = Some(at(reset));
}
fn weights(policy: &ThreadAffinityPolicy, input: &RoutingInput) -> Vec<f64> {
    policy.scores(input).iter().map(|s| s.weight_percent).collect()
}
fn close(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() < 1e-9
}
fn policy() -> ThreadAffinityPolicy {
    ThreadAffinityPolicy::new(RoutingConfig::default())
}
fn assert_use(input: &RoutingInput, account: &str, reason: Reason) {
    assert_eq!(
        policy().decide(input),
        Decision::Use {
            account: id(account),
            reason
        }
    );
}
fn outcome(outcome: Outcome) -> RoutingInput {
    input(RoutingEvent::AfterOutcome {
        current: assignment("a"),
        outcome,
        first_failure_at: at(0),
    })
}

#[test]
fn window_past_its_reset_counts_as_fresh_allowance() {
    let mut input = input(RoutingEvent::NewThread);
    // `a` was observed full, but that window has since reset; `b` is half used.
    for w in &mut input.accounts[0].windows {
        w.used_percent = 100.0;
        w.reset_at = Some(at(-60));
    }
    let scores = policy().scores(&input);
    let a = scores.iter().find(|s| s.account == id("a")).unwrap();
    assert!(a.stale, "an elapsed window must be marked stale");
    assert!(a.score > 0.0, "an elapsed window must not look exhausted");
    assert!(matches!(policy().decide(&input), Decision::Use { .. }));
}

#[test]
fn new_thread_uses_best_score_and_deterministic_ties() {
    let mut request = input(RoutingEvent::NewThread);
    request.accounts.reverse();
    assert_use(
        &request,
        "a",
        Reason::BestScore {
            detail: ScoreDetail::ResetSoon,
        },
    );
    request.accounts[0].account.priority = 1;
    assert_use(
        &request,
        "b",
        Reason::BestScore {
            detail: ScoreDetail::ResetSoon,
        },
    );
    request.accounts[1].credential_usable = false;
    assert_use(
        &request,
        "b",
        Reason::BestScore {
            detail: ScoreDetail::OnlyChoice,
        },
    );
}

#[test]
fn child_prefers_eligible_parent_and_falls_back_when_unavailable() {
    let mut request = input(RoutingEvent::ChildStart {
        parent_account: Some(id("b")),
    });
    assert_use(&request, "b", Reason::ParentAffinity);
    request.accounts[1].cooling_until = Some(at(20));
    assert_use(
        &request,
        "a",
        Reason::ParentUnavailable {
            detail: ScoreDetail::OnlyChoice,
        },
    );
    request.event = RoutingEvent::ChildStart { parent_account: None };
    assert_use(
        &request,
        "a",
        Reason::ParentUnavailable {
            detail: ScoreDetail::OnlyChoice,
        },
    );
}

#[test]
fn continuation_keeps_even_when_cooling_or_another_account_scores_higher() {
    let mut request = input(RoutingEvent::Continue {
        current: assignment("b"),
    });
    request.accounts[1].cooling_until = Some(at(20));
    request.accounts[0].account.capacity = overridden(20.0);
    assert_use(&request, "b", Reason::Kept);
    request.accounts[1].exhausted_until = Some(Some(at(0)));
    assert_use(&request, "b", Reason::Kept);
}

#[test]
fn continuation_moves_on_exhaustion_including_unknown_reset() {
    let mut request = input(RoutingEvent::Continue {
        current: assignment("a"),
    });
    for exhausted in [Some(at(3600)), None] {
        request.accounts[0].exhausted_until = Some(exhausted);
        request.accounts[0].windows[1].used_percent = 100.0;
        assert_use(
            &request,
            "b",
            Reason::MovedExhausted {
                from: id("a"),
                window: WindowKind::Weekly,
            },
        );
    }
}

#[test]
fn continuation_moves_on_every_hard_ineligibility() {
    for change in 0..5 {
        let mut request = input(RoutingEvent::Continue {
            current: assignment("a"),
        });
        match change {
            0 => request.accounts[0].revoked = true,
            1 => request.accounts[0].account.disabled = true,
            2 => request.accounts[0].credential_usable = false,
            3 => request.accounts[0].account.models = Some(vec!["other".into()]),
            _ => request.accounts[0].account.provider = Provider::Codex,
        }
        assert_use(&request, "b", Reason::MovedIneligible { from: id("a") });
    }
}

#[test]
fn temporary_failures_retry_same_until_deadline_and_never_move() {
    for failure in [
        Outcome::RateLimited {
            retry_after: Some(at(10)),
        },
        Outcome::Overloaded {
            retry_after: Some(at(10)),
        },
    ] {
        let mut request = outcome(failure);
        assert_eq!(
            policy().decide(&request),
            Decision::RetrySame {
                account: id("a"),
                at: at(10)
            }
        );
        request.accounts[0].cooling_until = Some(at(20));
        assert_eq!(
            policy().decide(&request),
            Decision::RetrySame {
                account: id("a"),
                at: at(20)
            }
        );
        request.now = at(60);
        assert!(matches!(
            policy().decide(&request),
            Decision::Reject {
                retryable: true,
                earliest_usable_at: None,
                ..
            }
        ));
    }
    let request = outcome(Outcome::RateLimited {
        retry_after: Some(at(60)),
    });
    assert_eq!(
        policy().decide(&request),
        Decision::RetrySame {
            account: id("a"),
            at: at(60)
        }
    );
    let request = outcome(Outcome::Overloaded { retry_after: None });
    assert_eq!(
        policy().decide(&request),
        Decision::RetrySame {
            account: id("a"),
            at: at(0)
        }
    );
}

#[test]
fn exhausted_outcome_excludes_current_without_requiring_runtime_update() {
    let request = outcome(Outcome::Exhausted {
        window: WindowKind::Weekly,
        reset_at: Some(at(3600)),
    });
    assert_use(
        &request,
        "b",
        Reason::MovedExhausted {
            from: id("a"),
            window: WindowKind::Weekly,
        },
    );
}

#[test]
fn revoked_and_model_ineligible_outcomes_move() {
    for failure in [Outcome::Revoked, Outcome::ModelIneligible] {
        assert_use(&outcome(failure), "b", Reason::MovedIneligible { from: id("a") });
    }
    assert_use(&outcome(Outcome::Success), "a", Reason::Kept);
}

#[test]
fn compaction_requires_strict_relative_margin() {
    let mut request = input(RoutingEvent::CompactionCompleted {
        current: assignment("a"),
    });
    assert_use(&request, "a", Reason::KeptAfterCompaction);
    // Shares are proportional to capacity, so b must exceed a by the 25 percent margin.
    request.accounts[1].account.capacity = overridden(1.25);
    assert_use(&request, "a", Reason::KeptAfterCompaction);
    request.accounts[1].account.capacity = overridden(1.26);
    assert_use(&request, "b", Reason::MovedAfterCompaction { from: id("a") });
    request.accounts[1].cooling_until = Some(at(50));
    assert_use(&request, "a", Reason::KeptAfterCompaction);
    request.accounts[0].revoked = true;
    assert!(matches!(policy().decide(&request), Decision::Reject { .. }));
}

#[test]
fn rejection_reports_known_reset_only_for_provider_and_model() {
    let mut request = input(RoutingEvent::NewThread);
    for a in &mut request.accounts {
        a.exhausted_until = Some(None);
        a.windows.clear();
    }
    assert!(matches!(
        policy().decide(&request),
        Decision::Reject {
            retryable: true,
            earliest_usable_at: None,
            ..
        }
    ));
    request.accounts[0].exhausted_until = Some(Some(at(30)));
    request.accounts[1].exhausted_until = Some(Some(at(10)));
    assert!(matches!(policy().decide(&request), Decision::Reject { earliest_usable_at: Some(t), .. } if t == at(10)));
    request.accounts[1].account.models = Some(vec![]);
    assert!(matches!(policy().decide(&request), Decision::Reject { earliest_usable_at: Some(t), .. } if t == at(30)));
    request.accounts[0].account.provider = Provider::Codex;
    assert!(matches!(
        policy().decide(&request),
        Decision::Reject {
            earliest_usable_at: None,
            ..
        }
    ));
    let mut request = outcome(Outcome::Exhausted {
        window: WindowKind::Weekly,
        reset_at: Some(at(90)),
    });
    request.accounts.truncate(1);
    request.accounts[0].windows.clear();
    assert!(matches!(policy().decide(&request), Decision::Reject { earliest_usable_at: Some(t), .. } if t == at(90)));
}

#[test]
fn weights_follow_weekly_allowance_left_over_time_to_reset() {
    // The dashboard case: learned capacities of 8057 and 383 gave a 99/1 split.
    // a has 84% left and resets in six days; b has 65% left and resets in 49 hours.
    let mut request = input(RoutingEvent::NewThread);
    request.accounts[0].account.capacity = Capacity {
        multiplier: 8057.0,
        source: CapacitySource::Learned,
    };
    request.accounts[1].account.capacity = Capacity {
        multiplier: 383.0,
        source: CapacitySource::Learned,
    };
    weekly(&mut request.accounts[0], 16.0, 6 * DAY);
    weekly(&mut request.accounts[1], 35.0, 49 * HOUR);
    let a = 0.84 / (6 * DAY) as f64;
    let b = 0.65 / (49 * HOUR) as f64;
    let w = weights(&policy(), &request);
    assert!(
        close(w[0], 100.0 * a / (a + b)) && close(w[1], 100.0 * b / (a + b)),
        "{w:?}"
    );
    assert!(w[1] > 69.0);
    // Below the taper start, the five-hour window does not change the split.
    request.accounts[1].windows[0].used_percent = 89.0;
    assert_eq!(weights(&policy(), &request), w);
}

#[test]
fn plan_and_override_scale_allowance_but_learned_capacity_does_not() {
    let mut request = input(RoutingEvent::NewThread);
    request.accounts[0].account.plan = Some("default_claude_max_20x".into());
    request.accounts[1].account.plan = Some("default_claude_max_5x".into());
    assert!(close(weights(&policy(), &request)[0], 80.0));
    request.accounts[1].account.capacity = overridden(20.0);
    assert!(close(weights(&policy(), &request)[0], 50.0));
}

#[test]
fn account_near_weekly_reset_drains_until_its_five_hour_window_fills() {
    let mut request = input(RoutingEvent::NewThread);
    weekly(&mut request.accounts[1], 70.0, 30 * HOUR);
    assert_eq!(weights(&policy(), &request), vec![0.0, 100.0]);
    request.pick = 0.999;
    assert_use(
        &request,
        "b",
        Reason::BestScore {
            detail: ScoreDetail::ResetSoon,
        },
    );
    // Above 90% of the five-hour window, the drain hands over linearly.
    request.accounts[1].windows[0].used_percent = 95.0;
    let w = weights(&policy(), &request);
    assert!(close(w[0], 50.0) && close(w[1], 50.0), "{w:?}");
    request.pick = 0.4;
    assert_use(
        &request,
        "a",
        Reason::BestScore {
            detail: ScoreDetail::ResetSoon,
        },
    );
    // A full five-hour window ends the drain; ordinary weights apply.
    request.accounts[1].windows[0].used_percent = 100.0;
    assert_eq!(weights(&policy(), &request)[1], 0.0);
    // With two draining accounts, the earlier reset drains first.
    request.accounts[1].windows[0].used_percent = 50.0;
    weekly(&mut request.accounts[0], 70.0, 20 * HOUR);
    assert_eq!(weights(&policy(), &request), vec![100.0, 0.0]);
    // A draining account with nothing left does not drain.
    weekly(&mut request.accounts[0], 100.0, 20 * HOUR);
    assert_eq!(weights(&policy(), &request), vec![0.0, 100.0]);
}

#[test]
fn compaction_moves_threads_onto_a_draining_account() {
    let mut request = input(RoutingEvent::CompactionCompleted {
        current: assignment("a"),
    });
    weekly(&mut request.accounts[1], 70.0, 30 * HOUR);
    assert_use(&request, "b", Reason::MovedAfterCompaction { from: id("a") });
    request.event = RoutingEvent::Continue {
        current: assignment("a"),
    };
    assert_use(&request, "a", Reason::Kept);
}

#[test]
fn pick_samples_in_proportion_to_weight() {
    let mut request = input(RoutingEvent::NewThread);
    request.accounts[1].account.capacity = overridden(3.0);
    // b holds 75%: samples below 0.75 take it, the rest take a.
    for (pick, expected) in [(0.0, "b"), (0.749, "b"), (0.75, "a"), (0.999, "a"), (f64::NAN, "b")] {
        request.pick = pick;
        let Decision::Use { account, .. } = policy().decide(&request) else {
            panic!("expected an account")
        };
        assert_eq!(account, id(expected), "pick {pick}");
    }
}

#[test]
fn stale_weekly_windows_are_penalized_and_unknown_resets_add_no_urgency() {
    let mut request = input(RoutingEvent::NewThread);
    for w in &mut request.accounts[1].windows {
        w.observed_at = at(-1801);
    }
    let scores = policy().scores(&request);
    assert!(scores[1].stale);
    assert!(close(scores[1].score, scores[0].score * 0.7));
    request.accounts[1].windows[0].observed_at = at(-1800);
    request.accounts[1].windows[1].observed_at = at(-1800);
    assert!(!policy().scores(&request)[1].stale);
    // A stale five-hour window alone marks the account but does not penalize it.
    request.accounts[1].windows[0].observed_at = at(-1801);
    let scores = policy().scores(&request);
    assert!(scores[1].stale && close(scores[1].score, 0.5));
    request.accounts[1].windows.pop();
    assert!(policy().scores(&request)[1].stale);
    assert_eq!(policy().scores(&request)[1].score, 0.0);
    for a in &mut request.accounts {
        for w in &mut a.windows {
            w.reset_at = None;
        }
    }
    assert!(policy().scores(&request).iter().all(|s| close(s.score, 0.5)));
}

#[test]
fn headroom_and_load_terms_remain_configurable() {
    let mut request = input(RoutingEvent::NewThread);
    request.accounts[0].windows[1].used_percent = 99.0;
    let headroom = ThreadAffinityPolicy::new(RoutingConfig {
        urgency_weight: 0.0,
        headroom_weight: 1.0,
        ..RoutingConfig::default()
    });
    assert_eq!(
        headroom.decide(&request),
        Decision::Use {
            account: id("b"),
            reason: Reason::BestScore {
                detail: ScoreDetail::MostHeadroom
            }
        }
    );
    request.accounts[0].windows[1].used_percent = 50.0;
    request.accounts[0].active_threads = 3;
    request.accounts[1].active_threads = 1;
    let load = ThreadAffinityPolicy::new(RoutingConfig {
        load_weight: 0.5,
        ..RoutingConfig::default()
    });
    // Scores 1 - 0.5 * 0.75 and 1 - 0.5 * 0.25.
    let w = weights(&load, &request);
    assert!(close(w[0], 100.0 * 0.625 / 1.5), "{w:?}");
    assert_eq!(
        load.decide(&request),
        Decision::Use {
            account: id("b"),
            reason: Reason::BestScore {
                detail: ScoreDetail::LeastLoaded
            }
        }
    );
    request.accounts[1].account.disabled = true;
    let scores = load.scores(&request);
    assert_eq!(scores[1].weight_percent, 0.0);
    assert_eq!(scores[1].ineligible_reason.as_deref(), Some("disabled"));
    assert_eq!(scores[0].weight_percent, 100.0);
}

#[test]
fn zero_scores_have_equal_fallback_weights_and_finite_results() {
    let mut request = input(RoutingEvent::NewThread);
    for a in &mut request.accounts {
        a.windows.clear();
    }
    let scores = policy().scores(&request);
    assert!(
        scores
            .iter()
            .all(|s| s.score == 0.5 && s.weight_percent == 50.0 && s.stale)
    );
    request.accounts[0].windows = account("x").windows;
    request.accounts[0].windows[0].used_percent = f64::NAN;
    request.accounts[0].windows[1].used_percent = -1.0;
    let p = ThreadAffinityPolicy::new(RoutingConfig {
        reset_floor_secs: 0,
        five_hour_taper_start_percent: f64::NAN,
        ..RoutingConfig::default()
    });
    assert!(
        p.scores(&request)
            .iter()
            .all(|s| s.score.is_finite() && s.weight_percent.is_finite())
    );
}
