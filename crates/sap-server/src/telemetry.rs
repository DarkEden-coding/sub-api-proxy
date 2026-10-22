//! Process-local counters. No request identity, bytes or credentials are retained.
use crate::node::Node;
use cpa_server::sap_hooks::{AttemptResult, Observation, SapCandidate, SapHooks, SapPick, SapRequest};
use sap_core::{Utc, health::PeerTelemetry};
use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Default)]
pub(crate) struct Metrics {
    // At most 60 one-second buckets, independent of request volume.
    requests: VecDeque<(u64, u64)>,
    // ponytail: latest 1024 selection durations within one minute; use a histogram
    // if deployments require an exact volume-weighted median beyond this ceiling.
    latency: VecDeque<(Instant, f64)>,
    rss: Option<u64>,
}
impl Metrics {
    fn request(&mut self) {
        let second = Utc::now().timestamp().max(0) as u64;
        self.requests.retain(|(at, _)| second.saturating_sub(*at) < 60);
        if let Some((at, count)) = self.requests.back_mut()
            && *at == second
        {
            *count = count.saturating_add(1);
        } else {
            self.requests.push_back((second, 1));
        }
        while self.requests.len() > 60 {
            self.requests.pop_front();
        }
    }
    fn latency(&mut self, elapsed: Duration) {
        self.latency.push_back((Instant::now(), elapsed.as_secs_f64() * 1000.0));
        while self.latency.len() > 1024 {
            self.latency.pop_front();
        }
    }
}
impl Node {
    pub(crate) fn telemetry(&self) -> PeerTelemetry {
        let now = Utc::now();
        let open_streams = self.lock().streams.values().copied().sum();
        let mut metrics = self.telemetry.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        metrics.latency.retain(|(at, _)| at.elapsed() < Duration::from_secs(60));
        let mut latency: Vec<_> = metrics.latency.iter().map(|(_, ms)| *ms).collect();
        latency.sort_by(f64::total_cmp);
        PeerTelemetry {
            node: self.config.node.clone(),
            role: self.config.role,
            state: sap_core::health::NodeState::Serving,
            version: env!("CARGO_PKG_VERSION").into(),
            observed_at: now,
            requests_per_minute: metrics
                .requests
                .iter()
                .filter(|(at, _)| (now.timestamp().max(0) as u64).saturating_sub(*at) < 60)
                .map(|(_, n)| *n as f64)
                .sum(),
            open_streams,
            added_latency_p50_ms: latency.get(latency.len() / 2).copied(),
            rss_bytes: metrics.rss,
        }
    }
    pub(crate) fn sample_rss(&self) {
        let rss = rss_bytes();
        self.telemetry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .rss = rss;
    }
}

/// Wrap the runtime hook boundary without changing quota/selection implementation.
pub(crate) struct Instrumented(pub Arc<Node>);
impl SapHooks for Instrumented {
    fn identify(&self, path: &str, headers: &http::HeaderMap, body: &[u8]) -> Option<Arc<SapRequest>> {
        self.0
            .telemetry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .request();
        self.0.identify(path, headers, body)
    }
    fn select(&self, request: &SapRequest, candidates: &[SapCandidate]) -> SapPick {
        let start = Instant::now();
        let result = self.0.select(request, candidates);
        self.0
            .telemetry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .latency(start.elapsed());
        result
    }
    fn on_outcome(&self, request: &SapRequest, credential: &str, result: &AttemptResult) {
        self.0.on_outcome(request, credential, result);
    }
    fn on_observation(&self, request: &SapRequest, credential: &str, observation: Observation) {
        self.0.on_observation(request, credential, observation);
    }
    fn on_stream(&self, request: &SapRequest, open: bool) {
        self.0.on_stream(request, open);
    }
    fn health(&self) -> sap_core::health::HealthReport {
        self.0.health()
    }
    fn routes(&self) -> Option<axum::Router> {
        self.0.routes()
    }
}

#[cfg(target_os = "linux")]
fn rss_bytes() -> Option<u64> {
    // VmRSS is current resident memory, unlike getrusage's peak RSS.
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let kb = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    kb.checked_mul(1024)
}
#[cfg(target_os = "macos")]
fn rss_bytes() -> Option<u64> {
    // libproc's stable C ABI; no subprocess, credentials or heavy dependency.
    #[repr(C)]
    #[derive(Default)]
    struct TaskInfo {
        virtual_size: u64,
        resident_size: u64,
        total_user: u64,
        total_system: u64,
        threads_user: u64,
        threads_system: u64,
        policy: i32,
        faults: i32,
        pageins: i32,
        cow_faults: i32,
        messages_sent: i32,
        messages_received: i32,
        syscalls_mach: i32,
        syscalls_unix: i32,
        csw: i32,
        threadnum: i32,
        numrunning: i32,
        priority: i32,
    }
    #[link(name = "proc")]
    unsafe extern "C" {
        fn proc_pidinfo(pid: i32, flavor: i32, arg: u64, buffer: *mut std::ffi::c_void, size: i32) -> i32;
    }
    let mut info = TaskInfo::default();
    let size = std::mem::size_of::<TaskInfo>() as i32;
    // SAFETY: buffer is writable and exactly size bytes; flavor 4 is PROC_PIDTASKINFO.
    let read = unsafe {
        proc_pidinfo(
            std::process::id() as i32,
            4,
            0,
            (&mut info as *mut TaskInfo).cast(),
            size,
        )
    };
    (read == size).then_some(info.resident_size)
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn rss_bytes() -> Option<u64> {
    None
}

/// Keep old samples for diagnostics, but never present expired values as current.
pub(crate) fn peer_view(
    id: sap_core::ids::NodeId,
    status: sap_sync::PeerStatus,
    at: sap_core::DateTime<Utc>,
) -> sap_core::api::NodeView {
    let now = at.timestamp().max(0) as u64;
    let fresh = status
        .telemetry_received_at
        .is_some_and(|at| now.saturating_sub(at) <= 15)
        && status
            .telemetry
            .as_ref()
            .is_some_and(|sample| (sample.observed_at.timestamp() as i128 - now as i128).abs() <= 15);
    let sample = status.telemetry.as_ref().filter(|_| fresh);
    sap_core::api::NodeView {
        id,
        role: sample.map(|s| s.role),
        state: sample.map(|s| s.state),
        reachable: status.last_error.is_none() && status.last_success.is_some_and(|at| now.saturating_sub(at) <= 60),
        version: sample.map(|s| s.version.clone()),
        requests_per_minute: sample.map(|s| s.requests_per_minute),
        open_streams: sample.map(|s| s.open_streams),
        added_latency_p50_ms: sample.and_then(|s| s.added_latency_p50_ms),
        rss_bytes: sample.and_then(|s| s.rss_bytes),
        sync_lag_secs: status.lag_secs.map(|s| s as f64),
        last_failover_at: None,
        last_seen: status
            .last_success
            .and_then(|t| sap_core::DateTime::from_timestamp(t as i64, 0)),
        telemetry_observed_at: status.telemetry.as_ref().map(|s| s.observed_at),
        telemetry_received_at: status
            .telemetry_received_at
            .and_then(|t| sap_core::DateTime::from_timestamp(t as i64, 0)),
        telemetry_stale: !fresh,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_and_expired_peer_samples_are_unknown_even_when_sync_is_healthy() {
        let now = Utc::now();
        let id = sap_core::ids::NodeId("peer".into());
        let mut status = sap_sync::PeerStatus {
            last_success: Some(now.timestamp() as u64),
            ..Default::default()
        };
        let missing = peer_view(id.clone(), status.clone(), now);
        assert!(missing.reachable);
        assert!(missing.telemetry_stale);
        assert!(missing.role.is_none());
        assert!(missing.open_streams.is_none());
        assert!(missing.requests_per_minute.is_none());
        status.telemetry_received_at = Some(now.timestamp() as u64);
        status.telemetry = Some(PeerTelemetry {
            node: id.clone(),
            role: sap_core::health::NodeRole::Primary,
            state: sap_core::health::NodeState::Serving,
            version: "test".into(),
            observed_at: now,
            requests_per_minute: 0.0,
            open_streams: 0,
            added_latency_p50_ms: None,
            rss_bytes: Some(1234),
        });
        let fresh = peer_view(id.clone(), status.clone(), now);
        assert_eq!(fresh.open_streams, Some(0));
        assert_eq!(fresh.requests_per_minute, Some(0.0));
        assert!(!fresh.telemetry_stale);
        let expired = peer_view(id.clone(), status.clone(), now + chrono::Duration::seconds(16));
        assert!(expired.reachable);
        assert!(expired.open_streams.is_none());
        assert!(expired.rss_bytes.is_none());
        assert!(expired.telemetry_stale);
        assert_eq!(expired.telemetry_observed_at, Some(now));
        // A recently received cached response must not extend an old sample's life.
        status.telemetry.as_mut().unwrap().observed_at = now - chrono::Duration::seconds(16);
        assert!(peer_view(id, status, now).telemetry_stale);
    }
}
