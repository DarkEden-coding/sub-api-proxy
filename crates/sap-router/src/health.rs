use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use http::Request;
use http_body_util::BodyExt;
use sap_core::health::{HEALTH_PATH, HealthReport, NodeState};
use serde::Serialize;
use tokio::{sync::watch, task::JoinSet};

use crate::{
    config::Config,
    proxy::{connect, full},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Healthy,
    Suspect,
    Down,
}

struct ServerHealth {
    state: State,
    failures: u32,
    healthy_since: Option<Instant>,
    recovered: bool,
    /// Down transitions so far; a lease remembers the count it started with.
    downs: u64,
}

pub struct Health {
    config: Arc<Config>,
    servers: Mutex<Vec<ServerHealth>>,
    /// Bumped on every observation and on shutdown, so leases re-check their server.
    changes: watch::Sender<u64>,
    stopped: std::sync::atomic::AtomicBool,
}

/// A request's or stream's claim on one server, released when that server is lost.
pub struct Lease {
    pub index: usize,
    downs: u64,
    changes: watch::Receiver<u64>,
}

#[derive(Serialize)]
pub struct Status {
    pub preferred: Option<String>,
    pub servers: Vec<ServerStatus>,
}

#[derive(Serialize)]
pub struct ServerStatus {
    name: String,
    state: State,
    consecutive_failures: u32,
    stable: bool,
}

impl Health {
    pub fn new(config: Arc<Config>) -> Arc<Self> {
        let servers = config
            .servers
            .iter()
            .map(|_| ServerHealth {
                state: State::Suspect,
                failures: 0,
                healthy_since: None,
                recovered: false,
                downs: 0,
            })
            .collect();
        Arc::new(Self {
            config,
            servers: Mutex::new(servers),
            changes: watch::channel(0).0,
            stopped: false.into(),
        })
    }

    pub fn name(&self, index: usize) -> &str {
        &self.config.servers[index].name
    }

    pub fn shutdown(&self) {
        self.stopped.store(true, std::sync::atomic::Ordering::SeqCst);
        self.changes.send_modify(|epoch| *epoch = epoch.wrapping_add(1));
    }

    fn is_stable(&self, server: &ServerHealth) -> bool {
        server.state == State::Healthy
            && (!server.recovered
                || server
                    .healthy_since
                    .is_some_and(|since| since.elapsed() >= Duration::from_secs(self.config.stable_secs)))
    }

    /// Servers in the order a request tries them: stable servers in configured order,
    /// then the rest (recovering, suspect, down) as a last resort. A request that cannot
    /// connect moves on without replaying bytes, so trying an unproven server costs at
    /// most a connect timeout, while refusing outright turned every probe blip on a
    /// single-server setup into 30 seconds of 503s.
    pub fn candidates(&self) -> Vec<usize> {
        let servers = self.servers.lock().unwrap();
        let rank = |server: &ServerHealth| match (self.is_stable(server), server.state) {
            (true, _) => 0,
            (false, State::Healthy) => 1,
            (false, State::Suspect) => 2,
            (false, State::Down) => 3,
        };
        let mut order: Vec<usize> = (0..servers.len()).collect();
        order.sort_by_key(|&index| rank(&servers[index]));
        order
    }

    pub fn is_preferred(&self, index: usize) -> bool {
        self.is_stable(&self.servers.lock().unwrap()[index])
    }

    pub fn state(&self, index: usize) -> State {
        self.servers.lock().unwrap()[index].state
    }

    pub fn lease(&self, index: usize) -> Lease {
        let servers = self.servers.lock().unwrap();
        Lease {
            index,
            downs: servers[index].downs,
            changes: self.changes.subscribe(),
        }
    }

    /// Resolves once the leased server has gone down and is still down while another
    /// server is stable, or on shutdown. With nowhere to fail over to, the transfer
    /// keeps its server: the server's own connection errors end it if it really died.
    pub async fn lost(&self, lease: &mut Lease) {
        loop {
            if self.stopped.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            {
                let servers = self.servers.lock().unwrap();
                let server = &servers[lease.index];
                if server.downs != lease.downs
                    && server.state != State::Healthy
                    && servers
                        .iter()
                        .enumerate()
                        .any(|(i, other)| i != lease.index && self.is_stable(other))
                {
                    return;
                }
            }
            if lease.changes.changed().await.is_err() {
                return;
            }
        }
    }

    pub fn observe(&self, index: usize, outcome: Result<(), String>) {
        let mut servers = self.servers.lock().unwrap();
        let server = &mut servers[index];
        let name = &self.config.servers[index].name;
        let before = server.state;
        match outcome {
            Ok(()) => {
                if server.healthy_since.is_none() {
                    server.healthy_since = Some(Instant::now());
                }
                server.failures = 0;
                server.state = State::Healthy;
                if before != State::Healthy {
                    if server.recovered {
                        tracing::info!(
                            server = %name,
                            "server healthy; preferred again after {}s of continuous health",
                            self.config.stable_secs
                        );
                    } else {
                        tracing::info!(server = %name, "server healthy");
                    }
                }
            }
            Err(reason) => {
                server.recovered = true;
                server.healthy_since = None;
                server.failures = server.failures.saturating_add(1);
                if server.failures >= self.config.down_after_failures {
                    if server.state != State::Down {
                        server.downs = server.downs.wrapping_add(1);
                    }
                    server.state = State::Down;
                } else {
                    server.state = State::Suspect;
                }
                if server.state != before {
                    tracing::warn!(
                        server = %name,
                        state = ?server.state,
                        failures = server.failures,
                        "server unhealthy: {reason}"
                    );
                }
            }
        }
        drop(servers);
        self.changes.send_modify(|epoch| *epoch = epoch.wrapping_add(1));
    }

    pub fn status(&self) -> Status {
        let servers = self.servers.lock().unwrap();
        let preferred = (0..servers.len())
            .find(|&i| self.is_stable(&servers[i]))
            .map(|i| self.config.servers[i].name.clone());
        let servers = servers
            .iter()
            .enumerate()
            .map(|(i, s)| ServerStatus {
                name: self.config.servers[i].name.clone(),
                state: s.state,
                consecutive_failures: s.failures,
                stable: self.is_stable(s),
            })
            .collect();
        Status { preferred, servers }
    }

    pub async fn run(self: Arc<Self>) {
        let mut tasks = JoinSet::new();
        for index in 0..self.config.servers.len() {
            let health = self.clone();
            tasks.spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_millis(health.config.probe_interval_ms));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                let mut probes = JoinSet::new();
                let mut sequence = 0_u64;
                let mut last_result = 0;
                loop {
                    tokio::select! {
                        _ = interval.tick() => {
                            sequence += 1;
                            let sequence = sequence;
                            let health = health.clone();
                            // Overlap probes when timeout exceeds interval. Serial probes would
                            // stretch the default two-failure blackhole detection past four seconds.
                            probes.spawn(async move {
                                let timeout = Duration::from_millis(health.config.probe_timeout_ms);
                                let start = Instant::now();
                                let outcome = match tokio::time::timeout(timeout, health.probe(index)).await {
                                    Ok(Ok(())) => Ok(()),
                                    Ok(Err(error)) => Err(format!("{error:#}")),
                                    Err(_) => Err(format!("no health response within {timeout:?}")),
                                };
                                let elapsed = start.elapsed();
                                if outcome.is_ok() && elapsed > timeout / 2 {
                                    tracing::info!(server = %health.name(index), "slow health probe: {elapsed:?}");
                                }
                                (sequence, outcome)
                            });
                        }
                        Some(result) = probes.join_next(), if !probes.is_empty() => {
                            if let Ok((sequence, outcome)) = result
                                && sequence > last_result {
                                    last_result = sequence;
                                    health.observe(index, outcome);
                            }
                        }
                    }
                }
            });
        }
        while tasks.join_next().await.is_some() {}
    }

    async fn probe(&self, index: usize) -> anyhow::Result<()> {
        let mut sender = connect(&self.config.servers[index], self.config.connect_timeout_ms).await?;
        let uri: http::Uri = self.config.servers[index].base_url.parse()?;
        let request = Request::builder()
            .uri(HEALTH_PATH)
            .header("host", uri.authority().unwrap().as_str())
            .body(full(""))?;
        let response = sender.sender.send_request(request).await?;
        anyhow::ensure!(response.status().is_success(), "health status {}", response.status());
        let bytes = http_body_util::Limited::new(response.into_body(), 64 * 1024)
            .collect()
            .await
            .map_err(anyhow::Error::from_boxed)?
            .to_bytes();
        let report: HealthReport = serde_json::from_slice(&bytes)?;
        anyhow::ensure!(report.state == NodeState::Serving, "not serving: {:?}", report.state);
        Ok(())
    }
}
