use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};
use sap_core::ids::NodeId;
use sap_core::sync::{Change, ChangeOp, SyncCursor, Table};
use sap_core::thread::ThreadRecord;
use sap_core::{DateTime, Utc};

use crate::log::write;
use crate::schema::APPEND_TABLES;
use crate::{Result, Store};

/// Retention ages and the SQLite disk byte budget. Safety metadata and
/// unacknowledged changes are never discarded to force the budget to fit.
#[derive(Debug, Clone)]
pub struct Limits {
    pub quota_max_age: Duration,
    pub usage_max_age: Duration,
    pub event_max_age: Duration,
    pub op_log_max_age: Duration,
    pub thread_max_age: Duration,
    pub max_bytes: u64,
}
impl Default for Limits {
    fn default() -> Self {
        let day = 86_400;
        Self {
            quota_max_age: Duration::from_secs(14 * day),
            usage_max_age: Duration::from_secs(90 * day),
            event_max_age: Duration::from_secs(7 * day),
            op_log_max_age: Duration::from_secs(14 * day),
            thread_max_age: Duration::from_secs(30 * day),
            max_bytes: 256 * 1024 * 1024,
        }
    }
}
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CompactionReport {
    pub append_rows_deleted: usize,
    pub op_log_rows_deleted: usize,
    pub threads_deleted: usize,
    pub retained_bytes: u64,
    pub budget_met: bool,
}
fn cutoff(now: DateTime<Utc>, age: Duration) -> i64 {
    now.timestamp_millis()
        .saturating_sub(i64::try_from(age.as_millis()).unwrap_or(i64::MAX))
}
// Live pages estimate the compacted file while a write transaction is active.
fn bytes(conn: &Connection) -> Result<u64> {
    let pages: u64 = conn.pragma_query_value(None, "page_count", |r| r.get(0))?;
    let free: u64 = conn.pragma_query_value(None, "freelist_count", |r| r.get(0))?;
    let size: u64 = conn.pragma_query_value(None, "page_size", |r| r.get(0))?;
    Ok((pages - free) * size + sidecar_bytes(conn, false)?)
}
fn sidecar_bytes(conn: &Connection, include_wal: bool) -> Result<u64> {
    let path: String = conn.query_row("SELECT file FROM pragma_database_list WHERE name='main'", [], |r| {
        r.get(0)
    })?;
    if path.is_empty() {
        return Ok(0);
    }
    let mut total = 0;
    for suffix in if include_wal {
        &["-shm", "-wal"][..]
    } else {
        &["-shm"][..]
    } {
        match std::fs::metadata(format!("{path}{suffix}")) {
            Ok(metadata) => total += metadata.len(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(total)
}
fn mark_pruned(conn: &Connection, origin: &str, seq: u64) -> Result<()> {
    let raw: Option<String> = conn
        .query_row("SELECT value FROM metadata WHERE key='log_floor'", [], |r| r.get(0))
        .optional()?;
    let mut floor: SyncCursor = raw.map(|s| serde_json::from_str(&s)).transpose()?.unwrap_or_default();
    let old = floor.entry(NodeId(origin.into())).or_default();
    *old = (*old).max(seq);
    conn.execute(
        "INSERT INTO metadata VALUES ('log_floor',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        [serde_json::to_string(&floor)?],
    )?;
    Ok(())
}
impl Store {
    pub fn compact(&self) -> Result<CompactionReport> {
        self.compact_at(Utc::now())
    }
    pub fn compact_at(&self, now: DateTime<Utc>) -> Result<CompactionReport> {
        let mut report = self.transaction(|tx| {
            let mut report = CompactionReport::default();
            for (table, age) in [
                ("quota_observations", self.limits.quota_max_age),
                ("usage_buckets", self.limits.usage_max_age),
                ("events", self.limits.event_max_age),
            ] {
                report.append_rows_deleted +=
                    tx.execute(&format!("DELETE FROM {table} WHERE at_ms<?1"), [cutoff(now, age)])?;
            }
            let peers: Vec<SyncCursor> = {
                let mut stmt = tx.prepare("SELECT value FROM peer_cursors")?;
                stmt.query_map([], |r| r.get::<_, String>(0))?
                    .map(|s| Ok(serde_json::from_str(&s?)?))
                    .collect::<Result<_>>()?
            };
            let logs: Vec<(i64, Change)> = {
                let mut stmt = tx.prepare("SELECT id,value FROM op_log ORDER BY wall_ms,id")?;
                stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
                    .map(|r| {
                        let (id, raw) = r?;
                        Ok((id, serde_json::from_str(&raw)?))
                    })
                    .collect::<Result<_>>()?
            };
            // No registered peers means no acknowledgement evidence. Register all
            // configured peers, even with empty cursors, before enabling compaction.
            let acknowledged =
                |c: &Change| !peers.is_empty() && peers.iter().all(|p| p.get(&c.origin).copied().unwrap_or(0) >= c.seq);
            for (id, c) in &logs {
                if c.hlc.wall_ms < cutoff(now, self.limits.op_log_max_age) && acknowledged(c) {
                    report.op_log_rows_deleted += tx.execute("DELETE FROM op_log WHERE id=?1", [id])?;
                    mark_pruned(tx, &c.origin.0, c.seq)?;
                }
            }
            let threads: Vec<ThreadRecord> = {
                let mut stmt = tx.prepare("SELECT value FROM threads")?;
                let mut threads = stmt
                    .query_map([], |r| r.get::<_, String>(0))?
                    .map(|s| Ok(serde_json::from_str(&s?)?))
                    .collect::<Result<Vec<ThreadRecord>>>()?;
                threads.sort_by_key(|t| (t.last_seen, t.key.to_string()));
                threads
            };
            for t in &threads {
                if t.open_streams == 0 && t.last_seen.timestamp_millis() < cutoff(now, self.limits.thread_max_age) {
                    write(tx, &self.node, Table::Threads, &t.key.to_string(), ChangeOp::Delete)?;
                    report.threads_deleted += 1;
                }
            }
            // Under pressure, discard oldest append data and acknowledged log rows
            // together in timestamp order. Cursors survive so replay cannot restore it.
            let mut candidates: Vec<(i64, String, String, u64)> = Vec::new();
            for table in APPEND_TABLES {
                let mut stmt = tx.prepare(&format!("SELECT at_ms,origin,seq FROM {table}"))?;
                for row in stmt.query_map([], |r| Ok((r.get(0)?, table.to_string(), r.get(1)?, r.get(2)?)))? {
                    candidates.push(row?);
                }
            }
            for (_, c) in logs {
                if acknowledged(&c) {
                    candidates.push((c.hlc.wall_ms, "op_log".into(), c.origin.0, c.seq));
                }
            }
            candidates.sort();
            let mut retained = bytes(tx)?;
            for (_, table, origin, seq) in candidates {
                if retained <= self.limits.max_bytes {
                    break;
                }
                let count = tx.execute(
                    &format!("DELETE FROM {table} WHERE origin=?1 AND seq=?2"),
                    params![origin, seq],
                )?;
                if table == "op_log" {
                    report.op_log_rows_deleted += count;
                    if count > 0 {
                        mark_pruned(tx, &origin, seq)?;
                    }
                } else {
                    report.append_rows_deleted += count;
                }
                retained = bytes(tx)?;
            }
            for t in threads {
                if retained <= self.limits.max_bytes {
                    break;
                }
                if t.open_streams == 0 && crate::log::record(tx, Table::Threads, &t.key.to_string())?.is_some() {
                    write(tx, &self.node, Table::Threads, &t.key.to_string(), ChangeOp::Delete)?;
                    report.threads_deleted += 1;
                    retained = bytes(tx)?;
                }
            }
            report.retained_bytes = retained;
            report.budget_met = retained <= self.limits.max_bytes;
            Ok(report)
        })?;
        let conn = self.connection()?;
        // VACUUM rebuilds live pages; checkpoint also reclaims the WAL file.
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")?;
        let pages: u64 = conn.pragma_query_value(None, "page_count", |r| r.get(0))?;
        let size: u64 = conn.pragma_query_value(None, "page_size", |r| r.get(0))?;
        report.retained_bytes = pages * size + sidecar_bytes(&conn, true)?;
        report.budget_met = report.retained_bytes <= self.limits.max_bytes;
        Ok(report)
    }
    pub fn peer_cursors(&self) -> Result<std::collections::BTreeMap<NodeId, SyncCursor>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare("SELECT peer,value FROM peer_cursors ORDER BY peer")?;
        stmt.query_map([], |r| Ok((NodeId(r.get(0)?), r.get::<_, String>(1)?)))?
            .map(|r| {
                let (node, raw) = r?;
                Ok((node, serde_json::from_str(&raw)?))
            })
            .collect()
    }
}
