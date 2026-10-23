//! Durable per-request usage history behind the dashboard's Usage page (additive; not in
//! Go). The usage queue forgets records after at most an hour and hands each one to a
//! single reader, so every record it accepts is also reduced to a few numbers and appended
//! to a JSONL file. Dollar figures are computed when the file is read, so a corrected price
//! list also corrects the past.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, PoisonError};

use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::usage_pricing::{Pricing, Rate, Snapshot};

pub const RETENTION_DAYS: i64 = 90;
const RECENT_REQUESTS: usize = 100;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// One request, as stored. Token counts are the normalized breakdown: `input` excludes
/// cached tokens, and `output` includes reasoning.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Unix milliseconds.
    pub t: i64,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub p: String,
    pub m: String,
    #[serde(default)]
    pub i: u64,
    #[serde(default)]
    pub r: u64,
    #[serde(default)]
    pub w: u64,
    #[serde(default)]
    pub o: u64,
    #[serde(default)]
    pub failed: bool,
}

#[derive(Deserialize, Default)]
struct RawRecord {
    #[serde(default)]
    timestamp: String,
    #[serde(default)]
    provider: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    alias: String,
    #[serde(default)]
    request_id: String,
    #[serde(default)]
    failed: bool,
    #[serde(default)]
    tokens: RawTokens,
    token_breakdown: Option<RawBreakdown>,
}

#[derive(Deserialize, Default)]
struct RawTokens {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_tokens: u64,
    #[serde(default)]
    cache_creation_tokens: u64,
}

#[derive(Deserialize, Default)]
struct RawBreakdown {
    #[serde(default)]
    quality: String,
    #[serde(default)]
    input: RawInput,
    #[serde(default)]
    output: RawOutput,
}

#[derive(Deserialize, Default)]
struct RawInput {
    #[serde(default)]
    uncached_tokens: u64,
    #[serde(default)]
    cache_read_tokens: u64,
    #[serde(default)]
    cache_write_tokens: u64,
}

#[derive(Deserialize, Default)]
struct RawOutput {
    #[serde(default)]
    total_tokens: u64,
}

/// Reduces a serialized queue record. `None` for anything that is not a usage record
/// (refresh markers) or has no model.
pub fn entry_from_record(payload: &[u8]) -> Option<Entry> {
    let raw: RawRecord = serde_json::from_slice(payload).ok()?;
    let model = if raw.model.trim().is_empty() {
        raw.alias.trim()
    } else {
        raw.model.trim()
    };
    if model.is_empty() {
        return None;
    }
    let (i, r, w, o) = match raw.token_breakdown {
        Some(b) if b.quality == "complete" => (
            b.input.uncached_tokens,
            b.input.cache_read_tokens,
            b.input.cache_write_tokens,
            b.output.total_tokens,
        ),
        // ponytail: an unclassified record's input may already include its cached tokens,
        // which would overstate the cost slightly; classify the provider to fix it.
        _ => (
            raw.tokens.input_tokens,
            raw.tokens.cache_read_tokens,
            raw.tokens.cache_creation_tokens,
            raw.tokens.output_tokens,
        ),
    };
    let t = DateTime::parse_from_rfc3339(&raw.timestamp)
        .map_or_else(|_| Utc::now().timestamp_millis(), |t| t.timestamp_millis());
    Some(Entry {
        t,
        id: raw.request_id,
        p: raw.provider,
        m: model.to_owned(),
        i,
        r,
        w,
        o,
        failed: raw.failed,
    })
}

pub struct UsageHistory {
    path: PathBuf,
    tx: Mutex<Sender<Entry>>,
    pub pricing: Pricing,
}

impl UsageHistory {
    /// Starts the writer thread for `path`, dropping entries past the retention window.
    pub fn open(path: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel::<Entry>();
        let file = path.clone();
        std::thread::Builder::new()
            .name("usage-history".into())
            .spawn(move || {
                prune(&file, Utc::now().timestamp_millis() - RETENTION_DAYS * DAY_MS);
                let Ok(mut out) = open_append(&file) else {
                    tracing::warn!("usage history unavailable: cannot open the history file");
                    return;
                };
                for entry in rx {
                    if let Ok(mut line) = serde_json::to_vec(&entry) {
                        line.push(b'\n');
                        // One write per line keeps lines whole and readable at once.
                        let _ = out.write_all(&line);
                    }
                }
            })
            .ok();
        let pricing = Pricing::with_cache(path.with_file_name("usage-pricing.json"));
        Self {
            path,
            tx: Mutex::new(tx),
            pricing,
        }
    }

    pub fn record(&self, payload: &[u8]) {
        if let Some(entry) = entry_from_record(payload) {
            let _ = self.tx.lock().unwrap_or_else(PoisonError::into_inner).send(entry);
        }
    }

    /// Everything in the last `days` days, newest data last in `recent`.
    pub fn summary(&self, days: i64, now_ms: i64) -> Value {
        let since = now_ms - days.clamp(1, RETENTION_DAYS) * DAY_MS;
        let entries = read_since(&self.path, since);
        summarize(&entries, &self.pricing.snapshot(), since, self.pricing.online_at_ms())
    }
}

#[cfg(unix)]
fn open_append(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new().create(true).append(true).mode(0o600).open(path)
}

#[cfg(not(unix))]
fn open_append(path: &Path) -> std::io::Result<std::fs::File> {
    OpenOptions::new().create(true).append(true).open(path)
}

fn read_since(path: &Path, since_ms: i64) -> Vec<Entry> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<Entry>(&line).ok())
        .filter(|e| e.t >= since_ms)
        .collect()
}

/// Rewrites the file without entries older than `cutoff_ms`, only if there are any.
fn prune(path: &Path, cutoff_ms: i64) {
    let Ok(file) = std::fs::File::open(path) else { return };
    let mut keep = Vec::new();
    let mut dropped = false;
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        match serde_json::from_str::<Entry>(&line) {
            Ok(e) if e.t >= cutoff_ms => keep.push(line),
            _ => dropped = true,
        }
    }
    if !dropped {
        return;
    }
    let tmp = path.with_extension("tmp");
    let mut text = keep.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

#[derive(Default)]
struct Sum {
    requests: u64,
    failed: u64,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    cost: f64,
    cost_without_cache: f64,
    unpriced: u64,
}

impl Sum {
    fn add(&mut self, e: &Entry, cost: Option<(f64, f64)>) {
        self.requests += 1;
        self.failed += u64::from(e.failed);
        self.input += e.i;
        self.output += e.o;
        self.cache_read += e.r;
        self.cache_write += e.w;
        match cost {
            Some((cost, without)) => {
                self.cost += cost;
                self.cost_without_cache += without;
            }
            None if e.i + e.r + e.w + e.o > 0 => self.unpriced += 1,
            None => {}
        }
    }

    fn json(&self) -> Value {
        json!({
            "requests": self.requests,
            "failed": self.failed,
            "input_tokens": self.input,
            "output_tokens": self.output,
            "cache_read_tokens": self.cache_read,
            "cache_write_tokens": self.cache_write,
            "cost_usd": self.cost,
            "cost_without_cache_usd": self.cost_without_cache,
            "savings_usd": self.cost_without_cache - self.cost,
            "unpriced_requests": self.unpriced,
        })
    }
}

/// What the request costs, and what it would cost if every input token were billed at the
/// plain input rate.
fn price(e: &Entry, rate: Rate) -> (f64, f64) {
    let (i, r, w, o) = (e.i as f64, e.r as f64, e.w as f64, e.o as f64);
    let cost = i * rate.input + r * rate.cache_read + w * rate.cache_write + o * rate.output;
    let without = (i + r + w) * rate.input + o * rate.output;
    (cost, without)
}

fn summarize(entries: &[Entry], prices: &Snapshot, since_ms: i64, prices_at_ms: Option<i64>) -> Value {
    let mut total = Sum::default();
    let mut days: BTreeMap<String, Sum> = BTreeMap::new();
    let mut models: BTreeMap<(String, String), Sum> = BTreeMap::new();
    let mut rates: BTreeMap<&str, Option<Rate>> = BTreeMap::new();
    let mut recent = Vec::new();
    for e in entries {
        let rate = *rates.entry(&e.m).or_insert_with(|| prices.rate(&e.m));
        let cost = rate.map(|r| price(e, r));
        total.add(e, cost);
        let day = Utc
            .timestamp_millis_opt(e.t)
            .single()
            .map_or_else(String::new, |t| t.format("%Y-%m-%d").to_string());
        days.entry(day).or_default().add(e, cost);
        models.entry((e.m.clone(), e.p.clone())).or_default().add(e, cost);
        recent.push((e, cost));
    }
    recent.sort_by_key(|(e, _)| std::cmp::Reverse(e.t));
    recent.truncate(RECENT_REQUESTS);
    let days: Vec<Value> = days
        .into_iter()
        .map(|(date, s)| {
            let mut v = s.json();
            v["date"] = date.into();
            v
        })
        .collect();
    let mut models: Vec<(String, String, Sum)> = models.into_iter().map(|((m, p), s)| (m, p, s)).collect();
    models.sort_by(|a, b| b.2.cost.total_cmp(&a.2.cost).then(b.2.requests.cmp(&a.2.requests)));
    let models: Vec<Value> = models
        .into_iter()
        .map(|(model, provider, s)| {
            let mut v = s.json();
            v["model"] = model.clone().into();
            v["provider"] = provider.into();
            v["priced"] = prices.rate(&model).is_some().into();
            v
        })
        .collect();
    let recent: Vec<Value> = recent
        .into_iter()
        .map(|(e, cost)| {
            json!({
                "at": e.t,
                "request_id": e.id,
                "model": e.m,
                "provider": e.p,
                "failed": e.failed,
                "input_tokens": e.i,
                "output_tokens": e.o,
                "cache_read_tokens": e.r,
                "cache_write_tokens": e.w,
                "cost_usd": cost.map(|c| c.0),
                "savings_usd": cost.map(|c| c.1 - c.0),
            })
        })
        .collect();
    json!({
        "since": since_ms,
        "prices": {
            "source": if prices_at_ms.is_some() { "online" } else { "built-in" },
            "updated_at": prices_at_ms,
        },
        "totals": total.json(),
        "days": days,
        "models": models,
        "recent": recent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(extra: &str) -> Vec<u8> {
        format!(
            r#"{{"timestamp":"2026-10-22T10:00:00.123456789Z","provider":"claude","model":"claude-sonnet-5-5","request_id":"r1",
            "failed":false,"tokens":{{"input_tokens":50,"output_tokens":10,"cache_read_tokens":0,"cache_creation_tokens":0}}{extra}}}"#
        )
        .into_bytes()
    }

    #[test]
    fn reduces_records_and_skips_markers() {
        let complete = record(
            r#","token_breakdown":{"quality":"complete","input":{"uncached_tokens":100,"cache_read_tokens":900,"cache_write_tokens":0},"output":{"total_tokens":20}}"#,
        );
        let e = entry_from_record(&complete).unwrap();
        assert_eq!(
            (e.i, e.r, e.w, e.o, e.m.as_str()),
            (100, 900, 0, 20, "claude-sonnet-5-5")
        );
        assert_eq!(e.t, 1_792_663_200_123);
        let fallback = entry_from_record(&record("")).unwrap();
        assert_eq!(
            (fallback.i, fallback.o),
            (50, 10),
            "unclassified records use the raw counts"
        );
        assert!(entry_from_record(br#"{"support_refresh":true}"#).is_none());
    }

    #[test]
    fn prices_cache_reads_and_flags_unknown_models() {
        let mk = |m: &str, r: u64| Entry {
            t: 1_792_663_200_000,
            id: String::new(),
            p: "claude".into(),
            m: m.into(),
            i: 1_000_000,
            r,
            w: 0,
            o: 1_000_000,
            failed: false,
        };
        let entries = [mk("claude-sonnet-5-5", 1_000_000), mk("unknown-model", 0)];
        let pricing = Pricing::default();
        let v = summarize(&entries, &pricing.snapshot(), 0, None);
        // Sonnet 5.5: 1M in @ $2 + 1M cached @ $0.20 + 1M out @ $10.
        assert!((v["totals"]["cost_usd"].as_f64().unwrap() - 12.2).abs() < 1e-9);
        // Without caching the 1M cached tokens would be billed at $2.
        assert!((v["totals"]["savings_usd"].as_f64().unwrap() - 1.8).abs() < 1e-9);
        assert_eq!(v["totals"]["unpriced_requests"], 1);
        assert_eq!(v["prices"]["source"], "built-in");
        assert_eq!(v["models"][1]["priced"], false);
        assert_eq!(v["days"][0]["date"], "2026-10-22");
    }

    #[test]
    fn writes_reads_and_prunes() {
        let dir = std::env::temp_dir().join(format!("usage-history-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("usage-history.jsonl");
        let old = Entry {
            t: 1_000,
            id: "old".into(),
            p: String::new(),
            m: "claude-opus-5-5".into(),
            i: 1,
            r: 0,
            w: 0,
            o: 1,
            failed: false,
        };
        let now = Utc::now().timestamp_millis();
        let new = Entry {
            t: now,
            id: "new".into(),
            ..old.clone()
        };
        std::fs::write(
            &path,
            format!(
                "{}\nnot json\n{}\n",
                serde_json::to_string(&old).unwrap(),
                serde_json::to_string(&new).unwrap()
            ),
        )
        .unwrap();
        prune(&path, now - DAY_MS);
        let kept = read_since(&path, 0);
        assert_eq!(kept.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["new"]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
