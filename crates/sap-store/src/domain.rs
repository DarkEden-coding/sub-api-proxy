use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use sap_core::account::{Account, NodeCredential};
use sap_core::api::EventView;
use sap_core::ids::{AccountId, NodeId, ThreadKey};
use sap_core::quota::{QuotaObservation, QuotaWindow};
use sap_core::sync::{Change, ChangeOp, Conflict, SyncCursor, Table};
use sap_core::thread::{ThreadRecord, ThreadUsage};
use sap_core::{DateTime, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::log::{cursor, name, record, write};
use crate::{Error, Result, Store};

#[derive(Serialize, Deserialize)]
struct UsageDelta {
    account: AccountId,
    node: NodeId,
    bucket_start: DateTime<Utc>,
    delta: ThreadUsage,
}

pub(crate) fn validate_append(c: &Change, value: &Value) -> Result<(String, i64)> {
    match c.table {
        Table::QuotaObservations => {
            let q: QuotaObservation = serde_json::from_value(value.clone())?;
            if q.node != c.origin || q.seq != c.seq || q.account.0 != c.key {
                return Err(Error::Invalid("quota identity differs from change".into()));
            }
            Ok((
                q.account.0,
                q.windows
                    .iter()
                    .map(|w| w.observed_at.timestamp_millis())
                    .max()
                    .unwrap_or(c.hlc.wall_ms),
            ))
        }
        Table::UsageBuckets => {
            let u: UsageDelta = serde_json::from_value(value.clone())?;
            if u.node != c.origin || u.account.0 != c.key {
                return Err(Error::Invalid("usage identity differs from change".into()));
            }
            Ok((u.account.0, u.bucket_start.timestamp_millis()))
        }
        Table::Events => {
            let e: EventView = serde_json::from_value(value.clone())?;
            if e.id != c.key {
                return Err(Error::Invalid("event identity differs from change".into()));
            }
            Ok((e.account.map(|a| a.0).unwrap_or_default(), e.at.timestamp_millis()))
        }
        _ => Err(Error::Invalid("not an append table".into())),
    }
}
pub(crate) fn validate_record(c: &Change, value: &Value) -> Result<()> {
    match c.table {
        Table::Threads => {
            let t: ThreadRecord = serde_json::from_value(value.clone())?;
            if t.key.to_string() != c.key {
                return Err(Error::Invalid("thread key differs from change".into()));
            }
        }
        Table::Conflicts => {
            let conflict: Conflict = serde_json::from_value(value.clone())?;
            if conflict.id != c.key {
                return Err(Error::Invalid("conflict key differs from change".into()));
            }
        }
        _ => (),
    }
    Ok(())
}
fn records<T: DeserializeOwned>(conn: &Connection, table: Table) -> Result<Vec<T>> {
    let mut stmt = conn.prepare(&format!("SELECT value FROM {} ORDER BY key", name(table)))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    rows.map(|s| Ok(serde_json::from_str(&s?)?)).collect()
}
fn append_rows<T: DeserializeOwned>(
    conn: &Connection,
    table: Table,
    account: &AccountId,
    since: DateTime<Utc>,
) -> Result<Vec<T>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT value FROM {} WHERE account=?1 AND at_ms>=?2 ORDER BY at_ms,origin,seq",
        name(table)
    ))?;
    let rows = stmt.query_map(params![account.0, since.timestamp_millis()], |r| r.get::<_, String>(0))?;
    rows.map(|s| Ok(serde_json::from_str(&s?)?)).collect()
}
fn sum(total: &mut ThreadUsage, delta: &ThreadUsage) -> Result<()> {
    let overflow = || Error::Invalid("usage total overflow".into());
    total.requests = total.requests.checked_add(delta.requests).ok_or_else(overflow)?;
    total.input_tokens = total
        .input_tokens
        .checked_add(delta.input_tokens)
        .ok_or_else(overflow)?;
    total.cache_read_tokens = total
        .cache_read_tokens
        .checked_add(delta.cache_read_tokens)
        .ok_or_else(overflow)?;
    total.cache_write_tokens = total
        .cache_write_tokens
        .checked_add(delta.cache_write_tokens)
        .ok_or_else(overflow)?;
    total.output_tokens = total
        .output_tokens
        .checked_add(delta.output_tokens)
        .ok_or_else(overflow)?;
    Ok(())
}
impl Store {
    pub(crate) fn transaction<T>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = f(&tx)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn upsert_account(&self, account: Account) -> Result<()> {
        self.transaction(|tx| {
            let value = serde_json::to_value(&account)?;
            if let Some(previous) = record(tx, Table::Accounts, &account.id.0)? {
                for (field, value) in value.as_object().expect("Account is an object") {
                    if previous.get(field) != Some(value) {
                        write(
                            tx,
                            &self.node,
                            Table::Accounts,
                            &account.id.0,
                            ChangeOp::Set {
                                field: Some(field.clone()),
                                value: value.clone(),
                            },
                        )?;
                    }
                }
            } else {
                write(
                    tx,
                    &self.node,
                    Table::Accounts,
                    &account.id.0,
                    ChangeOp::Set { field: None, value },
                )?;
            }
            Ok(())
        })
    }
    /// Validate the resulting Account before appending any field changes.
    pub fn patch_account(&self, id: &AccountId, changes: &BTreeMap<String, Value>) -> Result<()> {
        self.transaction(|tx| {
            let mut value =
                record(tx, Table::Accounts, &id.0)?.ok_or_else(|| Error::Invalid("account not found".into()))?;
            let fields = value.as_object_mut().expect("Account is an object");
            for (field, value) in changes {
                if field == "id" || !fields.contains_key(field) {
                    return Err(Error::Invalid("unknown or immutable account field".into()));
                }
                fields.insert(field.clone(), value.clone());
            }
            let _: Account = serde_json::from_value(value)?;
            for (field, value) in changes {
                write(
                    tx,
                    &self.node,
                    Table::Accounts,
                    &id.0,
                    ChangeOp::Set {
                        field: Some(field.clone()),
                        value: value.clone(),
                    },
                )?;
            }
            Ok(())
        })
    }
    pub fn accounts(&self) -> Result<Vec<Account>> {
        records(&*self.connection()?, Table::Accounts)
    }
    pub fn put_thread(&self, thread: ThreadRecord) -> Result<()> {
        self.transaction(|tx| {
            write(
                tx,
                &self.node,
                Table::Threads,
                &thread.key.to_string(),
                ChangeOp::Set {
                    field: None,
                    value: serde_json::to_value(&thread)?,
                },
            )?;
            Ok(())
        })
    }
    pub fn threads(&self) -> Result<Vec<ThreadRecord>> {
        records(&*self.connection()?, Table::Threads)
    }
    pub fn delete_thread(&self, key: &ThreadKey) -> Result<()> {
        self.transaction(|tx| {
            write(tx, &self.node, Table::Threads, &key.to_string(), ChangeOp::Delete)?;
            Ok(())
        })
    }
    /// The store assigns the observation's node and sequence, replacing caller values.
    pub fn append_quota(&self, mut observation: QuotaObservation) -> Result<QuotaObservation> {
        self.transaction(|tx| {
            observation.node = self.node.clone();
            observation.seq = cursor(tx)?.get(&self.node).copied().unwrap_or(0) + 1;
            write(
                tx,
                &self.node,
                Table::QuotaObservations,
                &observation.account.0,
                ChangeOp::Append {
                    value: serde_json::to_value(&observation)?,
                },
            )?;
            Ok(observation)
        })
    }
    pub fn quota_history(&self, account: &AccountId, since: DateTime<Utc>) -> Result<Vec<QuotaObservation>> {
        append_rows(&*self.connection()?, Table::QuotaObservations, account, since)
    }
    pub fn latest_windows(&self, account: &AccountId) -> Result<Vec<QuotaWindow>> {
        let observations = self.quota_history(account, DateTime::<Utc>::MIN_UTC)?;
        // Timestamp, then origin/sequence, gives the same tie break on every node.
        let mut latest: BTreeMap<String, (DateTime<Utc>, NodeId, u64, QuotaWindow)> = BTreeMap::new();
        for q in observations {
            for window in q.windows {
                let key = serde_json::to_string(&window.kind)?;
                let rank = (window.observed_at, q.node.clone(), q.seq);
                if latest.get(&key).is_none_or(|old| rank > (old.0, old.1.clone(), old.2)) {
                    latest.insert(key, (rank.0, rank.1, rank.2, window));
                }
            }
        }
        Ok(latest.into_values().map(|v| v.3).collect())
    }
    pub fn add_usage(&self, account: &AccountId, bucket_start: DateTime<Utc>, deltas: ThreadUsage) -> Result<()> {
        self.transaction(|tx| {
            let value = serde_json::to_value(UsageDelta {
                account: account.clone(),
                node: self.node.clone(),
                bucket_start,
                delta: deltas,
            })?;
            write(
                tx,
                &self.node,
                Table::UsageBuckets,
                &account.0,
                ChangeOp::Append { value },
            )?;
            Ok(())
        })
    }
    /// Sum the immutable per-node deltas into time buckets across all nodes.
    pub fn usage_buckets(
        &self,
        account: &AccountId,
        since: DateTime<Utc>,
    ) -> Result<BTreeMap<DateTime<Utc>, ThreadUsage>> {
        let rows: Vec<UsageDelta> = append_rows(&*self.connection()?, Table::UsageBuckets, account, since)?;
        let mut buckets = BTreeMap::new();
        for row in rows {
            sum(buckets.entry(row.bucket_start).or_default(), &row.delta)?;
        }
        Ok(buckets)
    }
    pub fn append_event(&self, event: EventView) -> Result<()> {
        self.transaction(|tx| {
            write(
                tx,
                &self.node,
                Table::Events,
                &event.id,
                ChangeOp::Append {
                    value: serde_json::to_value(&event)?,
                },
            )?;
            Ok(())
        })
    }
    pub fn events(&self, limit: usize) -> Result<Vec<EventView>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare("SELECT value FROM events ORDER BY at_ms DESC,origin DESC,seq DESC LIMIT ?1")?;
        let rows = stmt.query_map([i64::try_from(limit).unwrap_or(i64::MAX)], |r| r.get::<_, String>(0))?;
        rows.map(|s| Ok(serde_json::from_str(&s?)?)).collect()
    }
    pub fn conflicts(&self) -> Result<Vec<Conflict>> {
        records(&*self.connection()?, Table::Conflicts)
    }
    pub fn resolve_conflict(&self, id: &str, keep: NodeId) -> Result<Conflict> {
        self.transaction(|tx| {
            let raw = record(tx, Table::Conflicts, id)?.ok_or_else(|| Error::Invalid("conflict not found".into()))?;
            let mut conflict: Conflict = serde_json::from_value(raw)?;
            if conflict.resolved {
                return Ok(conflict);
            }
            let side = if conflict.applied.node == keep {
                &conflict.applied
            } else if conflict.other.node == keep {
                &conflict.other
            } else {
                return Err(Error::Invalid("chosen node is not a conflict side".into()));
            };
            write(
                tx,
                &self.node,
                conflict.table,
                &conflict.key,
                ChangeOp::Set {
                    field: Some(conflict.field.clone()),
                    value: side.value.clone(),
                },
            )?;
            conflict.resolved = true;
            write(
                tx,
                &self.node,
                Table::Conflicts,
                id,
                ChangeOp::Set {
                    field: None,
                    value: serde_json::to_value(&conflict)?,
                },
            )?;
            Ok(conflict)
        })
    }
    /// Each setting key is one independently merged value; any JSON value is accepted.
    pub fn set_setting(&self, key: &str, value: Value) -> Result<()> {
        self.transaction(|tx| {
            write(
                tx,
                &self.node,
                Table::Settings,
                key,
                ChangeOp::Set {
                    field: Some("value".into()),
                    value,
                },
            )?;
            Ok(())
        })
    }
    pub fn get_setting(&self, key: &str) -> Result<Option<Value>> {
        Ok(record(&*self.connection()?, Table::Settings, key)?.and_then(|v| v.get("value").cloned()))
    }
    pub fn set_client_access(&self, key: &str, value: Value) -> Result<()> {
        self.set_fields(Table::ClientAccess, key, value)
    }
    pub fn client_access(&self, key: &str) -> Result<Option<Value>> {
        record(&*self.connection()?, Table::ClientAccess, key)
    }
    fn set_fields(&self, table: Table, key: &str, value: Value) -> Result<()> {
        let fields = value
            .as_object()
            .ok_or_else(|| Error::Invalid("field records require an object".into()))?;
        self.transaction(|tx| {
            if let Some(previous) = record(tx, table, key)? {
                for (field, value) in fields {
                    if previous.get(field) != Some(value) {
                        write(
                            tx,
                            &self.node,
                            table,
                            key,
                            ChangeOp::Set {
                                field: Some(field.clone()),
                                value: value.clone(),
                            },
                        )?;
                    }
                }
            } else {
                write(
                    tx,
                    &self.node,
                    table,
                    key,
                    ChangeOp::Set {
                        field: None,
                        value: value.clone(),
                    },
                )?;
            }
            Ok(())
        })
    }
    pub fn put_credential(&self, credential: NodeCredential) -> Result<()> {
        if credential.node != self.node {
            return Err(Error::Invalid("credential belongs to another node".into()));
        }
        self.transaction(|tx| {
            tx.execute(
                "INSERT INTO node_credentials VALUES (?1,?2) ON CONFLICT(account) DO UPDATE SET value=excluded.value",
                params![credential.account.0, serde_json::to_string(&credential)?],
            )?;
            Ok(())
        })
    }
    pub fn credentials(&self) -> Result<Vec<NodeCredential>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare("SELECT value FROM node_credentials ORDER BY account")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|s| Ok(serde_json::from_str(&s?)?)).collect()
    }
    pub fn set_peer_cursor(&self, peer: NodeId, value: &SyncCursor) -> Result<()> {
        if peer == self.node {
            return Err(Error::Invalid("peer cannot be the local node".into()));
        }
        self.transaction(|tx| {
            let local = cursor(tx)?;
            // A peer may know other origins that this node has not pulled yet.
            // Only a future acknowledgement of our own sequence is impossible.
            if value.get(&self.node).copied().unwrap_or(0) > local.get(&self.node).copied().unwrap_or(0) {
                return Err(Error::Invalid("peer acknowledges future local changes".into()));
            }
            let raw: Option<String> = tx
                .query_row("SELECT value FROM peer_cursors WHERE peer=?1", [&peer.0], |r| r.get(0))
                .optional()?;
            let mut merged: SyncCursor = raw.map(|s| serde_json::from_str(&s)).transpose()?.unwrap_or_default();
            for (node, seq) in value {
                let old = merged.entry(node.clone()).or_default();
                *old = (*old).max(*seq);
            }
            tx.execute(
                "INSERT INTO peer_cursors VALUES (?1,?2) ON CONFLICT(peer) DO UPDATE SET value=excluded.value",
                params![peer.0, serde_json::to_string(&merged)?],
            )?;
            Ok(())
        })
    }
}
