//! Coordinator persistence adapter: SQLite schema, migrations and repository methods.
//! The API is synchronous; async callers wrap each call in `spawn_blocking`.
//! It persists what the domain decided and makes no scheduling decisions itself.
// SHORTCUT: some methods have no caller until the coordinator wiring lands, remove this allow then.
#![allow(dead_code)]

mod advice;
mod controls;
mod devices;
mod events;
mod inputs;
mod jobs;
mod quota;
mod schema;
#[cfg(test)]
mod tests;
mod updates;

pub use updates::{NewEvent, UpdateWrites};

use std::path::{Path, PathBuf};

use idlemax_core::types::{JobId, SpecError, UnixMs};
use rusqlite::types::Type;
use rusqlite::{Connection, Row};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

const DAY_MS: i64 = 86_400_000;
/// Events, closed advice and applied-update records are kept this long.
const HISTORY_RETENTION_MS: i64 = 30 * DAY_MS;
const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const WAL_SIZE_LIMIT_BYTES: i64 = 8 * 1024 * 1024;

/// Errors from the coordinator store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The database was written by a newer idlemax; refuse to touch it.
    #[error("coord.db schema {found} is newer than this binary supports ({supported})")]
    SchemaTooNew { found: i64, supported: i64 },
    /// A value the store refuses to persist.
    #[error("invalid: {0}")]
    Invalid(String),
    #[error("invalid job spec: {0}")]
    InvalidSpec(#[source] SpecError),
    /// A stored value that no longer parses.
    #[error("corrupt store value: {0}")]
    Corrupt(String),
    #[error("job {0} not found")]
    JobNotFound(JobId),
    /// Another AI job already holds a lease on this account.
    #[error("AI account {0} already holds a lease")]
    AccountLeased(String),
}

pub type StoreResult<T> = Result<T, StoreError>;

/// Rows removed by one `prune` run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PruneReport {
    pub metrics: usize,
    pub quota: usize,
    pub events: usize,
    pub advice: usize,
    pub updates: usize,
}

/// The coordinator database. One connection, owned by one task or mutex.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens or creates `coord.db`, owner-only on unix, and applies pending migrations.
    pub fn open(path: &Path) -> StoreResult<Self> {
        create_private_file(path)?;
        let conn = Connection::open(path)?;
        Self::init(conn, true)
    }

    pub fn open_in_memory() -> StoreResult<Self> {
        Self::init(Connection::open_in_memory()?, false)
    }

    fn init(conn: Connection, on_disk: bool) -> StoreResult<Self> {
        conn.busy_timeout(BUSY_TIMEOUT)?;
        // auto_vacuum only takes effect before the first table exists.
        conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
        if on_disk {
            conn.pragma_update(None, "journal_mode", "WAL")?;
            conn.pragma_update(None, "journal_size_limit", WAL_SIZE_LIMIT_BYTES)?;
        }
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        let mut store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> StoreResult<()> {
        let found: i64 = self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        let supported = schema::MIGRATIONS.len() as i64;
        if found > supported {
            return Err(StoreError::SchemaTooNew { found, supported });
        }
        let applied = usize::try_from(found)
            .map_err(|_| StoreError::Corrupt(format!("schema version {found}")))?;
        for (done, sql) in (found..).zip(schema::MIGRATIONS.iter().skip(applied)) {
            let tx = self.conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", done + 1)?;
            tx.commit()?;
        }
        Ok(())
    }

    /// Drops metrics and quota history older than `metrics_retention_ms` (the newest
    /// quota snapshot per account window always stays), plus events and closed advice
    /// older than 30 days (and applied-update records of that age), then returns freed pages
    /// to the OS.
    pub fn prune(&mut self, now: UnixMs, metrics_retention_ms: i64) -> StoreResult<PruneReport> {
        let metrics_cutoff = now.saturating_sub(metrics_retention_ms);
        let history_cutoff = now.saturating_sub(HISTORY_RETENTION_MS);
        let tx = self.conn.transaction()?;
        let report = PruneReport {
            metrics: tx.execute(
                "DELETE FROM metrics_minute WHERE minute_ms < ?1",
                [metrics_cutoff],
            )?,
            quota: tx.execute(
                "DELETE FROM quota_snapshots AS s WHERE s.at_ms < ?1 AND s.at_ms < (
                   SELECT max(n.at_ms) FROM quota_snapshots AS n
                   WHERE n.account = s.account AND n.quota_window = s.quota_window)",
                [metrics_cutoff],
            )?,
            events: tx.execute("DELETE FROM events WHERE at_ms < ?1", [history_cutoff])?,
            advice: tx.execute(
                "DELETE FROM recommendations WHERE status <> 'open' AND at_ms < ?1",
                [history_cutoff],
            )?,
            updates: tx.execute(
                "DELETE FROM applied_updates WHERE at_ms < ?1",
                [history_cutoff],
            )?,
        };
        tx.commit()?;
        self.conn.execute_batch("PRAGMA incremental_vacuum")?;
        Ok(report)
    }
}

/// Creates the database file, or tightens an existing one, to mode 0600 so job specs
/// (env) stay private. SQLite gives new WAL and SHM files the same mode.
fn create_private_file(path: &Path) -> StoreResult<()> {
    let io_err = |source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    };
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(false);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let file = opts.open(path).map_err(io_err)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(io_err)?;
    }
    drop(file);
    Ok(())
}

/// u64 has no SQLite mapping without rusqlite's fallible_uint; saturate into i64.
fn u64_to_sql(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn u64_from_sql(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

fn json_text<T: Serialize>(v: &T) -> StoreResult<String> {
    Ok(serde_json::to_string(v)?)
}

/// snake_case text of a unit serde enum, e.g. EventKind::JobQueued -> "job_queued".
fn enum_text<T: Serialize>(v: &T) -> StoreResult<String> {
    match serde_json::to_value(v)? {
        Value::String(s) => Ok(s),
        other => Err(StoreError::Corrupt(format!(
            "enum is not a string: {other}"
        ))),
    }
}

fn conversion_err(
    idx: usize,
    e: impl Into<Box<dyn std::error::Error + Send + Sync>>,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(idx, Type::Text, e.into())
}

fn json_col<T: DeserializeOwned>(row: &Row<'_>, idx: usize) -> rusqlite::Result<T> {
    let text: String = row.get(idx)?;
    serde_json::from_str(&text).map_err(|e| conversion_err(idx, e))
}

fn opt_json_col<T: DeserializeOwned>(row: &Row<'_>, idx: usize) -> rusqlite::Result<Option<T>> {
    let text: Option<String> = row.get(idx)?;
    text.map(|t| serde_json::from_str(&t))
        .transpose()
        .map_err(|e| conversion_err(idx, e))
}

fn enum_col<T: DeserializeOwned>(row: &Row<'_>, idx: usize) -> rusqlite::Result<T> {
    let text: String = row.get(idx)?;
    serde_json::from_value(Value::String(text)).map_err(|e| conversion_err(idx, e))
}

/// SQLite LIMIT value: 0 means no limit.
fn sql_limit(limit: u32) -> i64 {
    if limit == 0 {
        -1
    } else {
        i64::from(limit)
    }
}
