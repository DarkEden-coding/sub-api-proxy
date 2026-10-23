use sap_core::routing::RoutingConfig;

/// First ASCII case-insensitive substring match in configuration table order.
pub fn plan_capacity(plan: &str, cfg: &RoutingConfig) -> Option<f64> {
    let plan = plan.to_ascii_lowercase();
    cfg.plan_capacity
        .iter()
        .find(|(name, _)| plan.contains(&name.to_ascii_lowercase()))
        .map(|(_, capacity)| *capacity)
}
