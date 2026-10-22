//! The shared fixtures must match the contract types exactly, so the dashboard and the
//! server agree on the JSON shape.

use sap_core::api::FlowSnapshot;
use serde_json::Value;

/// Numbers compare by value: the fixture may write `20` where serde writes `20.0`.
fn normalize(v: Value) -> Value {
    match v {
        Value::Number(n) => serde_json::json!(n.as_f64().unwrap()),
        Value::Array(a) => Value::Array(a.into_iter().map(normalize).collect()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, v)| (k, normalize(v))).collect()),
        other => other,
    }
}

#[test]
fn flow_fixture_round_trips() {
    let raw = include_str!("fixtures/flow.json");
    let snap: FlowSnapshot = serde_json::from_str(raw).expect("fixture matches FlowSnapshot");
    let back = normalize(serde_json::to_value(&snap).unwrap());
    let orig = normalize(serde_json::from_str(raw).unwrap());
    assert_eq!(back, orig, "serializing the parsed fixture must reproduce it");
}
