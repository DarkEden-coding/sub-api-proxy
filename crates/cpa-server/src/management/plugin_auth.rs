//! The plugin host's auth manager (Go `pluginHost.SetAuthManager(authManager)` in
//! internal/api/server.go): `host.auth.*` read the credential store, saves go through
//! the auth-file pipeline uploads use, and `host.affinity.lookup` answers
//! `unsupported` (Go `Manager.LookupSessionAffinity`).
//!
//! ponytail: Go's raw `Auth.Status` is approximated from the store as the auth-files
//! view does: `disabled`, `error` while a credential-wide cooldown runs, else
//! `active`; created and updated times are the file's modification time.
//! ponytail: an overwrite that changes a credential's type gets that type's auth
//! index at once; Go's `Update` keeps the old index until the next restart.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, TryLockError, Weak};
use std::time::SystemTime;

use cpa_core::config::credentials;
use cpa_core::credential::Credential;
use cpa_plugin::gojson::GoTime;
use cpa_plugin::hostauth::{AuthManager, HostAuth};
use serde_json::Value;

use super::Management;
use super::auth_files::{auth_kind, file_name, parse_time, path_of, recent_requests, status_message};

/// Gives the runtime's plugin host this management state as its auth manager.
pub(crate) fn attach(state: &Arc<Management>) {
    state
        .rt
        .plugins()
        .set_auth_manager(Some(Arc::new(PluginAuthManager(Arc::downgrade(state)))));
}

/// Held weakly: the runtime owns the plugin host, and management owns the runtime.
struct PluginAuthManager(Weak<Management>);

impl AuthManager for PluginAuthManager {
    fn list(&self) -> Vec<HostAuth> {
        let Some(state) = self.0.upgrade() else {
            return Vec::new();
        };
        state
            .rt
            .store()
            .snapshot()
            .iter()
            .map(|c| host_auth(&state, c))
            .collect()
    }

    fn get_by_id(&self, id: &str) -> Option<HostAuth> {
        let state = self.0.upgrade()?;
        state.rt.store().get(id).map(|c| host_auth(&state, &c))
    }

    /// Under the disk lock (the auth-file writes and reloads lock): the host writes the
    /// file, the file store rewrites it in its canonical form, and the credential is
    /// registered or replaced alone, keeping its runtime state by ID. Nothing else is
    /// reloaded and the config is not republished.
    fn save(&self, auth: HostAuth, write: &mut dyn FnMut() -> Result<(), String>) -> Result<(), String> {
        let Some(state) = self.0.upgrade() else {
            return write();
        };
        // A plugin may save while it parses an auth file for a scan that holds the disk
        // lock and waits for it; Go's synthesizer holds no lock the save needs. While a
        // parse runs, a busy lock is that scan's, so the save goes ahead without it.
        // ponytail: the scan's freshness check (`reconcile_scanned`) keeps a file
        // written here over the scan's older read, and the watcher reloads it; a save
        // racing a different scan's parse relies on the same check.
        let _disk = match state.disk.try_lock() {
            Ok(guard) => Some(guard),
            Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) if credentials::plugin_auth_parsing() => None,
            Err(TryLockError::WouldBlock) => Some(state.disk.lock().unwrap_or_else(PoisonError::into_inner)),
        };
        write()?;
        let path = PathBuf::from(auth.attributes.get("path").cloned().unwrap_or_default());
        let bytes = persist(&path, &auth)?;
        let cfg = state.rt.config();
        let credential = match credentials::from_file(&cfg, &cfg.auth_dir, &path, &bytes) {
            Ok(Some(c)) => {
                state
                    .fallbacks
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&path);
                c
            }
            // Go registers what its synthesizer would skip; uploads keep such files the
            // same way (see `credentials::upload_fallback`).
            Ok(None) => match credentials::upload_fallback(&cfg.auth_dir, &path, &bytes) {
                Some(c) => {
                    state
                        .fallbacks
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .insert(path.clone());
                    c
                }
                None => return Ok(()),
            },
            Err(e) => return Err(format!("register auth: {e}")),
        };
        state.rt.store().upsert(credential);
        Ok(())
    }

    /// Go `LookupSessionAffinity`: `unsupported` whenever a plugin scheduler is
    /// attached, and Go's server always attaches the plugin host
    /// (sdk/cliproxy/builder.go:240-246, :272-273; conductor_selection.go:471-474).
    fn lookup_session_affinity(&self, _: &str, _: &str, _: &str) -> (String, Option<HostAuth>) {
        ("unsupported".into(), None)
    }
}

/// Go `FileTokenStore.Save` for a saved auth: the metadata with the auth's `disabled`
/// flag, marshalled, replacing the file unless it already holds the same JSON.
/// Returns what the file holds.
fn persist(path: &Path, auth: &HostAuth) -> Result<Vec<u8>, String> {
    let mut meta: BTreeMap<String, Value> = auth.metadata.clone().into_iter().collect();
    meta.insert("disabled".into(), auth.disabled.into());
    let raw = cpa_plugin::gojson::to_vec(&meta);
    let existing = std::fs::read(path).map_err(|e| format!("auth filestore: read existing failed: {e}"))?;
    if json_equal(&existing, &raw) {
        return Ok(existing);
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|e| format!("auth filestore: open existing failed: {e}"))?;
    file.write_all(&raw)
        .map_err(|e| format!("auth filestore: write existing failed: {e}"))?;
    Ok(raw)
}

/// Go `jsonEqual`: both decode, and the decoded values are equal (numbers as float64).
fn json_equal(a: &[u8], b: &[u8]) -> bool {
    fn same(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
            (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(x, y)| same(x, y)),
            (Value::Object(x), Value::Object(y)) => {
                x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
            }
            _ => a == b,
        }
    }
    match (serde_json::from_slice::<Value>(a), serde_json::from_slice::<Value>(b)) {
        (Ok(a), Ok(b)) => same(&a, &b),
        _ => false,
    }
}

fn go_time(t: Option<SystemTime>) -> GoTime {
    GoTime(t.map(|t| chrono::DateTime::<chrono::Local>::from(t).fixed_offset()))
}

/// A stored credential as Go's `coreauth.Auth` presents it to the plugin host.
fn host_auth(state: &Management, c: &Credential) -> HostAuth {
    let store = state.rt.store();
    let cooldown = store
        .cooldowns(&c.id)
        .into_iter()
        .find(|cd| cd.model.is_empty() && !cd.remaining.is_zero());
    let (status, unavailable) = if c.disabled {
        ("disabled", false)
    } else if cooldown.is_some() {
        ("error", true)
    } else {
        ("active", false)
    };
    let mut attributes = c.attributes.clone();
    let path = path_of(c);
    if let Some(path) = path {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
        let path = path.display().to_string();
        attributes.entry("path".into()).or_insert_with(|| path.clone());
        attributes.entry("source".into()).or_insert(path);
    }
    let activity = store.activity(&c.id);
    let recent = match recent_requests(&activity) {
        Value::Array(buckets) => buckets
            .iter()
            .map(|b| {
                (
                    b["time"].as_str().unwrap_or_default().to_owned(),
                    b["success"].as_i64().unwrap_or_default(),
                    b["failed"].as_i64().unwrap_or_default(),
                )
            })
            .collect(),
        _ => Vec::new(),
    };
    let modified = path
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok());
    let last_refresh = ["last_refresh", "lastRefresh", "last_refreshed_at", "lastRefreshedAt"]
        .iter()
        .find_map(|key| c.metadata.get(*key).and_then(parse_time));
    let email = c
        .metadata
        .get("email")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    let (account_type, account) = match auth_kind(c) {
        Some("oauth") => ("oauth", email.to_owned()),
        Some(_) => (
            "api_key",
            c.attributes
                .get("api_key")
                .map(|k| k.trim().to_owned())
                .unwrap_or_default(),
        ),
        None => ("", String::new()),
    };
    HostAuth {
        id: c.id.clone(),
        index: credentials::auth_index(c),
        provider: c.provider.clone(),
        file_name: file_name(c).unwrap_or_default(),
        label: c.label.clone(),
        status: status.into(),
        status_message: status_message(state, c).into(),
        disabled: c.disabled,
        unavailable,
        attributes,
        metadata: c.metadata.clone(),
        success: activity.success as i64,
        failed: activity.failed as i64,
        recent_requests: recent,
        created_at: go_time(modified),
        updated_at: go_time(modified),
        last_refreshed_at: go_time(last_refresh),
        next_retry_after: go_time(cooldown.map(|cd| SystemTime::now() + cd.remaining)),
        account_type: account_type.into(),
        account,
    }
}
