//! `scheduled_job` persistence.

use crate::db::{now_ms, Db};
use crate::error::AppError;

#[derive(Debug, Clone)]
pub struct ScheduledJob {
    pub id: i64,
    pub name: String,
    pub job_type: String,
    pub target: Option<String>,
    pub cron: Option<String>,
    pub command: Option<String>,
    pub enabled: bool,
    pub notes: Option<String>,
    pub last_run_at: Option<i64>,
    pub last_status: Option<String>,
}

const JOB_COLUMNS: &str = "id, name, job_type, target, cron, command, enabled, notes, last_run_at, last_status";

fn job_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScheduledJob> {
    Ok(ScheduledJob {
        id: row.get(0)?,
        name: row.get(1)?,
        job_type: row.get(2)?,
        target: row.get(3)?,
        cron: row.get(4)?,
        command: row.get(5)?,
        enabled: row.get::<_, i64>(6)? != 0,
        notes: row.get(7)?,
        last_run_at: row.get(8)?,
        last_status: row.get(9)?,
    })
}

pub fn list_jobs(db: &Db) -> Result<Vec<ScheduledJob>, AppError> {
    db.with(|c| {
        let mut stmt = c.prepare(&format!("SELECT {JOB_COLUMNS} FROM scheduled_job ORDER BY id"))?;
        let rows = stmt.query_map([], job_from_row)?;
        rows.collect()
    })
}

pub fn get_job(db: &Db, id: i64) -> Result<Option<ScheduledJob>, AppError> {
    db.with(|c| {
        let mut stmt = c.prepare(&format!("SELECT {JOB_COLUMNS} FROM scheduled_job WHERE id = ?1"))?;
        let mut rows = stmt.query_map([id], job_from_row)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    })
}

pub struct JobInput<'a> {
    pub name: &'a str,
    pub job_type: &'a str,
    pub target: Option<&'a str>,
    pub cron: Option<&'a str>,
    pub command: Option<&'a str>,
    pub enabled: bool,
    pub notes: Option<&'a str>,
}

pub fn create_job(db: &Db, input: &JobInput<'_>) -> Result<i64, AppError> {
    let now = now_ms();
    db.with(|c| {
        c.execute(
            "INSERT INTO scheduled_job(name, job_type, target, cron, command, enabled, notes, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            (
                input.name,
                input.job_type,
                input.target,
                input.cron,
                input.command,
                input.enabled as i64,
                input.notes,
                now,
            ),
        )?;
        Ok(c.last_insert_rowid())
    })
}

pub fn update_job(db: &Db, id: i64, input: &JobInput<'_>) -> Result<(), AppError> {
    let now = now_ms();
    db.with(|c| {
        c.execute(
            "UPDATE scheduled_job
                SET name = ?1, job_type = ?2, target = ?3, cron = ?4, command = ?5, enabled = ?6, notes = ?7
              WHERE id = ?8",
            (
                input.name,
                input.job_type,
                input.target,
                input.cron,
                input.command,
                input.enabled as i64,
                input.notes,
                id,
            ),
        ).map(|_| ())
    })
}

pub fn delete_job(db: &Db, id: i64) -> Result<(), AppError> {
    db.with(|c| c.execute("DELETE FROM scheduled_job WHERE id = ?1", [id]).map(|_| ()))
}

pub fn mark_job_run(db: &Db, job_id: i64, status: &str, at: i64) -> Result<(), AppError> {
    db.with(|c| {
        c.execute(
            "UPDATE scheduled_job SET last_run_at = ?1, last_status = ?2 WHERE id = ?3",
            (at, status, job_id),
        ).map(|_| ())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_crud_roundtrip() {
        let db = Db::open_in_memory().unwrap();
        let id = create_job(&db, &JobInput {
            name: "nightly backup",
            job_type: "custom_shell",
            target: None,
            cron: Some("0 3 * * *"),
            command: Some("echo hello"),
            enabled: true,
            notes: Some("test"),
        }).unwrap();
        let job = get_job(&db, id).unwrap().unwrap();
        assert_eq!(job.name, "nightly backup");
        assert_eq!(job.cron.as_deref(), Some("0 3 * * *"));
        assert!(job.enabled);
        assert_eq!(job.command.as_deref(), Some("echo hello"));

        delete_job(&db, id).unwrap();
        assert!(get_job(&db, id).unwrap().is_none());
    }
}