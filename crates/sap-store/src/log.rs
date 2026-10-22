use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use sap_core::Utc;
use sap_core::api::{EventKind, EventView};
use sap_core::hlc::Hlc;
use sap_core::ids::{NodeId, ThreadKey};
use sap_core::sync::{
    ApplyReport, Change, ChangeBatch, ChangeLog, ChangeOp, Conflict, ConflictSide, SyncCursor, Table,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Error, Result, Store};

pub(crate) fn name(table: Table) -> &'static str {
    match table {
        Table::Accounts => "accounts",
        Table::Settings => "settings",
        Table::ClientAccess => "client_access",
        Table::Threads => "threads",
        Table::QuotaObservations => "quota_observations",
        Table::UsageBuckets => "usage_buckets",
        Table::Events => "events",
        Table::Conflicts => "conflicts",
    }
}
fn field_table(table: Table) -> bool {
    matches!(table, Table::Accounts | Table::Settings | Table::ClientAccess)
}
fn append_table(table: Table) -> bool {
    matches!(table, Table::QuotaObservations | Table::UsageBuckets | Table::Events)
}

// ponytail: Change has no causal context. Version 1 peers carry it in Set values;
// move this to a Change.context contract field when sap-core provides one.
#[derive(Serialize, Deserialize)]
struct Envelope {
    sap_store_version: u8,
    context: SyncCursor,
    value: Value,
}
#[derive(Serialize, Deserialize)]
struct Version {
    change: Change,
    context: SyncCursor,
    value: Value,
}

pub(crate) fn cursor(conn: &Connection) -> Result<SyncCursor> {
    let mut stmt = conn.prepare("SELECT origin, seq FROM cursor ORDER BY origin")?;
    let rows = stmt.query_map([], |r| Ok((NodeId(r.get(0)?), r.get(1)?)))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}
fn clock(conn: &Connection) -> Result<Hlc> {
    let raw: String = conn.query_row("SELECT value FROM metadata WHERE key='hlc'", [], |r| r.get(0))?;
    Ok(serde_json::from_str(&raw)?)
}
fn save_clock(conn: &Connection, clock: &Hlc) -> Result<()> {
    conn.execute(
        "UPDATE metadata SET value=?1 WHERE key='hlc'",
        [serde_json::to_string(clock)?],
    )?;
    Ok(())
}
pub(crate) fn record(conn: &Connection, table: Table, key: &str) -> Result<Option<Value>> {
    let raw: Option<String> = conn
        .query_row(&format!("SELECT value FROM {} WHERE key=?1", name(table)), [key], |r| {
            r.get(0)
        })
        .optional()?;
    raw.map(|s| serde_json::from_str(&s).map_err(Into::into)).transpose()
}
pub(crate) fn put_record(conn: &Connection, table: Table, key: &str, value: &Value) -> Result<()> {
    conn.execute(
        &format!(
            "INSERT INTO {}(key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            name(table)
        ),
        params![key, serde_json::to_string(value)?],
    )?;
    Ok(())
}

pub(crate) fn write(tx: &Transaction<'_>, node: &NodeId, table: Table, key: &str, mut op: ChangeOp) -> Result<Change> {
    let context = cursor(tx)?;
    let seq = context
        .get(node)
        .copied()
        .unwrap_or(0)
        .checked_add(1)
        .filter(|n| *n <= i64::MAX as u64)
        .ok_or_else(|| Error::Invalid("sequence exhausted".into()))?;
    if (field_table(table) || table == Table::Threads)
        && let ChangeOp::Set { value, .. } = &mut op
    {
        *value = serde_json::to_value(Envelope {
            sap_store_version: 1,
            context,
            value: value.take(),
        })?;
    }
    let current = clock(tx)?;
    let now = Utc::now().timestamp_millis();
    if now <= current.wall_ms && current.counter >= u32::MAX - 1 {
        return Err(Error::Invalid("HLC counter exhausted".into()));
    }
    let hlc = current.tick(now);
    save_clock(tx, &hlc)?;
    let change = Change {
        origin: node.clone(),
        seq,
        hlc,
        table,
        key: key.into(),
        op,
    };
    merge(tx, node, &change, &mut ApplyReport::default())?;
    persist(tx, &change)?;
    Ok(change)
}
fn persist(tx: &Transaction<'_>, c: &Change) -> Result<()> {
    tx.execute(
        "INSERT INTO op_log(origin,seq,wall_ms,value) VALUES (?1,?2,?3,?4)",
        params![c.origin.0, c.seq, c.hlc.wall_ms, serde_json::to_string(c)?],
    )?;
    tx.execute(
        "INSERT INTO cursor(origin,seq) VALUES (?1,?2) ON CONFLICT(origin) DO UPDATE SET seq=excluded.seq",
        params![c.origin.0, c.seq],
    )?;
    Ok(())
}
fn version(conn: &Connection, c: &Change, field: &str) -> Result<Option<Version>> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM field_versions WHERE table_name=?1 AND key=?2 AND field=?3",
            params![name(c.table), c.key, field],
            |r| r.get(0),
        )
        .optional()?;
    raw.map(|s| serde_json::from_str(&s).map_err(Into::into)).transpose()
}
fn save_version(conn: &Connection, c: &Change, field: &str, context: &SyncCursor, value: &Value) -> Result<()> {
    let v = Version {
        change: c.clone(),
        context: context.clone(),
        value: value.clone(),
    };
    conn.execute("INSERT INTO field_versions VALUES (?1,?2,?3,?4) ON CONFLICT(table_name,key,field) DO UPDATE SET value=excluded.value", params![name(c.table), c.key, field, serde_json::to_string(&v)?])?;
    Ok(())
}
fn saw(context: &SyncCursor, c: &Change) -> bool {
    context.get(&c.origin).copied().unwrap_or(0) >= c.seq
}
fn concurrent(c: &Change, context: &SyncCursor, old: &Version) -> bool {
    c.origin != old.change.origin && !saw(context, &old.change) && !saw(&old.context, c)
}
fn conflict(
    tx: &Transaction<'_>,
    node: &NodeId,
    c: &Change,
    field: &str,
    value: &Value,
    old: &Version,
    report: &mut ApplyReport,
) -> Result<()> {
    if old.value == *value {
        return Ok(());
    }
    let side = |change: &Change, value: &Value| ConflictSide {
        node: change.origin.clone(),
        hlc: change.hlc.clone(),
        value: value.clone(),
    };
    let (applied, other) = if c.hlc > old.change.hlc {
        (side(c, value), side(&old.change, &old.value))
    } else {
        (side(&old.change, &old.value), side(c, value))
    };
    let mut pair = [(c.origin.clone(), c.seq), (old.change.origin.clone(), old.change.seq)];
    pair.sort();
    let id = serde_json::to_string(&(c.table, &c.key, field, pair))?;
    if record(tx, Table::Conflicts, &id)?.is_none() {
        let conflict = Conflict {
            id: id.clone(),
            table: c.table,
            key: c.key.clone(),
            field: field.into(),
            detected_at: sap_core::DateTime::from_timestamp_millis(c.hlc.wall_ms.max(old.change.hlc.wall_ms))
                .ok_or_else(|| Error::Invalid("HLC timestamp out of range".into()))?,
            applied,
            other,
            resolved: false,
        };
        write(
            tx,
            node,
            Table::Conflicts,
            &id,
            ChangeOp::Set {
                field: None,
                value: serde_json::to_value(&conflict)?,
            },
        )?;
        report.conflicts.push(conflict);
    }
    Ok(())
}
fn unpack(c: &Change, raw: &Value) -> Result<(SyncCursor, Value)> {
    if (field_table(c.table) || c.table == Table::Threads) && raw.get("sap_store_version").is_some() {
        let e: Envelope = serde_json::from_value(raw.clone())?;
        if e.sap_store_version != 1 {
            return Err(Error::Invalid("unsupported causal envelope".into()));
        }
        if e.context.get(&c.origin).copied().unwrap_or(0) >= c.seq {
            return Err(Error::Invalid("change observes its own future".into()));
        }
        Ok((e.context, e.value))
    } else {
        Ok((SyncCursor::new(), raw.clone()))
    }
}

fn merge(tx: &Transaction<'_>, node: &NodeId, c: &Change, report: &mut ApplyReport) -> Result<()> {
    if append_table(c.table) {
        let ChangeOp::Append { value } = &c.op else {
            return Err(Error::Invalid("append table requires Append".into()));
        };
        let (account, at) = crate::domain::validate_append(c, value)?;
        tx.execute(
            &format!("INSERT INTO {} VALUES (?1,?2,?3,?4,?5)", name(c.table)),
            params![c.origin.0, c.seq, at, account, serde_json::to_string(value)?],
        )?;
        report.applied += 1;
        return Ok(());
    }
    let (field, context, value) = match &c.op {
        ChangeOp::Set { field, value } => {
            let (context, value) = unpack(c, value)?;
            (field.as_deref(), context, value)
        }
        ChangeOp::Delete => (None, SyncCursor::new(), Value::Null),
        ChangeOp::Append { .. } => return Err(Error::Invalid("record table requires Set or Delete".into())),
    };
    if field.is_some_and(str::is_empty) {
        return Err(Error::Invalid("empty field is reserved for record versions".into()));
    }
    if field.is_some() && !field_table(c.table) {
        return Err(Error::Invalid("table only supports whole records".into()));
    }
    let deleting = matches!(c.op, ChangeOp::Delete);
    let tombstone = version(tx, c, "")?;
    if tombstone
        .as_ref()
        .is_some_and(|v| matches!(v.change.op, ChangeOp::Delete) && v.change.hlc >= c.hlc)
    {
        report.ignored += 1;
        return Ok(());
    }
    if field_table(c.table) && !deleting {
        let fields: Vec<(String, Value)> = if let Some(field) = field {
            vec![(field.to_owned(), value)]
        } else {
            value
                .as_object()
                .ok_or_else(|| Error::Invalid("field table requires an object".into()))?
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        };
        let stored = record(tx, c.table, &c.key)?;
        let mut current = stored.clone().unwrap_or_else(|| serde_json::json!({}));
        // A newer field can revive a deleted record. Restore its prior fields so
        // delete/set delivery order cannot produce an incomplete typed record.
        if stored.is_none()
            && tombstone
                .as_ref()
                .is_some_and(|v| matches!(v.change.op, ChangeOp::Delete))
        {
            let mut stmt =
                tx.prepare("SELECT field,value FROM field_versions WHERE table_name=?1 AND key=?2 AND field<>''")?;
            for row in stmt.query_map(params![name(c.table), c.key], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })? {
                let (field, raw) = row?;
                let old: Version = serde_json::from_str(&raw)?;
                current.as_object_mut().expect("object").insert(field, old.value);
            }
        }
        let mut applied = false;
        if fields.is_empty() && record(tx, c.table, &c.key)?.is_none() {
            put_record(tx, c.table, &c.key, &current)?;
            save_version(tx, c, "", &context, &current)?;
            applied = true;
        }
        let object = current
            .as_object_mut()
            .ok_or_else(|| Error::Invalid("stored field record is not an object".into()))?;
        for (field, value) in fields {
            if field.is_empty() {
                return Err(Error::Invalid("empty field is reserved".into()));
            }
            let old = version(tx, c, &field)?;
            if let Some(old) = &old
                && concurrent(c, &context, old)
            {
                conflict(tx, node, c, &field, &value, old, report)?;
            }
            if old.as_ref().is_none_or(|v| c.hlc > v.change.hlc) {
                object.insert(field.clone(), value.clone());
                save_version(tx, c, &field, &context, &value)?;
                applied = true;
            }
        }
        if applied {
            if c.table == Table::Accounts {
                let account: sap_core::account::Account = serde_json::from_value(current.clone())?;
                if account.id.0 != c.key || serde_json::to_value(account)? != current {
                    return Err(Error::Invalid("invalid account fields or key".into()));
                }
            }
            put_record(tx, c.table, &c.key, &current)?;
            report.applied += 1;
        } else {
            report.ignored += 1;
        }
    } else {
        if tombstone.as_ref().is_some_and(|v| v.change.hlc >= c.hlc) {
            report.ignored += 1;
            return Ok(());
        }
        if deleting {
            // A newer field keeps the record alive. Preserve its other fields so
            // a delayed tombstone cannot leave a partially typed Account behind.
            let current = record(tx, c.table, &c.key)?;
            let mut keep = false;
            if field_table(c.table)
                && let Some(Value::Object(fields)) = &current
            {
                for field in fields.keys() {
                    if version(tx, c, field)?.is_some_and(|v| v.change.hlc > c.hlc) {
                        keep = true;
                        break;
                    }
                }
            }
            if !keep {
                tx.execute(&format!("DELETE FROM {} WHERE key=?1", name(c.table)), [&c.key])?;
            }
        } else {
            crate::domain::validate_record(c, &value)?;
            if c.table == Table::Conflicts
                && value.get("resolved") == Some(&Value::Bool(false))
                && record(tx, c.table, &c.key)?.is_some_and(|old| old.get("resolved") == Some(&Value::Bool(true)))
            {
                report.ignored += 1;
                return Ok(());
            }
            put_record(tx, c.table, &c.key, &value)?;
        }
        save_version(tx, c, "", &context, &value)?;
        report.applied += 1;
        if c.table == Table::Threads
            && !deleting
            && let Some(old) = tombstone
            && !matches!(old.change.op, ChangeOp::Delete)
            && old.value != value
            && concurrent(c, &context, &old)
            && old.value.get("assignment") != value.get("assignment")
        {
            let event = EventView {
                id: format!(
                    "thread-divergence:{}:{}:{}:{}",
                    old.change.origin, old.change.seq, c.origin, c.seq
                ),
                at: Utc::now(),
                kind: EventKind::SyncConflict,
                message: "Concurrent thread assignments converged by HLC".into(),
                node: node.clone(),
                thread: ThreadKey::parse(&c.key),
                account: None,
            };
            write(
                tx,
                node,
                Table::Events,
                &event.id,
                ChangeOp::Append {
                    value: serde_json::to_value(&event)?,
                },
            )?;
        }
    }
    Ok(())
}

impl ChangeLog for Store {
    type Error = Error;
    fn node(&self) -> &NodeId {
        &self.node
    }
    fn cursor(&self) -> Result<SyncCursor> {
        cursor(&*self.connection()?)
    }
    fn changes_since(&self, since: &SyncCursor, limit: usize) -> Result<ChangeBatch> {
        let conn = self.connection()?;
        let floor: Option<String> = conn
            .query_row("SELECT value FROM metadata WHERE key='log_floor'", [], |r| r.get(0))
            .optional()?;
        let floor: SyncCursor = floor.map(|s| serde_json::from_str(&s)).transpose()?.unwrap_or_default();
        if floor
            .iter()
            .any(|(origin, seq)| since.get(origin).copied().unwrap_or(0) < *seq)
        {
            return Err(Error::Invalid(
                "cursor predates retained history: install a snapshot".into(),
            ));
        }
        let mut stmt = conn.prepare("SELECT value FROM op_log ORDER BY id")?;
        let mut rows = stmt.query([])?;
        let mut changes = Vec::new();
        let mut more = false;
        while let Some(row) = rows.next()? {
            let c: Change = serde_json::from_str(&row.get::<_, String>(0)?)?;
            if c.seq <= since.get(&c.origin).copied().unwrap_or(0) {
                continue;
            }
            if changes.len() == limit {
                more = true;
                break;
            }
            changes.push(c);
        }
        Ok(ChangeBatch {
            from: self.node.clone(),
            changes,
            more,
        })
    }
    fn apply_remote(&self, changes: &[Change]) -> Result<ApplyReport> {
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut seen = cursor(&tx)?;
        let mut report = ApplyReport::default();
        let mut hlc = clock(&tx)?;
        for c in changes {
            if c.origin != c.hlc.node
                || c.seq == 0
                || c.seq > i64::MAX as u64
                || c.hlc.counter == u32::MAX
                || sap_core::DateTime::from_timestamp_millis(c.hlc.wall_ms).is_none()
            {
                return Err(Error::Invalid("invalid change identity or HLC".into()));
            }
            let previous = seen.get(&c.origin).copied().unwrap_or(0);
            if c.seq <= previous {
                report.ignored += 1;
                continue;
            }
            if c.origin == self.node {
                return Err(Error::Invalid("remote change claims the local origin".into()));
            }
            if c.seq != previous + 1 {
                return Err(Error::Invalid(
                    "sequence gap: request missing changes or install a snapshot".into(),
                ));
            }
            let now = Utc::now().timestamp_millis();
            if hlc.wall_ms >= now && hlc.wall_ms >= c.hlc.wall_ms && hlc.counter == u32::MAX {
                return Err(Error::Invalid("HLC counter exhausted".into()));
            }
            hlc = hlc.observe(&c.hlc, now);
            // Persist first: a locally generated divergence event must see this change.
            persist(&tx, c)?;
            save_clock(&tx, &hlc)?;
            merge(&tx, &self.node, c, &mut report)?;
            hlc = clock(&tx)?;
            seen = cursor(&tx)?;
        }
        save_clock(&tx, &hlc)?;
        tx.commit()?;
        Ok(report)
    }
}
