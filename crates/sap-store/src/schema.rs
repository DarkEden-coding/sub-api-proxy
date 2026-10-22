use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sap_core::{hlc::Hlc, ids::NodeId};

use crate::{Error, Result};

pub(crate) const RECORD_TABLES: &[&str] = &["accounts", "settings", "client_access", "threads", "conflicts"];
pub(crate) const APPEND_TABLES: &[&str] = &["quota_observations", "usage_buckets", "events"];

pub(crate) fn migrate(conn: &mut Connection, node: &NodeId) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: u32 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version > 1 {
        return Err(Error::Invalid("database schema is newer than this store".into()));
    }
    if version == 0 {
        tx.execute_batch(
            "
            CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE cursor (origin TEXT PRIMARY KEY, seq INTEGER NOT NULL);
            CREATE TABLE peer_cursors (peer TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE op_log (
                id INTEGER PRIMARY KEY, origin TEXT NOT NULL, seq INTEGER NOT NULL,
                wall_ms INTEGER NOT NULL, value TEXT NOT NULL, UNIQUE(origin, seq));
            CREATE TABLE field_versions (
                table_name TEXT NOT NULL, key TEXT NOT NULL, field TEXT NOT NULL,
                value TEXT NOT NULL, PRIMARY KEY(table_name, key, field));
            CREATE TABLE node_credentials (
                account TEXT PRIMARY KEY, value TEXT NOT NULL);
        ",
        )?;
        for table in RECORD_TABLES {
            tx.execute_batch(&format!(
                "CREATE TABLE {table} (key TEXT PRIMARY KEY, value TEXT NOT NULL)"
            ))?;
        }
        for table in APPEND_TABLES {
            tx.execute_batch(&format!(
                "
                CREATE TABLE {table} (
                    origin TEXT NOT NULL, seq INTEGER NOT NULL, at_ms INTEGER NOT NULL,
                    account TEXT NOT NULL, value TEXT NOT NULL, PRIMARY KEY(origin, seq));
                CREATE INDEX {table}_age ON {table}(at_ms);
                CREATE INDEX {table}_account ON {table}(account, at_ms);
            "
            ))?;
        }
        tx.pragma_update(None, "user_version", 1)?;
    }
    let owner: Option<String> = tx
        .query_row("SELECT value FROM metadata WHERE key='node'", [], |r| r.get(0))
        .optional()?;
    if let Some(owner) = owner {
        if owner != node.0 {
            return Err(Error::Invalid("database belongs to another node".into()));
        }
    } else {
        tx.execute("INSERT INTO metadata VALUES ('node', ?1)", [&node.0])?;
        tx.execute(
            "INSERT INTO metadata VALUES ('hlc', ?1)",
            params![serde_json::to_string(&Hlc::zero(node.clone()))?],
        )?;
    }
    tx.commit()?;
    Ok(())
}
