//! SQLite access.
//!
//! One connection behind a mutex: this is a single-user console, every query is
//! sub-millisecond, and keeping a real async pool would only buy contention.
//! The rule is: never hold the lock across a subprocess call or file copy.

pub mod repo;

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;

use crate::error::AppError;

const SCHEMA: &str = include_str!("schema.sql");
const SCHEMA_VERSION: &str = "1";

#[derive(Clone)]
pub struct Db {
    inner: Arc<Mutex<Connection>>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self, AppError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).map_err(|e| AppError::internal("open db", e))?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self, AppError> {
        let conn = Connection::open_in_memory().map_err(|e| AppError::internal("open db", e))?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self, AppError> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;
             PRAGMA synchronous = NORMAL;",
        )
        .map_err(|e| AppError::internal("db pragmas", e))?;
        conn.execute_batch(SCHEMA)
            .map_err(|e| AppError::internal("db schema", e))?;
        conn.execute(
            "INSERT INTO schema_meta(key, value) VALUES ('version', ?1)
             ON CONFLICT(key) DO NOTHING",
            [SCHEMA_VERSION],
        )?;
        Ok(Self {
            inner: Arc::new(Mutex::new(conn)),
        })
    }

    /// Run a closure with the connection. The mutex is poisoned only if a
    /// previous closure panicked while holding it; we recover instead of
    /// turning every later request into a 500.
    pub fn with<T>(&self, f: impl FnOnce(&mut Connection) -> rusqlite::Result<T>) -> Result<T, AppError> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut guard).map_err(AppError::from)
    }

    /// Same as [`Db::with`], but for closures that fail with a richer error.
    pub fn try_with<T, E: Into<AppError>>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<T, E>,
    ) -> Result<T, AppError> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut guard).map_err(Into::into)
    }
}

/// Current time as unix milliseconds — the only timestamp format the schema stores.
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Render a stored timestamp for the UI (`2026-10-23 07:27`).
pub fn fmt_ms(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "-".into())
}

pub fn fmt_ms_opt(ms: Option<i64>) -> String {
    ms.map(fmt_ms).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_idempotent() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| c.execute_batch(SCHEMA)).unwrap();
        let n: i64 = db
            .with(|c| c.query_row("SELECT count(*) FROM schema_meta", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn foreign_keys_are_enforced() {
        let db = Db::open_in_memory().unwrap();
        let res = db.with(|c| {
            c.execute(
                "INSERT INTO backup_run(task_id, status, started_at) VALUES (999, 'running', 0)",
                [],
            )
        });
        assert!(res.is_err(), "FK to a missing task must fail");
    }

    #[test]
    fn status_check_constraint() {
        let db = Db::open_in_memory().unwrap();
        let res = db.with(|c| {
            c.execute(
                "INSERT INTO update_run(target, status, started_at) VALUES ('immich', 'bogus', 0)",
                [],
            )
        });
        assert!(res.is_err(), "unknown status must be rejected");
    }

    #[test]
    fn timestamps_roundtrip() {
        assert_eq!(fmt_ms(0), "1970-01-01 00:00");
        assert_eq!(fmt_ms_opt(None), "");
    }
}
