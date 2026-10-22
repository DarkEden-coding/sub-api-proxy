//! Optional telemetry never affects replication success or its retry schedule.
use crate::{Peer, PeerStatus};
use http::Method;
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::TokioExecutor,
};
use sap_core::{health::PeerTelemetry, ids::NodeId};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

pub(crate) async fn poll_loop(local: NodeId, peer: Peer, statuses: Arc<Mutex<BTreeMap<NodeId, PeerStatus>>>) {
    let mut connector = HttpConnector::new();
    connector.set_connect_timeout(Some(Duration::from_secs(2)));
    let client = Client::builder(TokioExecutor::new()).build(connector);
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        let sample = fetch(&client, &local, &peer).await;
        let mut statuses = statuses.lock().expect("status lock poisoned");
        let status = statuses.get_mut(&peer.node).expect("configured peer");
        match sample {
            Ok(sample) => {
                status.telemetry = Some(sample);
                status.telemetry_received_at = Some(crate::now());
                status.telemetry_error = None;
            }
            Err(error) => status.telemetry_error = Some(error),
        }
    }
}

pub(crate) async fn fetch(
    client: &crate::agent::HttpClient,
    local: &NodeId,
    peer: &Peer,
) -> Result<PeerTelemetry, String> {
    let body = crate::agent::request(client, local, peer, Method::GET, "/sap/sync/telemetry", Vec::new()).await?;
    let sample: PeerTelemetry = serde_json::from_slice(&body).map_err(|_| "invalid peer telemetry")?;
    let valid_latency = sample.added_latency_p50_ms.is_none_or(|v| v.is_finite() && v >= 0.0);
    if sample.node != peer.node
        || sample.version.len() > 128
        || !sample.requests_per_minute.is_finite()
        || sample.requests_per_minute < 0.0
        || !valid_latency
        || (sample.observed_at.timestamp() as i128 - crate::now() as i128).abs() > 60
    {
        return Err("invalid peer telemetry identity, time or metrics".into());
    }
    Ok(sample)
}
