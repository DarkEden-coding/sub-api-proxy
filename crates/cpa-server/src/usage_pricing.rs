//! API-equivalent prices for the usage history (additive; not in Go). A small built-in
//! table is the floor; a copy of LiteLLM's public price list, refreshed at most once a day
//! and cached on disk, overrides it. Prices only turn token counts into a dollar estimate
//! of what the same traffic would cost on pay-per-token API billing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PRICE_LIST_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
const REFRESH_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
const RETRY_AFTER_MS: i64 = 10 * 60 * 1000;
const MAX_LIST_BYTES: usize = 32 * 1024 * 1024;

/// USD per token.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rate {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

/// Built-in floor, USD per million tokens: input, output, cache read, cache write (5 minute).
/// ponytail: only the models in use at the time of writing; the online list covers the rest,
/// and a model in neither shows as unpriced.
const BUILT_IN: &[(&str, [f64; 4])] = &[
    ("claude-fable-5-1", [10.0, 50.0, 0.25, 12.5]),
    ("claude-fable-5", [10.0, 50.0, 0.25, 12.5]),
    ("claude-opus-5-5", [4.0, 20.0, 0.20, 5.0]),
    ("claude-opus-5", [5.0, 25.0, 0.50, 6.25]),
    ("claude-opus-4-8", [5.0, 25.0, 0.50, 6.25]),
    ("claude-opus-4-7", [5.0, 25.0, 0.50, 6.25]),
    ("claude-opus-4-6", [5.0, 25.0, 0.50, 6.25]),
    ("claude-sonnet-5-5", [2.0, 10.0, 0.20, 2.5]),
    ("claude-sonnet-5", [2.0, 10.0, 0.20, 2.5]),
    ("claude-sonnet-4-6", [3.0, 15.0, 0.30, 3.75]),
    ("claude-haiku-5-5", [0.10, 0.50, 0.01, 0.125]),
    ("claude-haiku-4-5", [1.0, 5.0, 0.10, 1.25]),
    ("gpt-5", [1.25, 10.0, 0.125, 1.25]),
    ("gpt-5-codex", [1.25, 10.0, 0.125, 1.25]),
];

type Table = HashMap<String, Rate>;

#[derive(Default, Serialize, Deserialize)]
struct Cached {
    fetched_at_ms: i64,
    rates: Table,
}

struct State {
    table: Arc<Table>,
    /// Unix milliseconds of the online list in `table`, if one has been loaded.
    online_at_ms: Option<i64>,
}

pub struct Pricing {
    state: RwLock<State>,
    built_in: Arc<Table>,
    cache_path: Mutex<Option<PathBuf>>,
    refreshing: AtomicBool,
    last_attempt_ms: AtomicI64,
}

impl Default for Pricing {
    fn default() -> Self {
        let built_in: Arc<Table> = Arc::new(
            BUILT_IN
                .iter()
                .map(|(name, [i, o, r, w])| {
                    let per = |v: &f64| v / 1_000_000.0;
                    (
                        (*name).to_owned(),
                        Rate {
                            input: per(i),
                            output: per(o),
                            cache_read: per(r),
                            cache_write: per(w),
                        },
                    )
                })
                .collect(),
        );
        Self {
            state: RwLock::new(State {
                table: built_in.clone(),
                online_at_ms: None,
            }),
            built_in,
            cache_path: Mutex::default(),
            refreshing: AtomicBool::new(false),
            last_attempt_ms: AtomicI64::new(0),
        }
    }
}

impl Pricing {
    /// Loads the on-disk copy of the last online list, if any.
    pub fn with_cache(path: PathBuf) -> Self {
        let pricing = Self::default();
        if let Some(cached) = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Cached>(&b).ok())
        {
            pricing.install(cached.rates, cached.fetched_at_ms);
        }
        *pricing.cache_path.lock().unwrap_or_else(PoisonError::into_inner) = Some(path);
        pricing
    }

    fn install(&self, online: Table, fetched_at_ms: i64) {
        let mut merged = (*self.built_in).clone();
        merged.extend(online);
        let mut state = self.state.write().unwrap_or_else(PoisonError::into_inner);
        state.table = Arc::new(merged);
        state.online_at_ms = Some(fetched_at_ms);
    }

    /// The current table, so many rows are priced without taking the lock for each.
    pub fn snapshot(&self) -> Snapshot {
        let state = self.state.read().unwrap_or_else(PoisonError::into_inner);
        Snapshot(state.table.clone())
    }

    pub fn online_at_ms(&self) -> Option<i64> {
        self.state.read().unwrap_or_else(PoisonError::into_inner).online_at_ms
    }

    pub fn is_stale(&self, now_ms: i64) -> bool {
        self.online_at_ms()
            .is_none_or(|at| now_ms.saturating_sub(at) > REFRESH_AFTER.as_millis() as i64)
    }

    /// Stale, and not tried in the last ten minutes, so an offline machine does not retry
    /// on every dashboard poll.
    pub fn refresh_due(&self, now_ms: i64) -> bool {
        self.is_stale(now_ms) && now_ms.saturating_sub(self.last_attempt_ms.load(Ordering::SeqCst)) > RETRY_AFTER_MS
    }

    /// Fetches the online list once; concurrent callers return at once. Failures keep
    /// whatever list is already loaded.
    pub async fn refresh(&self, client: &wreq::Client, url: &str) -> anyhow::Result<()> {
        if self.refreshing.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.last_attempt_ms
            .store(chrono::Utc::now().timestamp_millis(), Ordering::SeqCst);
        let result = self.fetch(client, url).await;
        self.refreshing.store(false, Ordering::SeqCst);
        result
    }

    async fn fetch(&self, client: &wreq::Client, url: &str) -> anyhow::Result<()> {
        let response = client
            .get(url)
            .header("User-Agent", "cliproxy-rs")
            .timeout(Duration::from_secs(30))
            .send()
            .await?;
        anyhow::ensure!(
            response.status().is_success(),
            "price list status {}",
            response.status()
        );
        let body = response.bytes().await?;
        anyhow::ensure!(body.len() <= MAX_LIST_BYTES, "price list too large");
        let rates = parse_list(&body)?;
        anyhow::ensure!(!rates.is_empty(), "price list has no usable models");
        let now = chrono::Utc::now().timestamp_millis();
        if let Some(path) = self.cache_path.lock().unwrap_or_else(PoisonError::into_inner).clone() {
            save(
                &path,
                &Cached {
                    fetched_at_ms: now,
                    rates: rates.clone(),
                },
            );
        }
        self.install(rates, now);
        Ok(())
    }
}

fn save(path: &Path, cached: &Cached) {
    let Ok(bytes) = serde_json::to_vec(cached) else { return };
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

pub struct Snapshot(Arc<Table>);

impl Snapshot {
    pub fn rate(&self, model: &str) -> Option<Rate> {
        lookup(&self.0, model)
    }
}

/// Lower-cases, drops a `provider/` prefix, a trailing `-YYYYMMDD`/`@YYYYMMDD` date and a
/// `[1m]`-style suffix, and turns dots into dashes so `claude-opus-4.5` meets `claude-opus-4-5`.
fn normalize(model: &str) -> String {
    let lower = model.trim().to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    let name = name.split('[').next().unwrap_or(name).replace('.', "-");
    let name = name.split('@').next().unwrap_or(&name);
    match name.rsplit_once('-') {
        Some((head, tail)) if tail.len() == 8 && tail.bytes().all(|b| b.is_ascii_digit()) => head.to_owned(),
        _ => name.to_owned(),
    }
}

/// Exact match, then the longest known name that the model name extends (`gpt-5-codex-max`
/// finds `gpt-5-codex`).
fn lookup(table: &Table, model: &str) -> Option<Rate> {
    let name = normalize(model);
    if let Some(rate) = table.get(&name) {
        return Some(*rate);
    }
    table
        .iter()
        .filter(|(key, _)| {
            name.strip_prefix(key.as_str())
                .is_some_and(|rest| rest.starts_with('-'))
        })
        .max_by_key(|(key, _)| key.len())
        .map(|(_, rate)| *rate)
}

/// LiteLLM's `model_prices_and_context_window.json`: model name -> per-token costs. Entries
/// with a `provider/` prefix only fill names the plain entries do not cover. A missing
/// cache-read price means no discount; a missing cache-write price is 1.25x input for Claude
/// and the input price otherwise.
fn parse_list(body: &[u8]) -> anyhow::Result<Table> {
    let root: Value = serde_json::from_slice(body)?;
    let entries = root
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("price list is not an object"))?;
    let mut plain = Table::new();
    let mut prefixed = Table::new();
    for (key, entry) in entries {
        let cost = |field: &str| entry.get(field).and_then(Value::as_f64).filter(|v| *v >= 0.0);
        let (Some(input), Some(output)) = (cost("input_cost_per_token"), cost("output_cost_per_token")) else {
            continue;
        };
        let name = normalize(key);
        let rate = Rate {
            input,
            output,
            cache_read: cost("cache_read_input_token_cost").unwrap_or(input),
            cache_write: cost("cache_creation_input_token_cost").unwrap_or(if name.starts_with("claude") {
                input * 1.25
            } else {
                input
            }),
        };
        if key.contains('/') {
            prefixed.entry(name).or_insert(rate);
        } else {
            plain.insert(name, rate);
        }
    }
    for (name, rate) in prefixed {
        plain.entry(name).or_insert(rate);
    }
    Ok(plain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_names() {
        assert_eq!(normalize("Anthropic/Claude-Opus-4.5"), "claude-opus-4-5");
        assert_eq!(normalize("claude-sonnet-4-5-20250929"), "claude-sonnet-4-5");
        assert_eq!(normalize("claude-opus-5-5[1m]"), "claude-opus-5-5");
        assert_eq!(normalize("claude-haiku-4-5@20251001"), "claude-haiku-4-5");
    }

    #[test]
    fn finds_exact_then_longest_prefix() {
        let pricing = Pricing::default();
        assert_eq!(pricing.snapshot().rate("claude-opus-5-5").unwrap().input, 4.0 / 1e6);
        assert_eq!(pricing.snapshot().rate("gpt-5-codex-max").unwrap().input, 1.25 / 1e6);
        assert!(
            pricing.snapshot().rate("gpt-50").is_none(),
            "a prefix must end at a dash"
        );
        assert!(pricing.snapshot().rate("mystery-model").is_none());
    }

    #[test]
    fn online_list_overrides_built_in_and_fills_gaps() {
        let list = br#"{
            "claude-opus-5-5": {"input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6,
                "cache_read_input_token_cost": 1e-7, "cache_creation_input_token_cost": 3e-6},
            "vertex_ai/claude-opus-5-5": {"input_cost_per_token": 9e-6, "output_cost_per_token": 9e-6},
            "gemini/gemini-2.5-pro": {"input_cost_per_token": 1.25e-6, "output_cost_per_token": 1e-5},
            "claude-new": {"input_cost_per_token": 2e-6, "output_cost_per_token": 4e-6},
            "sample_spec": {"max_tokens": 1}
        }"#;
        let rates = parse_list(list).unwrap();
        assert_eq!(rates["claude-opus-5-5"].input, 1e-6, "plain entry beats prefixed one");
        assert_eq!(rates["claude-new"].cache_write, 2e-6 * 1.25);
        assert_eq!(rates["gemini-2-5-pro"].cache_read, 1.25e-6, "no discount when unlisted");
        assert!(!rates.contains_key("sample_spec"));

        let pricing = Pricing::default();
        assert!(pricing.is_stale(0));
        pricing.install(rates, 1_000);
        assert_eq!(pricing.snapshot().rate("claude-opus-5-5").unwrap().cache_write, 3e-6);
        assert!(
            pricing.snapshot().rate("claude-sonnet-5-5").is_some(),
            "built-in stays as the floor"
        );
        assert!(!pricing.is_stale(2_000));
    }
}
