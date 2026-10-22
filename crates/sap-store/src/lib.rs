//! Transactional SQLite state and replication. See the crate README for the wire
//! envelope, retention guarantees, and integration requirements.

mod domain;
mod log;
mod retention;
mod schema;

use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::Connection;
use sap_core::ids::NodeId;

pub use retention::{CompactionReport, Limits};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Sql(rusqlite::Error),
    Json(serde_json::Error),
    Io(std::io::Error),
    Invalid(String),
    Poisoned,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sql(e) => write!(f, "SQLite: {e}"),
            Self::Json(_) => f.write_str("invalid stored or replicated JSON"),
            Self::Io(e) => write!(f, "database file: {e}"),
            Self::Invalid(e) => f.write_str(e),
            Self::Poisoned => f.write_str("store mutex poisoned"),
        }
    }
}

impl std::error::Error for Error {}
impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sql(e)
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// A connection serialized by a mutex; each write uses an IMMEDIATE transaction.
/// Separate handles to the same file coordinate through SQLite's writer lock.
pub struct Store {
    pub(crate) conn: Mutex<Connection>,
    pub(crate) node: NodeId,
    pub(crate) limits: Limits,
}

impl Store {
    pub fn open(path: impl AsRef<Path>, node: NodeId, limits: Limits) -> Result<Self> {
        let path = path.as_ref();
        // Create securely before SQLite can create the WAL and SHM sidecars.
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            options.mode(0o600);
            let file = options.open(path)?;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        #[cfg(not(unix))]
        let _ = options.open(path)?;
        Self::initialize(Connection::open(path)?, node, limits)
    }

    pub fn open_in_memory(node: NodeId) -> Result<Self> {
        Self::initialize(Connection::open_in_memory()?, node, Limits::default())
    }

    fn initialize(mut conn: Connection, node: NodeId, limits: Limits) -> Result<Self> {
        conn.busy_timeout(Duration::from_secs(5))?;
        schema::migrate(&mut conn, &node)?;
        Ok(Self {
            conn: Mutex::new(conn),
            node,
            limits,
        })
    }

    pub(crate) fn connection(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|_| Error::Poisoned)
    }
}
