//! `backup_task` and `backup_run` persistence.

use crate::db::{now_ms, Db};
use crate::error::AppError;

/// The kinds the UI offers. `all` runs the repository's backup script,
/// `custom` runs an operator-supplied shell command; the rest are built in.
pub const KINDS: [&str; 8] = [
    "immich_db",
    "nextcloud_db",
    "immich_config",
    "nextcloud_config",
    "stalwart_db",
    "stalwart_config",
    "all",
    "custom",
];

pub fn valid_kind(kind: &str) -> bool {
    KINDS.contains(&kind)
}

#[derive(Debug, Clone)]
pub struct BackupTask {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub cron: Option<String>,
    pub command: Option<String>,
    pub enabled: bool,
    pub retention_days: i64,
    pub last_run_at: Option<i64>,
    pub last_status: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BackupRun {
    pub id: i64,
    pub task_id: Option<i64>,
    pub status: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub outdir: Option<String>,
    pub files: Option<String>,
    pub exit_code: Option<i64>,
}

const TASK_COLUMNS: &str = "id, name, kind, cron, command, enabled, retention_days, last_run_at, last_status";

fn task_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BackupTask> {
    Ok(BackupTask {
        id: row.get(0)?,
        name: row.get(1)?,
        kind: row.get(2)?,
        cron: row.get(3)?,
        command: row.get(4)?,
        enabled: row.get::<_, i64>(5)? != 0,
        retention_days: row.get(6)?,
        last_run_at: row.get(7)?,
        last_status: row.get(8)?,
    })
}

pub fn list_tasks(db: &Db) -> Result<Vec<BackupTask>, AppError> {
    db.with(|c| {
        let sql = format!("SELECT {TASK_COLUMNS} FROM backup_task ORDER BY id");
        let mut stmt = c.prepare(&sql)?;
        let rows = stmt.query_map([], task_from_row)?;
        rows.collect()
    })
}

pub fn get_task(db: &Db, id: i64) -> Result<Option<BackupTask>, AppError> {
    db.with(|c| {
        let sql = format!("SELECT {TASK_COLUMNS} FROM backup_task WHERE id = ?1");
        let mut stmt = c.prepare(&sql)?;
        let mut rows = stmt.query([id])?;
        match rows.next()? {
            Some(row) => Ok(Some(task_from_row(row)?)),
            None => Ok(None),
        }
    })
}

pub struct TaskInput<'a> {
    pub name: &'a str,
    pub kind: &'a str,
    pub cron: Option<&'a str>,
    pub command: Option<&'a str>,
    pub enabled: bool,
    pub retention_days: i64,
}

pub fn create_task(db: &Db, input: &TaskInput<'_>) -> Result<i64, AppError> {
    let now = now_ms();
    db.with(|c| {
        c.execute(
            "INSERT INTO backup_task(name, kind, cron, command, enabled, retention_days, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            (
                input.name,
                input.kind,
                input.cron,
                input.command,
                input.enabled as i64,
                input.retention_days,
                now,
            ),
        )?;
        Ok(c.last_insert_rowid())
    })
}

pub fn update_task(db: &Db, id: i64, input: &TaskInput<'_>) -> Result<(), AppError> {
    let now = now_ms();
    db.with(|c| {
        c.execute(
            "UPDATE backup_task
                SET name = ?1, kind = ?2, cron = ?3, command = ?4, enabled = ?5,
                    retention_days = ?6, updated_at = ?7
              WHERE id = ?8",
            (
                input.name,
                input.kind,
                input.cron,
                input.command,
                input.enabled as i64,
                input.retention_days,
                now,
                id,
            ),
        )
        .map(|_| ())
    })
}

pub fn delete_task(db: &Db, id: i64) -> Result<(), AppError> {
    db.with(|c| c.execute("DELETE FROM backup_task WHERE id = ?1", [id]).map(|_| ()))
}

/// Open a run row before the background thread starts, so the UI can show a
/// `running` entry immediately.
pub fn create_run(db: &Db, task_id: i64) -> Result<i64, AppError> {
    let now = now_ms();
    db.with(|c| {
        c.execute(
            "INSERT INTO backup_run(task_id, status, started_at) VALUES (?1, 'running', ?2)",
            (task_id, now),
        )?;
        Ok(c.last_insert_rowid())
    })
}

pub struct RunOutcome<'a> {
    pub status: &'a str,
    pub finished_at: i64,
    pub outdir: Option<&'a str>,
    pub files: &'a str,
    pub log: &'a str,
    pub exit_code: i64,
}

pub fn finish_run(db: &Db, id: i64, outcome: &RunOutcome<'_>) -> Result<(), AppError> {
    db.with(|c| {
        c.execute(
            "UPDATE backup_run
                SET status = ?1, finished_at = ?2, outdir = ?3, files = ?4, log = ?5, exit_code = ?6
              WHERE id = ?7",
            (
                outcome.status,
                outcome.finished_at,
                outcome.outdir,
                outcome.files,
                outcome.log,
                outcome.exit_code,
                id,
            ),
        )
        .map(|_| ())
    })
}

pub fn mark_task_run(db: &Db, task_id: i64, status: &str, at: i64) -> Result<(), AppError> {
    db.with(|c| {
        c.execute(
            "UPDATE backup_task SET last_run_at = ?1, last_status = ?2 WHERE id = ?3",
            (at, status, task_id),
        )
        .map(|_| ())
    })
}

pub fn get_run(db: &Db, id: i64) -> Result<Option<BackupRun>, AppError> {
    db.with(|c| {
        let mut stmt = c.prepare(
            "SELECT id, task_id, status, started_at, finished_at, outdir, files, exit_code
               FROM backup_run WHERE id = ?1",
        )?;
        let mut rows = stmt.query([id])?;
        match rows.next()? {
            Some(row) => Ok(Some(BackupRun {
                id: row.get(0)?,
                task_id: row.get(1)?,
                status: row.get(2)?,
                started_at: row.get(3)?,
                finished_at: row.get(4)?,
                outdir: row.get(5)?,
                files: row.get(6)?,
                exit_code: row.get(7)?,
            })),
            None => Ok(None),
        }
    })
}

pub fn recent_runs(db: &Db, limit: i64) -> Result<Vec<BackupRun>, AppError> {
    db.with(|c| {
        let mut stmt = c.prepare(
            "SELECT id, task_id, status, started_at, finished_at, outdir, files, exit_code
               FROM backup_run ORDER BY started_at DESC, id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], |row| {
            Ok(BackupRun {
                id: row.get(0)?,
                task_id: row.get(1)?,
                status: row.get(2)?,
                started_at: row.get(3)?,
                finished_at: row.get(4)?,
                outdir: row.get(5)?,
                files: row.get(6)?,
                exit_code: row.get(7)?,
            })
        })?;
        rows.collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> TaskInput<'static> {
        TaskInput {
            name: "nightly immich",
            kind: "immich_db",
            cron: Some("0 4 * * *"),
            command: None,
            enabled: true,
            retention_days: 14,
        }
    }

    #[test]
    fn task_crud_roundtrip() {
        let db = Db::open_in_memory().unwrap();
        let id = create_task(&db, &input()).unwrap();
        let task = get_task(&db, id).unwrap().unwrap();
        assert_eq!(task.name, "nightly immich");
        assert_eq!(task.cron.as_deref(), Some("0 4 * * *"));
        assert!(task.enabled);
        assert_eq!(task.retention_days, 14);

        let changed = TaskInput {
            name: "renamed",
            kind: "nextcloud_db",
            cron: None,
            command: None,
            enabled: false,
            retention_days: 30,
        };
        update_task(&db, id, &changed).unwrap();
        let task = get_task(&db, id).unwrap().unwrap();
        assert_eq!(task.name, "renamed");
        assert_eq!(task.kind, "nextcloud_db");
        assert_eq!(task.cron, None);
        assert!(!task.enabled);

        delete_task(&db, id).unwrap();
        assert!(get_task(&db, id).unwrap().is_none());
    }

    #[test]
    fn runs_start_and_finish() {
        let db = Db::open_in_memory().unwrap();
        let task_id = create_task(&db, &input()).unwrap();
        let run_id = create_run(&db, task_id).unwrap();
        let run = get_run(&db, run_id).unwrap().unwrap();
        assert_eq!(run.status, "running");
        assert_eq!(run.task_id, Some(task_id));

        finish_run(
            &db,
            run_id,
            &RunOutcome {
                status: "success",
                finished_at: now_ms(),
                outdir: Some("/mnt/backup/backups"),
                files: "[]",
                log: "ok",
                exit_code: 0,
            },
        )
        .unwrap();
        let run = get_run(&db, run_id).unwrap().unwrap();
        assert_eq!(run.status, "success");
        assert_eq!(run.exit_code, Some(0));
        assert_eq!(recent_runs(&db, 10).unwrap().len(), 1);
    }

    #[test]
    fn kind_whitelist() {
        assert!(valid_kind("stalwart_db"));
        assert!(valid_kind("custom"));
        assert!(!valid_kind("rm -rf"));
        assert_eq!(KINDS.len(), 8);
    }
}
