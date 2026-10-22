use crate::node::Node;
use cpa_core::credential::{Credential, Source};
use sap_core::{
    Utc,
    account::{Account, Capacity, CapacitySource, NodeCredential},
    ids::{AccountId, Provider},
};
use std::collections::BTreeMap;

pub(crate) fn identity(c: &Credential) -> Option<(AccountId, Provider)> {
    let (p, keys): (_, &[&str]) = match c.provider.as_str() {
        "claude" => (Provider::Claude, &["account_uuid", "email"]),
        "codex" => (Provider::Codex, &["account_id"]),
        _ => return None,
    };
    let id = keys.iter().find_map(|k| c.str(k).filter(|s| !s.trim().is_empty()))?;
    Some((AccountId(format!("{p}:{id}")), p))
}
impl Node {
    pub(crate) fn discover(&self) -> anyhow::Result<BTreeMap<String, AccountId>> {
        let Some(rt) = self.runtime.upgrade() else {
            return Ok(BTreeMap::new());
        };
        let mut mapping = BTreeMap::new();
        let existing = self.store.accounts()?;
        let prior_credentials = self.store.credentials()?;
        for c in rt.store().snapshot().iter() {
            let Some((id, provider)) = identity(c) else { continue };
            let last_refresh = c
                .str("last_refresh")
                .and_then(|t| sap_core::DateTime::parse_from_rfc3339(t).ok())
                .map(|t| t.with_timezone(&Utc));
            {
                let mut state = self.lock();
                let changed = state
                    .revisions
                    .insert(c.id.clone(), c.revision)
                    .is_some_and(|old| old != c.revision);
                let refreshed = prior_credentials
                    .iter()
                    .find(|credential| credential.account == id)
                    .is_some_and(|credential| last_refresh > credential.last_refresh);
                if (changed || refreshed)
                    && !c.disabled
                    && let Some(local) = state.accounts.get_mut(&id)
                {
                    local.revoked = false;
                    self.persist_account(&state, &id)?;
                }
            }
            if !c.disabled {
                mapping.insert(c.id.clone(), id.clone());
            }
            if !existing.iter().any(|a| a.id == id) {
                let plan = ["plan_type", "subscription_type", "rate_limit_tier"]
                    .iter()
                    .find_map(|k| c.str(k).or_else(|| c.attributes.get(*k).map(String::as_str)))
                    .map(str::to_owned);
                let capacity = plan
                    .as_deref()
                    .and_then(|p| sap_routing::plan_capacity(p, &self.config.routing))
                    .map(|multiplier| Capacity {
                        multiplier,
                        source: CapacitySource::Plan,
                    })
                    .unwrap_or_default();
                self.store.upsert_account(Account {
                    id: id.clone(),
                    provider,
                    label: c.label.clone(),
                    email: c.str("email").map(str::to_owned),
                    plan,
                    capacity,
                    priority: 0,
                    disabled: false,
                    models: None,
                    created_at: Utc::now(),
                })?;
                self.changed();
            }
            let credential_file = match &c.source {
                Source::File(p) => p.to_string_lossy().into_owned(),
                _ => c.id.clone(),
            };
            let credential = NodeCredential {
                account: id,
                node: self.config.node.clone(),
                credential_file,
                last_refresh,
                last_error: c.disabled.then(|| "credential disabled".into()),
            };
            if !prior_credentials.contains(&credential) {
                self.store.put_credential(credential)?;
            }
        }
        Ok(mapping)
    }
}
