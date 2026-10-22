use sap_core::account::{Account, AccountRuntime, Capacity};
use sap_core::ids::{AccountId, Provider};
use sap_core::quota::{ObservationSource, QuotaWindow, WindowKind};
use sap_core::routing::{
    Decision, Outcome, Reason, RoutingConfig, RoutingEvent, RoutingInput, RoutingPolicy, ScoreDetail,
};
use sap_core::thread::Assignment;
use sap_core::{DateTime, Utc};
use sap_routing::ThreadAffinityPolicy;

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
        windows: [WindowKind::FiveHour, WindowKind::Weekly]
            .into_iter()
            .map(|kind| QuotaWindow {
                kind,
                used_percent: 50.0,
                reset_at: Some(at(3600)),
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
    }
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
    request.accounts[0].account.capacity.multiplier = 20.0;
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
    request.accounts[1].account.capacity.multiplier = 1.25;
    assert_use(&request, "a", Reason::KeptAfterCompaction);
    request.accounts[1].account.capacity.multiplier = 1.26;
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
fn scores_favor_capacity_reset_urgency_and_lower_thread_load() {
    let mut request = input(RoutingEvent::NewThread);
    request.accounts[1].account.capacity.multiplier = 5.0;
    assert!(policy().scores(&request)[1].score > policy().scores(&request)[0].score);
    request.accounts[1].account.capacity.multiplier = 1.0;
    for w in &mut request.accounts[1].windows {
        w.reset_at = Some(at(600));
    }
    assert_use(
        &request,
        "b",
        Reason::BestScore {
            detail: ScoreDetail::ResetSoon,
        },
    );
    for w in &mut request.accounts[1].windows {
        w.reset_at = Some(at(3600));
    }
    request.accounts[0].active_threads = 10;
    request.accounts[1].active_streams = 100;
    assert_use(
        &request,
        "b",
        Reason::BestScore {
            detail: ScoreDetail::LeastLoaded,
        },
    );
    let scores = policy().scores(&request);
    assert!((scores.iter().map(|s| s.weight_percent).sum::<f64>() - 100.0).abs() < 1e-10);
}

#[test]
fn stale_missing_and_unknown_reset_windows_are_explicit() {
    let mut request = input(RoutingEvent::NewThread);
    for w in &mut request.accounts[1].windows {
        w.observed_at = at(-1801);
    }
    let scores = policy().scores(&request);
    assert!(scores[1].stale);
    assert_eq!(scores[1].score, scores[0].score * 0.7);
    request.accounts[1].windows[0].observed_at = at(-1800);
    request.accounts[1].windows[1].observed_at = at(-1800);
    assert!(!policy().scores(&request)[1].stale);
    request.accounts[1].windows.pop();
    assert!(policy().scores(&request)[1].stale);
    request.accounts[1].windows.clear();
    assert_eq!(policy().scores(&request)[1].score, 0.0);
    for w in &mut request.accounts[0].windows {
        w.reset_at = None;
    }
    assert_eq!(policy().scores(&request)[0].score, 1.0);
}

#[test]
fn tightest_window_limits_headroom_and_ineligible_accounts_have_zero_weight() {
    let mut request = input(RoutingEvent::NewThread);
    request.accounts[0].windows[1].used_percent = 99.0;
    let cfg = RoutingConfig {
        urgency_weight: 0.0,
        ..RoutingConfig::default()
    };
    let p = ThreadAffinityPolicy::new(cfg);
    assert_eq!(
        p.decide(&request),
        Decision::Use {
            account: id("b"),
            reason: Reason::BestScore {
                detail: ScoreDetail::MostHeadroom
            }
        }
    );
    request.accounts[1].account.disabled = true;
    let scores = p.scores(&request);
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
            .all(|s| s.score == 0.0 && s.weight_percent == 50.0 && s.stale)
    );
    request.accounts[0].windows = account("x").windows;
    request.accounts[0].windows[0].used_percent = f64::NAN;
    request.accounts[0].windows[1].used_percent = -1.0;
    let p = ThreadAffinityPolicy::new(RoutingConfig {
        reset_floor_secs: 0,
        ..RoutingConfig::default()
    });
    assert!(
        p.scores(&request)
            .iter()
            .all(|s| s.score.is_finite() && s.weight_percent.is_finite())
    );
}

#[test]
fn formula_uses_max_normalization_and_multiplies_entire_score_when_stale() {
    let mut request = input(RoutingEvent::NewThread);
    request.accounts[1].account.capacity.multiplier = 2.0;
    request.accounts[0].active_threads = 1;
    request.accounts[1].active_threads = 3;
    let scores = policy().scores(&request);
    assert_eq!(scores[0].score, 0.5 + 0.5 - 0.5 * 0.25);
    assert_eq!(scores[1].score, 1.0 + 1.0 - 0.5 * 0.75);
    for window in &mut request.accounts[0].windows {
        window.observed_at = at(-1801);
    }
    assert_eq!(policy().scores(&request)[0].score, scores[0].score * 0.7);
    request.accounts[0].account.disabled = true;
    assert_eq!(policy().scores(&request)[1].score, 1.5);
}

#[test]
fn negative_compaction_scores_require_a_real_improvement() {
    let mut request = input(RoutingEvent::CompactionCompleted {
        current: assignment("a"),
    });
    for a in &mut request.accounts {
        a.windows.clear();
    }
    request.accounts[0].active_threads = 4;
    request.accounts[1].active_threads = 3;
    // Scores -2/7 and -1.5/7 meet the margin exactly: keep at the boundary.
    assert_use(&request, "a", Reason::KeptAfterCompaction);
    request.accounts[1].active_threads = 2;
    assert_use(&request, "b", Reason::MovedAfterCompaction { from: id("a") });
}
