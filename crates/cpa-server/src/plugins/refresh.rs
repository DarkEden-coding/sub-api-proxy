//! Refresh of credentials that a plugin auth provider refreshes (Go
//! `executorAdapter.Refresh` and `pluginRefreshCompatExecutor.Refresh`). Built-in
//! providers never take this path: their executors refresh them as before.

use chrono::{DateTime, Utc};
use cpa_common::gostr::GoStr;
use cpa_core::config::Config;
use cpa_core::credential::{Credential, MetadataPatch};
use cpa_core::exec::{ExecError, FailureScope};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::Runtime;

/// Which Go executor refreshes a credential.
pub(crate) enum Route {
    /// A plugin executor serves it (Go `executorAdapter.Refresh`).
    Executor,
    /// Go's OpenAI-compatible executor wrapped by `pluginRefreshCompatExecutor`;
    /// `identifier` is the wrapped executor's provider key.
    Compat { identifier: String },
}

/// The plugin refresh route of `c`, or `None` when its executor refreshes it.
///
/// Go binds a plugin executor, or registers `pluginRefreshCompatExecutor`, only for
/// providers outside its built-in executor table (`registerExecutorForAuth`): built-in
/// providers return `None` here, compat-named ones included (see
/// docs/DIFFERENCES-FROM-GO.md). For any other provider, a credential a plugin executor
/// serves takes [`Route::Executor`]; the rest are bound to the OpenAI-compatible
/// executor, which Go wraps when a plugin auth provider matches the provider, the
/// `provider_key` or `compat_name` attribute, or the executor's own key
/// (`wrapOpenAICompatIfPluginAuth`). Go asks Home first when Home is enabled
/// (`RefreshAuthViaHome`); that is not ported, so Home mode keeps the compat executor's
/// refresh off.
pub(crate) fn route(rt: &Runtime, c: &Credential, cfg: &Config) -> Option<Route> {
    let provider = c.provider.trim().go_lower();
    if super::BUILTIN.contains(&provider.as_str()) {
        return None;
    }
    if rt.plugin_runtime().executor(c, cfg).is_some() {
        return Some(Route::Executor);
    }
    // Home mode installs the remote dispatcher; the config may not carry `home.enabled`.
    let home = rt.remote_dispatch().is_some()
        || cfg
            .document
            .get("home")
            .and_then(|h| h.get("enabled"))
            .and_then(serde_yaml_ng::Value::as_bool)
            .unwrap_or(false);
    if home {
        return None;
    }
    let attr = |k: &str| c.attributes.get(k).map_or("", |v| v.trim());
    // Go `openAICompatInfoFromAuth`, else the provider (`registerExecutorForAuth`).
    let identifier = if !attr("compat_name").is_empty() {
        let key = if attr("provider_key").is_empty() {
            attr("compat_name")
        } else {
            attr("provider_key")
        };
        cpa_core::config::credentials::openai_compat_provider(key)
    } else if provider == "openai-compatibility" {
        cpa_core::config::credentials::openai_compat_provider(&c.label)
    } else if provider.is_empty() {
        "openai-compatibility".to_owned()
    } else {
        provider.clone()
    };
    let host = rt.plugins();
    [
        provider.as_str(),
        attr("provider_key"),
        attr("compat_name"),
        identifier.as_str(),
    ]
    .iter()
    .any(|key| !key.is_empty() && host.has_auth_provider(key))
    .then_some(Route::Compat { identifier })
}

/// When `c` comes due for a plugin refresh: Go `shouldRefresh` for a provider with no
/// refresh lead, so only a `refresh_interval` schedules one. The last refresh is the
/// one this process committed for the credential's current revision (Go
/// `LastRefreshedAt`), else the credential's own timestamp; without either it is due
/// at once. `None` when it never comes due or no plugin refreshes it.
pub(crate) fn due_at(rt: &Runtime, c: &Credential, cfg: &Config) -> Option<DateTime<Utc>> {
    let interval = cpa_exec::preferred_interval(c)?;
    route(rt, c, cfg)?;
    let refreshed = rt
        .plugin_runtime()
        .refreshed
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&c.id)
        .filter(|(revision, _)| *revision == c.revision)
        .map(|(_, at)| *at);
    let by_last = refreshed
        .or_else(|| cpa_exec::last_refresh(c))
        .map_or(DateTime::<Utc>::MIN_UTC, |last| {
            last.checked_add_signed(interval).unwrap_or(DateTime::<Utc>::MAX_UTC)
        });
    let by_expiry =
        cpa_exec::expiration(c).map(|expiry| expiry.checked_sub_signed(interval).unwrap_or(DateTime::<Utc>::MIN_UTC));
    Some(by_expiry.map_or(by_last, |at| at.min(by_last)))
}

/// Credential ID to the revision a plugin refresh committed and when.
pub(crate) type Refreshed = Arc<Mutex<HashMap<String, (u64, DateTime<Utc>)>>>;

/// Where [`committed`] records plugin refreshes; cloned into the commit, which a
/// cancelled caller cannot stop.
pub(crate) fn refreshed(rt: &Runtime) -> Refreshed {
    rt.plugin_runtime().refreshed.clone()
}

/// Records a committed plugin refresh of revision `revision` (Go `LastRefreshedAt`).
pub(crate) fn committed(refreshed: &Refreshed, id: &str, revision: u64, at: DateTime<Utc>) {
    refreshed
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(id.to_owned(), (revision, at));
}

/// Refreshes `c` through its plugin and returns the metadata change to commit.
// ponytail: only the credential's JSON document is committed (a file-backed one; the
// commit refuses config-backed credentials, as for built-in refreshes); label, prefix,
// proxy URL, attribute and provider changes wait for the next reload of the file. The
// refresh time lasts for the committed revision only, so a later edit falls back to the
// document's `last_refresh`; Go keeps `LastRefreshedAt` until re-registration.
pub(crate) async fn refresh(rt: &Runtime, route: &Route, c: &Credential) -> Result<MetadataPatch, ExecError> {
    let metadata: cpa_plugin::gojson::Metadata = c.metadata.clone().into_iter().collect();
    let view = cpa_plugin::auth::AuthView {
        id: c.id.clone(),
        provider: c.provider.clone(),
        file_name: c
            .attributes
            .get("path")
            .and_then(|p| std::path::Path::new(p).file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        label: c.label.clone(),
        prefix: c.attributes.get("prefix").cloned().unwrap_or_default(),
        proxy_url: c.attributes.get("proxy_url").cloned().unwrap_or_default(),
        // Go `storageJSONFromAuth`: the credential's JSON document.
        storage_json: bytes::Bytes::from(cpa_plugin::gojson::to_vec(&metadata)),
        metadata,
        attributes: crate::management::quota::go_attributes(c),
        ..Default::default()
    };
    let refreshed = rt
        .plugins()
        .refresh_auth(&view, &Default::default())
        .await
        .map_err(|e| {
            let status = match e.status() {
                0 => 500,
                status => status,
            };
            ExecError::local(status, FailureScope::Credential, e.to_string())
        })?;
    let Some(refreshed) = refreshed else {
        // No plugin answers for the credential's provider.
        return match route {
            Route::Executor => Ok(MetadataPatch::default()),
            Route::Compat { identifier } => {
                let filled = |k: &str| c.str(k).is_some_and(|v| !v.trim().is_empty());
                if filled("refresh_token") || filled("refreshToken") {
                    Err(ExecError::local(
                        500,
                        FailureScope::Credential,
                        format!("plugin auth provider refresh is unavailable for provider {identifier}"),
                    ))
                } else {
                    Ok(MetadataPatch::default())
                }
            }
        };
    };
    // What Go's `pluginTokenStorage` saves: the storage overlaid with the metadata. Go
    // fails to persist storage that does not decode, and the refresh fails with it.
    let next: Map<String, Value> = refreshed
        .storage_payload()
        .and_then(|payload| serde_json::from_slice(&payload).map_err(|e| e.to_string()))
        .map_err(|e| ExecError::local(500, FailureScope::Credential, format!("persist refreshed auth: {e}")))?;
    Ok(diff(&c.metadata, &next))
}

/// The patch turning `current` into `next`. `type` stays (a refresh cannot move a
/// credential to another provider here) and the store writes `disabled` itself.
fn diff(current: &Map<String, Value>, next: &Map<String, Value>) -> MetadataPatch {
    let kept = |k: &str| k != "type" && k != "disabled";
    MetadataPatch {
        set: next
            .iter()
            .filter(|(k, v)| kept(k) && current.get(*k) != Some(v))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        remove: current
            .keys()
            .filter(|k| kept(k) && !next.contains_key(*k))
            .cloned()
            .collect(),
    }
}
