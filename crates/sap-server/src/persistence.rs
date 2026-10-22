use crate::node::Node;
use sap_core::{DateTime, Utc, ids::AccountId};
use std::collections::BTreeMap;

impl Node {
    pub(crate) fn record_success(&self, account: &AccountId) -> anyhow::Result<()> {
        let now = Utc::now();
        let bucket = DateTime::from_timestamp(now.timestamp().div_euclid(900) * 900, 0).unwrap_or(now);
        self.store.add_usage(
            account,
            bucket,
            sap_core::thread::ThreadUsage {
                requests: 1,
                ..Default::default()
            },
        )?;
        Ok(())
    }
    pub(crate) fn record_failure(&self, account: &AccountId) -> anyhow::Result<()> {
        let key = format!("failed-requests:{}:{account}", self.config.node);
        let mut buckets: BTreeMap<DateTime<Utc>, u64> = self
            .store
            .get_setting(&key)?
            .map(serde_json::from_value)
            .transpose()?
            .unwrap_or_default();
        let now = Utc::now();
        let bucket = DateTime::from_timestamp(now.timestamp().div_euclid(900) * 900, 0).unwrap_or(now);
        buckets.retain(|start, _| *start >= now - chrono::Duration::hours(3));
        let count = buckets.entry(bucket).or_default();
        *count = count.saturating_add(1);
        // A bounded three-hour map per node/account avoids cross-node counter overwrites.
        self.store.set_setting(&key, serde_json::to_value(buckets)?)?;
        Ok(())
    }
    pub(crate) fn failure_buckets(&self, account: &AccountId) -> BTreeMap<DateTime<Utc>, u64> {
        let mut totals = BTreeMap::<_, u64>::new();
        for node in std::iter::once(&self.config.node).chain(self.config.peers.iter().map(|p| &p.id)) {
            let buckets: BTreeMap<DateTime<Utc>, u64> = self
                .store
                .get_setting(&format!("failed-requests:{node}:{account}"))
                .ok()
                .flatten()
                .and_then(|value| serde_json::from_value(value).ok())
                .unwrap_or_default();
            for (start, count) in buckets {
                if start >= Utc::now() - chrono::Duration::hours(3) {
                    let total = totals.entry(start).or_default();
                    *total = total.saturating_add(count);
                }
            }
        }
        totals
    }
}

impl Node {
    pub(crate) fn reconcile(&self) -> anyhow::Result<()> {
        use std::collections::BTreeSet;
        let records = self.store.threads()?;
        let mut state = self.lock();
        for record in &records {
            if let Some(assignment) = &record.assignment {
                let moved = state
                    .registry
                    .get(&record.key)
                    .and_then(|r| r.assignment.as_ref())
                    .is_some_and(|a| a.account != assignment.account);
                if moved {
                    state.resend.insert(record.key.clone());
                }
                state.registry.assign(&record.key, assignment.clone());
            }
        }
        let stored: BTreeSet<_> = records.iter().map(|r| r.key.clone()).collect();
        let local = state.registry.export();
        if local.iter().any(|r| r.open_streams == 0 && !stored.contains(&r.key)) {
            // Rebuild only for tombstones/retention; preserve live usage and in-flight guards.
            let kept: Vec<_> = local
                .into_iter()
                .filter(|r| r.open_streams > 0 || stored.contains(&r.key))
                .collect();
            let mut registry = sap_threads::Registry::new(self.config.registry_max);
            registry.import(kept.clone());
            for record in kept {
                for _ in 0..record.open_streams {
                    registry.stream_opened(&record.key);
                }
            }
            state.registry = registry;
        }
        state.registry.import(records);
        let retained: BTreeSet<_> = state.registry.export().into_iter().map(|r| r.key).collect();
        state.failures.retain(|k, _| retained.contains(k));
        state.compacted.retain(|k| retained.contains(k));
        state.resend.retain(|k| retained.contains(k));
        state.persisted.retain(|k, _| retained.contains(k));
        for account in self.store.accounts()? {
            for peer in &self.config.peers {
                if let Some(value) = self
                    .store
                    .get_setting(&format!("account-runtime:{}:{}", peer.id, account.id))?
                {
                    let remote: crate::node::AccountState = serde_json::from_value(value)?;
                    let local = state.accounts.entry(account.id.clone()).or_default();
                    if remote.quota_at > local.quota_at {
                        local.quota_at = remote.quota_at;
                        local.exhausted = remote.exhausted;
                        local.exhausted_window = remote.exhausted_window;
                    }
                }
            }
        }
        Ok(())
    }
}
