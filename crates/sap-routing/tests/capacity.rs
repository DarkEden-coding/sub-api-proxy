use sap_core::DateTime;
use sap_core::account::CapacitySource;
use sap_core::ids::AccountId;
use sap_core::quota::{ObservationSource, QuotaWindow, WindowKind};
use sap_core::routing::RoutingConfig;
use sap_routing::{CapacityLearner, plan_capacity};

fn window(used: f64, seconds: i64, reset: i64) -> QuotaWindow {
    QuotaWindow {
        kind: WindowKind::FiveHour,
        used_percent: used,
        reset_at: DateTime::from_timestamp(reset, 0),
        window_minutes: None,
        observed_at: DateTime::from_timestamp(seconds, 0).unwrap(),
        source: ObservationSource::Passive,
    }
}

#[test]
fn plan_lookup_is_ascii_case_insensitive_and_uses_table_order() {
    let cfg = RoutingConfig::default();
    assert_eq!(plan_capacity("default_CLAUDE_MAX_20X", &cfg), Some(20.0));
    assert_eq!(plan_capacity("MAX_5x", &cfg), Some(5.0));
    assert_eq!(plan_capacity("Enterprise", &cfg), None);
    assert_eq!(plan_capacity("pluſ", &cfg), None);
    let cfg = RoutingConfig {
        plan_capacity: vec![("PRO".into(), 2.0), ("pro_plus".into(), 5.0)],
        ..cfg
    };
    assert_eq!(plan_capacity("pro_plus", &cfg), Some(2.0));
}

#[test]
fn learner_requires_samples_and_computes_ewma_per_account_and_window() {
    let mut learner = CapacityLearner::new(0.5, 2, 100.0).unwrap();
    let account = AccountId("a".into());
    assert_eq!(learner.observe(&account, &window(10.0, 0, 100), 0), None);
    assert_eq!(learner.observe(&account, &window(11.0, 1, 100), 200), None);
    let capacity = learner.observe(&account, &window(13.0, 2, 100), 1000).unwrap();
    assert_eq!(capacity.source, CapacitySource::Learned);
    assert_eq!(capacity.multiplier, 3.0);
    assert_eq!(
        learner.observe(&AccountId("b".into()), &window(13.0, 2, 100), 1000),
        None
    );
    let mut weekly = window(13.0, 2, 100);
    weekly.kind = WindowKind::Weekly;
    assert_eq!(learner.observe(&account, &weekly, 1000), None);
    learner.forget_account(&account);
    assert_eq!(learner.observe(&account, &window(14.0, 3, 100), 1100), None);
}

#[test]
fn learner_ignores_duplicates_and_accumulates_rounded_plateaus() {
    let mut learner = CapacityLearner::new(1.0, 1, 100.0).unwrap();
    let account = AccountId("a".into());
    learner.observe(&account, &window(10.0, 0, 100), 0);
    learner.observe(&account, &window(10.0, 2, 100), 100);
    assert_eq!(learner.observe(&account, &window(11.0, 1, 100), 10000), None);
    assert_eq!(learner.observe(&account, &window(10.0, 2, 100), 10000), None);
    assert_eq!(
        learner
            .observe(&account, &window(11.0, 3, 100), 200)
            .unwrap()
            .multiplier,
        2.0
    );
}

#[test]
fn reset_changes_usage_decreases_and_counter_restarts_do_not_train() {
    let mut learner = CapacityLearner::new(1.0, 1, 100.0).unwrap();
    let account = AccountId("a".into());
    learner.observe(&account, &window(10.0, 0, 100), 0);
    assert_eq!(learner.observe(&account, &window(20.0, 1, 200), 50000), None);
    assert_eq!(learner.observe(&account, &window(19.0, 2, 200), 60000), None);
    assert_eq!(learner.observe(&account, &window(20.0, 3, 200), 0), None);
    assert_eq!(
        learner
            .observe(&account, &window(21.0, 4, 200), 100)
            .unwrap()
            .multiplier,
        1.0
    );
    let capacity = learner.observe(&account, &window(0.0, 5, 300), 200).unwrap();
    assert_eq!(capacity.multiplier, 1.0);
}

#[test]
fn invalid_inputs_and_unknown_resets_do_not_produce_estimates() {
    for (alpha, samples, baseline) in [
        (0.0, 1, 100.0),
        (1.1, 1, 100.0),
        (f64::NAN, 1, 100.0),
        (1.0, 0, 100.0),
        (1.0, 1, 0.0),
    ] {
        assert!(CapacityLearner::new(alpha, samples, baseline).is_err());
    }
    let mut learner = CapacityLearner::new(0.5, 1, 100.0).unwrap();
    let account = AccountId("a".into());
    for second in 0..3 {
        let mut w = window(second as f64, second, 100);
        w.reset_at = None;
        assert_eq!(learner.observe(&account, &w, second as u64 * 100), None);
    }
    assert_eq!(learner.observe(&account, &window(f64::NAN, 4, 100), 400), None);
}
