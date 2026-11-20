use sap_core::routing::RoutingConfig;

/// First ASCII case-insensitive substring match in configuration table order.
pub fn plan_capacity(plan: &str, cfg: &RoutingConfig) -> Option<f64> {
    lookup(plan, &cfg.plan_capacity)
}

/// Share multiplier for a plan, matched like `plan_capacity`.
pub fn plan_share(plan: &str, cfg: &RoutingConfig) -> Option<f64> {
    lookup(plan, &cfg.plan_share)
}

fn lookup(plan: &str, table: &[(String, f64)]) -> Option<f64> {
    let plan = plan.to_ascii_lowercase();
    table
        .iter()
        .find(|(name, _)| plan.contains(&name.to_ascii_lowercase()))
        .map(|(_, value)| *value)
}
