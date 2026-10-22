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
    down: watch::Sender<u64>,
}

pub struct Health {
    config: Arc<Config>,
    servers: Mutex<Vec<ServerHealth>>,
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
                down: watch::channel(0).0,
            })
            .collect();
        Arc::new(Self {
            config,
            servers: Mutex::new(servers),
        })
    }

    pub fn shutdown(&self) {
        for server in self.servers.lock().unwrap().iter_mut() {
            server
                .down
                .send_modify(|generation| *generation = generation.wrapping_add(1));
        }
    }

    pub fn candidates(&self) -> Vec<usize> {
        let servers = self.servers.lock().unwrap();
        let stable = Duration::from_secs(self.config.stable_secs);
        servers
            .iter()
            .enumerate()
            .filter(|(_, server)| {
                server.state == State::Healthy
                    && (!server.recovered || server.healthy_since.is_some_and(|since| since.elapsed() >= stable))
            })
            .map(|(index, _)| index)
            .collect()
    }

    pub fn subscribe(&self, index: usize) -> Option<(watch::Receiver<u64>, u64)> {
        let servers = self.servers.lock().unwrap();
        if servers[index].state != State::Healthy {
            return None;
        }
        let receiver = servers[index].down.subscribe();
        let generation = *receiver.borrow();
        Some((receiver, generation))
    }

    pub fn observe(&self, index: usize, success: bool) {
        let mut servers = self.servers.lock().unwrap();
        let server = &mut servers[index];
        if success {
            if server.healthy_since.is_none() {
                server.healthy_since = Some(Instant::now());
            }
            server.failures = 0;
            server.state = State::Healthy;
        } else {
            server.recovered = true;
            server.healthy_since = None;
            server.failures = server.failures.saturating_add(1);
            if server.failures >= self.config.down_after_failures {
                if server.state != State::Down {
                    server
                        .down
                        .send_modify(|generation| *generation = generation.wrapping_add(1));
                }
                server.state = State::Down;
            } else {
                server.state = State::Suspect;
            }
        }
    }

    pub fn status(&self) -> Status {
        let preferred = self.candidates().first().map(|&i| self.config.servers[i].name.clone());
        let servers = self
            .servers
            .lock()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, s)| ServerStatus {
                name: self.config.servers[i].name.clone(),
                state: s.state,
                consecutive_failures: s.failures,
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
                                let success = tokio::time::timeout(
                                    Duration::from_millis(health.config.probe_timeout_ms),
                                    health.probe(index),
                                ).await.is_ok_and(|result| result.is_ok());
                                (sequence, success)
                            });
                        }
                        Some(result) = probes.join_next(), if !probes.is_empty() => {
                            if let Ok((sequence, success)) = result
                                && sequence > last_result {
                                    last_result = sequence;
                                    health.observe(index, success);
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
        anyhow::ensure!(response.status().is_success(), "health status");
        let bytes = http_body_util::Limited::new(response.into_body(), 64 * 1024)
            .collect()
            .await
            .map_err(anyhow::Error::from_boxed)?
            .to_bytes();
        let report: HealthReport = serde_json::from_slice(&bytes)?;
        anyhow::ensure!(report.state == NodeState::Serving, "not serving");
        Ok(())
    }
}
