//! `update_run` and `preserve` persistence.

use crate::db::{now_ms, Db};
use crate::error::AppError;
use serde_json::json;

/// What the UI shows in the "recent runs" table.
#[derive(Debug, Clone)]
pub struct UpdateRun {
    pub id: i64,
    pub target: String,
    pub from_version: Option<String>,
    pub to_version: Option<String>,
    pub status: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub log_path: Option<String>,
    pub backup_file: Option<String>,
    pub steps: Option<String>,
}

/// What the UI shows in the "preserves" table.
#[derive(Debug, Clone)]
pub struct Preserve {
    pub id: i64,
    pub target: String,
    pub from_version: Option<String>,
    pub status: String,
    pub storage_dir: Option<String>,
    pub db_dump: Option<String>,
    pub config_path: Option<String>,
    pub extra_files: Option<String>,
    pub git_before: Option<String>,
    pub git_stash: Option<String>,
    pub log_path: Option<String>,
    pub error: Option<String>,
    pub steps: Option<String>,
    pub created_at: i64,
    pub finished_at: Option<i64>,
    pub restored_at: Option<i64>,
    pub note: Option<String>,
}

const RUN_COLUMNS: &str = "id, target, from_version, to_version, status, started_at, finished_at, log_path, backup_file, steps";

fn run_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<UpdateRun> {
    Ok(UpdateRun {
        id: row.get(0)?,
        target: row.get(1)?,
        from_version: row.get(2)?,
        to_version: row.get(3)?,
        status: row.get(4)?,
        started_at: row.get(5)?,
        finished_at: row.get(6)?,
        log_path: row.get(7)?,
        backup_file: row.get(8)?,
        steps: row.get(9)?,
    })
}

pub fn recent_runs(db: &Db, limit: i64) -> Result<Vec<UpdateRun>, AppError> {
    db.with(|c| {
        let mut stmt = c.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM update_run ORDER BY started_at DESC, id DESC LIMIT ?1"
        ))?;
        let rows = stmt.query_map([limit], run_from_row)?;
        rows.collect()
    })
}

pub fn get_run(db: &Db, id: i64) -> Result<Option<UpdateRun>, AppError> {
    db.with(|c| {
        let mut stmt = c.prepare(&format!("SELECT {RUN_COLUMNS} FROM update_run WHERE id = ?1"))?;
        let mut rows = stmt.query_map([id], run_from_row)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    })
}

pub fn create_run(
    db: &Db,
    target: &str,
    tag: &str,
    from_version: &str,
    log_path: &str,
) -> Result<i64, AppError> {
    let now = now_ms();
    db.try_with(|c| {
        // Check for existing running update for this target
        let mut stmt = c.prepare("SELECT id FROM update_run WHERE target = ?1 AND status = 'running'")?;
        let mut rows = stmt.query([target])?;
        if rows.next()?.is_some() {
            return Err(AppError::BadRequest(format!("{target} update already running")));
        }

        c.execute(
            "INSERT INTO update_run(target, from_version, to_version, status, started_at, log_path, steps)
             VALUES (?1, ?2, ?3, 'running', ?4, ?5, '[]')",
            (target, from_version, tag, now, log_path),
        )?;
        Ok(c.last_insert_rowid())
    })
}

pub fn finish_run(
    db: &Db,
    id: i64,
    status: &str,
    to_version: Option<&str>,
    steps: &[crate::updates::steps::Step],
) -> Result<(), AppError> {
    let now = now_ms();
    db.with(|c| {
        c.execute(
            "UPDATE update_run SET status = ?1, to_version = ?2, finished_at = ?3, steps = ?4 WHERE id = ?5",
            (status, to_version, now, json!(steps).to_string(), id),
        ).map(|_| ())
    })
}

/// Preserve table operations
const PRESERVE_COLUMNS: &str = "id, target, from_version, status, storage_dir, db_dump, config_path, extra_files, git_before, git_stash, log_path, error, steps, created_at, finished_at, restored_at, note";

fn preserve_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Preserve> {
    Ok(Preserve {
        id: row.get(0)?,
        target: row.get(1)?,
        from_version: row.get(2)?,
        status: row.get(3)?,
        storage_dir: row.get(4)?,
        db_dump: row.get(5)?,
        config_path: row.get(6)?,
        extra_files: row.get(7)?,
        git_before: row.get(8)?,
        git_stash: row.get(9)?,
        log_path: row.get(10)?,
        error: row.get(11)?,
        steps: row.get(12)?,
        created_at: row.get(13)?,
        finished_at: row.get(14)?,
        restored_at: row.get(15)?,
        note: row.get(16)?,
    })
}

pub fn list_preserves(db: &Db, target: Option<&str>, limit: i64) -> Result<Vec<Preserve>, AppError> {
    db.with(|c| {
        let sql = match target {
            Some(t) => format!("SELECT {PRESERVE_COLUMNS} FROM preserve WHERE target = ?1 ORDER BY created_at DESC, id DESC LIMIT ?2"),
            None => format!("SELECT {PRESERVE_COLUMNS} FROM preserve ORDER BY created_at DESC, id DESC LIMIT ?1"),
        };
        let mut stmt = c.prepare(&sql)?;
        let rows = match target {
            Some(t) => {
                let limit_str = limit.to_string();
                stmt.query_map([t, &limit_str], preserve_from_row)?
            }
            None => stmt.query_map([limit], preserve_from_row)?,
        };
        rows.collect()
    })
}

pub fn get_preserve(db: &Db, id: i64) -> Result<Option<Preserve>, AppError> {
    db.with(|c| {
        let mut stmt = c.prepare(&format!("SELECT {PRESERVE_COLUMNS} FROM preserve WHERE id = ?1"))?;
        let mut rows = stmt.query_map([id], preserve_from_row)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    })
}

pub fn create_preserve(
    db: &Db,
    target: &str,
    log_path: &str,
    storage_dir: &str,
) -> Result<i64, AppError> {
    let now = now_ms();
    db.with(|c| {
        c.execute(
            "INSERT INTO preserve(target, status, log_path, storage_dir, created_at, steps)
             VALUES (?1, 'pending', ?2, ?3, ?4, '[]')",
            (target, log_path, storage_dir, now),
        )?;
        Ok(c.last_insert_rowid())
    })
}

pub fn update_preserve_status(db: &Db, id: i64, status: &str) -> Result<(), AppError> {
    db.with(|c| c.execute("UPDATE preserve SET status = ?1 WHERE id = ?2", (status, id)).map(|_| ()))
}

pub fn finish_preserve(
    db: &Db,
    id: i64,
    status: &str,
    from_version: Option<&str>,
    git_before: Option<&str>,
    git_stash: Option<&str>,
    db_dump: Option<&str>,
    config_path: Option<&str>,
    extra_files: Option<&str>,
    steps: &[crate::updates::steps::Step],
) -> Result<(), AppError> {
    let now = now_ms();
    db.with(|c| {
        c.execute(
            "UPDATE preserve
                SET status = ?1, from_version = ?2, git_before = ?3, git_stash = ?4,
                    db_dump = ?5, config_path = ?6, extra_files = ?7, finished_at = ?8, steps = ?9
              WHERE id = ?10",
            (status, from_version, git_before, git_stash, db_dump, config_path, extra_files, now, json!(steps).to_string(), id),
        ).map(|_| ())
    })
}

pub fn set_preserve_error(db: &Db, id: i64, error: &str, steps: &[crate::updates::steps::Step]) -> Result<(), AppError> {
    let now = now_ms();
    db.with(|c| {
        c.execute(
            "UPDATE preserve SET status = 'failed', error = ?1, finished_at = ?2, steps = ?3 WHERE id = ?4",
            (error, now, json!(steps).to_string(), id),
        ).map(|_| ())
    })
}

pub fn mark_preserve_restored(db: &Db, id: i64, steps: &[crate::updates::steps::Step]) -> Result<(), AppError> {
    let now = now_ms();
    db.with(|c| {
        c.execute(
            "UPDATE preserve SET status = 'restored', restored_at = ?1, steps = ?2 WHERE id = ?3",
            (now, json!(steps).to_string(), id),
        ).map(|_| ())
    })
}

pub fn delete_preserve(db: &Db, id: i64) -> Result<(), AppError> {
    db.with(|c| c.execute("DELETE FROM preserve WHERE id = ?1", [id]).map(|_| ()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_run_crud() {
        let db = Db::open_in_memory().unwrap();
        let id = create_run(&db, "immich", "v3.4.0", "v3.3.5", "/tmp/log").unwrap();
        let run = get_run(&db, id).unwrap().unwrap();
        assert_eq!(run.target, "immich");
        assert_eq!(run.from_version.as_deref(), Some("v3.3.5"));
        assert_eq!(run.to_version.as_deref(), Some("v3.4.0"));
        assert_eq!(run.status, "running");

        finish_run(&db, id, "success", Some("v3.4.0"), &[]).unwrap();
        let run = get_run(&db, id).unwrap().unwrap();
        assert_eq!(run.status, "success");
        assert!(run.finished_at.is_some());

        let runs = recent_runs(&db, 10).unwrap();
        assert_eq!(runs.len(), 1);
    }

    #[test]
    fn preserve_crud() {
        let db = Db::open_in_memory().unwrap();
        let id = create_preserve(&db, "immich", "/tmp/log", "/mnt/backup/preserves/immich/1").unwrap();
        let p = get_preserve(&db, id).unwrap().unwrap();
        assert_eq!(p.target, "immich");
        assert_eq!(p.status, "pending");

        finish_preserve(&db, id, "ready", Some("v3.3.5"), Some("v3.3.5"), Some("stash123"),
            Some("/mnt/backup/preserves/immich/1/immich_db.dump"), Some("/mnt/backup/preserves/immich/1/.env"),
            Some("{}"), &[]).unwrap();
        let p = get_preserve(&db, id).unwrap().unwrap();
        assert_eq!(p.status, "ready");
        assert_eq!(p.from_version.as_deref(), Some("v3.3.5"));
    }
}