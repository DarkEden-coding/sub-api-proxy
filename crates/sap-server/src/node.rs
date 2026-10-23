use crate::{config::Config, control::ControlEvent};
use anyhow::Context;
use cpa_server::Runtime;
use sap_core::{
    DateTime, Utc,
    account::AccountRuntime,
    ids::{AccountId, ThreadKey},
    routing::{Outcome, RoutingEvent, RoutingInput},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct AccountState {
    pub exhausted: Option<Option<DateTime<Utc>>>,
    #[serde(default)]
    pub exhausted_window: Option<sap_core::quota::WindowKind>,
    #[serde(default)]
    pub quota_at: Option<DateTime<Utc>>,
    pub cooling: Option<DateTime<Utc>>,
    pub revoked: bool,
}
pub(crate) struct State {
    pub registry: sap_threads::Registry,
    pub streams: BTreeMap<ThreadKey, u32>,
    pub accounts: BTreeMap<AccountId, AccountState>,
    pub failures: BTreeMap<ThreadKey, (Outcome, DateTime<Utc>)>,
    pub compacted: BTreeSet<ThreadKey>,
    pub resend: BTreeSet<ThreadKey>,
    pub persisted: BTreeMap<ThreadKey, DateTime<Utc>>,
    pub revisions: BTreeMap<String, u64>,
    pub upstream_at: BTreeMap<sap_core::ids::Provider, DateTime<Utc>>,
}
pub struct Node {
    pub(crate) self_weak: Weak<Node>,
    pub(crate) refresh: Mutex<crate::reset_aware::Refresh>,
    pub(crate) config: Config,
    pub(crate) telemetry: Mutex<crate::telemetry::Metrics>,
    pub(crate) management: Mutex<Option<(Weak<cpa_server::management::Management>, String)>>,
    pub(crate) runtime: Weak<Runtime>,
    pub(crate) store: Arc<sap_store::Store>,
    pub(crate) state: Arc<Mutex<State>>,
    pub(crate) notify: Arc<tokio::sync::Notify>,
    pub(crate) changes: tokio::sync::broadcast::Sender<ControlEvent>,
    pub(crate) started: DateTime<Utc>,
    pub(crate) agent: sap_sync::SyncAgent,
    pub(crate) sync_routes: axum::Router,
    /// The management API and dashboard, built once management is attached.
    pub(crate) api_routes: std::sync::OnceLock<axum::Router>,
}
impl Node {
    /// Attach the existing management instance and its loopback-only password.
    /// Also builds the management API, which reuses this instance's key check. Call it
    /// before the router is built so `SapHooks::routes` can mount the API.
    pub fn attach_management(self: &Arc<Self>, management: &Arc<cpa_server::management::Management>, password: String) {
        *self
            .management
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((Arc::downgrade(management), password));
        let control: Arc<dyn crate::control::Control> = self.clone();
        let _ = self.api_routes.set(crate::api::router(
            control,
            crate::api::ManagementAuth::new(management.clone()),
        ));
    }

    pub(crate) fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    pub(crate) fn persist_account(&self, state: &State, account: &AccountId) -> anyhow::Result<()> {
        if let Some(local) = state.accounts.get(account) {
            self.store.set_setting(
                &format!("account-runtime:{}:{account}", self.config.node),
                serde_json::to_value(local)?,
            )?;
            self.changed();
        }
        Ok(())
    }
    pub(crate) fn changed(&self) {
        self.notify.notify_waiters();
        let _ = self.changes.send(ControlEvent::Changed);
    }
    pub(crate) fn persist(&self, state: &mut State, key: &ThreadKey, force: bool) -> anyhow::Result<()> {
        let now = Utc::now();
        if (force
            || state
                .persisted
                .get(key)
                .is_none_or(|t| now.signed_duration_since(*t).num_seconds() >= 60))
            && let Some(record) = state.registry.get(key)
        {
            self.store.put_thread(record.clone())?;
            state.persisted.insert(key.clone(), now);
            self.changed();
        }
        Ok(())
    }
    pub(crate) fn input(
        &self,
        state: &State,
        provider: sap_core::ids::Provider,
        model: Option<String>,
        event: RoutingEvent,
        available: &BTreeSet<AccountId>,
    ) -> anyhow::Result<RoutingInput> {
        let now = Utc::now();
        let active = state
            .registry
            .active(now, Duration::from_secs(self.config.idle_window_secs));
        let accounts = self
            .store
            .accounts()?
            .into_iter()
            .filter(|a| a.provider == provider)
            .map(|account| {
                let local = state.accounts.get(&account.id);
                Ok(AccountRuntime {
                    windows: self.store.latest_windows(&account.id)?,
                    active_threads: active
                        .iter()
                        .filter(|t| t.assignment.as_ref().is_some_and(|a| a.account == account.id))
                        .count() as u32,
                    active_streams: active
                        .iter()
                        .filter(|t| t.assignment.as_ref().is_some_and(|a| a.account == account.id))
                        .map(|t| state.streams.get(&t.key).copied().unwrap_or_default())
                        .sum(),
                    exhausted_until: local.and_then(|a| a.exhausted).filter(|t| t.is_none_or(|t| t > now)),
                    cooling_until: local.and_then(|a| a.cooling).filter(|t| *t > now),
                    revoked: local.is_some_and(|a| a.revoked),
                    credential_usable: available.contains(&account.id),
                    account,
                })
            })
            .collect::<Result<Vec<_>, sap_store::Error>>()?;
        Ok(RoutingInput {
            now,
            provider,
            model,
            event,
            accounts,
            pick: uniform_sample(),
        })
    }
    pub(crate) fn event(
        &self,
        kind: sap_core::api::EventKind,
        key: Option<ThreadKey>,
        account: Option<AccountId>,
        message: &str,
    ) {
        let event = sap_core::api::EventView {
            id: format!(
                "{}-{}",
                self.config.node,
                Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ),
            at: Utc::now(),
            kind,
            message: message.into(),
            node: self.config.node.clone(),
            thread: key,
            account,
        };
        if self.store.append_event(event.clone()).is_ok() {
            let _ = self.changes.send(ControlEvent::Event(event));
            self.changed();
        }
    }
}

/// Uniform sample in `[0, 1)` for weighted account choice. Each `RandomState` is
/// randomly keyed, which spreads load well enough without a random-number dependency.
fn uniform_sample() -> f64 {
    use std::hash::BuildHasher;
    let bits = std::collections::hash_map::RandomState::new().hash_one(0u8);
    (bits >> 11) as f64 / (1u64 << 53) as f64
}

pub fn install(rt: &Arc<Runtime>, path: &std::path::Path) -> anyhow::Result<Arc<Node>> {
    let config = Config::load(path).context("loading SAP configuration")?;
    let store = Arc::new(
        sap_store::Store::open(&config.store_path, config.node.clone(), config.limits.store())
            .context("opening SAP store")?,
    );
    let notify = Arc::new(tokio::sync::Notify::new());
    let mut secrets = BTreeMap::new();
    let mut peers = Vec::new();
    for p in &config.peers {
        // Keep unacknowledged history for configured peers, including peers not yet seen.
        store.set_peer_cursor(p.id.clone(), &Default::default())?;
        let secret = std::fs::read(&p.secret_file).context("reading peer secret file")?;
        peers.push(sap_sync::Peer::new(p.id.clone(), &p.url, secret.clone()).map_err(anyhow::Error::msg)?);
        secrets.insert(p.id.clone(), secret);
    }
    let telemetry_node = Arc::new(std::sync::OnceLock::<Weak<Node>>::new());
    let source = telemetry_node.clone();
    let server = sap_sync::ServerState::new(store.clone(), secrets, notify.clone())
        .with_telemetry(move || source.get()?.upgrade().map(|node| node.telemetry()));
    let server = if config.allow_loopback_peers {
        server.allow_loopback()
    } else {
        server
    };
    let sync_routes = sap_sync::router(Arc::new(server));
    let agent = sap_sync::SyncAgent::spawn(store.clone(), peers, notify.clone());
    let mut registry = sap_threads::Registry::new(config.registry_max);
    registry.import(store.threads()?);
    let mut accounts = BTreeMap::new();
    for account in store.accounts()? {
        if let Some(value) = store.get_setting(&format!("account-runtime:{}:{}", config.node, account.id))? {
            accounts.insert(account.id, serde_json::from_value(value)?);
        }
    }
    let node = Arc::new_cyclic(|weak| Node {
        self_weak: weak.clone(),
        refresh: Mutex::default(),
        config,
        telemetry: Mutex::default(),
        management: Mutex::new(None),
        runtime: Arc::downgrade(rt),
        store,
        notify,
        agent,
        sync_routes,
        api_routes: std::sync::OnceLock::new(),
        state: Arc::new(Mutex::new(State {
            registry,
            streams: BTreeMap::new(),
            accounts,
            revisions: BTreeMap::new(),
            upstream_at: BTreeMap::new(),
            failures: BTreeMap::new(),
            compacted: BTreeSet::new(),
            resend: BTreeSet::new(),
            persisted: BTreeMap::new(),
        })),
        changes: tokio::sync::broadcast::channel(256).0,
        started: Utc::now(),
    });
    let _ = telemetry_node.set(Arc::downgrade(&node));
    node.sample_rss();
    node.discover()?;
    rt.set_sap_hooks(Arc::new(crate::telemetry::Instrumented(node.clone())));
    let weak = Arc::downgrade(&node);
    let wake = node.notify.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        let mut compact_at = std::time::Instant::now() + Duration::from_secs(60);
        let mut rss_at = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            tokio::select! { _ = interval.tick() => {}, _ = wake.notified() => {} }
            let Some(node) = weak.upgrade() else { break };
            if std::time::Instant::now() >= rss_at {
                node.sample_rss();
                rss_at = std::time::Instant::now() + Duration::from_secs(1);
            }
            if node.reconcile().is_err() {
                tracing::warn!("SAP state reconciliation failed");
            }
            if node.discover().is_err() {
                tracing::warn!("SAP credential discovery failed");
            }
            if std::time::Instant::now() >= compact_at {
                // Compaction scans the op log and may VACUUM: keep it off the async workers.
                let store = node.store.clone();
                if !matches!(tokio::task::spawn_blocking(move || store.compact()).await, Ok(Ok(_))) {
                    tracing::warn!("SAP store compaction failed");
                }
                compact_at = std::time::Instant::now() + Duration::from_secs(60);
            }
            let _ = node.changes.send(ControlEvent::Changed);
        }
    });
    Ok(node)
}
