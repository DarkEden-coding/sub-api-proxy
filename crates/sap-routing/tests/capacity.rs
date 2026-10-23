use sap_core::routing::RoutingConfig;
use sap_routing::plan_capacity;

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
